#!/usr/bin/env bash
# Dormant-lifecycle service smoke (macOS/LaunchAgent).
# Proves our rendered plist round-trips through real launchctl: install writes a
# valid plist and loads nothing, our enable bootstraps it, and disable +
# uninstall remove it.
#
# Dormancy here is a property of the lifecycle, not of RunAtLoad: the plist lives
# under the global root rather than a launchd search path, so nothing reads it
# until enable bootstraps it. The plist starts the job at load precisely so the
# activation never calls `launchctl kickstart`, which waits out launchd's
# ThrottleInterval when the started process exits within milliseconds — the 10s
# wedge that made this leg red on every run (#124).
set -euo pipefail
BIN="${1:?path to loam binary required}"
# Short root on purpose: the connector endpoint is a Unix socket and macOS
# caps sun_path at 104 bytes, which the default /var/folders/... temp path can
# exceed on its own.
ROOT="$(mktemp -d /tmp/loam-svc-XXXXXX)"
LABEL="io.loam.connector"
DOMAIN="gui/$(id -u)"
PLIST="$ROOT/launchagents/$LABEL.plist"
cleanup() {
  launchctl bootout "$DOMAIN/$LABEL" >/dev/null 2>&1 || true
  pkill -f "federation service run --global-root $ROOT" >/dev/null 2>&1 || true
  "$BIN" federation service uninstall --global-root "$ROOT" >/dev/null 2>&1 || true
  rm -rf "$ROOT"
}
trap cleanup EXIT

"$BIN" federation service install --global-root "$ROOT"
test -f "$PLIST" || { echo "FAIL: plist not written"; exit 1; }
plutil -lint "$PLIST" || { echo "FAIL: plist invalid"; exit 1; }
grep -q "<key>RunAtLoad</key><true/>" "$PLIST" || { echo "FAIL: plist does not start at load"; exit 1; }
# The real dormancy check: install loaded nothing. A definition launchctl already
# knows about here would mean install started the connector behind our back.
if launchctl print "$DOMAIN/$LABEL" >/dev/null 2>&1; then
  echo "FAIL: install left a loaded job; the definition must stay dormant until enable"
  exit 1
fi
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

# --- enrolled start: the positive control every absence check above needs ---
# Seed one enrollment through the real registry API (no broker, no probe, no
# credential), then let the real LaunchAgent start the connector and observe the
# process. Without an observed start, "no process" proves nothing.
LOAM_SMOKE_ROOT="$ROOT" cargo +1.94.1 test --release --locked --test federation_connector -- \
  --ignored --exact seed_one_enrollment_for_the_service_smoke
test -f "$ROOT/loam.sqlite3" || { echo "FAIL: enrollment not seeded"; exit 1; }

"$BIN" federation service enable --global-root "$ROOT"
PID=""
for _ in $(seq 1 30); do
  PID="$(launchctl print "$DOMAIN/$LABEL" 2>/dev/null | sed -n 's/^[[:space:]]*pid = \([0-9][0-9]*\).*$/\1/p' | head -1)"
  [ -n "$PID" ] && break
  sleep 1
done
if [ -z "$PID" ]; then
  echo "FAIL: no connector process observed after an enrolled enable"
  launchctl print "$DOMAIN/$LABEL" 2>&1 | head -40 || true
  exit 1
fi
echo "enrolled start observed under launchd (pid $PID)"
# launchd reports the pid the moment it spawns the job; the connector still has
# to open the registry and bind. Poll rather than race it — but keep the check,
# because an observed process that never binds is exactly the failure the
# positive control exists to catch.
for _ in $(seq 1 30); do
  [ -S "$ROOT/run/connector.sock" ] && break
  sleep 1
done
if [ ! -S "$ROOT/run/connector.sock" ]; then
  echo "FAIL: enrolled connector bound no endpoint"
  # Distinguish a connector that died from one that is alive and never bound.
  if ps -p "$PID" >/dev/null 2>&1; then echo "pid $PID is still alive"; else echo "pid $PID has exited"; fi
  ls -la "$ROOT/run" 2>&1 | head -10 || true
  launchctl print "$DOMAIN/$LABEL" 2>&1 | head -40 || true
  exit 1
fi

# Final disconnect equivalent: disable stops the agent and leaves nothing behind.
"$BIN" federation service disable --global-root "$ROOT"
for _ in $(seq 1 30); do
  launchctl print "$DOMAIN/$LABEL" >/dev/null 2>&1 || break
  sleep 1
done
if launchctl print "$DOMAIN/$LABEL" >/dev/null 2>&1; then
  echo "FAIL: agent still loaded after disable"
  exit 1
fi

"$BIN" federation service uninstall --global-root "$ROOT"
test ! -f "$PLIST" || { echo "FAIL: plist not removed"; exit 1; }
echo "macos service smoke OK"
