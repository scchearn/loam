#!/usr/bin/env bash
# Slice C T8 dormant-lifecycle service smoke (macOS/LaunchAgent).
# Proves our rendered plist round-trips through real launchctl: install writes a
# valid dormant plist, launchctl bootstraps and prints it, then bootout + our
# uninstall remove it. RunAtLoad=false, so bootstrapping never starts it.
set -euo pipefail
BIN="${1:?path to loam binary required}"
ROOT="$(mktemp -d)"
LABEL="io.loam.connector"
DOMAIN="gui/$(id -u)"
PLIST="$ROOT/launchagents/$LABEL.plist"
cleanup() {
  launchctl bootout "$DOMAIN/$LABEL" >/dev/null 2>&1 || true
  "$BIN" federation service uninstall --global-root "$ROOT" >/dev/null 2>&1 || true
  rm -rf "$ROOT"
}
trap cleanup EXIT

"$BIN" federation service install --global-root "$ROOT"
test -f "$PLIST" || { echo "FAIL: plist not written"; exit 1; }
plutil -lint "$PLIST" || { echo "FAIL: plist invalid"; exit 1; }
grep -q "<key>RunAtLoad</key><false/>" "$PLIST" || { echo "FAIL: plist not dormant"; exit 1; }
test ! -f "$ROOT/loam.sqlite3" || { echo "FAIL: database created by install"; exit 1; }
# Read-only status (the verb packaged setup delegates to for verification) must
# not create a database or start anything; a dormant definition reports disabled.
"$BIN" federation service status --global-root "$ROOT" >/dev/null 2>&1 || true
test ! -f "$ROOT/loam.sqlite3" || { echo "FAIL: database created by status"; exit 1; }
# Exercise the real manager through the delegated lifecycle verbs (enable/disable
# are what setup uses to preserve active desired state across a runtime update).
# The empty registry keeps the connector inert, so enable never leaves a daemon.
"$BIN" federation service enable --global-root "$ROOT"
launchctl print "$DOMAIN/$LABEL" >/dev/null || { echo "FAIL: agent not loaded after enable"; exit 1; }
test ! -f "$ROOT/loam.sqlite3" || { echo "FAIL: database created by enable (inert violated)"; exit 1; }
"$BIN" federation service disable --global-root "$ROOT"
"$BIN" federation service uninstall --global-root "$ROOT"
test ! -f "$PLIST" || { echo "FAIL: plist not removed"; exit 1; }
echo "macos service smoke OK"
