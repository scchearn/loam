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

_Filled by T4._ Create the org CA (`pki/init-ca.sh`). The CA config now carries
`copy_extensions = copy` so the auto-enrollment signer can issue a machine's
CSR **verbatim** (CN + its own SAN). Manual per-node issuance remains available
via `pki/issue-client.sh` alongside it; the auto-enrollment path (step 3.5) is
what a new machine uses. See `docs/federation/RESOLUTION-CONTRACT.md` and
`docs/federation/INSTANCE-ID-CONTRACT.md` at the repo root.

## 3.5 Auto-enrollment signer (specs/federation-auto-enrollment.md)

Install the HTTPS signer that turns a machine's `{password, CSR}` into a signed
mTLS cert — the machine mints its own keypair + CSR, nothing travels by hand:

```sh
./enroll/install-signer.sh install
```

- Creates the shared enrollment password at `${ENROLL_DIR}/password` (`0600`),
  generated like `openssl rand -base64 24`. **Rotation** = replace that file and
  re-share via wiki/1Password; already-issued certs are unaffected.
- Reuses the host's Let's Encrypt server cert (same FQDN), so machines verify
  its TLS with public roots — no custom CA on the client. Install copies
  `fullchain.pem` + `privkey.pem` from the (0700 root) certbot live dir into
  `${ENROLL_DIR}/tls/` (key `0640 root:loam-enroll`) and installs a certbot
  renewal-hooks deploy hook that re-copies them + restarts the signer on every
  certificate rotation (~90 days) — the signer never breaks when certbot
  renews.
- Copies the org CA's `ca.crt` + `ca.key` into `${ENROLL_DIR}/ca/` (key
  `0640 root:loam-enroll`) because `${PKI_DIR}/private/` is root-only. The
  signer keeps using the authoritative `${PKI_DIR}` OpenSSL database, granting
  it ACL access only to the database paths needed for atomic issuance; manual
  `pki/issue-client.sh` and auto-enrollment therefore share one serial/index
  history rather than diverging. OpenSSL also needs directory write access for
  its temporary/backup database files; the signer ACL grants that on `${PKI_DIR}`
  while the CA private directory remains inaccessible.
- Serializes every `openssl ca` write on `${PKI_DIR}/ca.lock` (provisioned
  `0600 root:root` with an ACL for the signer user). `openssl ca` locks nothing
  of its own, so two overlapping writes can issue the same serial and lose an
  index entry while both report success. The signer takes the lock, and so do
  `pki/issue-client.sh` and `pki/revoke-client.sh` — issuing by hand during an
  onboarding burst is exactly when the two would collide. A signing that cannot
  take the lock within `ENROLL_CA_LOCK_TIMEOUT_SECONDS` (default 30) is refused
  rather than written unserialized.
- Binds `ENROLL_BIND_ADDRESS` (default `0.0.0.0` — the port is public);
  rate-limits per client (default 10 per 60s against the spec's
  brute-force-on-public-port threat); verifies the password in constant time;
  **never logs the password, CSR, or cert**. TLS + the shared password + the
  rate limit are the security walls on a public VPS.
- Mosquitto is untouched: this service only issues org-CA-signed certs.
- Uninstall removes the deploy hook and the `$ENROLL_DIR` copies too; the
  certbot live dir, org CA, and broker are untouched.

The machine-side command, one shot (no admin ceremony):

```sh
loam federation connect <workspace> mqtts://<host>:8883 --token "$(cat /path/enroll password)"
```

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
