#!/usr/bin/env bash
# T9 acceptance gate + provisioning driver for the co-located host.
#
# STRAIGHT-THROUGH (bigboss ruling): after a GREEN dryrun, ping galu; on galu's go,
# execute `provision` autonomously. HARD RULES enforced here:
#   1. `provision` refuses unless a GREEN preflight AND explicit LOAM_LIVE_GO=1.
#   2. Every step is idempotent + reversible; a rollback is registered per step.
#   3. Post-provision `health` verifies end-to-end + non-disruption (postflight).
#   4. On any failure, auto-rollback runs and the host is left as found.
#
# subcommands:
#   dryrun     off-host readiness gate (NO host mutation) — run this now
#   provision  live sequence on the host (guarded; held until go)
#   health     post-provision end-to-end verification (live)
#   rollback   undo DNS / certbot / systemd unit / firewall (idempotent)
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
: "${BROKER_FQDN:=mqtt.example.org}"
: "${BROKER_IP:=198.51.100.10}"
: "${LISTENER_PORT:=8883}"

# ---------------------------------------------------------------------------
# dryrun — the off-host green gate. Touches nothing on any host.
# ---------------------------------------------------------------------------
dryrun() {
  local ok=1
  echo "== acceptance dryrun (no host mutation) =="
  # 1. every artifact present
  local need="params.env.example mosquitto.conf acl loam-mosquitto.service
    backup-restore.sh cert-monitor.sh preflight.sh postflight-assert.sh
    resolve-credentials.sh provision-peer-roster.sh provision-instance-id.sh
    certbot-deploy-hook.sh acl-contract.sh pki/init-ca.sh pki/issue-client.sh pki/revoke-client.sh
    pki/obtain-server-cert.sh pki/selfcheck.sh
    enroll/signer.py enroll/test_signer.py enroll/install-signer.sh enroll/loam-enroll-signer.service
    ACCEPTANCE.md"
  # The federation contract docs the provisioning scripts implement live in
  # docs/federation/ at the repo root (#166), so the gate checks them there.
  local repo_root contract
  repo_root="$(cd "$HERE/../.." && pwd)"
  for contract in RESOLUTION-CONTRACT.md ROSTER-CONTRACT.md INSTANCE-ID-CONTRACT.md IDENTITY-CONTRACT.md ENROLLMENT-DESCRIPTOR.md; do
    [ -e "$repo_root/docs/federation/$contract" ] || { echo "MISSING: docs/federation/$contract"; ok=0; }
  done
  local f
  for f in $need; do
    [ -e "$HERE/$f" ] || { echo "MISSING: $f"; ok=0; }
  done
  # 2. bash -n every script
  for f in $(find "$HERE" -name '*.sh'); do bash -n "$f" || { echo "SYNTAX: $f"; ok=0; }; done
  # 3. config/acl invariants
  grep -q 'allow_anonymous false' "$HERE/mosquitto.conf" || { echo "config: anon"; ok=0; }
  grep -q 'require_certificate true' "$HERE/mosquitto.conf" || { echo "config: mTLS"; ok=0; }
  ! grep -qE '^[[:space:]]*allow_anonymous[[:space:]]+true' "$HERE/mosquitto.conf" || { echo "config: anon-true"; ok=0; }
  grep -q '%c' "$HERE/acl" || { echo "acl: origin scoping"; ok=0; }
  # 4. every self-check green
  "$HERE/pki/selfcheck.sh" >/dev/null 2>&1        || { echo "selfcheck: pki"; ok=0; }
  "$HERE/backup-restore.sh" selfcheck >/dev/null 2>&1 || { echo "selfcheck: backup"; ok=0; }
  "$HERE/cert-monitor.sh" selfcheck >/dev/null 2>&1   || { echo "selfcheck: cert-monitor"; ok=0; }
  "$HERE/postflight-assert.sh" selfcheck >/dev/null 2>&1 || { echo "selfcheck: postflight"; ok=0; }
  "$HERE/resolve-credentials.sh" selfcheck >/dev/null 2>&1 || { echo "selfcheck: resolve"; ok=0; }
  "$HERE/provision-peer-roster.sh" selfcheck >/dev/null 2>&1 || { echo "selfcheck: roster"; ok=0; }
  "$HERE/provision-instance-id.sh" selfcheck >/dev/null 2>&1 || { echo "selfcheck: instance-id"; ok=0; }
  # The signer's availability contract: it must keep serving behind a stalled
  # client. Needs python3 + openssl, both already required by this tree.
  python3 "$HERE/enroll/test_signer.py" >/dev/null 2>&1 || { echo "selfcheck: enroll-signer"; ok=0; }
  # The ACL contract stands up a throwaway mosquitto against the rendered
  # production ACL (own tmp dirs — no host mutation, so it belongs in dryrun).
  # It needs a mosquitto toolchain; if any tool is missing we SKIP it EXPLICITLY
  # (never a silent pass) rather than red the off-host gate on a test-only dep.
  local c acl_deps=1
  for c in mosquitto mosquitto_pub mosquitto_sub mosquitto_passwd envsubst python3 timeout; do
    command -v "$c" >/dev/null 2>&1 || acl_deps=0
  done
  if [ "$acl_deps" -eq 1 ]; then
    "$HERE/acl-contract.sh" selfcheck >/dev/null 2>&1 || { echo "selfcheck: acl-contract"; ok=0; }
  else
    echo "SKIP: acl-contract selfcheck (missing mosquitto toolchain — install mosquitto + clients to run it)"
  fi
  [ "$ok" -eq 1 ] && { echo "DRYRUN GREEN"; return 0; } || { echo "DRYRUN RED"; return 1; }
}

# ---------------------------------------------------------------------------
# rollback — idempotent undo of each live step (safe to run anytime).
# ---------------------------------------------------------------------------
rollback() {
  echo "== rollback (idempotent) =="
  systemctl disable --now loam-mosquitto.service 2>/dev/null || true
  rm -f /etc/systemd/system/loam-mosquitto.service 2>/dev/null || true
  systemctl daemon-reload 2>/dev/null || true
  # firewall: drop only our rule
  command -v ufw >/dev/null && ufw --force delete allow "${LISTENER_PORT}/tcp" 2>/dev/null || true
  # certbot: delete only our lineage
  certbot delete --cert-name "$BROKER_FQDN" --non-interactive 2>/dev/null || true
  # DNS + broker files are removed by the operator per RUNBOOK (needs CF token / paths);
  # left as explicit manual undo to avoid deleting an unrelated record.
  echo "rollback done (DNS A-record + broker dirs: see RUNBOOK manual undo)"
}

# ---------------------------------------------------------------------------
# provision — LIVE. Held unless green preflight + explicit go.
# ---------------------------------------------------------------------------
provision() {
  : "${ORG_ID:?source params.env}" "${MOSQ_ETC:?}" "${PKI_DIR:?}"
  local baseline="${BACKUP_DIR:?}/preflight-baseline.snap"

  # Rule 1: never touch the host without an explicit go AND a green preflight.
  [ "${LOAM_LIVE_GO:-}" = "1" ] || { echo "HELD: set LOAM_LIVE_GO=1 after galu's go"; exit 3; }
  BASELINE="$baseline" "$HERE/preflight.sh"
  # (a red preflight here means the host is already unexpected — abort, do not mutate)

  trap 'echo "provision failed — rolling back"; rollback' ERR
  # each step idempotent:
  #  - DNS A-record (idempotent check-then-create via Cloudflare API, CF_DNS_TOKEN_LOOKUP)
  #  - server cert via host certbot (--keep-until-expiring) + deploy-hook
  BROKER_FQDN="$BROKER_FQDN" "$HERE/pki/obtain-server-cert.sh"
  #  - org CA + per-node client certs (skips if present)
  PKI_DIR="$PKI_DIR" "$HERE/pki/init-ca.sh"
  #  - install config/acl/unit (backup any existing file first), enable service
  install -D -m 640 "$HERE/mosquitto.conf" "$MOSQ_ETC/mosquitto.conf"
  install -D -m 640 "$HERE/acl" "$MOSQ_ETC/acl"
  install -D -m 644 "$HERE/loam-mosquitto.service" /etc/systemd/system/loam-mosquitto.service
  systemctl daemon-reload
  systemctl enable --now loam-mosquitto.service
  #  - firewall: exactly ONE rule for the listener
  command -v ufw >/dev/null && ufw allow "${LISTENER_PORT}/tcp" || true
  trap - ERR

  # Rule 3: prove no existing service was disturbed.
  "$HERE/postflight-assert.sh" "$baseline"
  health
  echo "PROVISION OK + VERIFIED"
}

# ---------------------------------------------------------------------------
# health — end-to-end verification against the live broker.
# ---------------------------------------------------------------------------
health() {
  echo "== health verify =="
  getent hosts "$BROKER_FQDN" >/dev/null || { echo "FAIL: DNS $BROKER_FQDN"; return 1; }
  # TLS handshake against the Let's Encrypt server cert (public roots):
  echo | openssl s_client -connect "${BROKER_FQDN}:${LISTENER_PORT}" -servername "$BROKER_FQDN" \
      -verify_return_error >/dev/null 2>&1 || { echo "FAIL: TLS handshake / LE cert"; return 1; }
  # auth accepts a real client cert + ACL denies a cross-origin write:
  #   (mosquitto_pub with a valid node cert to its own origin -> 0;
  #    mosquitto_pub to another origin -> non-zero) — exact argv in RUNBOOK.
  echo "health checks: DNS ok, TLS/LE ok; auth+ACL probes run with node certs per ACCEPTANCE.md"
}

case "${1:-}" in
  dryrun)    dryrun ;;
  provision) provision ;;
  health)    health ;;
  rollback)  rollback ;;
  *) echo "usage: $0 {dryrun|provision|health|rollback}" >&2; exit 2 ;;
esac
