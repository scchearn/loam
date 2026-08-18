#!/usr/bin/env python3
"""Self-check for the enrollment signer's availability contract.

Runs signer.py for real over TLS on a loopback port with a throwaway
self-signed certificate, and asserts the property that failed in production:
a client that connects and then stalls must not stop the signer serving
everybody else.

Dependency-free by construction, like the signer itself: Python 3 stdlib plus
the `openssl` the deploy already manages. Run it directly:

    python3 deploy/mqtt-broker/enroll/test_signer.py
"""

import http.client
import json
import os
import socket
import ssl
import subprocess
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
SIGNER = os.path.join(HERE, "signer.py")
PASSWORD = "correct-horse-battery-staple"
# Generous enough that a slow CI runner is not a false failure, tight enough
# that a wedged accept loop is not mistaken for slowness.
DEADLINE_SECONDS = 15.0


def make_self_signed(work):
    """A throwaway server certificate; this test never verifies the chain."""
    cert = os.path.join(work, "server.pem")
    key = os.path.join(work, "server.key")
    subprocess.run(
        [
            "openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes",
            "-keyout", key, "-out", cert, "-days", "1",
            "-subj", "/CN=localhost",
            "-addext", "subjectAltName=IP:127.0.0.1",
        ],
        check=True,
        capture_output=True,
    )
    return cert, key


def free_port():
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


def start_signer(work, port):
    cert, key = make_self_signed(work)
    password_file = os.path.join(work, "enroll-password")
    with open(password_file, "w", encoding="ascii") as handle:
        handle.write(PASSWORD + "\n")
    env = dict(os.environ)
    env.update(
        ENROLL_PKI_DIR=work,
        ENROLL_PASSWORD_FILE=password_file,
        ENROLL_CERT_FILE=cert,
        ENROLL_KEY_FILE=key,
        ENROLL_PORT=str(port),
        ENROLL_BIND_ADDRESS="127.0.0.1",
        # The stall must be resolved by the accept loop staying free, not by
        # the stalled client timing out first.
        ENROLL_CONNECTION_TIMEOUT_SECONDS="60",
        # High enough that the test's own requests never trip the limiter.
        ENROLL_RATE_LIMIT="100",
    )
    child = subprocess.Popen([sys.executable, SIGNER], env=env)
    deadline = time.monotonic() + 20.0
    while time.monotonic() < deadline:
        if child.poll() is not None:
            raise AssertionError(f"signer exited during startup: {child.returncode}")
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=0.5):
                return child
        except OSError:
            time.sleep(0.1)
    child.kill()
    raise AssertionError(f"signer never listened on 127.0.0.1:{port}")


def post_enroll(port, password, timeout):
    """One real HTTPS enrollment request. Returns the response status.

    A wrong password is a complete, cheap round trip through accept, TLS,
    routing, and the constant-time check — everything this test cares about —
    without needing a CA to sign against.
    """
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
    context.check_hostname = False
    context.verify_mode = ssl.CERT_NONE
    connection = http.client.HTTPSConnection(
        "127.0.0.1", port, context=context, timeout=timeout
    )
    try:
        body = json.dumps({"password": password, "csr": "not-a-real-csr"})
        connection.request("POST", "/v1/enroll", body=body)
        return connection.getresponse().status
    finally:
        connection.close()


def main():
    port = free_port()
    with tempfile.TemporaryDirectory(prefix="loam-signer-test-") as work:
        child = start_signer(work, port)
        stalled = None
        try:
            assert post_enroll(port, "wrong", 10.0) == 401, "baseline request failed"

            # The regression: connect, complete the TCP handshake, then send
            # nothing at all. With the TLS handshake on the accept loop this
            # single silent client stops the signer accepting anything.
            stalled = socket.create_connection(("127.0.0.1", port), timeout=10.0)

            started = time.monotonic()
            try:
                status = post_enroll(port, "wrong", DEADLINE_SECONDS)
            except OSError as error:
                raise AssertionError(
                    "the signer stopped serving behind one stalled client "
                    f"(the #93 wedge): {error!r}"
                ) from error
            elapsed = time.monotonic() - started
            assert status == 401, f"expected 401 behind a stalled client, got {status}"
            assert elapsed < DEADLINE_SECONDS, f"served but took {elapsed:.1f}s"

            # And the signer keeps serving after the stall, not just once.
            assert post_enroll(port, PASSWORD, DEADLINE_SECONDS) == 500, (
                "a valid password should reach signing and fail there "
                "(no CA in this fixture), proving the path is still live"
            )
            print(f"ok: served behind a stalled client in {elapsed:.2f}s")
        finally:
            if stalled is not None:
                stalled.close()
            child.terminate()
            child.wait(timeout=10)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
