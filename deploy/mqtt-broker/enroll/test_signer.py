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
import select
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


def start_signer(work, port, timeouts=None):
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
        # By default the stall must be resolved by the accept loop staying
        # free, not by the stalled client timing out first — so the bounds are
        # pushed out of the way. The bounds get their own case below, which
        # passes its own values.
        ENROLL_CONNECTION_TIMEOUT_SECONDS="60",
        ENROLL_CONNECTION_MAX_SECONDS="120",
        # High enough that the test's own requests never trip the limiter.
        ENROLL_RATE_LIMIT="100",
    )
    env.update(timeouts or {})
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


def assert_dropped_within(port, limit, what):
    """Connect, say nothing, and require the signer to hang up within `limit`.

    `recv` returning b"" is the server having closed or shut down the
    connection. A socket read timeout here means the signer did not.
    """
    client = socket.create_connection(("127.0.0.1", port), timeout=limit)
    try:
        started = time.monotonic()
        try:
            assert client.recv(1) == b"", f"{what}: expected the signer to hang up"
        except socket.timeout:
            raise AssertionError(
                f"{what}: the signer held the connection past {limit:.0f}s; "
                "an unbounded connection is one worker thread and one fd a "
                "silent client can hold for free"
            ) from None
        return time.monotonic() - started
    finally:
        client.close()


def check_bounds_drop_a_silent_client():
    """The other half of the fix: a connection that goes nowhere is dropped.

    The accept-loop case above deliberately disables the bounds, so without
    this the timeout half of the fix has no coverage at all.
    """
    # A client that never starts the TLS handshake trips the idle timeout.
    port = free_port()
    with tempfile.TemporaryDirectory(prefix="loam-signer-idle-") as work:
        child = start_signer(
            work,
            port,
            {"ENROLL_CONNECTION_TIMEOUT_SECONDS": "2", "ENROLL_CONNECTION_MAX_SECONDS": "60"},
        )
        try:
            elapsed = assert_dropped_within(port, 20.0, "idle timeout")
            print(f"ok: idle client dropped after {elapsed:.2f}s")
        finally:
            child.terminate()
            child.wait(timeout=10)

    # And a slowloris — a client that completes the handshake and then dribbles
    # a request header one byte at a time, faster than the idle timeout. Every
    # byte rearms that timeout, so only the ceiling can end this. The TLS
    # handshake must be a real one: garbage bytes would be refused by the
    # record parser in about a second and prove nothing about the ceiling.
    port = free_port()
    with tempfile.TemporaryDirectory(prefix="loam-signer-drip-") as work:
        child = start_signer(
            work,
            port,
            {
                "ENROLL_CONNECTION_TIMEOUT_SECONDS": "30",
                "ENROLL_CONNECTION_MAX_SECONDS": "3",
            },
        )
        try:
            context = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
            context.check_hostname = False
            context.verify_mode = ssl.CERT_NONE
            client = context.wrap_socket(
                socket.create_connection(("127.0.0.1", port), timeout=30.0)
            )
            # Never terminated with a blank line, so the server stays in its
            # header read for as long as the bytes keep coming.
            header = b"POST /v1/enroll HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Drip: "
            header += b"a" * 200
            started = time.monotonic()
            dropped = False
            try:
                for byte in header:
                    try:
                        client.send(bytes([byte]))
                    except OSError:
                        dropped = True
                        break
                    # Idle rather than sleeping, so the drop is noticed when it
                    # happens instead of one tick later.
                    if select.select([client], [], [], 0.25)[0]:
                        try:
                            if client.recv(1) == b"":
                                dropped = True
                                break
                        except OSError:
                            dropped = True
                            break
                    if time.monotonic() - started > 25.0:
                        break
            finally:
                client.close()
            elapsed = time.monotonic() - started
            assert dropped, (
                f"the signer held a dripping client for {elapsed:.1f}s against "
                "a 3s ceiling: the idle timeout is rearmed by every byte, so "
                "only the ceiling bounds this"
            )
            # Dropped eventually is not dropped by the ceiling: with the
            # ceiling gone this lands near the 30s idle timeout instead.
            assert elapsed < 20.0, (
                f"dropped, but only after {elapsed:.1f}s against a 3s ceiling "
                "— that is the idle timeout, not the ceiling"
            )
            print(f"ok: dripping client dropped after {elapsed:.2f}s")
        finally:
            child.terminate()
            child.wait(timeout=10)


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
    check_bounds_drop_a_silent_client()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
