#!/usr/bin/env bash
# T4 self-check: build the org CA, issue a verifiable client cert with the requested
# CN + instance SAN, then revoke it into the CRL — all in a throwaway sandbox.
# Nothing committed; the certbot server cert is not exercised here (certbot's job).
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
export PKI_DIR="$tmp"

"$here/init-ca.sh" >/dev/null
crt="$("$here/issue-client.sh" "dev@example.com" "01TESTINSTANCE0000000000000" "Dev Example" "agent-7" 2>/dev/null)"

# 1. client verifies against the CA
openssl verify -CAfile "$tmp/ca.crt" "$crt" >/dev/null

# 2. CN is exactly the git email (principal_id)
openssl x509 -in "$crt" -noout -subject | grep -q 'dev@example.com' \
  || { echo "FAIL: client CN != git email"; exit 1; }

# 3. display name (git user.name) is bound in the signed subject (GN)
openssl x509 -in "$crt" -noout -subject | grep -q 'Dev Example' \
  || { echo "FAIL: display name not bound in cert subject"; exit 1; }

# 4. SAN carries the instance URN
openssl x509 -in "$crt" -noout -ext subjectAltName \
  | grep -q 'urn:loam:instance:01TESTINSTANCE0000000000000' \
  || { echo "FAIL: SAN instance URN missing"; exit 1; }

# 5. clientAuth EKU present
openssl x509 -in "$crt" -noout -ext extendedKeyUsage \
  | grep -q 'TLS Web Client Authentication' \
  || { echo "FAIL: clientAuth EKU missing"; exit 1; }

# 6. SAME-PERSON SECOND INSTANCE: a second cert with the SAME CN (email) but a
#    different instance must issue (unique_subject=no) — the laptop+MacBook case.
crt2="$("$here/issue-client.sh" "dev@example.com" "02OTHERINSTANCE000000000000" "Dev Example" "agent-2" 2>/dev/null)" \
  || { echo "FAIL: second same-CN instance cert refused (unique_subject)"; exit 1; }
[ "$crt2" != "$crt" ] || { echo "FAIL: second node cert collided with first"; exit 1; }
openssl verify -CAfile "$tmp/ca.crt" "$crt2" >/dev/null || { echo "FAIL: second cert invalid"; exit 1; }

# 7. revoke -> serial appears in the CRL
"$here/revoke-client.sh" "dev@example.com" "01TESTINSTANCE0000000000000" >/dev/null
openssl crl -in "$tmp/crl.pem" -noout -text | grep -q 'Serial Number:' \
  || { echo "FAIL: CRL has no revoked serial"; exit 1; }

echo "T4 selfcheck PASS"
