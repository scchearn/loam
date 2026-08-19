#!/usr/bin/env python3
"""Loam federation auto-enrollment signer (specs/federation-auto-enrollment.md).

A tiny HTTPS endpoint, next to Mosquitto on the broker host, that lets a
machine mint its own mTLS identity: the machine generates a keypair + CSR,
POSTs {password, csr}, and this service — holding the org CA key and the one
shared enrollment password — signs the CSR (subject verbatim, SAN carried from
the CSR via `copy_extensions = copy`) and returns the certificate PEM.

Security posture (the spec's named threat is brute force on a public port):
  * binds to ENROLL_BIND_ADDRESS (default 0.0.0.0 — the port is public);
  * verifies the password in constant time (hmac.compare_digest);
  * rate-limits attempts per client address;
  * never logs the password, the CSR, or the issued certificate;
  * Mosquitto itself is untouched — this service only writes CA-issued certs.

TLS + the shared enrollment password + rate limiting are the security walls
on a public VPS; binding is not itself a security boundary.

Dependency-free by construction: Python 3 stdlib + the system `openssl` the
deploy already manages. Mirrors the deploy's bash+openssl style.
"""

import argparse
import hmac
import http.server
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
        self.openssl_config = os.environ.get(
            "ENROLL_OPENSSL_CONFIG", os.path.join(self.pki_dir, "openssl.cnf")
        )
        # Requests per client per window; a burst above this is 429.
        self.rate_limit = int(os.environ.get("ENROLL_RATE_LIMIT", "10"))
        self.rate_window = float(os.environ.get("ENROLL_RATE_WINDOW_SECONDS", "60"))
        # Idle timeout: the longest one socket operation may block — the TLS
        # handshake, or any single read or write. A client that connects and
        # then says nothing must not hold a worker thread — or, before the
        # accept-loop fix below, the whole service — open forever.
        self.connection_timeout = float(
            os.environ.get("ENROLL_CONNECTION_TIMEOUT_SECONDS", "10")
        )
        # Whole-connection ceiling. The idle timeout alone is not a bound:
        # every byte that does arrive rearms it, so a client dripping one byte
        # per nine seconds holds a worker thread and an fd for as long as it
        # likes. On a public port with daemon threads that is a slowloris
        # against the service that already wedged once, so the connection gets
        # a hard deadline as well.
        self.connection_max = float(
            os.environ.get(
                "ENROLL_CONNECTION_MAX_SECONDS", str(self.connection_timeout * 3)
            )
        )
        self.openssl = os.environ.get("ENROLL_OPENSSL", "openssl")


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

        openssl_cnf = config.openssl_config
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

        try:
            expected = _load_password(self.server)  # type: ignore[attr-defined]
        except RuntimeError as error:
            # An unreadable or empty password file is a signer-side
            # misconfiguration, not a client error. Without this the exception
            # escaped the handler and the client saw a bare closed connection
            # — one more silent hang to debug from the outside.
            self.log_error("password file unusable: %s", error)
            self.send_error(503, "signer misconfigured")
            return
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


def _shutdown_quietly(sock: socket.socket) -> None:
    """Unblock a worker stuck on a connection that ran out of time.

    Called from the reaper, so the socket may already be closed by the worker
    finishing normally — that race is the expected case, not an error.
    """
    try:
        sock.shutdown(socket.SHUT_RDWR)
    except OSError:
        pass


class ConnectionReaper:
    """Shuts down connections that outlive the whole-connection ceiling.

    The worker cannot enforce its own ceiling: it is blocked inside a `recv`,
    and nothing short of shutting the socket down returns it. A timer per
    connection would do it, but that doubles the thread count on exactly the
    public port being hardened — one thread each for the worker and its own
    watchdog, both driven by whoever is connecting. So every connection shares
    one reaper thread and a deadline registry instead.
    """

    def __init__(self, poll_interval: float = 0.5) -> None:
        self.poll_interval = poll_interval
        self.lock = threading.Lock()
        self.deadlines: dict[socket.socket, float] = {}
        thread = threading.Thread(target=self._run, name="loam-enroll-reaper")
        thread.daemon = True
        thread.start()

    def register(self, sock: socket.socket, seconds: float) -> None:
        with self.lock:
            self.deadlines[sock] = time.monotonic() + seconds

    def release(self, sock: socket.socket) -> None:
        with self.lock:
            self.deadlines.pop(sock, None)

    def _run(self) -> None:
        while True:
            time.sleep(self.poll_interval)
            now = time.monotonic()
            with self.lock:
                expired = [
                    sock for sock, deadline in self.deadlines.items() if deadline <= now
                ]
                for sock in expired:
                    del self.deadlines[sock]
            # Outside the lock: shutdown can block briefly and must never hold
            # up the registry every worker thread touches.
            for sock in expired:
                _shutdown_quietly(sock)


class ThreadingEnrollServer(http.server.ThreadingHTTPServer):
    """Threaded HTTPS whose TLS handshake never runs on the accept loop.

    ThreadingHTTPServer on its own does not stop the wedge: wrapping the
    *listening* socket — the obvious spelling — makes `ssl.SSLSocket.accept()`
    perform the handshake itself, inside `serve_forever`'s single accept loop.
    One client that completes the TCP connect and then stays silent blocks
    every subsequent accept for as long as it holds the connection, which is
    the LISTEN-with-an-undrained-backlog wedge observed in production: threads
    were already in play and the service still stopped accepting.

    So the listening socket is wrapped with `do_handshake_on_connect=False`
    (the pattern wptserve and Tornado use for the same reason). `accept()`
    then returns a handshake-pending SSLSocket immediately, and the
    per-connection worker thread completes the handshake under
    `connection_timeout`. A stalled client now only ever costs one thread,
    and only until its bounds expire.

    Two bounds, because one is not enough. `connection_timeout` is an *idle*
    timeout — the longest a single socket operation may block — and every byte
    that arrives rearms it, so it alone does not bound a slow drip.
    `connection_max` is the ceiling on the whole connection, enforced by the
    shared `ConnectionReaper`: it shuts the socket down, which unblocks
    whatever read or write the worker is sitting in.
    """

    daemon_threads = True
    # Replaced from Config in main(); the class defaults keep a directly
    # constructed server bounded too.
    connection_timeout = 10.0
    connection_max = 30.0
    reaper: "ConnectionReaper | None" = None

    def finish_request(self, request, client_address) -> None:
        # Runs on the worker thread. The idle timeout is armed before the
        # handshake, so it covers the handshake and every later read/write.
        request.settimeout(self.connection_timeout)
        reaper = self.reaper
        if reaper is not None:
            reaper.register(request, self.connection_max)
        try:
            request.do_handshake()
            super().finish_request(request, client_address)
        finally:
            if reaper is not None:
                reaper.release(request)

    def handle_error(self, request, client_address) -> None:
        error = sys.exc_info()[1]
        if isinstance(error, (ssl.SSLError, socket.timeout, ConnectionError)):
            # Routine traffic on a public port: TLS scanners, plain-HTTP
            # probes, and clients that hit connection_timeout. One line each,
            # no traceback. OpenSSL's message is an alert code, never request
            # bytes, so it is safe to log.
            peer = client_address[0] if client_address else "?"
            sys.stderr.write("loam-enroll: dropped %s: %s\n" % (peer, error))
            return
        super().handle_error(request, client_address)


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
        print(f"connection_timeout={config.connection_timeout}")
        print(f"connection_max={config.connection_max}")
        return 0

    # The port is public on a broker VPS; ENROLL_BIND_ADDRESS (default
    # 0.0.0.0) is an explicit override for an operator who wants a private
    # interface. Binding is not itself a security boundary.
    bind = config.bind_address

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
    server.connection_timeout = config.connection_timeout
    server.connection_max = config.connection_max
    # One reaper for the whole server; see ConnectionReaper for why it is not
    # a timer per connection.
    server.reaper = ConnectionReaper()
    # do_handshake_on_connect=False is load-bearing, not a micro-optimisation:
    # accept() propagates this flag to each accepted socket, which is what
    # keeps the handshake off the accept loop. See ThreadingEnrollServer.
    server.socket = context.wrap_socket(
        server.socket, server_side=True, do_handshake_on_connect=False
    )
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
