#!/usr/bin/env bash
# Install/lifecycle for the federation auto-enrollment signer (see the spec:
# machine mints keypair + CSR, this service signs it against the org CA).
#
# Mirrors the deploy's style: envsubst from params.env, backup-before-edit,
# idempotent, reversible. The signer reuses the broker host's Let's Encrypt
# server certificate (same FQDN) so machines verify its TLS with public roots.
#
# The live dir is 0700 root-owned and unreadable by the dedicated service
# user, so install copies fullchain.pem + privkey.pem into $ENROLL_DIR/tls/
# (key 0640 root:loam-enroll) and a certbot renewal-hooks deploy hook re-copies
# them + restarts the signer on certificate rotation (~90 days).
#
# The org CA private directory is likewise root-only. Install copies ca.crt and
# ca.key into $ENROLL_DIR/ca/; the signer uses those copies while sharing the
# authoritative CA database under $PKI_DIR. Only that database's index, serial,
# and newcerts paths are made writable by loam-enroll, so manual issue-client.sh
# and auto-enrollment share one issuance history.
#
# usage: install-signer.sh [install|uninstall|status]
set -euo pipefail

: "${ORG_ID:?source params.env}" "${PKI_DIR:?}" "${CERTBOT_LIVE_DIR:?}"
ENROLL_DIR="${ENROLL_DIR:-/etc/loam/enroll}"
ENROLL_USER="${ENROLL_USER:-loam-enroll}"
ENROLL_PORT="${ENROLL_PORT:-8443}"
PYTHON_BIN="${PYTHON_BIN:-/usr/bin/python3}"
BROKER_FQDN="${BROKER_FQDN:-mqtt.example.org}"

UNIT=/etc/systemd/system/loam-enroll-signer.service
DEPLOY_HOOK=/etc/letsencrypt/renewal-hooks/deploy/loam-enroll-signer.sh
HERE="$(cd "$(dirname "$0")" && pwd)"

# Copy the Let's Encrypt server certificate/key into the signer's own TLS dir.
# The source live dir is 0700 root:certbot, which the dedicated loam-enroll
# user cannot read; the copies are readable by it instead (key 0640
# root:loam-enroll). Group-read is granted so the service user can load the
# key without weakening root ownership. Idempotent; re-run refreshes a rotated
# cert.
copy_tls_material() {
  install -d -m 750 -o root -g "$ENROLL_USER" "$ENROLL_DIR/tls"
  install -m 644 -o root -g root "$CERTBOT_LIVE_DIR/fullchain.pem" "$ENROLL_DIR/tls/fullchain.pem"
  install -m 640 -o root -g "$ENROLL_USER" "$CERTBOT_LIVE_DIR/privkey.pem" "$ENROLL_DIR/tls/privkey.pem"
}

# Expose the CA key/certificate without exposing /etc/mosquitto/pki/private.
copy_ca_material() {
  install -d -m 750 -o root -g "$ENROLL_USER" "$ENROLL_DIR/ca"
  install -m 644 -o root -g root "$PKI_DIR/ca.crt" "$ENROLL_DIR/ca/ca.crt"
  install -m 640 -o root -g "$ENROLL_USER" "$PKI_DIR/private/ca.key" "$ENROLL_DIR/ca/ca.key"
  cat > "$ENROLL_DIR/ca/openssl.cnf" <<EOF
[ ca ]
default_ca = CA_default
[ CA_default ]
dir              = $PKI_DIR
database         = \$dir/index.txt
new_certs_dir    = \$dir/newcerts
certificate      = $ENROLL_DIR/ca/ca.crt
private_key      = $ENROLL_DIR/ca/ca.key
serial           = \$dir/serial
crlnumber        = \$dir/crlnumber
default_md       = sha256
default_days     = 825
default_crl_days = 30
policy           = policy_anything
copy_extensions  = copy
[ policy_anything ]
commonName = supplied
emailAddress = optional
givenName = optional
[ v3_client ]
basicConstraints = CA:FALSE
keyUsage = critical, digitalSignature
extendedKeyUsage = clientAuth
EOF
  chmod 644 "$ENROLL_DIR/ca/openssl.cnf"
}

# Keep auto-enrollment on the same CA database as manual issuance. OpenSSL ca
# updates these paths; the service unit grants write access only to them.
prepare_ca_database() {
  command -v setfacl >/dev/null 2>&1 || {
    echo "setfacl is required to share the CA database safely" >&2
    exit 1
  }
  # OpenSSL ca writes temporary/backup database files beside index and serial,
  # so the database directory itself must be writable. File ACLs below keep
  # the writable set limited to the database files and new certificate store.
  setfacl -m "u:${ENROLL_USER}:rwx" "$PKI_DIR"
  setfacl -m "u:${ENROLL_USER}:rw" "$PKI_DIR/index.txt" "$PKI_DIR/serial"
  setfacl -m "u:${ENROLL_USER}:rwx" "$PKI_DIR/newcerts"
  setfacl -m "u:${ENROLL_USER}:r" "$PKI_DIR/crlnumber"
}

# Install the certbot deploy hook: whenever certbot renews the server cert
# (~90 days), re-copy the material and nudge the signer so it picks up the new
# chain without a manual restart. Certbot executes deploy hooks with -x
# (executable) and its own env; only the paths we bake below matter.
install_deploy_hook() {
  install -d -m 755 /etc/letsencrypt/renewal-hooks/deploy
  cat > "$DEPLOY_HOOK" <<EOF
#!/usr/bin/env bash
set -euo pipefail
case "\${RENEWED_LINEAGE:-}" in
  */"${BROKER_FQDN:-}") ;;
  *) exit 0 ;;
esac
copy() {
  install -d -m 750 -o root -g "${ENROLL_USER}" "${ENROLL_DIR}/tls"
  install -m 644 -o root -g root "${CERTBOT_LIVE_DIR}/fullchain.pem" "${ENROLL_DIR}/tls/fullchain.pem"
  install -m 640 -o root -g "${ENROLL_USER}" "${CERTBOT_LIVE_DIR}/privkey.pem" "${ENROLL_DIR}/tls/privkey.pem"
}
copy
systemctl try-restart loam-enroll-signer.service || true
EOF
  chmod 755 "$DEPLOY_HOOK"
}

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
    # 4. copy the LE server cert + key into the signer's readable TLS dir and
    #    install the certbot deploy hook that refreshes both on rotation.
    copy_tls_material
    copy_ca_material
    prepare_ca_database
    install_deploy_hook
    # 5. render + install the systemd unit (server cert = the host's LE cert,
    #    as copied into $ENROLL_DIR/tls — the certbot live dir is 0700 root).
    PYTHON_BIN="$PYTHON_BIN" \
    ENROLL_USER="$ENROLL_USER" \
    ENROLL_DIR="$ENROLL_DIR" \
    ENROLL_PORT="$ENROLL_PORT" \
    PKI_DIR="$PKI_DIR" \
    CERTBOT_LIVE_DIR="$CERTBOT_LIVE_DIR" \
      envsubst < "$HERE/loam-enroll-signer.service" > /tmp/loam-enroll-signer.service.$$
    install -D -m 644 /tmp/loam-enroll-signer.service.$$ "$UNIT"
    rm -f /tmp/loam-enroll-signer.service.$$
    # 6. systemd override for the runtime env (TLS cert + ports + rate limit).
    mkdir -p /etc/systemd/system/loam-enroll-signer.service.d
    cat > /etc/systemd/system/loam-enroll-signer.service.d/env.conf <<EOF
[Service]
Environment=ENROLL_CERT_FILE=${ENROLL_DIR}/tls/fullchain.pem
Environment=ENROLL_KEY_FILE=${ENROLL_DIR}/tls/privkey.pem
Environment=ENROLL_PASSWORD_FILE=%d/enrollment-password
Environment=ENROLL_PKI_DIR=${PKI_DIR}
Environment=ENROLL_OPENSSL_CONFIG=${ENROLL_DIR}/ca/openssl.cnf
# The port is public on a broker VPS; ENROLL_BIND_ADDRESS defaults to 0.0.0.0
# (TLS + password + rate limit are the walls). Override to an explicit private
# interface if the operator wants one.
Environment=ENROLL_BIND_ADDRESS=0.0.0.0
Environment=ENROLL_RATE_LIMIT=10
Environment=ENROLL_RATE_WINDOW_SECONDS=60
EOF
    systemctl daemon-reload
    systemctl enable loam-enroll-signer.service
    systemctl restart loam-enroll-signer.service
    systemctl status loam-enroll-signer.service --no-pager || true
    echo "signer installed: https://${BROKER_FQDN:-<host>}:${ENROLL_PORT}/v1/enroll"
    echo "share this password with machines joining the org:"
    echo "  $ENROLL_DIR/password  (0600; rotation = replace + re-share)"
    ;;
  uninstall)
    systemctl disable --now loam-enroll-signer.service 2>/dev/null || true
    rm -f "$UNIT" /etc/systemd/system/loam-enroll-signer.service.d/env.conf
    rm -f "$DEPLOY_HOOK"
    if command -v setfacl >/dev/null 2>&1; then
      setfacl -x "u:${ENROLL_USER}" "$PKI_DIR" "$PKI_DIR/index.txt" "$PKI_DIR/serial" "$PKI_DIR/newcerts" "$PKI_DIR/crlnumber" 2>/dev/null || true
    fi
    systemctl daemon-reload
    rm -rf "$ENROLL_DIR"
    echo "signer uninstalled (org CA, broker, and certbot deploy hooks for other services untouched)"
    ;;
  status)
    systemctl status loam-enroll-signer.service --no-pager || true
    ;;
  *)
    echo "usage: $0 [install|uninstall|status]" >&2
    exit 2
    ;;
esac
