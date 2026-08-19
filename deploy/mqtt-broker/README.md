# Loam MQTT broker deployment

This directory contains the production Mosquitto broker, its TLS/PKI material,
the enrollment signer, and the operational checks used by Loam federation. It
is separate from the temporary broker fixtures under `cli/tests/`.

> **HARD STOP — do not deploy or enable this production broker yet.** The
> checked-in ACL is incompatible with the current connector. See the
> [federation hard-stop warning](../../docs/federation/README.md#production-broker-hard-stop)
> for the missing live-transport grants and the cross-project denial blocker.

When the blocker is cleared, read
[`docs/federation/BROKER-SETUP.md`](../../docs/federation/BROKER-SETUP.md).
This page is the concise reference for the files and trust model. The intended
host procedure is [RUNBOOK.md](RUNBOOK.md); the evidence checklist is
[ACCEPTANCE.md](ACCEPTANCE.md).

## TLS trust model

There are two independent certificate directions:

- **Broker server certificate:** certbot/Let's Encrypt for the broker FQDN.
  Clients verify it with public/system roots. The organization client CA is not
  needed for this direction.
- **Client certificates:** the organization private CA created by
  [`pki/init-ca.sh`](pki/init-ca.sh). Mosquitto verifies client certificates
  with `cafile`, enforces the CRL with `crlfile`, and uses the certificate CN as
  the authenticated MQTT username.

The enrollment signer reuses the broker's Let's Encrypt certificate for its
HTTPS endpoint on port `8443`. It signs client CSRs with the organization CA;
the joining machine keeps its private key.

## Files

| Path | Purpose |
| --- | --- |
| `params.env.example` | Host-only parameter template; copy to ignored `params.env`. |
| `mosquitto.conf` | TLS-only Mosquitto listener, persistence, and quotas. |
| `acl` | Organization-rooted ACL with `%c` origin-write isolation. |
| `pki/init-ca.sh` | Create the organization client CA and CRL. |
| `pki/obtain-server-cert.sh` | Obtain/renew the broker's certbot server certificate. |
| `pki/issue-client.sh` / `pki/revoke-client.sh` | Manual client certificate issuance and revocation. |
| `pki/selfcheck.sh` | Throwaway CA, issuance, and revocation check. |
| `enroll/` | HTTPS enrollment signer, installer, unit, and availability test. |
| `loam-mosquitto.service` | Sandboxed systemd service for Mosquitto. |
| `backup-restore.sh` | Back up or restore broker persistence and organization CA data. |
| `cert-monitor.sh` and timer | Check organization client-certificate expiry. |
| `certbot-deploy-hook.sh` | Reload only Mosquitto after the broker server cert renews. |
| `preflight.sh` / `postflight-assert.sh` | Snapshot and prove that existing host services were not changed. |
| `acceptance-gate.sh` | Run the off-host checks and limited live DNS/TLS verification; `provision` is unavailable until template rendering is implemented. |
| `resolve-credentials.sh` | Compatibility/manual secret-store helper; the current runtime reads its local identity files directly. |
| `provision-peer-roster.sh` | Write and validate a per-project peer roster. |
| `provision-instance-id.sh` | Mint/check an id for manual certificate workflows. |
| `peer-roster.example.json` | Placeholder roster; replace every value before use. |

## Broker security shape

The rendered `mosquitto.conf` must have:

```text
allow_anonymous false
require_certificate true
use_identity_as_username true
```

Only the TLS listener is enabled. The ACL uses `%u` (certificate CN/principal)
for principal authorization and `%c` (bare instance id/client id) for origin
write isolation. A client may write only under its own origin. The organization
CA is used by Mosquitto to verify clients; the certbot server chain is used by
clients to verify the broker.

## Operational limits worth remembering

- The deployment uses one organization CA. A larger installation may later
  introduce an offline root and online issuing CA.
- The enrollment password protects future certificate issuance; rotating it does
  not revoke certificates already issued.
- Revoking a client certificate blocks future authentication and, after a broker
  reload, drops its live session. It does not automatically erase retained MQTT
  messages.
- The Loam `status` and `list` commands are read-only inventory surfaces. Their
  historical probe evidence must not be presented as proof of a current broker
  session.

For the local identity, instance, roster, and enrollment details, see the
[federation contracts](../../docs/federation/README.md).
