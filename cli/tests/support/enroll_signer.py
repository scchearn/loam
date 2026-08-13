#!/usr/bin/env python3
"""Hermetic enrollment signer for the runtime test tier (LOAM_MQTT_TEST).

A functional subset of the deployment signer (deploy/mqtt-broker/enroll/
signer.py in loam-deploy): HTTPS POST /v1/enroll {password, csr}, constant-time
password check, and signing via the deployment's own `openssl ca` with
copy_extensions = copy. Kept here so the runtime tier is self-contained
(needs only python3 + openssl, both already required by this tier).

The deployment's signer adds tailnet binding, rate limiting, and logging
hardening; this fixture keeps only the contract-observable behavior.
"""

import argparse
import hmac
import http.server
import json
import os
import ssl
import subprocess
import sys
import tempfile

pki_dir = os.environ.get("ENROLL_PKI_DIR", "")
password_file = os.environ.get("ENROLL_PASSWORD_FILE", "")
cert_file = os.environ.get("ENROLL_CERT_FILE", "")
key_file = os.environ.get("ENROLL_KEY_FILE", "")


class Handler(http.server.BaseHTTPRequestHandler):
    def do_POST(self):  # noqa: N802
        if self.path not in ("/v1/enroll", "/v1/enroll/"):
            self.send_error(404)
            return
        try:
            length = int(self.headers.get("Content-Length", "0"))
        except ValueError:
            length = 0
        if length <= 0 or length > 1 << 20:
            self.send_error(400)
            return
        try:
            payload = json.loads(self.rfile.read(length))
        except ValueError:
            self.send_error(400)
            return
        password = payload.get("password")
        csr = payload.get("csr")
        if not isinstance(password, str) or not isinstance(csr, str) or not csr:
            self.send_error(400)
            return
        with open(password_file, encoding="ascii") as handle:
            expected = handle.read().strip()
        if not hmac.compare_digest(password.encode(), expected.encode()):
            self.send_error(401)
            return
        try:
            with tempfile.TemporaryDirectory(prefix="loam-enroll-test-") as work:
                in_path = os.path.join(work, "request.csr")
                out_path = os.path.join(work, "signed.crt")
                with open(in_path, "w", encoding="ascii") as handle:
                    handle.write(csr)
                env = dict(os.environ)
                env["PKI_DIR"] = pki_dir
                result = subprocess.run(
                    [
                        "openssl",
                        "ca",
                        "-config",
                        os.path.join(pki_dir, "openssl.cnf"),
                        "-batch",
                        "-notext",
                        "-in",
                        in_path,
                        "-out",
                        out_path,
                    ],
                    capture_output=True,
                    text=True,
                    env=env,
                    # The CA config may use relative `dir = .`; run from the PKI
                    # dir so `openssl ca` resolves index.txt/serial locally, the
                    # same way the deployment's install does.
                    cwd=pki_dir,
                )
                if result.returncode != 0:
                    self.send_error(500)
                    return
                with open(out_path, "rb") as handle:
                    body = handle.read()
        except OSError:
            self.send_error(500)
            return
        self.send_response(200)
        self.send_header("Content-Type", "application/x-pem-file")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, fmt, *args):  # noqa: A002
        sys.stderr.write("%s - %s\n" % (self.address_string(), fmt % args))


def main(argv):
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", type=int, default=18443)
    args = parser.parse_args(argv)
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.minimum_version = ssl.TLSVersion.TLSv1_2
    context.load_cert_chain(cert_file, key_file)
    server = http.server.ThreadingHTTPServer(("127.0.0.1", args.port), Handler)
    server.socket = context.wrap_socket(server.socket, server_side=True)
    server.serve_forever(poll_interval=0.5)


if __name__ == "__main__":
    main(sys.argv[1:])
