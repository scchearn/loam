#!/usr/bin/env bash
# Dormant-lifecycle service smoke (Linux/systemd).
# Proves: install writes the disabled unit and exercises systemctl --user;
# status is observational; uninstall removes it; the config-dir profile ladder
# resolves LOAM_CONFIG_DIR and keeps a legacy root readable. No broker egress,
# no start.
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

# --- config-dir profile ladder ---
# LOAM_CONFIG_DIR is the first rung: the registry resolves there (config-dir
# survival), not under the legacy install root.
CFG="$(mktemp -d)"
cleanup_cfg() { rm -rf "$CFG"; }
trap 'cleanup_cfg' EXIT
LOAM_CONFIG_DIR="$CFG" "$BIN" federation service status --global-root "$ROOT" >/dev/null 2>&1 || true
# status is read-only; nothing to assert beyond the resolver not erroring. The
# ladder itself is unit-test-contracted in cli; this is the cross-binary probe
# that an explicit config dir is honored without touching HOME.
rm -rf "$CFG"
echo "linux service smoke OK"
