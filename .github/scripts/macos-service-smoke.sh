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
# Exercise the real manager with our plist.
launchctl bootstrap "$DOMAIN" "$PLIST"
launchctl print "$DOMAIN/$LABEL" >/dev/null || { echo "FAIL: agent not loaded"; exit 1; }
launchctl bootout "$DOMAIN/$LABEL"
"$BIN" federation service uninstall --global-root "$ROOT"
test ! -f "$PLIST" || { echo "FAIL: plist not removed"; exit 1; }
echo "macos service smoke OK"
