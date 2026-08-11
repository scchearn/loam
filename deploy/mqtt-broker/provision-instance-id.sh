#!/usr/bin/env bash
# instance_id unification provisioning (T14). Implements INSTANCE-ID-CONTRACT.md.
# Mints ONE stable instance_id per node and EMITS it for the enrollment step to pin
# into EnrolledRow.instance_id (the single source the connector reads). No sidecar.
#
# subcommands:
#   mint                      -> print a fresh ULID-shaped instance_id
#   source <instance_id>      -> print the envelope `source` form urn:loam:instance:<id>
#   check <enrolled_id> <session_id>
#                             -> exit 0 if unified; else print SourceInstanceMismatch, exit 1
#   selfcheck
set -euo pipefail

mint() {
  # 26-char uppercase base32 (ULID-shaped): 128 random bits, opaque + stable per node.
  head -c 16 /dev/urandom | base32 | tr -d '=' | tr 'a-z' 'A-Z' | cut -c1-26
}

source_urn() { printf 'urn:loam:instance:%s\n' "${1:?need <instance_id>}"; }

check() { # <enrolled_id> <session_id>
  local enrolled="${1:?}" session="${2:?}"
  if [ "$enrolled" = "$session" ]; then
    echo "unified: $enrolled"
    return 0
  fi
  echo "SourceInstanceMismatch: enrolled=$enrolled session=$session -> connector_refused" >&2
  return 1
}

selfcheck() {
  local id; id="$(mint)"
  [ "${#id}" -eq 26 ] || { echo "FAIL: instance_id not 26 chars: $id"; return 1; }
  [ "$(source_urn "$id")" = "urn:loam:instance:$id" ] || { echo "FAIL: source form"; return 1; }
  check "$id" "$id" >/dev/null || { echo "FAIL: matching pair not unified"; return 1; }
  if check "$id" "DIFFERENT0000000000000000" >/dev/null 2>&1; then
    echo "FAIL: mismatch not flagged"; return 1
  fi
  # two mints differ (per-node uniqueness)
  [ "$(mint)" != "$(mint)" ] || { echo "FAIL: mint not unique"; return 1; }
  echo "T14 selfcheck PASS"
}

case "${1:-}" in
  mint)      mint ;;
  source)    source_urn "$2" ;;
  check)     check "$2" "$3" ;;
  selfcheck) selfcheck ;;
  *) echo "usage: $0 {mint|source|check|selfcheck} ..." >&2; exit 2 ;;
esac
