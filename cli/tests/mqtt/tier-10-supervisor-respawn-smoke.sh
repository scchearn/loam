#!/usr/bin/env bash
#
# Tier-10 operational smoke: prove that the OS supervisor actually respawns the
# connector's watchdog exit and leaves an inert exit alone — the half of
# connector-self-healing that a definition-rendering unit test can assert while
# the real path stays broken (the lesson of the acceptance saga, and the exact
# shape of the #112 incident on macOS).
#
# It does NOT stand up a broker or loam's mTLS identity. It exercises the two
# supervisor directives the connector's rendered definitions ship with, against
# a stub ExecStart, so the claim under test is precisely "given these directives,
# the supervisor respawns code 75 (EX_TEMPFAIL, the watchdog exit) and does not
# respawn code 0 (the inert exit)". The connector's own decision to exit 75 is
# proven by the unit tests (watchdog_verdict, the closed-port arming test); this
# proves the OS honors it.
#
# Linux: systemd --user, mirroring render_systemd_unit (Restart=on-failure,
#   RestartSec). macOS: launchd, mirroring render_launchagent_plist
#   (KeepAlive{SuccessfulExit=false}). A missing prerequisite is a reported
#   blocker (exit 2), never a fabricated pass. All state is transient.
#
set -euo pipefail

FAILED=0
pass() { printf '  [PASS] %s\n' "$1" >&2; }
fail() { printf '  [FAIL] %s\n' "$1" >&2; FAILED=1; }
blocker() { printf 'BLOCKER: %s\n' "$1" >&2; exit 2; }

# The watchdog's exit code and the connector's restart spacing, kept in lockstep
# with the WATCHDOG_EXIT_CODE const and the RestartSec in render_systemd_unit.
WATCHDOG_EXIT=75
RESTART_SEC=1

RUN_ID="$$-${RANDOM}"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/loam-tier10-${RUN_ID}.XXXXXX")"
# A counter file the stub reads to decide its exit code: fail (75) the first two
# runs, then succeed (0). If the supervisor respawns the failures we will see the
# counter climb past its start; if it respawns the success too, it would climb
# without bound (asserted against below).
COUNTER="${WORK}/runs"
printf '0' >"${COUNTER}"
STUB="${WORK}/stub.sh"
cat >"${STUB}" <<STUB_EOF
#!/usr/bin/env bash
n=\$(cat "${COUNTER}")
n=\$((n + 1))
printf '%s' "\${n}" >"${COUNTER}"
# First two invocations exit 75 (the watchdog's temp-fail); after that exit 0
# (the inert exit that must NOT be respawned).
if [[ "\${n}" -le 2 ]]; then
  exit ${WATCHDOG_EXIT}
fi
exit 0
STUB_EOF
chmod +x "${STUB}"

cleanup() {
  set +e
  case "$(uname -s)" in
    Linux) systemctl --user reset-failed "${UNIT}" >/dev/null 2>&1
           systemctl --user stop "${UNIT}" >/dev/null 2>&1 ;;
    Darwin) launchctl bootout "gui/$(id -u)/${LABEL}" >/dev/null 2>&1 ;;
  esac
  rm -rf "${WORK}"
}
trap cleanup EXIT

runs() { cat "${COUNTER}"; }

case "$(uname -s)" in
  Linux)
    command -v systemd-run >/dev/null || blocker "systemd-run is not available"
    command -v systemctl >/dev/null || blocker "systemctl is not available"
    systemctl --user show-environment >/dev/null 2>&1 ||
      blocker "no working 'systemctl --user' session bus on this host"
    UNIT="loam-tier10-${RUN_ID}.service"
    # The two directives from render_systemd_unit that govern respawn.
    systemd-run --user --unit="${UNIT}" \
      --property=Restart=on-failure \
      --property=RestartSec=${RESTART_SEC} \
      "${STUB}" >/dev/null 2>&1 ||
      blocker "systemd-run could not start the transient unit"

    # Give the supervisor time to run the two failing invocations plus RestartSec
    # spacing, then the successful one; then a margin to prove it stops there.
    sleep $(( (RESTART_SEC + 1) * 3 + 2 ))

    n="$(runs)"
    # Two watchdog exits were respawned into a third invocation: the counter
    # reaching 3 is systemd having restarted code 75 at least twice.
    if [[ "${n}" -ge 3 ]]; then
      pass "systemd respawned the watchdog exit (code ${WATCHDOG_EXIT}) — ${n} invocations"
    else
      fail "systemd did not respawn the watchdog exit — only ${n} invocation(s)"
    fi

    # The third invocation exits 0; Restart=on-failure must leave it down. Wait
    # again and prove the counter did not climb further.
    before="${n}"
    sleep $(( (RESTART_SEC + 1) * 2 ))
    after="$(runs)"
    if [[ "${after}" == "${before}" ]]; then
      pass "systemd left the inert exit (code 0) down — no respawn past ${after}"
    else
      fail "systemd respawned a clean exit: ${before} -> ${after}"
    fi
    ;;

  Darwin)
    command -v launchctl >/dev/null || blocker "launchctl is not available"
    LABEL="io.loam.tier10.${RUN_ID}"
    PLIST="${WORK}/${LABEL}.plist"
    # Mirror render_launchagent_plist's respawn directive: KeepAlive with
    # SuccessfulExit=false respawns a nonzero exit, leaves a clean exit down.
    cat >"${PLIST}" <<PLIST_EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>${LABEL}</string>
  <key>ProgramArguments</key><array><string>${STUB}</string></array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
</dict></plist>
PLIST_EOF
    launchctl bootstrap "gui/$(id -u)" "${PLIST}" >/dev/null 2>&1 ||
      blocker "launchctl could not bootstrap the transient job"
    sleep 8
    n="$(runs)"
    if [[ "${n}" -ge 3 ]]; then
      pass "launchd respawned the watchdog exit (code ${WATCHDOG_EXIT}) — ${n} invocations"
    else
      fail "launchd did not respawn the watchdog exit — only ${n} invocation(s)"
    fi
    before="${n}"
    sleep 5
    after="$(runs)"
    if [[ "${after}" == "${before}" ]]; then
      pass "launchd left the inert exit (code 0) down — no respawn past ${after}"
    else
      fail "launchd respawned a clean exit: ${before} -> ${after}"
    fi
    ;;

  *)
    blocker "tier-10 respawn smoke supports Linux (systemd) and macOS (launchd) only"
    ;;
esac

if [[ "${FAILED}" == "1" ]]; then
  printf 'tier-10 supervisor respawn smoke: FAIL\n' >&2
  exit 1
fi
printf 'tier-10 supervisor respawn smoke: PASS\n' >&2
