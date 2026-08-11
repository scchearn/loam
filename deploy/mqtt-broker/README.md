# Loam MQTT broker deployment (`deploy/mqtt-broker/`)

Production, single-org, self-hosted **Mosquitto** broker that the Loam federation
(slices A–E) connects to. This is **not** the Slice B ephemeral test broker
(`cli/tests/mqtt/`) — this is the real broker + PKI + ACLs + operational wiring.

## TLS trust model (split — read this first)

Two **different** CAs, one per direction:

- **Server cert** (broker → clients): issued by the host's existing **certbot /
  Let's Encrypt** for `mqtt.example.org`. Clients verify it against **public/system
  roots** — no custom CA needed on the client for server verification.
- **Client certs** (clients → broker): issued by the **org-owned private CA**. The
  broker verifies them (`cafile = org CA`) and, with `use_identity_as_username
  true`, the client-cert **CN becomes the authenticated principal** that authorizes
  `data.from.principal_id`.

## Layout

| Path | Purpose | Task |
| ---- | ------- | ---- |
| `params.env.example` | parameter manifest (copy to gitignored `params.env`) | T1 |
| `mosquitto.conf` | TLS listener, no-anon, mTLS, persistence, quotas | T2 |
| `acl` | `{org-id}`-rooted ACL, origin-prefix write scoping | T3 |
| `pki/` | org CA + client-cert issue/revoke; certbot server-cert wrapper | T4 |
| `loam-mosquitto.service` | sandboxed system service | T5 |
| `backup-restore.sh` | persistence + org-CA backup/restore | T6 |
| `cert-monitor.sh` + timer | org-CA client-cert expiry monitoring | T7 |
| `RUNBOOK.md` / `ACCEPTANCE.md` | ordered procedure + T2/T9 acceptance map | T8 |
| `acceptance-gate.sh` | live T2/T9 probe (host-gated) | T9 |
| `preflight.sh` / `postflight-assert.sh` | live-host service-integrity | T11 |
| `resolve-credentials.sh` + `RESOLUTION-CONTRACT.md` | credential resolution (seam A) | T12 |
| `provision-peer-roster.sh` + `ROSTER-CONTRACT.md` + `peer-roster.example.json` | peer roster (seam B) | T13 |
| `provision-instance-id.sh` + `INSTANCE-ID-CONTRACT.md` | instance_id unification (seam C) | T14 |

## Contracts for the connector-side wiring slice

`RESOLUTION-CONTRACT.md`, `ROSTER-CONTRACT.md`, and `INSTANCE-ID-CONTRACT.md` are the
interface the connector-side operational-wiring slice implements to. They are the
authority for the two halves not disagreeing.

## Acceptance

The deployed broker must pass the parent spec's **T2** security shape and **T9**
operational checklist (reused, not re-implemented here), plus the preflight/postflight
non-disruption proof. Live provisioning (T9) is gated on an explicit bigboss go.
