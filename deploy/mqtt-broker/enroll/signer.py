#!/usr/bin/env python3
"""Loam federation auto-enrollment signer (specs/federation-auto-enrollment.md).

A tiny HTTPS endpoint, next to Mosquitto on the broker host, that lets a
machine mint its own mTLS identity: the machine generates a keypair + CSR,
POSTs {password, csr}, and this service — holding the org CA key and the one
shared enrollment password — signs the CSR (subject verbatim, SAN carried from
the CSR via `copy_extensions = copy`) and returns the certificate PEM.

Security posture (the spec's named threat is brute force on a public port):
  * binds to the tailnet interface (100.x) when one is present;
  * verifies the password in constant time (hmac.compare_digest);
  * rate-limits attempts per client address;
  * never logs the password, the CSR, or the issued certificate;
  * Mosquitto itself is untouched — this service only writes CA-issued certs.

Dependency-free by construction: Python 3 stdlib + the system `openssl` the
deploy already manages. Mirrors the deploy's bash+openssl style.
"""

import argparse
import hmac
import http.server
import ipaddress
import json
import os
import re
import socket
import ssl
import subprocess
import sys
import tempfile
import threading
import time

# ---------------------------------------------------------------------------
# Configuration (env-driven; the install script renders the systemd unit).
# ---------------------------------------------------------------------------


class Config:
    def __init__(self) -> None:
        self.pki_dir = os.environ.get("ENROLL_PKI_DIR", "/etc/mosquitto/pki")
        self.password_file = os.environ.get(
            "ENROLL_PASSWORD_FILE", "/etc/mosquitto/enroll-password"
        )
        self.listen_port = int(os.environ.get("ENROLL_PORT", "8443"))
        self.bind_address = os.environ.get("ENROLL_BIND_ADDRESS", "0.0.0.0")
        self.cert_file = os.environ.get("ENROLL_CERT_FILE", "")
        self.key_file = os.environ.get("ENROLL_KEY_FILE", "")
        # Requests per client per window; a burst above this is 429.
        self.rate_limit = int(os.environ.get("ENROLL_RATE_LIMIT", "10"))
        self.rate_window = float(os.environ.get("ENROLL_RATE_WINDOW_SECONDS", "60"))
        self.openssl = os.environ.get("ENROLL_OPENSSL", "openssl")

    def tailnet_address(self) -> str | None:
        """The first 100.x.y.z address on this host, if any (Tailscale/CGNAT).
        Binding there keeps the signer off the public port entirely."""
        fns = getattr(socket, "if_nameindex", None)
        if fns is None:
            return None
        try:
            for _, name in fns():
                for family, kind, _, _, addr in socket.getaddrinfo(
                    name, None, 0, 0, socket.SOCK_DGRAM
                ):
                    if kind == getattr(socket, "SOCK_DGRAM", 2) and family == socket.AF_INET:
                        host = addr[0]
                        if host.startswith("100."):
                            try:
                                ip = ipaddress.ip_address(host)
                            except ValueError:
                                continue
                            if ip.is_private and host.startswith("100."):
                                return host
        except OSError:
            pass
        return None


# ---------------------------------------------------------------------------
# Constant-time password check + rate limiting
# ---------------------------------------------------------------------------


class RateLimiter:
    """A tiny sliding-window counter per client address. Not a replacement for
    a reverse proxy; it bounds a brute-force attempt on the port this service
    owns, which is the spec's named threat."""

    def __init__(self, limit: int, window: float) -> None:
        self.limit = limit
        self.window = window
        self.hits: dict[str, list[float]] = {}
        self.lock = threading.Lock()

    def allow(self, peer: str) -> bool:
        now = time.monotonic()
        with self.lock:
            recent = [t for t in self.hits.get(peer, []) if now - t < self.window]
            if len(recent) >= self.limit:
                self.hits[peer] = recent
                return False
            recent.append(now)
            self.hits[peer] = recent
            return True


# ---------------------------------------------------------------------------
# Signing: shell out to the deploy's own openssl CA, exact same craft as
# pki/issue-client.sh, but with `copy_extensions = copy` so the machine's own
# SAN (urn:loam:instance:<ulid>) flows into the signed certificate verbatim.
# ---------------------------------------------------------------------------


def sign_csr(config: Config, csr_pem: str) -> bytes:
    """Sign one CSR with the org CA, returning the certificate PEM. Raises
    SigningError on any failure. The subject is signed verbatim (the signer
    asserts nothing about the CN beyond the password check)."""
    with tempfile.TemporaryDirectory(prefix="loam-enroll-") as work:
        csr_path = os.path.join(work, "request.csr")
        crt_path = os.path.join(work, "signed.crt")
        with open(csr_path, "w", encoding="ascii") as handle:
            handle.write(csr_pem)

        openssl_cnf = os.path.join(config.pki_dir, "openssl.cnf")
        # The deploy's CA config signs the subject verbatim and, via
        # copy_extensions = copy, carries the CSR's SAN into the cert. The
        # config is authoritative; we never synthesize a subject.
        argv = [
            config.openssl,
            "ca",
            "-config",
            openssl_cnf,
            "-batch",
            "-notext",
            "-in",
            csr_path,
            "-out",
            crt_path,
        ]
        env = dict(os.environ)
        env["PKI_DIR"] = config.pki_dir
        result = subprocess.run(argv, capture_output=True, text=True, env=env)
        if result.returncode != 0:
            raise SigningError(
                "openssl ca rejected the CSR: "
                f"stdout={result.stdout!r} stderr={result.stderr!r}"
            )
        with open(crt_path, "rb") as handle:
            return handle.read()


class SigningError(Exception):
    pass


# ---------------------------------------------------------------------------
# HTTP handler
# ---------------------------------------------------------------------------


class EnrollHandler(http.server.BaseHTTPRequestHandler):
    server_version = "loam-enroll/1"

    def do_POST(self) -> None:  # noqa: N802 (stdlib casing)
        peer = self.client_address[0] if self.client_address else ""
        limiter: RateLimiter = self.server.limiter  # type: ignore[attr-defined]
        if not limiter.allow(peer):
            self.send_error(429, "too many requests")
            return
        if self.path not in ("/v1/enroll", "/v1/enroll/"):
            self.send_error(404)
            return

        try:
            length = int(self.headers.get("Content-Length", "0"))
        except ValueError:
            length = 0
        if length <= 0 or length > 1 << 20:
            self.send_error(400, "bad request size")
            return
        raw = self.rfile.read(length)
        try:
            payload = json.loads(raw)
        except ValueError:
            self.send_error(400, "body must be JSON")
            return
        password = payload.get("password")
        csr = payload.get("csr")
        if not isinstance(password, str) or not isinstance(csr, str) or not csr:
            self.send_error(400, "password and csr are required")
            return

        expected = _load_password(self.server)  # type: ignore[attr-defined]
        # Constant-time comparison: timing must not reveal how much of the
        # password matched, even under a guessed prefix.
        if not hmac.compare_digest(password.encode(), expected.encode()):
            self.send_error(401, "bad token")
            return

        try:
            certificate = sign_csr(self.server.config, csr)  # type: ignore[attr-defined]
        except SigningError as error:
            self.log_error("signing failed: %s", error)
            self.send_error(500, "signing failed")
            return

        body = certificate
        self.send_response(200)
        self.send_header("Content-Type", "application/x-pem-file")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, fmt: str, *args: object) -> None:
        # Keep the password, CSR, and certificate out of every log line. The
        # stdlib default logs the request line (path only — no body), which is
        # safe; a failure detail that could echo request bytes is suppressed.
        if fmt.startswith("code ") or fmt.startswith("\"%s\""):
            sys.stderr.write("%s - %s\n" % (self.address_string(), fmt % args))
        else:
            sys.stderr.write("loam-enroll: %s\n" % (fmt % args))


def _load_password(server: http.server.HTTPServer) -> str:
    password_file = server.config.password_file  # type: ignore[attr-defined]
    try:
        with open(password_file, encoding="ascii") as handle:
            value = handle.read().strip()
    except OSError as error:
        raise RuntimeError(f"cannot read enrollment password file {password_file}: {error}")
    if not value:
        raise RuntimeError(f"enrollment password file {password_file} is empty")
    return value


class ThreadingEnrollServer(http.server.ThreadingHTTPServer):
    daemon_threads = True


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--ping",
        action="store_true",
        help="read-only self-check: print config and exit (no network)",
    )
    args = parser.parse_args(argv)

    config = Config()
    if args.ping:
        print(f"pki_dir={config.pki_dir}")
        print(f"password_file={config.password_file}")
        print(f"listen={config.bind_address}:{config.listen_port}")
        print(f"tailnet_address={config.tailnet_address()}")
        return 0

    # Bind to the tailnet interface when one exists (the spec's primary
    # hardening); ENROLL_BIND_ADDRESS still wins for an explicit override.
    bind = config.bind_address
    if bind == "0.0.0.0":
        tailnet = config.tailnet_address()
        if tailnet:
            bind = tailnet

    if not config.cert_file or not config.key_file:
        print("ENROLL_CERT_FILE and ENROLL_KEY_FILE are required", file=sys.stderr)
        return 2
    if not os.path.exists(config.password_file):
        print(
            f"enrollment password file does not exist: {config.password_file}",
            file=sys.stderr,
        )
        return 2

    try:
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        context.minimum_version = ssl.TLSVersion.TLSv1_2
        context.load_cert_chain(config.cert_file, config.key_file)
    except ssl.SSLError as error:
        print(f"TLS setup failed: {error}", file=sys.stderr)
        return 2

    server = ThreadingEnrollServer((bind, config.listen_port), EnrollHandler)
    server.limiter = RateLimiter(config.rate_limit, config.rate_window)  # type: ignore[attr-defined]
    server.config = config  # type: ignore[attr-defined]
    server.socket = context.wrap_socket(server.socket, server_side=True)
    print(
        f"loam-enroll listening on {bind}:{config.listen_port} "
        f"(password file {config.password_file})",
        flush=True,
    )
    try:
        server.serve_forever(poll_interval=0.5)
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
