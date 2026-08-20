# Loam federation

Federation lets Loam installations share work state and messages through an
MQTT broker. This directory is the operator-facing reference for the identity,
enrollment, and broker contracts.

## Production broker status

The checked-in `deploy/mqtt-broker/acl` grants every live surface the connector
needs — project-membership and agent-inbox reads, and organization member-card
subscription (`members/+`) and retained publication (`members/%c`) — proven by
[`acl-contract.sh`](../../deploy/mqtt-broker/acl-contract.sh) against a
throwaway Mosquitto. The broker is a dumb pipe: organization is the only trust
boundary, project is a routing/capability concept, and the org-scoped project
`+` wildcard is correct. Cross-project filtering lives in the connector
application layer, not the ACL; see the
[settled trust model](../../deploy/mqtt-broker/ACCEPTANCE.md#trust-model-settled).

> **Automated `provision` stage still unavailable.** The gate's `provision`
> path copies the tracked templates without rendering `${VARS}`. Deploy with
> the manual `envsubst` sequence in [Broker setup](BROKER-SETUP.md), section 5,
> not `LOAM_LIVE_GO=1 ./acceptance-gate.sh provision`.

Start here:

1. A broker operator follows [Broker setup](BROKER-SETUP.md). It is the
   intended newcomer path from a new host to a broker and enrollment signer.
2. A machine operator follows the [first connection](BROKER-SETUP.md#8-first-machine-connection)
   section, then verifies the local inventory with `status` and `list`.
3. For details, use the [broker deployment reference](../../deploy/mqtt-broker/README.md),
   [deployment runbook](../../deploy/mqtt-broker/RUNBOOK.md), and the contracts
   below.

## The short version

On the broker host, the two TLS directions use different trust roots:

- the broker's server certificate comes from the host's certbot/Let's Encrypt
  setup and is trusted by clients through public roots;
- client certificates come from the organization's private CA, which Mosquitto
  uses to authenticate clients.

On a new machine, the normal path is:

```sh
git -C /path/to/workspace config user.email you@example.org
export LOAM_FEDERATION_ORG=example-org
loam federation connect /path/to/workspace mqtts://mqtt.example.org:8883 \
  --token-file "$HOME/.config/loam/enroll-token"
loam federation status
loam federation list
```

The token is the enrollment password created by the broker's signer. Keep the
token file private and remove it after the connection succeeds. Use
`--project example-org/project-name` instead of `LOAM_FEDERATION_ORG` when a
one-off command should explicitly name both parts of the project scope.

`connect` without a token and without `--global-root` only validates the
workspace and broker inputs; it does not create an enrollment. Supplying
`--global-root` permits the full connect path. A machine without a client
identity still needs a token for automatic enrollment; a machine with an
existing identity can use the full path without a token. Installed runtimes
normally discover the platform configuration root, so use an explicit
`--global-root` only when running outside that installed layout.

The broker endpoint must be TLS-only, in the form `mqtts://host:port`. The
normal production port is `8883`; a plaintext `mqtt://` endpoint is refused.

## Reading verification output correctly

Enrollment records include historical evidence from the connection probe:
authentication, publish, subscribe, and receipt of the probe's own event. That
evidence is useful, but it is not a permanent liveness guarantee.

Both commands below are read-only:

```sh
loam federation status --json
loam federation list --json
```

`status` reports the enrollment registry, whether the local service definition
is present, and the service manager's enabled/disabled state. It deliberately
reports `live broker session not observed`; it does not dial the broker.
`list` is an inventory of joined projects and their last verification time. It
makes no claim about which peers or broker sessions are online. To investigate
current liveness, inspect the connector service and its logs, and use a broker
side health check; do not treat an enrollment row alone as proof of a live
session.

## Contract documents

| Document | What it defines |
| --- | --- |
| [Enrollment and connect](ENROLLMENT-DESCRIPTOR.md) | The current `connect` inputs and the compatible descriptor shape. |
| [Identity](IDENTITY-CONTRACT.md) | How the client certificate binds the email and display name. |
| [Instance identity](INSTANCE-ID-CONTRACT.md) | How the one machine instance id flows into the certificate, session, and MQTT client id. |
| [Credential resolution](RESOLUTION-CONTRACT.md) | Which local files and trust anchors the current runtime reads. |
| [Peer roster](ROSTER-CONTRACT.md) | The per-project peer allow-list and its path/schema. |

The scripts that provision broker-side material live in
[`deploy/mqtt-broker/`](../../deploy/mqtt-broker/). The [broker setup
walkthrough](BROKER-SETUP.md) links each step to the script that performs it;
the contract pages above explain the values those scripts must produce.

## Common operator mistakes

- **No organization configured:** `connect` will not guess an organization
  from a Git hosting account. Set `LOAM_FEDERATION_ORG`, write `org` in the
  Loam config `config.json` (for example, `$HOME/.config/loam/config.json` on
  Linux), or pass `--project org/project`.
- **Using the broker CA on the machine:** the organization CA authenticates
  clients to Mosquitto. It is not normally the trust anchor for the broker's
  Let's Encrypt server certificate.
- **Using a plaintext endpoint:** use `mqtts://...:8883`, never `mqtt://`.
- **Putting the token on the command line:** use `--token-file`; command lines
  can be visible in process listings and shell history.
- **Reading `status` as a ping:** it is intentionally egress-free. A successful
  read says what is recorded locally, not what is connected right now.

For typed refusal messages and the operator action for each one, see the
[failure guide](BROKER-SETUP.md#failure-and-refusal-guide).
