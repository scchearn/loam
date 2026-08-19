#!/usr/bin/env bash
# Live-host preflight snapshot (T11). READ-ONLY — starts/stops/edits nothing.
# Captures the baseline the postflight assert compares against, so provisioning can
# prove it disrupted no existing service on the co-located host.
#
# Sourceable: `snapshot` prints a normalized snapshot to stdout.
# As a script: writes the baseline to ${BASELINE:-$BACKUP_DIR/preflight-baseline.snap}.
set -euo pipefail

# Normalized snapshot: one fact per line, stable ordering.
snapshot() {
  local units="${WATCH_UNITS:-}" configs="${WATCH_CONFIGS:-}" u c
  for u in $units; do
    local act ena pid st
    act="$(systemctl is-active "$u" 2>/dev/null || echo unknown)"
    ena="$(systemctl is-enabled "$u" 2>/dev/null || echo unknown)"
    pid="$(systemctl show -p MainPID --value "$u" 2>/dev/null || echo 0)"
    st="$(systemctl show -p ExecMainStartTimestamp --value "$u" 2>/dev/null || echo -)"
    echo "unit $u active=$act enabled=$ena pid=$pid start=$st"
  done
  # listening TCP sockets (local addr:port), deduplicated.
  ss -H -tln 2>/dev/null | awk '{print "port "$4}' | sort -u
  for c in $configs; do
    if [ -f "$c" ]; then
      echo "config $c $(sha256sum "$c" | cut -d' ' -f1)"
    else
      echo "config $c MISSING"
    fi
  done
}

# main
if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
  BASELINE="${BASELINE:-${BACKUP_DIR:-/tmp}/preflight-baseline.snap}"
  mkdir -p "$(dirname "$BASELINE")"
  snapshot > "$BASELINE"
  echo "preflight baseline written: $BASELINE ($(wc -l < "$BASELINE") facts)"
fi
