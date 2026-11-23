#!/usr/bin/env bash
# Real-Mosquitto contract for the production broker ACL (#173).
#
# Renders the checked-in `./acl` and proves it against a throwaway ephemeral
# mosquitto (no host mutation — its own tmp dir, plaintext loopback listener,
# password-file auth). TLS is orthogonal to topic authorization: the ACL keys
# only on %u (username = principal_id) and %c (client-id = instance_id), which a
# password-file listener reproduces exactly (username = -u, client-id = -i).
#
# What it proves (topic shapes derived from the connector, not re-guessed):
#   (a) the enrollment probe's required_filters deliver          [connector.rs:187]
#   (b) EVERY connector live_filters subscription delivers        [connector.rs:2445]
#   (c) the retained self member-card publish succeeds            [connector.rs:2597]
#   (d) a foreign-ORGANISATION read AND write are DENIED
#
# Settled trust model: the broker is a dumb pipe; ORGANISATION is the only trust
# boundary; project is routing (the org-scoped `+` wildcard is correct).
# "Foreign" therefore means foreign-ORG — a topic under a different org root.
#
# Mosquitto grants every SUBSCRIBE and enforces reads at DELIVERY, so a read
# grant can only be proven by a message actually arriving. Each read target is
# seeded as a retained message by a legitimately-scoped writer; the connector
# then subscribes and must receive it. Writes are proven via the MQTTv5 QoS1
# PUBACK ("Not authorized" => reason 135).
#
# Against today's un-fixed ACL, (b) agent-inbox/membership/member-card reads and
# (c) the member-card write FAIL by design — a peer change adds those grants.
# Do not weaken this test to match the current ACL.
#
# Requires: mosquitto, mosquitto_pub, mosquitto_sub, mosquitto_passwd, envsubst,
#           python3, timeout.
#
# subcommands:
#   selfcheck   render ./acl and run the full contract (throwaway broker)
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ACL_SRC="${ACL_SRC:-$HERE/acl}"     # overridable so the contract can be dry-run
                                    # against a candidate ACL before it ships.

# ---- fixed identities the whole contract is expressed in --------------------
ORG="loam-org"; PROJ="proj-alpha"; FOREIGN_ORG="loam-org-evil"
CONN_PRIN="conn-prin"; CONN_INST="conn-inst"; CONN_AGENT="conn-agent"
PEER_INST="peer-inst"
SEED_AUTH="seed-auth"               # models the enrollment authority (see block below)
PW="x"                              # ACL is username-scoped; the password is irrelevant

RECV_W=1                            # mosquitto_sub -W: seconds to wait for retained
FAILED=0

fail() { echo "  FAIL: $*"; FAILED=1; }

# --- broker lifecycle --------------------------------------------------------
start_broker() {
  DIR="$(mktemp -d)"; BROKER=""
  # arm cleanup BEFORE any early exit so a failed broker start never leaks $DIR.
  trap stop_broker EXIT
  # Render the production ACL (only ${ORG_ID} is substituted; %u/%c are mosquitto's).
  ORG_ID="$ORG" envsubst '${ORG_ID}' < "$ACL_SRC" > "$DIR/acl"
  # TEST SCAFFOLDING (never shipped). The ACL header documents that provisioning
  # appends `user <principal>` blocks; this is one such block standing in for the
  # enrollment authority. It lets the harness plant the retained membership card
  # (no pattern rule writes `membership`) and a foreign-org message the org client
  # must be denied from reading. The connector-under-test uses ONLY the pattern
  # rules above — never this user.
  cat >> "$DIR/acl" <<EOF

user $SEED_AUTH
topic write loam/v1/$ORG/+/membership
topic write loam/v1/$FOREIGN_ORG/+/event/+
EOF
  chmod 0700 "$DIR/acl"
  printf '%s:%s\n' "$CONN_PRIN" "$PW" > "$DIR/pw.txt"   # any user; password not enforced by ACL
  mosquitto_passwd -U "$DIR/pw.txt" 2>/dev/null
  # extra known users share one password file
  for u in "$PEER_INST" "$SEED_AUTH" foreign-prin; do
    mosquitto_passwd -b "$DIR/pw.txt" "$u" "$PW" 2>/dev/null
  done
  chmod 0700 "$DIR/pw.txt"
  # ponytail: OS-assigned free port, tiny bind-then-release race — fine for a
  # throwaway loopback broker; the readiness poll below fails loudly if it clashed.
  PORT="$(python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1]);s.close()')"
  cat > "$DIR/m.conf" <<EOF
listener $PORT 127.0.0.1
allow_anonymous false
password_file $DIR/pw.txt
acl_file $DIR/acl
EOF
  mosquitto -c "$DIR/m.conf" > "$DIR/broker.log" 2>&1 &
  BROKER=$!
  # wait until the listener is actually accepting
  for _ in $(seq 1 50); do
    grep -q "listen socket on port $PORT" "$DIR/broker.log" && return 0
    kill -0 "$BROKER" 2>/dev/null || { cat "$DIR/broker.log"; echo "broker died"; exit 2; }
    sleep 0.1
  done
  cat "$DIR/broker.log"; echo "broker not ready"; exit 2
}
stop_broker() { kill "$BROKER" 2>/dev/null || true; wait "$BROKER" 2>/dev/null || true; rm -rf "$DIR"; }

# --- primitives --------------------------------------------------------------
# seed a RETAINED message; abort the harness if the *writer* itself is denied
# (that would mean the scaffolding, not the ACL-under-test, is wrong).
seed() { # user clientid topic payload
  local out
  out="$(mosquitto_pub -V 5 -q 1 -r -p "$PORT" -u "$1" -P "$PW" -i "$2" -t "$3" -m "$4" 2>&1 || true)"
  if grep -qi 'not authorized' <<<"$out"; then
    echo "HARNESS ERROR: seed writer '$1' denied on $3 — $out"; stop_broker; exit 2
  fi
}
recv() { # user clientid filter  -> stdout = payloads received (retained)
  timeout 5 mosquitto_sub -V 5 -p "$PORT" -u "$1" -P "$PW" -i "$2" -t "$3" -W "$RECV_W" 2>/dev/null || true
}
conn_recv() { recv "$CONN_PRIN" "$CONN_INST" "$1"; }
pub_out() { # user clientid topic [-r] -> stdout = mosquitto_pub stderr
  mosquitto_pub -V 5 -q 1 ${4:-} -p "$PORT" -u "$1" -P "$PW" -i "$2" -t "$3" -m payload 2>&1 || true
}

expect_recv() { # label filter marker
  grep -q "$3" <<<"$(conn_recv "$2")" || fail "$1: no delivery on '$2'"
}
expect_empty() { # label filter
  [ -z "$(conn_recv "$2")" ] || fail "$1: unexpected delivery on '$2'"
}
expect_write_ok() { # label user clientid topic
  grep -qi 'not authorized' <<<"$(pub_out "$2" "$3" "$4")" && fail "$1: write denied on '$4'" || true
}
expect_write_denied() { # label user clientid topic
  grep -qi 'not authorized' <<<"$(pub_out "$2" "$3" "$4")" || fail "$1: write NOT denied on '$4'"
}

selfcheck() {
  local dep
  for dep in mosquitto mosquitto_pub mosquitto_sub mosquitto_passwd envsubst python3 timeout; do
    command -v "$dep" >/dev/null || { echo "acl-contract: needs $dep (requires: mosquitto mosquitto_pub mosquitto_sub mosquitto_passwd envsubst python3 timeout)"; exit 2; }
  done
  start_broker                        # arms the stop_broker EXIT trap itself
  local B="loam/v1/$ORG/$PROJ"        # org+project root
  local M="loam/v1/$ORG/members"      # org-scoped member-card root

  # -- seed every read target as retained, each via a legitimately-scoped writer
  # probe + own-origin reads (connector writes its own origin):
  seed "$CONN_PRIN" "$CONN_INST" "$B/event/$CONN_INST"                 PROBE_EVT
  seed "$CONN_PRIN" "$CONN_INST" "$B/state/$CONN_INST/k"               PROBE_STATE
  # peer-origin reads (a colleague writes its own origin / addresses the connector):
  seed "$PEER_INST" "$PEER_INST" "$B/event/$PEER_INST"                 PEER_EVT
  seed "$PEER_INST" "$PEER_INST" "$B/state/$PEER_INST/k"               PEER_STATE
  seed "$PEER_INST" "$PEER_INST" "$B/inbox/instance/$CONN_INST/$PEER_INST/m"  IN_INST
  seed "$PEER_INST" "$PEER_INST" "$B/inbox/principal/$CONN_PRIN/$PEER_INST/m" IN_PRIN
  seed "$PEER_INST" "$PEER_INST" "$B/inbox/agent/$CONN_AGENT/$PEER_INST/m"    IN_AGENT
  # membership (broker-served, written by the enrollment authority):
  seed "$SEED_AUTH" "$SEED_AUTH" "$B/membership"                       MEMBERSHIP
  # a foreign-ORG message that must stay invisible to the org client:
  seed "$SEED_AUTH" "$SEED_AUTH" "loam/v1/$FOREIGN_ORG/$PROJ/event/evil" FOREIGN_EVT

  echo "== (a) enrollment probe: required_filters deliver =="
  expect_recv "probe event"          "$B/event/$CONN_INST"                 PROBE_EVT
  expect_recv "probe state"          "$B/state/$CONN_INST/+"               PROBE_STATE
  expect_recv "probe own-inbox"      "$B/inbox/instance/$CONN_INST/+/+"    IN_INST

  echo "== (b) live_filters: every subscription delivers =="
  expect_recv "live event/+"         "$B/event/+"                          PEER_EVT
  expect_recv "live state/+/+"       "$B/state/+/+"                        PEER_STATE
  expect_recv "live inbox/instance"  "$B/inbox/instance/$CONN_INST/+/+"    IN_INST
  expect_recv "live inbox/principal" "$B/inbox/principal/$CONN_PRIN/+/+"   IN_PRIN
  expect_recv "live inbox/agent"     "$B/inbox/agent/$CONN_AGENT/+/+"      IN_AGENT   # post-fix grant
  expect_recv "live membership"      "$B/membership"                       MEMBERSHIP # post-fix grant

  echo "== (c) retained self member-card publish succeeds =="
  expect_write_ok "member-card write" "$CONN_PRIN" "$CONN_INST" "$M/$CONN_INST"   # post-fix grant
  # publish it retained (may be denied pre-fix — that is the failure above, not a
  # harness error, so this does NOT use seed()), then prove members/+ delivers it:
  mosquitto_pub -V 5 -q 1 -r -p "$PORT" -u "$CONN_PRIN" -P "$PW" -i "$CONN_INST" \
    -t "$M/$CONN_INST" -m OWN_CARD 2>/dev/null || true
  grep -q OWN_CARD <<<"$(conn_recv "$M/+")" \
    || fail "member-card read: own card not delivered on '$M/+'"                   # post-fix grant

  echo "== (d) foreign-ORGANISATION read AND write are denied; own-origin write scoping =="
  expect_empty        "foreign read"  "loam/v1/$FOREIGN_ORG/+/event/+"
  expect_write_denied "foreign write" "$CONN_PRIN" "$CONN_INST" "loam/v1/$FOREIGN_ORG/$PROJ/event/$CONN_INST"
  # same-ORG cross-origin: writes are bound to the writer's own %c, so the
  # connector may not write into a peer's origin topic — proves origin isolation.
  expect_write_denied "cross-origin write" "$CONN_PRIN" "$CONN_INST" "$B/event/$PEER_INST"

  if [ "$FAILED" -eq 0 ]; then echo "ACL CONTRACT PASS"; else echo "ACL CONTRACT FAIL"; return 1; fi
}

case "${1:-}" in
  selfcheck) selfcheck ;;
  *) echo "usage: $0 selfcheck" >&2; exit 2 ;;
esac
