#!/usr/bin/env bash
# Revoke one client cert and regenerate the CRL (T4). Revocation is effective only
# once the broker reloads the regenerated crl.pem (crlfile in mosquitto.conf).
#
# usage: revoke-client.sh <principal_id>
set -euo pipefail

PKI_DIR="${PKI_DIR:?set PKI_DIR}"
EMAIL="${1:?usage: revoke-client.sh <git_email> <instance_id>}"
INSTANCE="${2:?need <instance_id> (a node is (email, instance))}"
node="$(printf '%s__%s' "$EMAIL" "$INSTANCE" | tr -c 'A-Za-z0-9_.-' '_')"
crt="$PKI_DIR/${node}.crt"

[ -f "$crt" ] || { echo "no cert for ${EMAIL}/${INSTANCE} at ${crt}" >&2; exit 1; }

openssl ca -config "$PKI_DIR/openssl.cnf" -revoke "$crt"
openssl ca -config "$PKI_DIR/openssl.cnf" -gencrl -out "$PKI_DIR/crl.pem"

echo "revoked ${EMAIL}/${INSTANCE}; CRL regenerated -> $PKI_DIR/crl.pem (reload broker to enforce)"
