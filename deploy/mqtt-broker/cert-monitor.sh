#!/usr/bin/env bash
# Cert-expiry monitor (T7). Watches the ORG-CA client certs + the org CA itself.
# The certbot SERVER cert is renewed by certbot's own timer; this only observes it.
# Exits non-zero (alert) if any watched cert expires within THRESH_SECONDS.
#
#   THRESH_SECONDS   default 1209600 (14 days) — calibration knob
#   CERT_SCAN_DIR    default ${PKI_DIR} — dir of *.crt to watch
#   CERTBOT_LIVE_DIR optional — observe (not renew) the server cert
#
# subcommands: (default) run | selfcheck
set -euo pipefail

THRESH_SECONDS="${THRESH_SECONDS:-1209600}"

run() {
  local dir="${CERT_SCAN_DIR:-${PKI_DIR:?set PKI_DIR or CERT_SCAN_DIR}}"
  local bad=0 c
  for c in "$dir"/*.crt; do
    [ -e "$c" ] || continue
    if openssl x509 -checkend "$THRESH_SECONDS" -noout -in "$c" >/dev/null 2>&1; then
      : # ok — will NOT expire within the window
    else
      echo "ALERT: $c expires within ${THRESH_SECONDS}s ($(openssl x509 -enddate -noout -in "$c"))"
      bad=1
    fi
  done
  # Observe (do not renew) the certbot server cert if present.
  if [ -n "${CERTBOT_LIVE_DIR:-}" ] && [ -f "${CERTBOT_LIVE_DIR}/fullchain.pem" ]; then
    openssl x509 -checkend "$THRESH_SECONDS" -noout -in "${CERTBOT_LIVE_DIR}/fullchain.pem" >/dev/null 2>&1 \
      || echo "NOTE: server cert near expiry — certbot's timer should renew it; check certbot.timer"
  fi
  [ "$bad" -eq 0 ] && echo "cert-monitor: all watched client certs healthy"
  return "$bad"
}

selfcheck() {
  local t; t="$(mktemp -d)"; trap 'rm -rf "$t"' RETURN
  # long-lived cert only -> healthy (exit 0)
  openssl req -x509 -newkey rsa:2048 -nodes -keyout "$t/far.key" -out "$t/far.crt" \
    -days 3650 -subj "/CN=far" >/dev/null 2>&1
  CERT_SCAN_DIR="$t" THRESH_SECONDS=172800 run >/dev/null \
    || { echo "FAIL: healthy cert wrongly alerted"; return 1; }
  # add a near-expiry cert (valid 1 day) -> alert (exit non-zero) with a 2-day window
  openssl req -x509 -newkey rsa:2048 -nodes -keyout "$t/near.key" -out "$t/near.crt" \
    -days 1 -subj "/CN=near" >/dev/null 2>&1
  if CERT_SCAN_DIR="$t" THRESH_SECONDS=172800 run >/dev/null 2>&1; then
    echo "FAIL: near-expiry cert not flagged"; return 1
  fi
  echo "T7 selfcheck PASS"
}

case "${1:-run}" in
  run)       run ;;
  selfcheck) selfcheck ;;
  *) echo "usage: $0 {run|selfcheck}" >&2; exit 2 ;;
esac
