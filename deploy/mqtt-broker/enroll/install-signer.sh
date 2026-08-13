#!/usr/bin/env bash
# Install/lifecycle for the federation auto-enrollment signer (see the spec:
# machine mints keypair + CSR, this service signs it against the org CA).
#
# Mirrors the deploy's style: envsubst from params.env, backup-before-edit,
# idempotent, reversible. The signer reuses the broker host's Let's Encrypt
# server certificate (same FQDN) so machines verify its TLS with public roots.
#
# usage: install-signer.sh [install|uninstall|status]
set -euo pipefail

: "${ORG_ID:?source params.env}" "${PKI_DIR:?}" "${CERTBOT_LIVE_DIR:?}"
ENROLL_DIR="${ENROLL_DIR:-/etc/loam/enroll}"
ENROLL_USER="${ENROLL_USER:-loam-enroll}"
ENROLL_PORT="${ENROLL_PORT:-8443}"
PYTHON_BIN="${PYTHON_BIN:-/usr/bin/python3}"

UNIT=/etc/systemd/system/loam-enroll-signer.service
HERE="$(cd "$(dirname "$0")" && pwd)"

case "${1:-install}" in
  install)
    # 0. The org CA must admit multiple certs with the same subject DN: two
    #    nodes of one person share a CN (the email). init-ca.sh writes
    #    index.txt.attr, but the signer guarantees it idempotently so a
    #    pre-existing CA from an older init cannot 500 on the second instance.
    [ -f "$PKI_DIR/index.txt.attr" ] || echo 'unique_subject = no' > "$PKI_DIR/index.txt.attr"
    # 1. provision a dedicated unprivileged user (idempotent).
    id "$ENROLL_USER" >/dev/null 2>&1 || useradd -r -s /usr/bin/nologin "$ENROLL_USER"
    # 2. install the signer sources (world-readable: no secrets here).
    install -D -m 755 "$HERE/signer.py" "$ENROLL_DIR/signer.py"
    # 3. create the shared enrollment password (0600) if absent. Rotation =
    #    replace this file with a new `openssl rand -base64 24` and re-share.
    if [ ! -s "$ENROLL_DIR/password" ]; then
      umask 077
      openssl rand -base64 24 > "$ENROLL_DIR/password"
      chmod 600 "$ENROLL_DIR/password"
    fi
    # 4. render + install the systemd unit (server cert = the host's LE cert).
    PYTHON_BIN="$PYTHON_BIN" \
    ENROLL_USER="$ENROLL_USER" \
    ENROLL_DIR="$ENROLL_DIR" \
    ENROLL_PORT="$ENROLL_PORT" \
    PKI_DIR="$PKI_DIR" \
    CERTBOT_LIVE_DIR="$CERTBOT_LIVE_DIR" \
      envsubst < "$HERE/loam-enroll-signer.service" > /tmp/loam-enroll-signer.service.$$
    install -D -m 644 /tmp/loam-enroll-signer.service.$$ "$UNIT"
    rm -f /tmp/loam-enroll-signer.service.$$
    # 5. systemd override for the runtime env (TLS cert + ports + rate limit).
    mkdir -p /etc/systemd/system/loam-enroll-signer.service.d
    cat > /etc/systemd/system/loam-enroll-signer.service.d/env.conf <<EOF
[Service]
Environment=ENROLL_CERT_FILE=${CERTBOT_LIVE_DIR}/fullchain.pem
Environment=ENROLL_KEY_FILE=${CERTBOT_LIVE_DIR}/privkey.pem
Environment=ENROLL_PASSWORD_FILE=${ENROLL_DIR}/password
# 0.0.0.0 = auto-bind: signer.py picks the first 100.x tailnet address it
# finds, refusing to sit on a public port when the tailnet is present. Override
# ENROLL_BIND_ADDRESS here to force an explicit interface.
Environment=ENROLL_BIND_ADDRESS=0.0.0.0
Environment=ENROLL_RATE_LIMIT=10
Environment=ENROLL_RATE_WINDOW_SECONDS=60
EOF
    systemctl daemon-reload
    systemctl enable --now loam-enroll-signer.service
    systemctl status loam-enroll-signer.service --no-pager || true
    echo "signer installed: https://<tailnet>:${ENROLL_PORT}/v1/enroll"
    echo "share this password with machines joining the org:"
    echo "  $ENROLL_DIR/password  (0600; rotation = replace + re-share)"
    ;;
  uninstall)
    systemctl disable --now loam-enroll-signer.service 2>/dev/null || true
    rm -f "$UNIT" /etc/systemd/system/loam-enroll-signer.service.d/env.conf
    systemctl daemon-reload
    rm -rf "$ENROLL_DIR"
    echo "signer uninstalled (org CA and broker untouched)"
    ;;
  status)
    systemctl status loam-enroll-signer.service --no-pager || true
    ;;
  *)
    echo "usage: $0 [install|uninstall|status]" >&2
    exit 2
    ;;
esac
