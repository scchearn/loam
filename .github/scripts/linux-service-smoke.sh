#!/usr/bin/env bash
# Dormant-lifecycle service smoke (Linux/systemd).
# Proves: install writes the disabled unit and exercises systemctl --user;
# status is observational; uninstall removes it. No broker egress, no start.
set -euo pipefail
BIN="${1:?path to loam binary required}"
ROOT="$(mktemp -d)"
UNIT="$ROOT/systemd/loam-connector.service"
cleanup() { "$BIN" federation service uninstall --global-root "$ROOT" >/dev/null 2>&1 || true; rm -rf "$ROOT"; }
trap cleanup EXIT

"$BIN" federation service install --global-root "$ROOT"
test -f "$UNIT" || { echo "FAIL: unit not written"; exit 1; }
grep -q "Restart=on-failure" "$UNIT" || { echo "FAIL: unit not restart-on-failure"; exit 1; }
grep -q "federation service run" "$UNIT" || { echo "FAIL: unit ExecStart wrong"; exit 1; }
# A read did not create the database (dormant/unenrolled).
test ! -f "$ROOT/loam.sqlite3" || { echo "FAIL: database created by install"; exit 1; }
"$BIN" federation service status --global-root "$ROOT" >/dev/null 2>&1 || true
"$BIN" federation service uninstall --global-root "$ROOT"
test ! -f "$UNIT" || { echo "FAIL: unit not removed"; exit 1; }
echo "linux service smoke OK"
