# Loam MQTT broker — deployment runbook

Ordered procedure. Off-host stages (author + dry-run) are runnable anywhere; the
**live-host stage is gated** on an explicit bigboss go and a coordinated window.

> `the co-located host` is a LIVE production host running several existing services
> (a comms server, an invoicing app, web/DB stack, certbot, and others). Every live
> step is read-before-write, backup-before-edit, idempotent, reversible, and touches
> only broker-owned paths + the single 8883 rule. No host-wide restarts.

## 0. Parameters

Copy `params.env.example` → `params.env` (gitignored) and fill FILL-IN-LATER values
on the host. `set -a; . ./params.env; set +a`.

## 1. Deploy target

`DEPLOY_TARGET=colocate` (locked) — the broker co-locates on `the co-located host`.

## 2. Server cert (certbot / Let's Encrypt)

_Filled by T4/T9._ Ensure `mqtt.example.org` A → `198.51.100.10` (Cloudflare API,
token via `CF_DNS_TOKEN_LOOKUP`), then obtain the server cert via the host's existing
certbot DNS-01. Install the deploy-hook that reloads **only** mosquitto.

## 3. Org-CA + client PKI

_Filled by T4._ Create the org CA; issue one client cert per node (CN = principal_id,
SAN `urn:loam:instance:<instance_id>`). See `RESOLUTION-CONTRACT.md` and
`INSTANCE-ID-CONTRACT.md`.

## 4. Broker config + ACL

_Filled by T2/T3._ Install `mosquitto.conf` and `acl` under `${MOSQ_ETC}`. The config
sets `allow_anonymous false` (no anonymous listener, ever), `require_certificate true`,
and `use_identity_as_username true`; the ACL is `{org-id}`-rooted with origin-prefix
write scoping via `%c` (client-id = instance_id). Verify no plaintext listener exists.

## 5. systemd service

_Filled by T5._ Install `loam-mosquitto.service`, `systemctl enable --now`.

## 6. Backup / restore

_Filled by T6._ Back up persistence + org-CA material (never the certbot server cert).

## 7. Cert monitoring

_Filled by T7._ Timer monitors org-CA **client** certs; certbot's own timer covers
the server cert.

## 8. Operational wiring

_Filled by T12/T13/T14._ Resolve credentials into the org secret-service; provision
the two-node peer roster; pin the unified instance_ids into enrollment.

## 9. Acceptance gate (T2 + T9)

_Filled by T8/T9._ Run `preflight.sh` → provision → `postflight-assert.sh` →
`acceptance-gate.sh`. Record evidence per `ACCEPTANCE.md`.

## 10. Gated live provisioning

Hold until an explicit bigboss go + coordinated window after the dry-run passes.
