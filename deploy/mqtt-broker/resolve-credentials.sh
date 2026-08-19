#!/usr/bin/env bash
# Credential resolution provisioning (T12). Implements docs/federation/RESOLUTION-CONTRACT.md.
# Provisioning side STORES; the connector side LOOKS UP with the identical keys.
#
# Backends (chosen by OS, overridable via LOAM_SECRET_BACKEND):
#   secret-tool  (Linux / libsecret)   security (macOS / Keychain)   mock (test-only, file)
# Lookup keys are the enrollment's credential_ref / ca_ref VERBATIM.
#
# subcommands:
#   store  <ref> <blob_file>        store PEM material under ref
#   lookup <ref>                    print PEM material to stdout (what the connector does)
#   provision <principal> <instance> <credential_ref> [agent]
#                                   issue client cert (T4) + store cert||key under credential_ref
#   resolve <credential_ref>        print cert then key (round-trip check)
#   selfcheck
set -euo pipefail

SECRET_SERVICE_LABEL="${SECRET_SERVICE_LABEL:-loam-federation}"

_backend() {
  if [ -n "${LOAM_SECRET_BACKEND:-}" ]; then echo "$LOAM_SECRET_BACKEND"; return; fi
  case "$(uname -s)" in Darwin) echo security ;; *) echo secret-tool ;; esac
}

_kv_store() { # <ref> <blob_file>
  local ref="$1" file="$2"
  case "$(_backend)" in
    secret-tool)
      secret-tool store --label "${SECRET_SERVICE_LABEL}:${ref}" \
        service "$SECRET_SERVICE_LABEL" ref "$ref" < "$file" ;;
    security)
      # -U update-if-exists; -w takes the value (provisioning-time, trusted host).
      security add-generic-password -U -s "$SECRET_SERVICE_LABEL" -a "$ref" \
        -w "$(cat "$file")" ;;
    mock)
      local d="${LOAM_MOCK_SECRET_DIR:?mock backend needs LOAM_MOCK_SECRET_DIR}"
      mkdir -p "$d"; local k; k="$(printf '%s' "$ref" | sha256sum | cut -d' ' -f1)"
      install -m 600 /dev/null "$d/$k"; cat "$file" > "$d/$k" ;;
  esac
}

_kv_lookup() { # <ref> -> stdout
  local ref="$1"
  case "$(_backend)" in
    secret-tool) secret-tool lookup service "$SECRET_SERVICE_LABEL" ref "$ref" ;;
    security)    security find-generic-password -s "$SECRET_SERVICE_LABEL" -a "$ref" -w ;;
    mock)        local d="${LOAM_MOCK_SECRET_DIR:?}"; local k; k="$(printf '%s' "$ref" | sha256sum | cut -d' ' -f1)"; cat "$d/$k" ;;
  esac
}

provision() { # <git_email> <instance> <display_name> <credential_ref> [agent]
  local email="$1" instance="$2" display="$3" ref="$4" agent="${5:-}"
  local here; here="$(cd "$(dirname "$0")" && pwd)"
  local crt node key
  crt="$(PKI_DIR="${PKI_DIR:?set PKI_DIR}" "$here/pki/issue-client.sh" "$email" "$instance" "$display" "$agent")"
  node="$(basename "$crt" .crt)"
  key="$PKI_DIR/private/$node.key"
  local blob; blob="$(mktemp)"
  cat "$crt" "$key" > "$blob"
  _kv_store "$ref" "$blob"
  rm -f "$blob"
  echo "stored credential for CN=$email (node $node) under ref=$ref (backend=$(_backend))"
}

resolve() { # <credential_ref> -> "cert PEM" then "key PEM" (connector splits identically)
  _kv_lookup "$1"
}

selfcheck() {
  local tmp; tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' RETURN
  export LOAM_SECRET_BACKEND=mock LOAM_MOCK_SECRET_DIR="$tmp/secrets" PKI_DIR="$tmp/pki"
  local here; here="$(cd "$(dirname "$0")" && pwd)"
  "$here/pki/init-ca.sh" >/dev/null
  provision "dev@example.com" "instLAP" "Dev Example" "vault://loam/proj-1/laptop" >/dev/null
  # connector-side: lookup by the SAME verbatim ref, split cert||key on PEM boundary
  local out cert key
  out="$(resolve 'vault://loam/proj-1/laptop')"
  cert="$(printf '%s\n' "$out" | awk '/BEGIN CERTIFICATE/{c=1} c{print} /END CERTIFICATE/{c=0}')"
  key="$(printf '%s\n' "$out"  | awk '/BEGIN.*PRIVATE KEY/{c=1} c{print} /END.*PRIVATE KEY/{c=0}')"
  printf '%s\n' "$cert" | openssl x509 -noout -subject | grep -q 'dev@example.com' \
    || { echo "FAIL: resolved cert CN wrong"; return 1; }
  printf '%s\n' "$key" | openssl pkey -noout 2>/dev/null \
    || { echo "FAIL: resolved key not a valid private key"; return 1; }
  # store perms are owner-only (no world-readable secret)
  local kf; kf="$(ls "$tmp/secrets")"; [ "$(stat -c '%a' "$tmp/secrets/$kf")" = "600" ] \
    || { echo "FAIL: secret store not 600"; return 1; }
  echo "T12 selfcheck PASS"
}

case "${1:-}" in
  store)     _kv_store "$2" "$3" ;;
  lookup)    _kv_lookup "$2" ;;
  provision) shift; provision "$@" ;;
  resolve)   resolve "$2" ;;
  selfcheck) selfcheck ;;
  *) echo "usage: $0 {store|lookup|provision|resolve|selfcheck} ..." >&2; exit 2 ;;
esac
