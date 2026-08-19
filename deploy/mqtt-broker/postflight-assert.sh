#!/usr/bin/env bash
# Live-host postflight assert (T11). Recompute the snapshot and prove every
# pre-existing service is unchanged: the ONLY allowed differences are the added
# loam-mosquitto.service unit and the added :8883 listener. Any other drift
# (a changed config sha, a moved PID/start = silent restart, a removed service, an
# unexpected new port) is a hard failure -> stop and roll back.
#
# usage: postflight-assert.sh <baseline.snap>   (recomputes current live)
#        postflight-assert.sh assert <baseline> <current>   (compare two files)
#        postflight-assert.sh selfcheck
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=/dev/null
. "$here/preflight.sh"   # provides snapshot(); main-guarded, does not run

_assert() { # <baseline_file> <current_file>
  local base="$1" cur="$2" removed added line
  removed="$(comm -23 <(sort "$base") <(sort "$cur"))"
  added="$(comm -13 <(sort "$base") <(sort "$cur"))"

  if [ -n "$removed" ]; then
    echo "DRIFT: pre-existing state changed or removed:" >&2
    printf '  - %s\n' $removed >&2
    return 1
  fi
  # Every added line must be one of exactly two allowed additions.
  local port="${LISTENER_PORT:-8883}"
  while IFS= read -r line; do
    [ -z "$line" ] && continue
    case "$line" in
      "unit loam-mosquitto.service "*) ;;
      "port "*":${port}")              ;;
      *) echo "DRIFT: unexpected addition: $line" >&2; return 1 ;;
    esac
  done <<< "$added"
  echo "postflight OK: only loam-mosquitto + :${port} added; all else unchanged"
}

selfcheck() {
  local t; t="$(mktemp -d)"; trap 'rm -rf "$t"' RETURN
  cat > "$t/base" <<'EOF'
unit httpd active=active enabled=enabled pid=1001 start=A
unit matrix-synapse active=active enabled=enabled pid=1002 start=B
port 127.0.0.1:8008
port 0.0.0.0:443
config /etc/httpd/conf/httpd.conf abc123
EOF
  # clean: exactly the two allowed additions -> PASS
  cat "$t/base" > "$t/ok"
  printf 'unit loam-mosquitto.service active=active enabled=enabled pid=2000 start=Z\nport 0.0.0.0:8883\n' >> "$t/ok"
  LISTENER_PORT=8883 _assert "$t/base" "$t/ok" >/dev/null || { echo "FAIL: clean case rejected"; return 1; }

  # mutated config sha -> FAIL
  sed 's/abc123/def456/' "$t/base" > "$t/badcfg"
  if LISTENER_PORT=8883 _assert "$t/base" "$t/badcfg" >/dev/null 2>&1; then echo "FAIL: config drift undetected"; return 1; fi

  # silent restart (pid/start changed) -> FAIL
  sed 's/pid=1001 start=A/pid=9999 start=Q/' "$t/base" > "$t/badpid"
  if LISTENER_PORT=8883 _assert "$t/base" "$t/badpid" >/dev/null 2>&1; then echo "FAIL: silent restart undetected"; return 1; fi

  # service went down -> FAIL
  sed 's/unit httpd active=active/unit httpd active=inactive/' "$t/base" > "$t/baddown"
  if LISTENER_PORT=8883 _assert "$t/base" "$t/baddown" >/dev/null 2>&1; then echo "FAIL: downed service undetected"; return 1; fi

  # unexpected extra port -> FAIL
  cat "$t/ok" > "$t/badport"; printf 'port 0.0.0.0:6666\n' >> "$t/badport"
  if LISTENER_PORT=8883 _assert "$t/base" "$t/badport" >/dev/null 2>&1; then echo "FAIL: rogue port undetected"; return 1; fi

  echo "T11 selfcheck PASS"
}

case "${1:-}" in
  selfcheck) selfcheck ;;
  assert)    _assert "$2" "$3" ;;
  "")        echo "usage: $0 <baseline.snap> | assert <base> <cur> | selfcheck" >&2; exit 2 ;;
  *)         cur="$(mktemp)"; snapshot > "$cur"; _assert "$1" "$cur"; rm -f "$cur" ;;
esac
