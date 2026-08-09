#!/usr/bin/env bash
# Slice D T7 — the four-harness compatibility matrix as an admission gate.
#
# Every row is evaluated against the harness's *released, installed* version.
# A harness is advertised only when every required row passes against it. A row
# that cannot be evaluated is WITHHELD, and withholding is a pass: advertising
# compatibility a harness cannot deliver is the specific failure this gate
# exists to prevent. No bridge, shim, or partial claim closes a gap.
#
# Opt-in: without LOAM_REAL_HARNESS=1 this skips cleanly with a stated reason,
# so CI keeps its fakes-only tier and never claims matrix coverage.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPORT=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --report) REPORT="${2:-}"; shift 2 ;;
    *) printf 'usage: harness-matrix.sh [--report <path>]\n' >&2; exit 64 ;;
  esac
done

if [[ "${LOAM_REAL_HARNESS:-}" != "1" ]]; then
  printf 'SKIP: the compatibility matrix is evaluated against genuinely installed\n'
  printf '      harnesses only. Set LOAM_REAL_HARNESS=1 to run it. A fakes-only\n'
  printf '      lane proves nothing about a released harness version and must not\n'
  printf '      report matrix coverage.\n'
  exit 0
fi

LOAM="${LOAM_RUNTIME:-$ROOT/target/debug/loam}"
[[ -x "$LOAM" ]] || { printf 'FAIL: no native runtime at %s (cargo build first)\n' "$LOAM" >&2; exit 1; }

version_of() {
  command -v "$1" >/dev/null 2>&1 || { printf 'not-installed'; return; }
  "$1" --version 2>&1 | head -1
}

printf 'Loam harness compatibility matrix — %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
printf 'runtime: %s\n\n' "$LOAM"
for id in claude codex opencode cursor; do
  printf '%-10s %s\n' "$id" "$(version_of "$id")"
done
printf '\n'

# Row: the native read path answers each harness's envelope. This is the only
# row this script can decide by itself; every other row needs a live session,
# a provisioned broker, or a running harness, and is recorded in the report.
status=0
for id in opencode claude codex cursor; do
  event=$([[ "$id" == "cursor" ]] && printf 'sessionStart' || printf 'SessionStart')
  out="$(printf '{"cwd":"%s"}' "$ROOT" | "$LOAM" hook "$id" --event "$event" 2>/dev/null || true)"
  case "$id" in
    claude|codex) key='additionalContext' ;;
    cursor) key='additional_context' ;;
    opencode) key='<LOAM_IMPORTANT>' ;;
  esac
  if [[ "$id" == "opencode" || "$id" == "codex" ]]; then
    # Plain-body harnesses: the body itself is the envelope.
    [[ "$out" == *'<LOAM_IMPORTANT>'* ]] && verdict=pass || { verdict=FAIL; status=1; }
  else
    [[ "$out" == *"$key"* ]] && verdict=pass || { verdict=FAIL; status=1; }
  fi
  printf 'native-envelope %-10s %s\n' "$id" "$verdict"
done

# Positive control: the same command must FAIL on an unknown harness id, or a
# passing row above would only prove the binary runs.
if printf '{}' | "$LOAM" hook definitely-not-a-harness >/dev/null 2>&1; then
  printf 'native-envelope control  FAIL (an unknown harness id was served)\n'
  status=1
else
  printf 'native-envelope control  pass (an unknown harness id is refused)\n'
fi

if [[ -n "$REPORT" ]]; then
  [[ -f "$REPORT" ]] || { printf '\nFAIL: no recorded matrix at %s\n' "$REPORT" >&2; exit 1; }
  printf '\nrecorded matrix: %s\n' "$REPORT"
  # The recorded verdicts are the artifact; this only checks it was not left
  # with an unevaluated row silently reading as a pass.
  if grep -q 'TODO\|TBD' "$REPORT"; then
    printf 'FAIL: the recorded matrix still contains an unevaluated row\n' >&2
    exit 1
  fi
fi

exit "$status"
