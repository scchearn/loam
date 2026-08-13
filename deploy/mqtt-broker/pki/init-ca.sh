#!/usr/bin/env bash
# Initialise the org-owned CLIENT-identity CA (T4). Issues the mTLS client certs
# whose CN = principal_id. The broker's SERVER cert comes from certbot, not here.
#
# ponytail: single-tier CA. A two-tier offline-root + online-issuing CA is more
# correct for a large org; upgrade path = make this cert an intermediate signed by
# an offline root and point issuance at the intermediate. One CA is sufficient for
# a single self-hosted broker.
set -euo pipefail

PKI_DIR="${PKI_DIR:?set PKI_DIR (from params.env)}"
CA_DAYS="${CA_DAYS:-3650}"
CA_CN="${CA_CN:-Loam Federation Org CA}"

mkdir -p "$PKI_DIR/private" "$PKI_DIR/newcerts"
chmod 700 "$PKI_DIR/private"
[ -f "$PKI_DIR/index.txt" ] || : > "$PKI_DIR/index.txt"
[ -f "$PKI_DIR/serial" ]    || echo 1000 > "$PKI_DIR/serial"
[ -f "$PKI_DIR/crlnumber" ] || echo 1000 > "$PKI_DIR/crlnumber"
# unique_subject = no: two instances of the SAME person share a CN (git email), so the
# CA must allow multiple valid certs with the same subject DN (differ only by SAN instance).
[ -f "$PKI_DIR/index.txt.attr" ] || echo 'unique_subject = no' > "$PKI_DIR/index.txt.attr"

cat > "$PKI_DIR/openssl.cnf" <<EOF
[ ca ]
default_ca = CA_default
[ CA_default ]
dir              = $PKI_DIR
database         = \$dir/index.txt
new_certs_dir    = \$dir/newcerts
certificate      = \$dir/ca.crt
private_key      = \$dir/private/ca.key
serial           = \$dir/serial
crlnumber        = \$dir/crlnumber
default_md       = sha256
default_days     = 825
default_crl_days = 30
policy           = policy_anything
# Auto-enrollment (specs/federation-auto-enrollment.md): the machine's CSR
# carries its own SAN (urn:loam:instance:<ulid>); the signer issues it verbatim.
# copy_extensions = copy makes `openssl ca` carry that SAN into the cert. It is
# inert for issue-client.sh, which supplies an explicit -extfile instead.
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

if [ ! -f "$PKI_DIR/ca.crt" ]; then
  openssl genrsa -out "$PKI_DIR/private/ca.key" 4096
  chmod 600 "$PKI_DIR/private/ca.key"
  openssl req -new -x509 -sha256 -days "$CA_DAYS" \
    -key "$PKI_DIR/private/ca.key" -subj "/CN=${CA_CN}" -out "$PKI_DIR/ca.crt"
fi

# (Re)generate an (initially empty) CRL so the broker's crlfile always exists.
openssl ca -config "$PKI_DIR/openssl.cnf" -gencrl -out "$PKI_DIR/crl.pem" 2>/dev/null || true

echo "org CA ready at $PKI_DIR (ca.crt, crl.pem)"
