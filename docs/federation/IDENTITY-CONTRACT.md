# Client identity contract

The client certificate is the identity presented to Mosquitto. Local Git
configuration helps create or check that identity; it is not allowed to rewrite
an already-issued certificate. The runtime reads the subject before opening the
session, then the broker independently authenticates the same certificate; the
certificate-derived values become session authority only after that acceptance.

## Identity model

- `principal_id` is the Git `user.email` and the certificate subject CN.
- The broker's `use_identity_as_username true` setting makes that CN the
  authenticated MQTT username.
- The display name is Git `user.name`, carried in the certificate's GN
  (`givenName`) subject attribute. It is descriptive, not an authorization
  field.
- The current runtime uses the enrolled instance id as its local `agent_id`.
- A second machine for the same person can share the same principal/CN while
  using a different instance id and client certificate.

The signed certificate binds the values that appear in messages. A caller cannot
claim another principal or display name by putting text in an operation.

## Certificate subject

[`pki/issue-client.sh`](../../deploy/mqtt-broker/pki/issue-client.sh) and the
automatic enrollment CSR use this subject shape:

```text
subject = CN=<git_email>, emailAddress=<git_email>, GN=<git_user_name>
subjectAltName = URI:urn:loam:instance:<instance_id>
```

The optional manual `agent_id` argument may add a second
`URI:urn:loam:agent:<agent_id>` SAN, but the current runtime does not need a
separate agent id. The instance SAN remains the load-bearing machine identity;
see [INSTANCE-ID-CONTRACT.md](INSTANCE-ID-CONTRACT.md).

- **CN** is the principal the broker authenticates.
- **emailAddress** repeats the email in a conventional subject slot.
- **GN** is the display name. A certificate without GN is valid and yields an
  empty display name.
- **SAN** carries the stable instance id.

The signer copies the SAN from the machine's CSR. The machine generates the
private key and never sends it to the signer.

## Connect-time checks

Automatic enrollment requires a Git `user.email` because it must name the CSR.
`user.name` is optional. After a certificate exists, the runtime:

1. reads the CN, GN, and instance SAN from the certificate it will present;
2. checks the SAN instance id against the enrolled row;
3. when a local Git email is configured, checks it against the certificate CN;
4. uses the authenticated certificate subject to build the session identity.

A mismatch is refused. The runtime never changes Git configuration to make a
mismatch disappear and never trusts a sender-supplied display name.

## ACL relationship

The broker uses `%u` for the certificate CN and `%c` for the MQTT client id.
The CN authorizes the principal, while the bare instance id in `%c` prevents
one of a person's machines from publishing under the other's origin. See
[`acl`](../../deploy/mqtt-broker/acl) and
[ROSTER-CONTRACT.md](ROSTER-CONTRACT.md).

## Operator checks

To inspect a manually issued certificate without exposing the private key:

```sh
openssl x509 -in /path/to/client.crt -noout -subject -ext subjectAltName
```

The CN should equal the Git email used for enrollment, and the SAN should carry
the same instance id recorded by the enrollment. The certificate must chain to
the organization CA configured as Mosquitto's `cafile`; the broker's public
server certificate is a separate Let's Encrypt certificate.

## Failure meanings

| Failure | Meaning | Repair |
| --- | --- | --- |
| `git-identity-required` | Automatic enrollment has no usable Git email. | Set `git config user.email`; set `user.name` if a display name is wanted. |
| `identity-mismatch` | The local Git email or certificate SAN disagrees with the enrollment. | Use the matching certificate/key and reconnect the intended workspace; do not edit the message identity. |
| `certificate-malformed` / `key-format-unsupported` | The local PEM material cannot be loaded. | Restore a complete certificate and a supported private-key encoding. |
| `key-cert-mismatch` | The certificate and private key are individually valid but do not belong together. | Replace the pair with the bundle from one enrollment. |
| `connect_probe_failed` with `probe_authentication_failed` | The broker was reached but rejected the certificate identity. | Check the organization CA, certificate expiry/revocation, CN, and Mosquitto ACL. |

The certificate proves who the session authenticated as. It does not prove that
the session will remain online after `connect` returns; current liveness is a
separate service and broker observation.
