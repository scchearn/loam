# Broker deployment acceptance checklist

This checklist records evidence required before the broker is safe to hand to
federation clients. It is intentionally separate from the newcomer walkthrough:
`BROKER-SETUP.md` explains how to perform the work; this page says what must be
observed afterward.

## Trust model (settled)

The broker is a **dumb pipe**. Organization is the only trust boundary: an
organization client certificate admits you to the org bus, and the ACL is
scoped to the single org root with own-origin (`%c`) write scoping and
own-inbox reads. Project is a routing/capability concept, not a broker-enforced
boundary — you are in a project because you were invited with its id, not
because the broker polices it. The org-scoped project `+` wildcard is therefore
correct and stays.

Cross-project confidentiality and delivery filtering live in the connector
application layer, not the ACL; the broker holds no project-membership state.
Member cards (`loam/v1/{org}/members/+`) make project sharing visible org-wide.
There is **no cross-project denial at the broker, and that is by design** — a
guessable project id is acceptable because project is not a confidentiality
boundary against fellow org members.

## Required evidence

| # | Acceptance criterion | Evidence |
| --- | --- | --- |
| 1 | TLS-only listener; anonymous and plaintext access are refused. | Rendered `mosquitto.conf`, `ss` output showing only the configured TLS port, and TLS/anonymous probes. |
| 2 | Client certificate CN is the authenticated MQTT principal. | A valid organization-CA client certificate receives an accepted CONNACK; Mosquitto has `require_certificate true` and `use_identity_as_username true`. |
| 3 | Organization ACL and origin writes are isolated. | `acl-contract.sh selfcheck` stands up a throwaway Mosquitto against the rendered production ACL and proves a foreign-**organization** read and write are both denied, alongside own-origin (`%c`) write scoping and own-inbox reads. Project is not a broker confidentiality boundary (by design), so no cross-project denial is claimed. |
| 4 | The organization CA issues valid client certificates and revocation works. | `pki/selfcheck.sh` passes; a revoked certificate is refused after the broker reloads `crl.pem`. |
| 5 | The broker restarts without losing retained state. | A retained sentinel survives a planned service restart and the service is enabled. |
| 6 | Backup and restore recover broker-owned data. | `backup-restore.sh selfcheck` passes and a before/after sentinel comparison is recorded. |
| 7 | Certificate expiry is visible without disrupting services. | `cert-monitor.sh selfcheck` passes; the monitor service/timer is installed and active. |
| 8 | Existing host services were not changed. | `preflight.sh` and `postflight-assert.sh` show no removed/changed pre-existing unit, config, PID, or listener. |
| 9 | The enrollment signer is available and bounded. | `python3 enroll/test_signer.py` passes; the signer service is active on the intended interface/port and does not log passwords, CSRs, or certificates. |
| 10 | A new machine can enroll without copying a private key. | A real `loam federation connect --token-file ...` creates a local identity, completes the broker capability probe, and stores the enrollment. |
| 11 | Peer admission uses concrete principals and bare instance ids. | `provision-peer-roster.sh selfcheck` passes; a complete roster admits the listed peer and refuses an unlisted peer. |
| 12 | Instance identity is consistent across certificate, enrollment, client id, and topic origin. | `provision-instance-id.sh selfcheck` passes; no identity-mismatch or source mismatch occurs in the connection probe. |
| 13 | Two machines can use one project without sharing a client id. | Both sessions authenticate at once, exchange authorized frames, and keep distinct instance ids. |
| 14 | Read-only inspection does not imply live liveness. | Human `loam federation status` output (and the JSON status field) records that the live broker session was not observed; `list` remains an inventory. Current liveness is evidenced separately through service/broker logs. |
| 15 | The connector stays online with project-membership and agent-inbox subscriptions plus organization member-card subscription/publication. | `acl-contract.sh selfcheck` proves every connector live subscription delivers (project `membership`, agent inbox, and organization member-card `members/+`) and the retained self member-card publish (`members/%c`) succeeds against the rendered production ACL. |

## Off-host checks

These checks use throwaway directories and do not mutate a production host:

```sh
./pki/selfcheck.sh
./backup-restore.sh selfcheck
./cert-monitor.sh selfcheck
./postflight-assert.sh selfcheck
./resolve-credentials.sh selfcheck
./provision-peer-roster.sh selfcheck
./provision-instance-id.sh selfcheck
./acl-contract.sh selfcheck
python3 enroll/test_signer.py
```

The ACL contract proves the rendered ACL's behaviour against a throwaway
Mosquitto; it does not prove a live production deployment. See
[what this checklist does not prove](#what-this-checklist-does-not-prove).

The complete safe gate runs them together and checks the required files and
configuration invariants:

```sh
./acceptance-gate.sh dryrun
```

## Live sequence

The `acceptance-gate.sh provision` stage is currently unavailable/unsafe: it
copies the tracked `${VARS}` templates without rendering them. Do **not** run
`LOAM_LIVE_GO=1 ./acceptance-gate.sh provision`; setting the guard does not
render the templates. The envsubst-rendered manual deployment sequence in
[`BROKER-SETUP.md`](../../docs/federation/BROKER-SETUP.md), section 5, is the
supported render-and-install path; the automated `provision` stage stays
unavailable until the template-rendering defect is fixed.

On the target host, capture a baseline before any broker-owned write:

```sh
BASELINE="$BACKUP_DIR/preflight-baseline.snap" ./preflight.sh
```

After the manual deployment, `acceptance-gate.sh health` remains a truthful
limited verification of DNS and the public TLS handshake only; it does not run
the client-authentication or ACL probes. Capture those remaining acceptance
criteria separately. Do not run `acceptance-gate.sh rollback` on a pre-existing
host: it removes the systemd unit, firewall rule, and certificate lineage
without proving this deployment created them. Use the canonical
[manual restore procedure](../../docs/federation/BROKER-SETUP.md#restore-that-manual-replacement)
instead. Review DNS records and broker directories separately so cleanup cannot
remove data owned by another service.

## What this checklist does not prove

- A successful enrollment proves that the broker accepted a capability probe at
  that moment. It does not prove continuous availability afterward.
- `status` and `list` are read-only and egress-free; they do not observe a live
  MQTT session.
- Certificate revocation does not erase retained MQTT state.
- A missing, empty, or one-sided peer roster is converted by provisioning into
  a self-only project session; it does not admit colleagues and is not proof
  that the broker is unavailable. Malformed or wildcard rosters produce
  `no-peer-roster` and are refused.
