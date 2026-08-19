#!/usr/bin/env bash
# Per-project peer-roster provisioning (T13). Implements docs/federation/ROSTER-CONTRACT.md.
# The connector reads the written file as its injected PeerRoster (D-T1).
# Requires: jq.
#
# subcommands:
#   validate <file>
#   write <org_id> <project_id> <principals_csv> <origins_csv>
#   admits <file> (--principal ID | --origin BARE_INSTANCE_ID)
#   selfcheck
set -euo pipefail

command -v jq >/dev/null || { echo "provision-peer-roster: jq is required" >&2; exit 2; }

ROSTER_DIR="${LOAM_FEDERATION_ROSTER_DIR:-$HOME/.agents/loam/federation/rosters}"

_has_wildcard() { # true if any principals/origins entry is empty or a wildcard
  jq -e '
    ([.principals[]?, .origins[]?]) as $e
    | ($e | any(. == "" or . == "*" or . == "**"))
  ' "$1" >/dev/null 2>&1
}

validate() {
  local f="$1"
  [ -f "$f" ] || { echo "roster absent: $f (-> no-peer-roster)"; return 1; }
  jq -e '.version and (.org_id|type=="string") and (.project_id|type=="string")
         and (.principals|type=="array") and (.origins|type=="array")' "$f" >/dev/null \
    || { echo "FAIL: malformed roster $f"; return 1; }
  # populated = at least one concrete principal or origin
  jq -e '((.principals|length) + (.origins|length)) > 0' "$f" >/dev/null \
    || { echo "FAIL: empty roster (-> refused, never a session)"; return 1; }
  if _has_wildcard "$f"; then echo "FAIL: wildcard/empty entry (-> refused)"; return 1; fi
  echo "roster OK: $f"
}

write() {
  local org="$1" proj="$2" pcsv="$3" ocsv="$4"
  local dest="$ROSTER_DIR/$org/$proj.json"
  mkdir -p "$ROSTER_DIR/$org"
  jq -n --arg org "$org" --arg proj "$proj" \
     --arg p "$pcsv" --arg o "$ocsv" '
     {version:1, org_id:$org, project_id:$proj,
      principals: ($p|split(",")|map(select(length>0))),
      origins:    ($o|split(",")|map(select(length>0)))}' > "$dest"
  validate "$dest" >/dev/null || { rm -f "$dest"; echo "refused to write invalid roster" >&2; exit 1; }
  echo "wrote $dest"
}

admits() {
  local f="$1"; shift
  case "${1:-}" in
    --principal) jq -e --arg v "$2" '.principals|index($v)' "$f" >/dev/null ;;
    --origin)    jq -e --arg v "$2" '.origins|index($v)'    "$f" >/dev/null ;;
    *) echo "usage: admits <file> (--principal ID|--origin ID)" >&2; return 2 ;;
  esac
}

selfcheck() {
  local tmp; tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' RETURN
  LOAM_FEDERATION_ROSTER_DIR="$tmp" ROSTER_DIR="$tmp"
  write "org-1" "proj-1" "emp-laptop,emp-macbook" "instLAP,instMAC" >/dev/null
  local f="$tmp/org-1/proj-1.json"
  admits "$f" --principal emp-laptop   || { echo "FAIL: listed principal not admitted"; return 1; }
  admits "$f" --origin    instMAC      || { echo "FAIL: listed origin not admitted"; return 1; }
  if admits "$f" --principal stranger 2>/dev/null; then echo "FAIL: stranger admitted"; return 1; fi
  # empty + wildcard rosters must be refused
  printf '{"version":1,"org_id":"o","project_id":"p","principals":[],"origins":[]}' > "$tmp/empty.json"
  if validate "$tmp/empty.json" >/dev/null 2>&1; then echo "FAIL: empty roster accepted"; return 1; fi
  printf '{"version":1,"org_id":"o","project_id":"p","principals":["*"],"origins":[]}' > "$tmp/wild.json"
  if validate "$tmp/wild.json" >/dev/null 2>&1; then echo "FAIL: wildcard roster accepted"; return 1; fi
  echo "T13 selfcheck PASS"
}

case "${1:-}" in
  validate) validate "$2" ;;
  write)    write "$2" "$3" "$4" "$5" ;;
  admits)   shift; admits "$@" ;;
  selfcheck) selfcheck ;;
  *) echo "usage: $0 {validate|write|admits|selfcheck} ..." >&2; exit 2 ;;
esac
