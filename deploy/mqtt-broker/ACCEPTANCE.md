# Acceptance mapping — broker deployment

Every spec acceptance criterion → the concrete T2 security-shape assertion and/or T9
operational-checklist item that proves it against the live deployment, plus the
recorded evidence. "Done" means verified on `the co-located host`, not merely authored.
This **reuses** the parent spec's tiers T2 (security shape) and T9 (operational smoke)
from `specs/loam-mqtt-transport.md` — it adds no task to any A–E plan.

The executable form of this table is `acceptance-gate.sh` (T9), run after
`preflight.sh` and before `postflight-assert.sh` on the host.

| # | Acceptance criterion | Proof (T2 shape / T9 item) | Evidence | Gate |
| - | -------------------- | -------------------------- | -------- | ---- |
| 1 | TLS-only, no anonymous/plaintext | T2: connect w/o cert refused; only 8883 TLS listener | `acceptance-gate.sh` anon+plaintext probes rejected | T9 |
| 2 | mTLS principal = client-cert CN authorizes `data.from.principal_id` | T2: `require_certificate`+`use_identity_as_username`; auth accepts a real org-CA client cert | gate: valid client cert connects; username = CN | T9 |
| 3 | `{org-id}` ACL, origin-prefix (`%c`) write scoping; cross-origin/project/org denied | T2/T6: own-origin publish allowed; cross-origin/cross-org denied | gate: allowed + denied publish probes | T9 |
| 4 | org CA issues client certs (issue/rotate/revoke+CRL); server cert via certbot | T4 selfcheck (off-host) + T9: server cert = LE for `mqtt.example.org` | `pki/selfcheck.sh` PASS; live LE cert present | T4+T9 |
| 5 | Persistence survives restart + reboot; systemd auto-restart | T9: retained sentinel survives `systemctl restart` | gate restart probe | T9 |
| 6 | Backup/restore recovers retained state (before/after evidence) | T9: `backup-restore.sh` cycle w/ observed sentinel | before/after sentinel match | T9 |
| 7 | Cert-expiry monitoring surfaces near-expiry; non-disruptive rotation | T7 selfcheck (off-host) + T9: timer installed | `cert-monitor.sh selfcheck` PASS; timer active | T7+T9 |
| 8 | Revoked client refused; retained state cleared; independent of Git | T9: revoke → reconnect denied after CRL reload | gate revoke probe | T9 |
| 9 | Passes T2 shape + T9 checklist on host, no A–E edits | whole gate | full `acceptance-gate.sh` green | T9 |
| 10 | Non-disruption: only mosquitto + 8883 added; all services unchanged | T11: `preflight` → provision → `postflight-assert` | postflight OK; zero drift | T9 |
| 11 | Op-wiring A: `credential_ref`/`ca_ref` resolve to real broker creds | T12 selfcheck (off-host) + T9: connector connects using resolved cred | `resolve-credentials.sh selfcheck` PASS; live connect | T12+T9 |
| 12 | Op-wiring B: peer roster admits colleague frames, rejects strangers | T13 selfcheck (off-host) + T9: two-node frame exchange | `provision-peer-roster.sh selfcheck` PASS; laptop↔MacBook frames | T13+T9 |
| 13 | Op-wiring C: enrolled == session `instance_id` (unified) | T14 selfcheck (off-host) + T9: no `SourceInstanceMismatch` | `provision-instance-id.sh selfcheck` PASS; envelopes accepted | T14+T9 |
| 14 | Two-instance run: two creds + ACLs + roster naming both | T9: laptop + MacBook each admit the other | bidirectional delivery | T9 |

## Off-host vs live

- **Off-host (authorable now, no host risk):** the `*selfcheck` columns above —
  T4/T11/T12/T13/T14 self-checks + config/ACL lints. All green = the dry-run gate.
- **Live (T9, straight-through after galu's green-confirm):** the `acceptance-gate.sh`
  probes on `the co-located host`, bracketed by preflight/postflight non-disruption proof,
  with rollback for DNS / certbot / systemd unit / firewall.
