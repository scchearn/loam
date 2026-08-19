# Instance identity contract

One enrolled machine has one stable `instance_id`. The same value must be used
by the client certificate, the enrollment row, the MQTT client id, the topic
origin, and the envelope source. If those values diverge, the broker ACL or the
connector rejects the message instead of accepting an ambiguous identity.

## One source of truth

The normal first connection is `loam federation connect --token`:

1. the machine generates a fresh 26-character Crockford-base32 id;
2. the CSR carries `urn:loam:instance:<instance_id>` in its SAN;
3. the enrollment signer returns an organization-CA-signed certificate;
4. the runtime stores the certificate and the matching private key locally;
5. the enrollment row records the same id;
6. session setup reads the certificate SAN and checks it equals the row value.

The connector does not mint an id when it starts, and it does not maintain a
sidecar identity file. Reconnecting the same machine reuses the enrolled id.
The [identity contract](IDENTITY-CONTRACT.md) defines the certificate subject;
this page defines the instance portion.

The helper
[`provision-instance-id.sh`](../../deploy/mqtt-broker/provision-instance-id.sh)
can mint and print an id for a manual certificate workflow. It is a provisioning
aid, not a file the runtime reads. If it is used, pass the same id to
`pki/issue-client.sh` and to the enrollment that stores the certificate.

## Where the value appears

| Context | Required value |
| --- | --- |
| Client certificate SAN | `URI:urn:loam:instance:<instance_id>` |
| Enrollment row | bare `<instance_id>` |
| MQTT client id | bare `<instance_id>` |
| Topic origin segment | bare `<instance_id>` |
| CloudEvents `source` | `urn:loam:instance:<instance_id>` |
| `data.from.instance_id` | bare `<instance_id>` |
| Peer-roster `origins` | bare `<instance_id>` |

The URN is a wire-format wrapper. The suffix is identical in every row above;
only the envelope/certificate form carries the `urn:loam:instance:` prefix.

## Principal and agent fields

The client certificate CN is the authenticated principal, normally the Git
`user.email`. The current runtime uses the enrolled instance id as the local
`agent_id`; it does not mint a second machine identifier. The display name is
read from the certificate's optional GN subject attribute. See
[IDENTITY-CONTRACT.md](IDENTITY-CONTRACT.md).

Two machines used by one person may therefore share a principal/CN while
having different instance ids. This is why the ACL uses `%u` for principal
authorization and `%c` for origin-write isolation.

## Broker ACL requirement

The rendered [`acl`](../../deploy/mqtt-broker/acl) uses `%c` for writes:

```text
pattern write loam/v1/${ORG_ID}/+/event/%c
pattern write loam/v1/${ORG_ID}/+/state/%c/#
pattern write loam/v1/${ORG_ID}/+/inbox/+/+/%c/#
```

The connector must set its client id to the bare instance id. A random client
id can still complete TLS authentication, but its publishes are denied because
the topic origin no longer equals `%c`.

## Manual issuance order

For an operator who is issuing a certificate rather than using automatic
enrollment:

1. mint one id with `provision-instance-id.sh mint`;
2. issue a client certificate with
   `pki/issue-client.sh <email> <instance_id> <display_name>`;
3. place the certificate and matching key in the profile's identity directory
   (`client.pem` and `key.pem`) with private permissions;
4. connect the workspace using the same broker/project and confirm the stored
   row uses that id.

Automatic enrollment performs those binding steps for the operator and is the
recommended path for a new machine.

## Detecting a mismatch

The helper can check two values before enrollment:

```sh
./provision-instance-id.sh check "$ENROLLED_ID" "$SESSION_ID"
```

A mismatch is reported as `SourceInstanceMismatch`/`connector_refused` in
low-level checks and normally surfaces during session setup as an identity
mismatch or `connect_probe_failed`. Replace the inconsistent certificate or
enrollment rather than changing the topic or client id by hand.

## Limits

The id is stable across reconnects, but it is not a proof that a connector is
currently connected. `loam federation status` and `loam federation list` read
local registry and service-manager state without observing a broker session.
Use service logs and broker-side evidence for current liveness.
