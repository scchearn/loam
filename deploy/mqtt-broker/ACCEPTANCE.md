# Broker deployment acceptance checklist

This checklist records evidence required before the broker is safe to hand to
federation clients. It is intentionally separate from the newcomer walkthrough:
`BROKER-SETUP.md` explains how to perform the work; this page says what must be
observed afterward.

## Current blockers

The following requirements are **currently unmet and blocked**. They are not
acceptance-passing evidence for this deployment:

- **Connector live transport:** blocked. The checked-in `acl` lacks grants for
  project-membership subscriptions, organization member-card subscription and
  publication, and the agent inbox. `open_transport` aborts on denied
  subscriptions, while the enrollment probe omits these paths; `connect` may
  succeed while the connector loops offline.
- **Cross-project denial:** blocked. The checked-in project wildcard grants
  cannot enforce cross-project denial. There is no supported ACL workaround
  yet; this needs an ACL/runtime correction outside documentation-only issue
  #163.

## Required evidence

| # | Acceptance criterion | Evidence |
| --- | --- | --- |
| 1 | TLS-only listener; anonymous and plaintext access are refused. | Rendered `mosquitto.conf`, `ss` output showing only the configured TLS port, and TLS/anonymous probes. |
| 2 | Client certificate CN is the authenticated MQTT principal. | A valid organization-CA client certificate receives an accepted CONNACK; Mosquitto has `require_certificate true` and `use_identity_as_username true`. |
| 3 | Organization/project ACLs and origin writes are isolated. | **BLOCKED — not acceptance-passing:** own-origin and cross-origin checks may be recorded, but the checked-in project wildcard grants cannot prove cross-project denial. No supported ACL workaround exists yet. |
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
| 15 | The connector stays online with project-membership and agent-inbox subscriptions plus organization member-card subscription/publication. | **BLOCKED — not acceptance-passing:** the checked-in `acl` lacks these grants; `open_transport` aborts on denied subscriptions, and the enrollment probe does not cover them. |

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
python3 enroll/test_signer.py
```

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
[`BROKER-SETUP.md`](../../docs/federation/BROKER-SETUP.md), section 5, is also
reference-only until the connector/ACL blockers and template-rendering defect
are cleared; do not execute or enable the broker from it while those blockers
remain.

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
