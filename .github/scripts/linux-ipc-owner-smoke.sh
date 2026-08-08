#!/usr/bin/env bash
# Slice C T6/T15 Linux Unix-socket owner smoke.
#
# Proves on a real host what no in-process test can, against the two barriers
# the Unix endpoint actually has — the same two-barrier shape the Windows gate
# proves, with the filesystem standing in for the pipe DACL:
#
#   1. The `0700` run directory and `0600` socket refuse a foreign UID at
#      connect(2). No connection exists, so there is nothing to reject later.
#   2. The SO_PEERCRED / getpeereid peer proof refuses a peer whose effective
#      UID differs from the connector's, before the codec reads a byte — even
#      one that reached the socket.
#
# Barrier 2 is the one that matters and the one that is easy to leave
# unproven: with barrier 1 in place a foreign peer never arrives, so the
# rejection branch of `verify_peer` never runs and the gate passes vacuously.
# To exercise it, the path is relaxed FROM THE OWNER SIDE ONLY, after bind, so
# a real second account can reach the socket and be refused by the credential
# check itself. That is deliberate and is exactly what the Windows gate does
# when it reaches the endpoint from a logon session the DACL admits.
#
# Verdicts are evidence, never exit codes: each client reports the UID it
# connected as, and the server reports the stage it rejected at and how many
# frames it has served. A missing prerequisite is a BLOCKER, not a pass.
set -euo pipefail

USER_NAME="loamsmoke"
NONCE="$$-$(date +%s%N)"
WORK="$(mktemp -d)"                 # owner-only: logs live here, not in the shared dir
SHARED="/tmp/loam-ipc-shared-${NONCE}"
ROOT="/tmp/loam-ipc-${NONCE}"
LOG="${WORK}/server.log"
ERRLOG="${WORK}/server.err.log"
CLIENT="${SHARED}/client.py"
SERVER_PID=""
CREATED=0

fail() { echo "linux ipc owner smoke: $*" >&2; exit 1; }

cleanup() {
  local status=$?
  if [ "$status" -ne 0 ]; then
    # The fixture's own output is the only view of the server side, so a red run
    # is diagnosable instead of just "the socket did not answer".
    echo "--- endpoint fixture output ---"
    [ -f "$LOG" ] && cat "$LOG" || true
    echo "--- endpoint fixture stderr ---"
    [ -f "$ERRLOG" ] && cat "$ERRLOG" || true
  fi
  [ -n "$SERVER_PID" ] && kill "$SERVER_PID" 2>/dev/null || true
  [ "$CREATED" -eq 1 ] && sudo userdel "$USER_NAME" 2>/dev/null || true
  rm -rf "$WORK" "$SHARED" "$ROOT" 2>/dev/null || true
  return $status
}
trap cleanup EXIT

command -v sudo >/dev/null || fail "sudo is required to create the temporary account"
command -v python3 >/dev/null || fail "python3 is required for the framed client"
sudo -n true 2>/dev/null || fail "passwordless sudo is required to create the temporary account"

mkdir -p "$SHARED"
chmod 0777 "$SHARED"

# 1. Build and launch the endpoint fixture, then learn its socket path.
cargo +1.94.1 test --locked --test ipc_owner --no-run >/dev/null 2>&1 ||
  fail "building the ipc_owner test binary failed"
EXE="$(find target/debug/deps -maxdepth 1 -name 'ipc_owner-*' -type f -perm -u+x -printf '%T@ %p\n' |
  sort -rn | head -1 | cut -d' ' -f2-)"
[ -n "$EXE" ] || fail "no ipc_owner test binary was produced"

LOAM_IPC_SMOKE_ROOT="$ROOT" LOAM_IPC_SMOKE_SECONDS=300 \
  "$EXE" --ignored --nocapture --exact unix_owner::unix_endpoint_serves_the_alternate_user_smoke \
  >"$LOG" 2>"$ERRLOG" &
SERVER_PID=$!

SOCKET=""
for _ in $(seq 1 300); do
  SOCKET="$(sed -n 's/^LOAM_SOCKET_PATH=//p' "$LOG" | head -1)"
  [ -n "$SOCKET" ] && break
  sleep 0.2
done
[ -n "$SOCKET" ] || fail "the endpoint fixture never reported its socket path"
OWNER_UID="$(sed -n 's/^LOAM_OWNER_UID=//p' "$LOG" | head -1)"
[ -n "$OWNER_UID" ] || fail "the endpoint fixture never reported its owning uid"
echo "endpoint socket: $SOCKET (owner uid $OWNER_UID)"
echo "endpoint modes: $(stat -c '%a %U' "$ROOT") dir, $(stat -c '%a %U' "$SOCKET") socket"
[ "$(stat -c '%a' "$ROOT")" = "700" ] || fail "the run directory is not 0700"
[ "$(stat -c '%a' "$SOCKET")" = "600" ] || fail "the socket is not 0600"

served_frames() { sed -n 's/^LOAM_SERVED_FRAMES=//p' "$LOG" | tail -1; }

# The one client every case runs. It reports what it observed — refused at
# connect, opened but never served, or opened and served — and the UID it
# observed it from.
cat >"$CLIENT" <<'PY'
import os, socket, struct, sys

path, out = sys.argv[1], sys.argv[2]
who = "uid %d" % os.geteuid()


def say(outcome, detail):
    with open(out, "w") as handle:
        handle.write("%s as %s :: %s\n" % (outcome, who, detail))


stream = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
stream.settimeout(15)
try:
    stream.connect(path)
except PermissionError as error:
    say("denied-at-connect", "%s: %s" % (type(error).__name__, error))
    sys.exit(3)
except OSError as error:
    say("error", "%s: %s" % (type(error).__name__, error))
    sys.exit(4)

# The connection exists, so the filesystem admitted this UID. Everything from
# here tests the second barrier: whether the connector will SERVE it.
body = b"ping"
try:
    stream.sendall(struct.pack(">I", len(body)) + body)
    header = stream.recv(4)
    if not header:
        say("unserved", "server closed without answering")
        sys.exit(0)
    if len(header) != 4:
        say("unserved", "server sent a short header")
        sys.exit(0)
    length = struct.unpack(">I", header)[0]
    if length == 0 or length > 1024:
        say("unserved", "server sent an implausible length %d" % length)
        sys.exit(0)
    payload = stream.recv(length)
    say("served", "server answered %r" % payload)
    sys.exit(5)
except (socket.timeout, ConnectionResetError, BrokenPipeError, OSError) as error:
    say("unserved", "connection failed after connect: %s: %s" % (type(error).__name__, error))
    sys.exit(0)
finally:
    stream.close()
PY
chmod 0755 "$CLIENT"

read_verdict() {
  local path="$1" seconds="$2" text=""
  for _ in $(seq 1 $((seconds * 5))); do
    if [ -s "$path" ]; then
      text="$(tr -d '\n' <"$path")"
      [ -n "$text" ] && { echo "$text"; return 0; }
    fi
    sleep 0.2
  done
  return 1
}

# 2. Positive control: the owner round-trips one frame. Without this every
#    denial below would prove nothing — an unreachable socket also "denies".
python3 "$CLIENT" "$SOCKET" "${SHARED}/owner.out" || true
verdict="$(read_verdict "${SHARED}/owner.out" 20)" || fail "the owner control left no verdict"
echo "same-user positive control: $verdict"
case "$verdict" in
  served*) ;;
  *) fail "the owner was not served: $verdict" ;;
esac
for _ in $(seq 1 50); do [ "$(served_frames)" = "1" ] && break; sleep 0.2; done
[ "$(served_frames)" = "1" ] || fail "the endpoint did not record serving the owner's frame"

sudo useradd -M -N -s /usr/sbin/nologin "$USER_NAME"
CREATED=1
PEER_UID="$(id -u "$USER_NAME")"
[ "$PEER_UID" != "$OWNER_UID" ] || fail "the temporary account shares the connector's uid"
echo "temporary account: $USER_NAME (uid $PEER_UID)"

# 3. Barrier 1 — the 0700 run directory. A foreign UID cannot even reach the
#    socket, so it is refused at connect(2) with no connection to reject later.
sudo -u "$USER_NAME" python3 "$CLIENT" "$SOCKET" "${SHARED}/barrier1.out" || true
verdict="$(read_verdict "${SHARED}/barrier1.out" 20)" || fail "the barrier-1 client left no verdict"
echo "foreign uid, 0700 directory: $verdict"
case "$verdict" in
  denied-at-connect*) ;;
  served*) fail "a foreign uid was served through the 0700 directory: $verdict" ;;
  *) fail "barrier 1 was inconclusive: $verdict" ;;
esac

# 4. Barrier 2 — SO_PEERCRED / getpeereid. Relax the path from the owner side so
#    the foreign peer actually reaches the socket, which is the only way this
#    branch of verify_peer ever runs. The endpoint checks modes at bind, not per
#    accept, so this does not disturb the live listener — and that is the point:
#    the credential proof must stand on its own, without the filesystem's help.
chmod 0711 "$ROOT"
chmod 0666 "$SOCKET"
echo "relaxed for barrier 2: $(stat -c '%a' "$ROOT") dir, $(stat -c '%a' "$SOCKET") socket"
before="$(served_frames)"
sudo -u "$USER_NAME" python3 "$CLIENT" "$SOCKET" "${SHARED}/barrier2.out" || true
verdict="$(read_verdict "${SHARED}/barrier2.out" 30)" || fail "the barrier-2 client left no verdict"
echo "foreign uid, reachable socket: $verdict"
case "$verdict" in
  served*) fail "the connector served a foreign uid: $verdict" ;;
  denied-at-connect*)
    fail "the foreign peer still could not reach the socket, so the peer proof was never exercised: $verdict" ;;
  unserved*) ;;
  *) fail "barrier 2 was inconclusive: $verdict" ;;
esac

# The verdict says the peer was not served; it does not say why. Require the
# endpoint to name the stage, so a future regression that drops the connection
# for some unrelated reason cannot be read as a credential rejection.
stage=""
for _ in $(seq 1 50); do
  stage="$(sed -n 's/^loam ipc: peer rejected at //p' "$ERRLOG" | tail -1)"
  [ -n "$stage" ] && break
  sleep 0.2
done
[ -n "$stage" ] || fail "the endpoint never reported rejecting the foreign peer"
echo "foreign uid was rejected at: $stage"
[ "$stage" = "unauthorized-peer" ] ||
  fail "the foreign peer was rejected at '$stage', not by the peer-credential proof"

# The pre-parse sentinel: the codec is reachable only through a VerifiedConn, so
# a rejected peer cannot advance the served-frame counter. If it moved, the
# parser ran for an unauthorized peer.
after="$(served_frames)"
[ "$before" = "$after" ] ||
  fail "the parser advanced for an unauthorized peer ($before -> $after frames)"
echo "served frames unchanged across the denial: $after"

# 5. The endpoint survived the rejection and still serves its owner.
python3 "$CLIENT" "$SOCKET" "${SHARED}/owner2.out" || true
verdict="$(read_verdict "${SHARED}/owner2.out" 20)" || fail "the second owner control left no verdict"
echo "same-user control after the denial: $verdict"
case "$verdict" in
  served*) ;;
  *) fail "the endpoint stopped serving its owner after a denial: $verdict" ;;
esac

echo "linux ipc owner smoke OK"
