#!/usr/bin/env bash
#
# Tier-9 operational smoke: run a real Mosquitto broker as an isolated,
# uniquely named transient systemd *user* unit and prove the operational
# properties Slice B depends on — TLS, no anonymous listener, restart
# persistence, and backup/restore — on an Arch-compatible host.
#
# This is a manual/operational gate, not a package installer or a CI job. It
# never edits /etc/mosquitto, never enables a system-wide service, never
# overwrites an existing persistence database, and never requires Docker. All
# broker state lives under one dedicated temporary directory that is removed on
# exit. A missing prerequisite is a reported blocker (exit 2), never a
# fabricated pass.
#
set -euo pipefail

# --- result tracking -------------------------------------------------------
declare -a CHECKS=()
record() { CHECKS+=("$1"); printf '  [%s] %s\n' "$2" "$1" >&2; }
pass() { record "$1" "PASS"; }
fail() { record "$1" "FAIL"; FAILED=1; }
FAILED=0

blocker() {
  printf 'BLOCKER: %s\n' "$1" >&2
  exit 2
}

# --- prerequisites ---------------------------------------------------------
MOSQUITTO="${LOAM_MOSQUITTO_BIN:-$(command -v mosquitto || true)}"
MOSQUITTO_PASSWD="${LOAM_MOSQUITTO_PASSWD_BIN:-$(command -v mosquitto_passwd || true)}"
MOSQUITTO_PUB="${LOAM_MOSQUITTO_PUB_BIN:-$(command -v mosquitto_pub || true)}"
MOSQUITTO_SUB="${LOAM_MOSQUITTO_SUB_BIN:-$(command -v mosquitto_sub || true)}"
OPENSSL="${LOAM_OPENSSL_BIN:-$(command -v openssl || true)}"

[[ -x "$MOSQUITTO" ]] || blocker "mosquitto is not installed"
[[ -x "$MOSQUITTO_PASSWD" ]] || blocker "mosquitto_passwd is not installed"
[[ -x "$MOSQUITTO_PUB" ]] || blocker "mosquitto_pub is not installed"
[[ -x "$MOSQUITTO_SUB" ]] || blocker "mosquitto_sub is not installed"
[[ -x "$OPENSSL" ]] || blocker "openssl is not installed"
command -v systemd-run >/dev/null || blocker "systemd-run is not available"
command -v systemctl >/dev/null || blocker "systemctl is not available"
systemctl --user show-environment >/dev/null 2>&1 ||
  blocker "no working 'systemctl --user' session bus on this host"

# --- isolated state --------------------------------------------------------
RUN_ID="$$-${RANDOM}"
NS="loam/v1/tier9-${RUN_ID}"
UNIT="loam-tier9-${RUN_ID}"
DIR="$(mktemp -d "${TMPDIR:-/tmp}/loam-tier9-${RUN_ID}.XXXXXX")"
BACKUP="${DIR}/backup"
PERSIST="${DIR}/persistence"
TOPIC="${NS}/state/instance-01/sentinel"
USER_NAME="tier9"
PASS_WORD="tier9-${RUN_ID}"
STARTED=0

cleanup() {
  set +e
  if [[ "$STARTED" == "1" ]]; then
    # Clear the retained sentinel while the broker is still reachable.
    port_open && "$MOSQUITTO_PUB" -h localhost -p "$PORT" --cafile "$DIR/ca.crt" \
      -u "$USER_NAME" -P "$PASS_WORD" -t "$TOPIC" -r -n -q 1 >/dev/null 2>&1
    systemctl --user stop "$UNIT" >/dev/null 2>&1
    systemctl --user reset-failed "$UNIT" >/dev/null 2>&1
  fi
  # Remove only our own temporary directory.
  case "$DIR" in
    "${TMPDIR:-/tmp}"/loam-tier9-* | /tmp/loam-tier9-*) rm -rf "$DIR" ;;
    *) printf 'refusing to remove unexpected temp dir %s\n' "$DIR" >&2 ;;
  esac
}
trap cleanup EXIT

# --- helpers ---------------------------------------------------------------
free_port() {
  local p
  for _ in $(seq 1 100); do
    p=$(((RANDOM % 20000) + 20000))
    if ! (exec 3<>"/dev/tcp/127.0.0.1/${p}") 2>/dev/null; then
      echo "$p"
      return 0
    fi
    exec 3>&- 2>/dev/null || true
  done
  return 1
}

port_open() { (exec 3<>"/dev/tcp/127.0.0.1/${PORT}") 2>/dev/null; }

wait_ready() {
  local deadline=$((SECONDS + 15))
  while ((SECONDS < deadline)); do
    port_open && return 0
    sleep 0.2
  done
  return 1
}

main_pid() { systemctl --user show "$UNIT" -p MainPID --value; }

# Start (or re-create) the transient unit. systemd garbage-collects a transient
# unit once it goes inactive, so after a stop this re-creates it under the same
# name; reset-failed clears any lingering failed state first.
start_unit() {
  systemctl --user reset-failed "$UNIT" >/dev/null 2>&1 || true
  systemd-run --user --unit="$UNIT" --property=Type=simple \
    "$MOSQUITTO" -c "$DIR/mosquitto.conf" >/dev/null
  STARTED=1
  wait_ready || blocker "broker did not open TLS listener on 127.0.0.1:${PORT}"
}

pub_retained() {
  "$MOSQUITTO_PUB" -h localhost -p "$PORT" --cafile "$DIR/ca.crt" \
    -u "$USER_NAME" -P "$PASS_WORD" -t "$TOPIC" -m "$1" -r -q 1
}

# Echo the retained payload if present within the timeout, else empty.
observe_retained() {
  "$MOSQUITTO_SUB" -h localhost -p "$PORT" --cafile "$DIR/ca.crt" \
    -u "$USER_NAME" -P "$PASS_WORD" -t "$TOPIC" -C 1 -W 5 2>/dev/null || true
}

# --- certificates (CA, server with localhost SAN, client) ------------------
mkdir -p "$PERSIST" "$BACKUP"
printf 'subjectAltName=DNS:localhost,IP:127.0.0.1\nextendedKeyUsage=serverAuth\n' >"$DIR/server.ext"
printf 'extendedKeyUsage=clientAuth\n' >"$DIR/client.ext"
"$OPENSSL" req -x509 -newkey rsa:2048 -sha256 -nodes -days 2 \
  -subj "/CN=Loam Tier-9 Test CA" -keyout "$DIR/ca.key" -out "$DIR/ca.crt" >/dev/null 2>&1
"$OPENSSL" req -newkey rsa:2048 -sha256 -nodes -subj "/CN=localhost" \
  -keyout "$DIR/server.key" -out "$DIR/server.csr" >/dev/null 2>&1
"$OPENSSL" x509 -req -in "$DIR/server.csr" -CA "$DIR/ca.crt" -CAkey "$DIR/ca.key" \
  -CAcreateserial -days 2 -sha256 -extfile "$DIR/server.ext" -out "$DIR/server.crt" >/dev/null 2>&1
"$OPENSSL" req -newkey rsa:2048 -sha256 -nodes -subj "/CN=tier9-client" \
  -keyout "$DIR/client.key" -out "$DIR/client.csr" >/dev/null 2>&1
"$OPENSSL" x509 -req -in "$DIR/client.csr" -CA "$DIR/ca.crt" -CAkey "$DIR/ca.key" \
  -CAcreateserial -days 2 -sha256 -extfile "$DIR/client.ext" -out "$DIR/client.crt" >/dev/null 2>&1
chmod 600 "$DIR"/*.key

if "$OPENSSL" x509 -checkend 0 -noout -in "$DIR/server.crt" &&
  "$OPENSSL" x509 -checkend 0 -noout -in "$DIR/ca.crt"; then
  pass "server and CA certificates are currently valid"
else
  fail "server or CA certificate is not valid"
fi

# --- credentials, ACL, broker config (same security shape as tier T2) ------
"$MOSQUITTO_PASSWD" -c -b "$DIR/passwords" "$USER_NAME" "$PASS_WORD" >/dev/null 2>&1
chmod 600 "$DIR/passwords"
cat >"$DIR/acl" <<EOF
user ${USER_NAME}
topic readwrite ${NS}/#
EOF
chmod 600 "$DIR/acl"

PORT="$(free_port)" || blocker "could not reserve a free TCP port"
cat >"$DIR/mosquitto.conf" <<EOF
per_listener_settings true
persistence true
persistence_location ${PERSIST}/
persistence_file mosquitto.db
autosave_interval 1
autosave_on_changes true
log_dest file ${DIR}/mosquitto.log
log_type all
connection_messages true
max_packet_size 400000

listener ${PORT} 127.0.0.1
cafile ${DIR}/ca.crt
certfile ${DIR}/server.crt
keyfile ${DIR}/server.key
require_certificate false
allow_anonymous false
password_file ${DIR}/passwords
acl_file ${DIR}/acl
EOF

# --- 1) start the transient user unit and round-trip a retained sentinel ---
start_unit
if systemctl --user is-active "$UNIT" >/dev/null 2>&1; then
  pass "transient systemd --user unit ${UNIT} is active"
else
  fail "transient systemd --user unit did not become active"
fi

pub_retained "tier9-sentinel-v1"
if [[ "$(observe_retained)" == "tier9-sentinel-v1" ]]; then
  pass "retained sentinel round-trips over TLS"
else
  fail "retained sentinel did not round-trip over TLS"
fi

# --- 2) anonymous connections are refused ----------------------------------
if "$MOSQUITTO_PUB" -h localhost -p "$PORT" --cafile "$DIR/ca.crt" \
  -t "$TOPIC" -m "anon" -q 1 >/dev/null 2>&1; then
  fail "anonymous publish was accepted (no anonymous listener expected)"
else
  pass "anonymous connection is refused"
fi

# --- 3) restart the unit; retained state persists across the restart -------
PID_BEFORE="$(main_pid)"
systemctl --user restart "$UNIT"
wait_ready || blocker "broker did not return after restart"
PID_AFTER="$(main_pid)"
if [[ -n "$PID_BEFORE" && "$PID_BEFORE" != "$PID_AFTER" && "$PID_AFTER" != "0" ]]; then
  pass "unit restarted (MainPID ${PID_BEFORE} -> ${PID_AFTER})"
else
  fail "unit did not demonstrably restart (MainPID ${PID_BEFORE} -> ${PID_AFTER})"
fi
if [[ "$(observe_retained)" == "tier9-sentinel-v1" ]]; then
  pass "retained sentinel persists across a restart"
else
  fail "retained sentinel was lost across a restart"
fi

# --- 4) backup / restore is non-vacuous ------------------------------------
# Stop, back up the persistence DB and security material, then prove that a
# wiped DB loses the sentinel and a restored DB brings it back. A transient
# unit is removed on stop, so each start below re-creates it under the same
# name via systemd-run, yielding a fresh MainPID as restart evidence.
PID_PRE_BACKUP="$(main_pid)"
systemctl --user stop "$UNIT"
[[ -f "$PERSIST/mosquitto.db" ]] || fail "expected a non-empty persistence database to back up"
cp "$PERSIST/mosquitto.db" "$BACKUP/mosquitto.db"
cp "$DIR/passwords" "$DIR/acl" "$DIR/ca.crt" "$DIR/server.crt" "$DIR/server.key" "$BACKUP/"

# Wipe only our own persistence DB and start fresh to prove the sentinel is gone.
rm -f "$PERSIST/mosquitto.db"
start_unit
if [[ -z "$(observe_retained)" ]]; then
  pass "wiped database loses the retained sentinel (backup source proven)"
else
  fail "sentinel survived a database wipe; backup/restore would be vacuous"
fi

# Restore the backup and start fresh to prove the sentinel returns.
systemctl --user stop "$UNIT"
cp "$BACKUP/mosquitto.db" "$PERSIST/mosquitto.db"
start_unit
PID_POST_RESTORE="$(main_pid)"
if [[ "$(observe_retained)" == "tier9-sentinel-v1" ]]; then
  pass "restored database brings the retained sentinel back"
else
  fail "restored database did not bring the retained sentinel back"
fi
if [[ -n "$PID_PRE_BACKUP" && "$PID_PRE_BACKUP" != "$PID_POST_RESTORE" && "$PID_POST_RESTORE" != "0" ]]; then
  pass "unit re-created across backup/restore (MainPID ${PID_PRE_BACKUP} -> ${PID_POST_RESTORE})"
else
  fail "backup/restore did not demonstrably restart the unit (MainPID ${PID_PRE_BACKUP} -> ${PID_POST_RESTORE})"
fi

# --- checklist -------------------------------------------------------------
printf '\nTier-9 systemd operational smoke — %s\n' "$([[ "$FAILED" == "0" ]] && echo PASS || echo FAIL)" >&2
printf '  namespace: %s\n  unit: %s\n  broker log: %s\n' "$NS" "$UNIT" "$DIR/mosquitto.log" >&2
if [[ "$FAILED" != "0" ]]; then
  # Preserve the broker log for diagnosis before cleanup on failure.
  cp "$DIR/mosquitto.log" "${TMPDIR:-/tmp}/loam-tier9-${RUN_ID}.log" 2>/dev/null || true
  printf '  (log preserved at %s)\n' "${TMPDIR:-/tmp}/loam-tier9-${RUN_ID}.log" >&2
  exit 1
fi
exit 0
