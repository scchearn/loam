#!/usr/bin/env bash
# Issue one mTLS client cert (T4). Identity model LOCKED (bigboss, 2026-08-09):
#   CN            = git user.email  (= principal_id = ACL principal = dedup key = data.from.principal_id)
#   emailAddress  = git user.email  (same value, conventional slot)
#   GN (givenName)= git user.name   (the DISPLAY NAME, cryptographically bound in the
#                                    signed subject so it cannot be spoofed by sender text)
#   SAN URI       = urn:loam:instance:<instance_id> [, urn:loam:agent:<agent_id>]
# The connector reads principal_id (CN) AND display_name (GN) from the CONNACK-
# authenticated cert. See IDENTITY-CONTRACT.md.
#
# usage: issue-client.sh <git_email> <instance_id> <display_name> [agent_id]
# stdout: the issued cert path (so callers can read it); human log goes to stderr.
set -euo pipefail

PKI_DIR="${PKI_DIR:?set PKI_DIR}"
EMAIL="${1:?usage: issue-client.sh <git_email> <instance_id> <display_name> [agent_id]}"
INSTANCE="${2:?need <instance_id>}"
DISPLAY_NAME="${3:?need <display_name> (git user.name)}"
AGENT="${4:-}"

san="URI:urn:loam:instance:${INSTANCE}"
[ -n "$AGENT" ] && san="${san},URI:urn:loam:agent:${AGENT}"

# Files keyed by (email, instance): two nodes of the SAME person (laptop + MacBook =
# same email principal) share a CN and would otherwise overwrite each other. CN stays
# the real email; only the on-disk filename is node-unique + filesystem-sanitized.
node="$(printf '%s__%s' "$EMAIL" "$INSTANCE" | tr -c 'A-Za-z0-9_.-' '_')"
key="$PKI_DIR/private/${node}.key"
crt="$PKI_DIR/${node}.crt"
csr="$PKI_DIR/${node}.csr"
ext="$PKI_DIR/${node}.ext"

openssl genrsa -out "$key" 2048
chmod 600 "$key"
# Display name + email bound in the signed subject:
openssl req -new -key "$key" -subj "/CN=${EMAIL}/emailAddress=${EMAIL}/GN=${DISPLAY_NAME}" -out "$csr"

cat > "$ext" <<EOF
[ v3_client ]
basicConstraints = CA:FALSE
keyUsage = critical, digitalSignature
extendedKeyUsage = clientAuth
subjectAltName = ${san}
EOF

openssl ca -config "$PKI_DIR/openssl.cnf" -batch -notext \
  -in "$csr" -out "$crt" -extfile "$ext" -extensions v3_client
rm -f "$csr" "$ext"

echo "issued CN=${EMAIL} GN=${DISPLAY_NAME} SAN=${san} -> ${crt}" >&2
echo "$crt"
