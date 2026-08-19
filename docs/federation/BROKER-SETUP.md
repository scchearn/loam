# Federation broker setup

This walkthrough is for an operator starting with a new Linux host and no
project history. It documents the intended path; production deployment is
currently blocked by the hard stop below.

1. install the host prerequisites and set deployment parameters;
2. point DNS at the host and obtain the public server certificate;
3. create the organization's private client CA;
4. render Mosquitto's TLS configuration and ACL, then start the broker after
   the current blockers are cleared;
5. install the HTTPS enrollment signer and its password;
6. connect the first machine with `loam federation connect`;
7. verify the recorded enrollment without confusing it with live-session health.

The commands assume a shell running as `root` on the broker host unless noted.
The repository scripts remain the source of truth for provisioning. This page
explains their order and the checks around them; it does not replace their
permissions, backups, or host-safety checks.

> **HARD STOP — do not deploy or enable this production broker yet.** The
> checked-in `deploy/mqtt-broker/acl` is incompatible with the current
> connector: it lacks grants for project-membership subscriptions, organization
> member-card subscription/publication, and the agent inbox, and its project
> wildcards cannot enforce cross-project denial. `loam federation connect` may
> succeed while the connector loops offline. There is no supported ACL
> workaround yet. See the [acceptance blockers](../../deploy/mqtt-broker/ACCEPTANCE.md#current-blockers).

## 1. Host prerequisites

Use a supported Linux distribution with systemd and a dedicated host name such
as `mqtt.example.org`. The host needs:

- root access and a dedicated unprivileged `mosquitto` service account;
- `bash`, `openssl`, `python3`, `envsubst`, `setfacl`, `flock`, `jq`, `ss`,
  `sudo`,
  `systemctl`, and `git`;
- Mosquitto and `mosquitto_passwd` (the package names vary by distribution);
- certbot with its Cloudflare DNS plugin, plus an existing Cloudflare API
  credential file whose path is kept out of Git;
- a firewall under the operator's control. The broker needs TCP `8883`; the
  signer normally needs TCP `8443` for machines joining from outside the host's
  private network;
- a DNS A/AAAA record for the broker name, and a route from client machines to
  both listener ports.

`setfacl` is required by the signer installer so the signer can update the CA
database without reading the CA's root-only private directory. `jq` is needed
by the optional peer-roster helper. The deployment's own dry-run checks the
complete list:

```sh
cd /path/to/loam-wiring/deploy/mqtt-broker
./acceptance-gate.sh dryrun
```

Do not continue to a live change until the dry run is green. For a shared host,
read [the deployment runbook](../../deploy/mqtt-broker/RUNBOOK.md) before
installing anything.

## 2. Set deployment parameters

Copy the tracked template to the host-only file. `params.env` must remain
gitignored and must contain paths, not secret values.

```sh
cd /path/to/loam-wiring/deploy/mqtt-broker
cp params.env.example params.env
chmod 600 params.env
$EDITOR params.env
set -a
. ./params.env
set +a
```

At minimum, set and review:

```text
ORG_ID                 the organization segment in loam/v1/<org>/...
BROKER_FQDN            the DNS name on the server certificate
BROKER_IP              the host address used by the DNS/firewall change
CERTBOT_CLOUDFLARE_INI path to the existing certbot credential file
MOSQ_ETC              usually /etc/mosquitto
MOSQ_PERSIST          usually /var/lib/mosquitto
PKI_DIR               usually /etc/mosquitto/pki
BACKUP_DIR            broker-only backup destination
ENROLL_DIR            usually /etc/loam/enroll
ENROLL_PORT            normally 8443
```

The template also contains the secret-service and roster settings used by
optional/manual provisioning helpers. Never put a password, token, private
key, or certificate blob in `params.env`.

## 3. DNS and the server certificate

Create the DNS record before asking certbot for the certificate:

```sh
getent hosts "$BROKER_FQDN"
```

The record must resolve to this host from the client network. The existing
certbot setup obtains a Let's Encrypt certificate with DNS-01 and installs a
deploy hook that reloads only the Loam Mosquitto service:

```sh
./pki/obtain-server-cert.sh
```

The script uses `BROKER_FQDN` and `CERTBOT_CLOUDFLARE_INI` from the exported
parameters. It does not create a second ACME client. Confirm the result before
continuing:

```sh
openssl x509 -in "$CERTBOT_LIVE_DIR/fullchain.pem" \
  -noout -subject -issuer -dates
```

The server certificate is for the broker host name and is trusted by clients
through public roots. It is a different trust direction from the organization
CA created in the next step. The signer reuses this same server certificate.

## 4. Create the organization client CA

The organization CA signs client certificates. Mosquitto uses its public
certificate to verify clients; the CA private key must stay on the broker host.

```sh
./pki/init-ca.sh
./pki/selfcheck.sh
```

`init-ca.sh` creates `$PKI_DIR/ca.crt`, `$PKI_DIR/crl.pem`, the OpenSSL CA
database, and `$PKI_DIR/private/ca.key`. It is idempotent: an existing CA is
not replaced. The script permits multiple certificates with the same email/CN
so one person can use more than one machine; the instance id distinguishes
those certificates.

This is a single-tier CA suitable for a small self-hosted deployment. A larger
organization may later place an offline root above an online issuing CA; that
is not required for this setup.

## 5. Install Mosquitto, TLS, and the ACL

The tracked `mosquitto.conf`, `acl`, and systemd unit contain `${VARS}`. Render
them after sourcing `params.env`; do not copy the unresolved templates to
`/etc`.

The `acceptance-gate.sh provision` stage is currently unavailable/unsafe: its
live path copies these tracked templates without rendering `${VARS}`. Do not
run it, even with `LOAM_LIVE_GO=1`. The explicit `envsubst` sequence below is
reference-only while the hard stop remains; it may be executed only after the
connector/ACL blockers and the template-rendering defect are cleared. The
gate's `dryrun` remains the safe off-host check; its `health` stage is limited
to DNS and public TLS verification.
Do not use `./acceptance-gate.sh rollback` on a pre-existing host: it
unconditionally disables/removes the broker unit and removes its firewall rule
and certbot lineage. Use the manual restore below instead.

### Back up the replacement targets

The three `install` commands below replace broker-owned files. Capture each
destination immediately before replacing it. This is separate from
`preflight.sh`: the preflight snapshot records hashes and service state for
comparison, but it does not contain file contents and cannot restore these
files.

Run this as `root` after sourcing `params.env`:

```bash
set -eu
umask 077

install -d -o root -g root -m 700 /root/loam-mosquitto-config-backups
MANUAL_BACKUP_DIR="/root/loam-mosquitto-config-backups/$(date -u +%Y%m%dT%H%M%SZ)-$$"
mkdir -m 700 "$MANUAL_BACKUP_DIR"

backup_target() {
  local name="$1" destination="$2"
  if [ -e "$destination" ] || [ -L "$destination" ]; then
    if [ ! -f "$destination" ] && [ ! -L "$destination" ]; then
      printf 'Refusing non-file destination: %s\n' "$destination" >&2
      return 1
    fi
    cp -a -- "$destination" "$MANUAL_BACKUP_DIR/$name"
  else
    : > "$MANUAL_BACKUP_DIR/$name.absent"
  fi
}

backup_target mosquitto.conf "$MOSQ_ETC/mosquitto.conf"
backup_target acl "$MOSQ_ETC/acl"
backup_target loam-mosquitto.service \
  /etc/systemd/system/loam-mosquitto.service
enabled_state="$(systemctl is-enabled loam-mosquitto.service 2>/dev/null || true)"
: "${enabled_state:=not-found}"
case "$enabled_state" in
  enabled|disabled|masked|not-found) ;;
  *)
    printf 'Unsupported original enable state: %s\n' "$enabled_state" >&2
    exit 1
    ;;
esac
active_state="$(systemctl is-active loam-mosquitto.service 2>/dev/null || true)"
if [ "$active_state" = active ]; then
  active_state=active
else
  active_state=inactive
fi
printf '%s\n' "$enabled_state" > "$MANUAL_BACKUP_DIR/systemd-enabled"
printf '%s\n' "$active_state" > "$MANUAL_BACKUP_DIR/systemd-active"
printf 'Manual config backup: %s\n' "$MANUAL_BACKUP_DIR"
```

`cp -a` preserves the captured file's mode, owner/group, timestamps, and
symlink type. An `.absent` marker means that restore must remove the replacement
rather than leave a newly created file behind. The backup also records the
original `enabled`/`disabled`/`masked`/`not-found` state and normalized
`active`/`inactive` state of `loam-mosquitto.service`. Keep the printed
`MANUAL_BACKUP_DIR` for a rollback; do not substitute the preflight snapshot
path.

```bash
set -eu
umask 077

# The checked-in unit runs as User=Group=$MOSQ_USER. Preserve existing mode
# bits, but make the config directory traversable and persistence user-owned.
if [ ! -d "$MOSQ_ETC" ]; then
  install -d -o root -g "$MOSQ_USER" -m 750 "$MOSQ_ETC"
else
  chown root:"$MOSQ_USER" "$MOSQ_ETC"
  chmod g+rx "$MOSQ_ETC"
fi
if [ ! -d "$MOSQ_PERSIST" ]; then
  install -d -o "$MOSQ_USER" -g "$MOSQ_USER" -m 700 "$MOSQ_PERSIST"
else
  chown "$MOSQ_USER":"$MOSQ_USER" "$MOSQ_PERSIST"
  chmod u+rwx "$MOSQ_PERSIST"
fi

render_dir="$(mktemp -d /root/loam-mosquitto-render.XXXXXX)"
trap 'rm -rf -- "$render_dir"' EXIT
envsubst < mosquitto.conf > "$render_dir/mosquitto.conf"
envsubst < acl > "$render_dir/acl"
envsubst < loam-mosquitto.service > "$render_dir/loam-mosquitto.service"

install -o root -g "$MOSQ_USER" -m 640 "$render_dir/mosquitto.conf" "$MOSQ_ETC/mosquitto.conf"
install -o root -g "$MOSQ_USER" -m 640 "$render_dir/acl" "$MOSQ_ETC/acl"
install -m 644 "$render_dir/loam-mosquitto.service" \
  /etc/systemd/system/loam-mosquitto.service

# The unit's unprivileged user must be able to read every configured input
# before the service is started. Use the host's existing certbot group or a
# targeted ACL on the key and its parent directories; never make the private
# key world-readable.
sudo -u "$MOSQ_USER" test -r "$MOSQ_ETC/mosquitto.conf"
sudo -u "$MOSQ_USER" test -r "$MOSQ_ETC/acl"
sudo -u "$MOSQ_USER" test -r "$PKI_DIR/ca.crt"
sudo -u "$MOSQ_USER" test -r "$PKI_DIR/crl.pem"
sudo -u "$MOSQ_USER" test -r "$CERTBOT_LIVE_DIR/fullchain.pem"
sudo -u "$MOSQ_USER" test -r "$CERTBOT_LIVE_DIR/privkey.pem"

mosquitto --test-config -c "$MOSQ_ETC/mosquitto.conf"
systemctl daemon-reload
systemctl enable --now loam-mosquitto.service
```

### Restore that manual replacement

Export `MANUAL_BACKUP_DIR` with the exact path printed above, then run this
block. It stops before changing anything if the variable is missing. The
function handles both captured files and destinations that were absent before
this deployment:

```bash
set -eu
umask 077
: "${MANUAL_BACKUP_DIR:?export MANUAL_BACKUP_DIR with the backup path printed above}"

if [ ! -d "$MANUAL_BACKUP_DIR" ] || [ -L "$MANUAL_BACKUP_DIR" ]; then
  printf 'Backup directory is missing or is a symlink: %s\n' \
    "$MANUAL_BACKUP_DIR" >&2
  exit 1
fi

validate_restore_entry() {
  local name="$1" entry="$MANUAL_BACKUP_DIR/$1" absent="$MANUAL_BACKUP_DIR/$1.absent"
  if [ -e "$entry" ] || [ -L "$entry" ]; then
    if [ ! -f "$entry" ] && [ ! -L "$entry" ]; then
      printf 'Backup entry is not a file or symlink: %s\n' "$name" >&2
      return 1
    fi
    if [ -e "$absent" ] || [ -L "$absent" ]; then
      printf 'Backup has both present and absent entries: %s\n' "$name" >&2
      return 1
    fi
  elif [ -e "$absent" ] || [ -L "$absent" ]; then
    if [ ! -f "$absent" ] || [ -L "$absent" ]; then
      printf 'Absent marker is not a regular file: %s\n' "$name" >&2
      return 1
    fi
  else
    printf 'Backup entry is missing: %s\n' "$name" >&2
    return 1
  fi
}

validate_restore_entry mosquitto.conf
validate_restore_entry acl
validate_restore_entry loam-mosquitto.service
[ -f "$MANUAL_BACKUP_DIR/systemd-enabled" ] \
  && [ ! -L "$MANUAL_BACKUP_DIR/systemd-enabled" ] || {
    printf 'Backup systemd-enabled state is missing or invalid\n' >&2
    exit 1
  }
[ -f "$MANUAL_BACKUP_DIR/systemd-active" ] \
  && [ ! -L "$MANUAL_BACKUP_DIR/systemd-active" ] || {
    printf 'Backup systemd-active state is missing or invalid\n' >&2
    exit 1
  }
enabled_state="$(<"$MANUAL_BACKUP_DIR/systemd-enabled")"
active_state="$(<"$MANUAL_BACKUP_DIR/systemd-active")"
case "$enabled_state" in
  enabled|disabled|masked|not-found) ;;
  *) printf 'Unsupported saved enable state: %s\n' "$enabled_state" >&2; exit 1 ;;
esac
case "$active_state" in
  active|inactive) ;;
  *) printf 'Unsupported saved active state: %s\n' "$active_state" >&2; exit 1 ;;
esac

restore_target() {
  local name="$1" destination="$2"
  if [ -e "$MANUAL_BACKUP_DIR/$name" ] || [ -L "$MANUAL_BACKUP_DIR/$name" ]; then
    rm -f -- "$destination"
    cp -a -- "$MANUAL_BACKUP_DIR/$name" "$destination"
  else
    rm -f -- "$destination"
  fi
}

systemctl stop loam-mosquitto.service 2>/dev/null || true
systemctl disable loam-mosquitto.service 2>/dev/null || true
restore_target mosquitto.conf "$MOSQ_ETC/mosquitto.conf"
restore_target acl "$MOSQ_ETC/acl"
restore_target loam-mosquitto.service \
  /etc/systemd/system/loam-mosquitto.service
systemctl daemon-reload

case "$enabled_state" in
  enabled)
    systemctl unmask loam-mosquitto.service
    systemctl enable loam-mosquitto.service
    ;;
  disabled)
    systemctl unmask loam-mosquitto.service
    systemctl disable loam-mosquitto.service 2>/dev/null || true
    ;;
  not-found)
    systemctl unmask loam-mosquitto.service 2>/dev/null || true
    systemctl disable loam-mosquitto.service 2>/dev/null || true
    ;;
  masked)
    systemctl unmask loam-mosquitto.service 2>/dev/null || true
    systemctl mask loam-mosquitto.service
    ;;
esac

case "$active_state" in
  active)
    mosquitto --test-config -c "$MOSQ_ETC/mosquitto.conf"
    systemctl start loam-mosquitto.service
    ;;
  inactive)
    systemctl stop loam-mosquitto.service 2>/dev/null || true
    ;;
esac
```

The restore block validates the backup directory, all three present/absent
entries, and both saved service-state files before stopping or removing
anything. It then removes the deployment's enable link, restores files with
`cp -a`, reloads systemd, and restores the saved enable/mask and start/stop
state. Restore only a backup made by the procedure above; older backups without
the state files are refused rather than partially applied.

Check the rendered files before starting the service if this is the first
deployment:

```sh
grep -E '^(listener|allow_anonymous|certfile|keyfile|cafile|crlfile|require_certificate|use_identity_as_username|acl_file)' \
  "$MOSQ_ETC/mosquitto.conf"
grep -E '^(pattern (read|write))' "$MOSQ_ETC/acl"
ss -ltnp | grep ":${LISTENER_PORT:-8883}"
```

The expected security shape is:

- one TLS listener on `8883` (or the explicitly configured listener port);
- `allow_anonymous false` and `require_certificate true`;
- `use_identity_as_username true`, so the authenticated username is the client
  certificate CN;
- `cafile` pointing at the organization CA and `crlfile` pointing at its CRL;
- no plaintext `1883` listener;
- ACL writes restricted to the connecting machine's own bare instance id via
  `%c`. `%u` is the certificate CN and authorizes the principal.

Allow only the configured broker port through the firewall. With UFW, for
example:

```sh
ufw allow "${LISTENER_PORT:-8883}/tcp"
```

The server-side TLS check below verifies the public certificate. It is not an
MQTT authorization check because it intentionally does not present a client
certificate:

```sh
openssl s_client -connect "$BROKER_FQDN:${LISTENER_PORT:-8883}" \
  -servername "$BROKER_FQDN" -verify_return_error </dev/null
```

For the wider host-safety checklist and acceptance evidence, use
[`RUNBOOK.md`](../../deploy/mqtt-broker/RUNBOOK.md) and
[`ACCEPTANCE.md`](../../deploy/mqtt-broker/ACCEPTANCE.md). The backup and
restore procedure above is the canonical file-replacement rollback path.

## 6. Install the enrollment signer

The signer is a small HTTPS service on the broker host. A joining machine sends
its password and CSR; the signer signs that CSR with the organization CA and
returns only the certificate. The machine keeps its private key. TLS, the
shared password, and the request rate limit are the security controls; binding
to an address is not a security boundary.

Install it after the server certificate and organization CA exist:

```sh
./enroll/install-signer.sh install
systemctl status loam-enroll-signer.service --no-pager
ss -ltnp | grep ":${ENROLL_PORT:-8443}"
```

The installer:

- creates the dedicated `loam-enroll` user;
- copies the Let's Encrypt chain/key into `$ENROLL_DIR/tls/`;
- copies the organization CA material into `$ENROLL_DIR/ca/` while sharing
  only the CA database paths needed for issuance;
- creates `$ENROLL_DIR/password` with mode `0600` if it does not exist;
- installs a systemd unit and a certbot renewal hook;
- serializes signer, manual issuance, and revocation writes through the CA lock.

If clients join over the public network, allow TCP `8443` as well. If the
signer should be private, set `ENROLL_BIND_ADDRESS` and provide a route to that
interface instead of exposing it publicly.

The signer reuses the broker host name in its URL:
`https://<broker-host>:8443/v1/enroll`. If a non-default endpoint is required,
set `LOAM_FEDERATION_SIGNER` on the client. The signer certificate must still
name the host the client uses.

## 7. Enrollment password and rotation

The installer creates a random password once. Read it only through a protected
administrative channel and never commit it, put it in `params.env`, or paste it
into a command line. On a client, make a private token file:

```sh
install -d -m 700 "$HOME/.config/loam"
umask 077
read -r -s -p 'Enrollment password: ' LOAM_ENROLL_TOKEN
printf '\n'
printf '%s\n' "$LOAM_ENROLL_TOKEN" > "$HOME/.config/loam/enroll-token"
unset LOAM_ENROLL_TOKEN
chmod 600 "$HOME/.config/loam/enroll-token"
```

Rotate the password atomically on the broker and restart the signer:

```sh
tmp_password="$(mktemp)"
umask 077
openssl rand -base64 24 > "$tmp_password"
install -m 600 "$tmp_password" "$ENROLL_DIR/password"
rm -f "$tmp_password"
systemctl restart loam-enroll-signer.service
```

Re-share the new password through the same secure channel and replace client
token files. Certificates already issued remain valid; rotation changes only
future enrollment requests. Revoke a client certificate separately when that
machine must lose broker access.

## 8. First machine connection

Before connecting, the client needs:

- an installed `loam` runtime;
- a Git workspace with an `origin` remote;
- `git user.email` (the email becomes the client certificate CN). `git user.name`
  is optional but supplies the certificate display name;
- DNS and network access to the broker on `8883` and the signer on `8443`;
- the private token file from the previous step.

Set the organization explicitly. The command never guesses it from the Git
hosting account:

```sh
git -C /path/to/workspace config user.email you@example.org
git -C /path/to/workspace config user.name "Your Name"
export LOAM_FEDERATION_ORG=example-org
loam federation connect /path/to/workspace mqtts://mqtt.example.org:8883 \
  --token-file "$HOME/.config/loam/enroll-token"
```

Alternatively, provide both values for this invocation:

```sh
loam federation connect /path/to/workspace mqtts://mqtt.example.org:8883 \
  --project example-org/project-name \
  --token-file "$HOME/.config/loam/enroll-token"
```

With an installed runtime, the command discovers its runtime/global-root
layout. A development binary outside an installed layout may also need
`--global-root /path/to/global-root`; add it when the command reports that the
flag is required.

On the first successful run, `connect`:

1. resolves the project from `--project` or the workspace's `origin` remote;
2. creates a machine key and CSR locally;
3. asks the signer for an organization-CA client certificate;
4. stores `client.pem` and `key.pem` under the Loam federation profile with
   private permissions;
5. performs a real MQTT/TLS capability probe: authentication, subscriptions,
   a non-retained publish, and the exact self-received event;
6. commits the enrollment and enables the local per-user connector service.

The key never goes to the signer. Remove the token file after the run if it is
not needed for another machine:

```sh
rm -f "$HOME/.config/loam/enroll-token"
```

## 9. Verify the local record

Run both read-only commands:

```sh
loam federation status
loam federation list
```

A healthy local record normally shows an enrolled project, a present service
definition, and an enabled service manager state. `list` shows the project,
workspace, broker endpoint, and the time the capability probe last succeeded.

For machine-readable output:

```sh
loam federation status --json
loam federation list --json
```

These commands do not create a registry, start a service, or dial the broker.
The status JSON intentionally contains:

```json
"broker": {
  "session_observed": false,
  "session_state": "not-observed-in-read-only-status"
}
```

That is a limitation, not a warning to ignore. To inspect current service
activity on Linux, use the user service manager and connector journal:

```sh
systemctl --user status loam-connector.service --no-pager
journalctl --user -u loam-connector.service -n 100 --no-pager
```

The macOS and Windows service definitions use LaunchAgent and Task Scheduler;
their manager-specific inspection commands are described by the service
implementation and the release platform smoke tests. A current broker-side
session still requires a broker health check or connector log evidence; an
enrollment row is not that evidence.

## 10. Additional operations

For a second machine, repeat the first-machine connection with that machine's
workspace and the same organization/project scope. Each machine receives its
own instance id and certificate. A peer roster must list both concrete
principals and bare instance ids before either machine can admit the other;
see [ROSTER-CONTRACT.md](ROSTER-CONTRACT.md) and
`provision-peer-roster.sh`.

To remove one workspace locally:

```sh
loam federation disconnect /path/to/workspace
```

This removes the local enrollment and reconciles the local connector service.
It does not erase retained MQTT data automatically.

Back up broker persistence and the organization CA (not the certbot server
certificate) with:

```sh
./backup-restore.sh backup
```

Restore only during a planned maintenance window; stop or checkpoint Mosquitto
first. Monitor client-certificate expiry with the provided timer and
`cert-monitor.sh`. Revocation is performed with both the certificate email and
instance id:

```sh
PKI_DIR="$PKI_DIR" ./pki/revoke-client.sh you@example.org INSTANCE_ID
systemctl reload loam-mosquitto.service
```

Revocation blocks a future authenticated session and, after reload, drops the
revoked live session. It does not by itself remove retained state; clear or
expire retained messages according to the broker retention policy.

## Failure and refusal guide

Use `--json` while diagnosing. The stable `error.code` identifies the class;
the adjacent message/detail usually names the input or stage that needs repair.

### Before the signer is contacted

| Message/code | Meaning | What to do |
| --- | --- | --- |
| `federation_org_unconfigured` | No organization was configured, and the client will not infer one from a Git host account. | Set `LOAM_FEDERATION_ORG`, add `{"org":"..."}` to the Loam config `config.json` (for example, `$HOME/.config/loam/config.json` on Linux), or pass `--project org/project`. |
| `descriptor_invalid_endpoint` | The endpoint is not `mqtts://host:port`, or contains user info, a path, query, or fragment. | Use the broker FQDN and TLS port, normally `mqtts://mqtt.example.org:8883`. |
| `workspace_not_git` | The workspace is not a Git top-level directory. | Pass the repository path (or a subdirectory inside it) and check `git -C <workspace> rev-parse --show-toplevel`. |
| `remote_not_configured` | The required `origin` remote is missing, or a descriptor names a missing remote. | Add/fix the workspace's `origin` remote. |
| `credential_bearing_remote` | A remote URL embeds user information in an HTTP authority. | Remove credentials from the remote URL; use a credential helper or SSH instead. |
| `git_unavailable` | The runtime could not run Git. | Install Git and make sure it is on the runtime's `PATH`. |
| `descriptor_invalid_field` / `descriptor_invalid_commit` | A project, identifier, ref, or optional commit has an invalid shape. | Use `--project org/project` with simple topic-safe identifiers and full `refs/...` names. A commit is descriptive; connect does not prove its reachability. |

### Automatic enrollment and the signer

| Message/code | Meaning | What to do |
| --- | --- | --- |
| `git-identity-required` | The client cannot create a certificate subject without `git user.email`. | Set `git -C <workspace> config user.email ...`; set `user.name` if a display name is wanted. |
| `bad-token` | The signer rejected the password (HTTP 401/429). | Obtain the current password from the broker operator, replace the token file, and retry. Check rate limiting before repeated attempts. |
| `signer-unreachable` | DNS, TCP, or an early TLS connection to the signer failed. | Check the broker FQDN, port `8443`, firewall, signer unit, and `journalctl -u loam-enroll-signer`. |
| `signer-timeout` | The signer answered too slowly or stalled. | Check the signer host and load. The detail names the stage and the bound; `LOAM_ENROLL_TIMEOUT_SECONDS` may be set from 1 to 300 seconds, default 10, only for a genuinely slow link. |
| `signer-url-invalid` | `LOAM_FEDERATION_SIGNER` is not an HTTPS URL with a usable host. | Unset it for the default `https://<broker-host>:8443/v1/enroll`, or correct the explicit URL. |
| `trust-anchors-unresolved` | The client could not build trust for the signer's server certificate. The detail names the file/rung, such as a stale `SSL_CERT_FILE`. | Remove a stale `SSL_CERT_FILE`, repair the named PEM, or use a signer certificate trusted by the public roots. Do not substitute the organization client CA for the Let's Encrypt server chain. |
| `tls-setup-failed` | The local TLS client could not be constructed. | Check the runtime installation and the signer host name/certificate; retry after fixing the local platform error in the detail. |
| `malformed-signer-response` | The signer returned a non-certificate body or incomplete response. | Inspect `loam-enroll-signer.service` logs and verify the installed signer files, CA database, and TLS material. |
| `identity-store-failed` | The signer may already have issued a certificate, but the client could not write its local key/certificate. | Fix the profile directory, disk, and permissions before retrying. Treat the earlier certificate as issued and account for it on the broker. |

### Broker probe and service activation

| Message/code | Meaning | What to do |
| --- | --- | --- |
| `connect_probe_failed` with `probe_authentication_failed` | The broker answered MQTT but refused the client identity. | Check that the certificate is signed by the configured org CA, is not revoked, and has the expected CN; check `cafile`, `crlfile`, and the Mosquitto logs. |
| `connect_probe_failed` with `dial_refused` | The host rejected the TCP connection. | Check that Mosquitto is running and listening on `8883`, and that the firewall allows it. |
| `connect_probe_failed` with `transport_unreachable` | DNS, routing, or a dropped connection prevented a response. | Check name resolution, routes, VPN/private-network access, and host firewall rules. |
| `connect_probe_failed` with `tls_handshake_failed` | TCP reached the host but TLS verification failed. | Use the broker FQDN named by the server certificate; check certbot's `fullchain.pem`, renewal, and client trust. |
| `connect_probe_failed` with `tls_configuration_failure` | The client certificate, key, or trust store could not be loaded locally. | Check `client.pem`, `key.pem`, their permissions, key format, and the optional `ca_ref`/`SSL_CERT_FILE`. |
| `connect_probe_failed` with `probe_subscribe_denied` or `probe_publish_denied` | Mosquitto accepted the TLS session but the ACL rejected the probe topic/filter. | Render ACL with the correct `ORG_ID`; ensure the client id is the bare instance id and that the certificate CN matches the intended principal. |
| `connect_probe_failed` with `probe_no_self_receive`, `probe_wrong_self_receive`, or `probe_timeout` | The probe could not receive its exact non-retained self-event within the deadline. | Check ACL read/write rules, broker logs, listener health, and that the client is not using MQTT No Local for the probe filters. |
| `enrollment_conflict` | This physical workspace is already enrolled with a different binding. | Run `loam federation disconnect <workspace>` deliberately, then connect again with the intended broker/project. |
| `connect_activation_failed` | The broker probe passed, but the local service manager did not start the connector. | Read the detail, then inspect the platform service definition and manager logs. Fix the local service before retrying. |
| `rollback_incomplete` | Connect could not remove all local state after a later failure. | Stop and repair the local service/registry carefully; do not assume the machine is disconnected until `status` and `list` agree. |

### After enrollment

| Message/code | Meaning | What to do |
| --- | --- | --- |
| `no-peer-roster` with `roster-malformed` or `roster-wildcard` | A present roster contains invalid JSON/fields or a wildcard. | Replace it with a concrete roster containing both non-empty `principals` and `origins`; use `provision-peer-roster.sh validate`. |
| `workspace_unenrolled` | A hook or `federation emit` operation names a workspace with no local enrollment. | Run `loam federation connect` for that workspace first. |
| `connector_unreachable` | The CLI could not reach the local connector service. | Check the per-user service manager and connector logs; `status` itself is read-only and will not repair it. |
| `connector_refused` / `project_binding_mismatch` | The connector is running but refused an operation or the workspace/project binding is wrong. | Read the diagnostic, verify the workspace's enrollment and project scope, and correct the operation rather than bypassing the binding. |

## Final checks

After the hard stop is cleared and before handing the broker to other machines,
capture:

```sh
./acceptance-gate.sh dryrun
systemctl is-active loam-mosquitto.service
systemctl is-enabled loam-mosquitto.service
systemctl is-active loam-enroll-signer.service
ss -ltnp | grep -E ':8883|:8443'
```

Then complete the live checklist in
[`deploy/mqtt-broker/ACCEPTANCE.md`](../../deploy/mqtt-broker/ACCEPTANCE.md).
The final evidence should distinguish a successful enrollment-time probe from
the separate question of whether a connector has a live broker session now.
