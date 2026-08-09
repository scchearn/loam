#!/usr/bin/env bash
# The recorded real-harness / real-broker suite.
#
# This is the Phase-1 evidence tier: a local Mosquitto carries real frames, the
# connector holds a genuinely subscribed session, and `loam hook` / `loam
# federation emit` run as real processes. It is opt-in because a lane that
# cannot observe a real broker or a released harness must never report coverage
# it did not have — CI keeps the fakes-only tier and says so.
#
#   LOAM_REAL_HARNESS=1 bash bin/real-harness-suite.sh [--evidence <path>]
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
EVIDENCE="$ROOT/plans/research/loam-mqtt-harness-e2e-evidence.md"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --evidence) EVIDENCE="${2:-}"; shift 2 ;;
    *) printf 'usage: real-harness-suite.sh [--evidence <path>]\n' >&2; exit 64 ;;
  esac
done

if [[ "${LOAM_REAL_HARNESS:-}" != "1" ]]; then
  printf 'SKIP: the real-harness suite needs a real broker and genuinely installed\n'
  printf '      harnesses. Set LOAM_REAL_HARNESS=1 to run it. Without it this lane\n'
  printf '      reports nothing, which is the point: fakes prove nothing about a\n'
  printf '      released harness or a real broker.\n'
  exit 0
fi

missing=()
for tool in mosquitto mosquitto_passwd openssl git; do
  command -v "$tool" >/dev/null 2>&1 || missing+=("$tool")
done
if [[ ${#missing[@]} -gt 0 ]]; then
  printf 'SKIP: the real-broker tier needs %s on PATH.\n' "${missing[*]}"
  exit 0
fi

mkdir -p "$(dirname "$EVIDENCE")"
{
  printf '# Recorded e2e evidence\n\n'
  printf -- '- Recorded: %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  printf -- '- Broker: local %s, throwaway fixture per case\n' \
    "$(mosquitto -h 2>&1 | grep -om1 'mosquitto version [0-9.]*')"
  printf -- '- Session: explicitly provisioned in-test (the credential and\n'
  printf -- '  peer-roster seams). Production `provision_session` still returns\n'
  printf -- '  `None`, so a shipped connector still answers `credentials-unresolved`.\n'
  printf -- '- Suite: `cargo +1.94.1 test --locked --test mqtt_harness -- --ignored`\n'
} > "$EVIDENCE"

printf 'Recording the real-broker e2e evidence into %s\n\n' "$EVIDENCE"
LOAM_MQTT_TEST=1 LOAM_T8_EVIDENCE="$EVIDENCE" \
  cargo +1.94.1 test --locked --test mqtt_harness -- --ignored --test-threads=1

printf '\nEvaluating the installed-harness compatibility matrix\n\n'
bash "$ROOT/bin/harness-matrix.sh" "$@"

printf '\nOK: real-harness suite complete; evidence in %s\n' "$EVIDENCE"
