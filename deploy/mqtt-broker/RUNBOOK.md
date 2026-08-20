# Loam MQTT broker deployment runbook

This is the ordered host procedure. The commands are designed for a shared
Linux host: read before write, back up before replacing a broker-owned file,
make one change at a time, and never restart unrelated services. For a
newcomer-friendly explanation of the same path, see
[`docs/federation/BROKER-SETUP.md`](../../docs/federation/BROKER-SETUP.md).

The checked-in ACL grants every live surface the connector needs, proven by
[`acl-contract.sh`](acl-contract.sh). One operational caveat remains: the
automated `acceptance-gate.sh provision` stage copies unrendered templates, so
render and install with the manual sequence in section 3. See the
[production broker status](../../docs/federation/README.md#production-broker-status).

## 0. Prepare the host and parameters

Work from a checked-out copy of this directory as `root`:

```sh
cd /path/to/loam-wiring/deploy/mqtt-broker
cp params.env.example params.env
chmod 600 params.env
$EDITOR params.env
set -a
. ./params.env
set +a
```

Set `ORG_ID`, `BROKER_FQDN`, `BROKER_IP`, `CERTBOT_CLOUDFLARE_INI`, the
Mosquitto/PKI paths, and the signer paths. Keep passwords, Cloudflare tokens,
private keys, and certificate blobs out of both files. Confirm the host has
`bash`, `openssl`, `python3`, `envsubst`, `setfacl`, `flock`, `jq`, Mosquitto,
certbot with the Cloudflare plugin, and systemd.

Run the safe checks before changing the host:

```sh
./acceptance-gate.sh dryrun
```

Capture the existing host state immediately before a live deployment:

```sh
BASELINE="$BACKUP_DIR/preflight-baseline.snap" ./preflight.sh
```

This baseline records hashes and service state for postflight comparison. It
does not capture file contents and is not a restore backup.

## 1. DNS and broker server certificate

Create an A/AAAA record for `BROKER_FQDN` that reaches this host. Verify it from
the host and from a client network:

```sh
getent hosts "$BROKER_FQDN"
```

Obtain the public certificate using the host's existing certbot installation:

```sh
./pki/obtain-server-cert.sh
openssl x509 -in "$CERTBOT_LIVE_DIR/fullchain.pem" \
  -noout -subject -issuer -dates
```

The script installs a renewal hook that reloads only
`loam-mosquitto.service`. The certificate is not the organization client CA.

## 2. Organization client CA

Create the organization CA and its initial CRL. Re-running the command does not
replace an existing CA:

```sh
./pki/init-ca.sh
./pki/selfcheck.sh
```

Protect `$PKI_DIR/private/ca.key` as a root-only secret. The signer installer
shares only the CA files and database paths it needs; it does not make the
private directory generally readable.

## 3. Render and install Mosquitto

Use the one canonical rendered replacement,
backup, validation, daemon-reload, and restore sequence in
[`BROKER-SETUP.md`](../../docs/federation/BROKER-SETUP.md#5-install-mosquitto-tls-and-the-acl),
section 5. Do not maintain a second copy of the `envsubst`/`install` sequence
here: that section captures existing and absent destinations before replacing
them and records the exact backup directory needed for rollback. Continue with
the signer installation only after the canonical sequence's checks pass.

## 4. Install the enrollment signer

The signer needs the server certificate and organization CA from the previous
steps. Install and inspect its unit:

```sh
./enroll/install-signer.sh install
systemctl status loam-enroll-signer.service --no-pager
ss -ltnp | grep ":${ENROLL_PORT:-8443}"
```

If machines enroll across the public network, allow TCP `8443`; otherwise bind
the signer to a private interface with `ENROLL_BIND_ADDRESS` and route clients
there. The signer is HTTPS, not MQTT. It uses the same FQDN certificate as the
broker and signs the machine's CSR with the organization CA.

The installer creates `$ENROLL_DIR/password` mode `0600`. Share its contents
through a secure channel, never in `params.env` or a command line. The client
should use `--token-file`.

## 5. First machine enrollment

On the first client machine, configure a Git email and the organization:

```sh
git -C /path/to/workspace config user.email you@example.org
git -C /path/to/workspace config user.name "Your Name"
export LOAM_FEDERATION_ORG=example-org
loam federation connect /path/to/workspace mqtts://mqtt.example.org:8883 \
  --token-file "$HOME/.config/loam/enroll-token"
```

Use `--project example-org/project-name` to supply both scope values for one
command. If a development binary is not inside an installed runtime layout,
add its `--global-root` path when the CLI requests it.

The command generates the client key locally, obtains the certificate through
the signer, performs the real broker capability probe, writes the local
enrollment, and starts the per-user connector. It does not prove that the
connector will remain online after the command exits.

## 6. Verify the record and service

Use both read-only inventory commands:

```sh
loam federation status
loam federation list
loam federation status --json
loam federation list --json
```

`status` reports historical verification and service-manager state but says
`live broker session not observed` by design. `list` reports joined projects,
workspace paths, broker endpoints, and last verification times; it is not a
peer-presence or liveness query.

For current Linux service evidence, inspect the user service and journal:

```sh
systemctl --user status loam-connector.service --no-pager
journalctl --user -u loam-connector.service -n 100 --no-pager
```

Use the platform service manager and broker logs for current liveness. Do not
claim that a registry row alone proves a live broker session.

## 7. Add machines and maintain access

Repeat the client enrollment for each workspace/machine. Each one gets its own
instance id, while two machines used by one person may share the certificate
CN/email. A complete peer roster contains concrete principal and bare instance
entries; see [ROSTER-CONTRACT.md](../../docs/federation/ROSTER-CONTRACT.md).

Rotate the enrollment password by replacing it atomically and restarting only
the signer:

```sh
tmp_password="$(mktemp)"
umask 077
openssl rand -base64 24 > "$tmp_password"
install -m 600 "$tmp_password" "$ENROLL_DIR/password"
rm -f "$tmp_password"
systemctl restart loam-enroll-signer.service
```

Already-issued certificates are unaffected. To revoke one client certificate:

```sh
GIT_EMAIL=you@example.org
INSTANCE_ID=machine-instance-id
PKI_DIR="$PKI_DIR" ./pki/revoke-client.sh "$GIT_EMAIL" "$INSTANCE_ID"
systemctl reload loam-mosquitto.service
```

Revocation does not erase retained MQTT messages. Clear or expire retained
state separately when policy requires it.

Before either operation, schedule a maintenance window and stop or checkpoint
Mosquitto so the persistence database is consistent. The bounded stop-and-copy
path is:

```sh
systemctl stop loam-mosquitto.service
./backup-restore.sh backup
systemctl start loam-mosquitto.service  # only if it was active before the backup
```

If the service must remain up, checkpoint persistence instead of stopping it:

```sh
systemctl kill -s SIGUSR1 loam-mosquitto.service
./backup-restore.sh backup
```

This covers Mosquitto persistence and the organization CA, not certbot's server
certificate. Restore only during a planned maintenance window:

```sh
systemctl stop loam-mosquitto.service
ARCHIVE=/path/to/loam-mqtt-archive.tgz
./backup-restore.sh restore "$ARCHIVE"
mosquitto --test-config -c "$MOSQ_ETC/mosquitto.conf"
systemctl restart loam-mosquitto.service
systemctl is-active loam-mosquitto.service
systemctl status loam-mosquitto.service --no-pager
```

Confirm the restored persistence and CA paths, listener, and broker logs before
returning the host to service.

## 8. Monitoring and acceptance

Install the supplied certificate monitor service/timer and inspect it after
deployment. Certbot's own timer renews the public server certificate; the
monitor observes that certificate and checks organization client certificates.

Before the change window, run `./acceptance-gate.sh dryrun`; this remains the
safe off-host gate. The `acceptance-gate.sh provision` stage is currently
unavailable/unsafe because it copies unresolved `${VARS}` templates. Do **not**
run `LOAM_LIVE_GO=1 ./acceptance-gate.sh provision`. The envsubst-rendered
manual deployment sequence in
[`BROKER-SETUP.md`](../../docs/federation/BROKER-SETUP.md), section 5, is the
supported render-and-install path until that template-rendering defect is
fixed. After an approved deployment, run:

```sh
./postflight-assert.sh "$BACKUP_DIR/preflight-baseline.snap"
./acceptance-gate.sh health
```

The postflight assertion must show that only the broker unit and configured
listener were added and that existing units/configurations were unchanged.
`acceptance-gate.sh health` is limited to DNS and public TLS verification; it
does not execute the client-authentication or ACL probes. Capture those
remaining acceptance checks separately.

## 9. Rollback

If a live step fails, stop and inspect before doing anything else. Do **not** use
`./acceptance-gate.sh rollback` on a pre-existing host: it unconditionally
disables/removes the broker unit and removes the firewall rule and certbot
lineage, so it is unsafe here. The preflight snapshot cannot restore the three
replaced files; restore them only from the exact `MANUAL_BACKUP_DIR` captured by
the canonical procedure in
[`BROKER-SETUP.md`](../../docs/federation/BROKER-SETUP.md#restore-that-manual-replacement),
after confirming the target paths and stopping Mosquitto. That procedure
validates every backup entry before mutation, restores absent destinations as
absent, restores the saved systemd state, and runs `systemctl daemon-reload`
before starting or stopping the unit.
