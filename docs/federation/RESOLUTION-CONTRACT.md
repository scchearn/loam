# Credential and trust resolution

This document describes what the current runtime reads when it turns an
enrollment into a live MQTT session. The important distinction is between the
broker's two TLS directions:

- the broker verifies **client certificates** with the organization's private
  CA;
- the client verifies the broker's **server certificate**, normally a public
  Let's Encrypt certificate, with public trust roots.

The current `loam federation connect --token` path stores the machine identity
locally. It does not put a password or a private key in the enrollment row.

## Local identity bundle

The runtime reads two files from the configured federation identity directory:

```text
<profile>/identity/client.pem
<profile>/identity/key.pem
```

`LOAM_FEDERATION_IDENTITY_DIR` overrides the directory directly. Otherwise the
profile follows the same configuration ladder as the registry and roster:
`LOAM_CONFIG_DIR`, the platform config directory, `LOAM_HOME`, then the legacy
`~/.agents/loam` location.

On Unix, the runtime enforces mode `0700` on the directory and `0600` on both
files whenever it reads them. A missing bundle is reported as
`credentials-unresolved` with `identity-required`; a malformed certificate,
unsupported key, or certificate/key mismatch names the corresponding input.

The key reader accepts the forms commonly produced by OpenSSL and by this
repository's scripts:

- PKCS#8 (`BEGIN PRIVATE KEY`);
- SEC1 EC (`BEGIN EC PRIVATE KEY`), including an optional EC parameters block;
- PKCS#1 RSA (`BEGIN RSA PRIVATE KEY`).

The certificate must be first-class PEM, and the private key must match its
public key. A mismatch is `key-cert-mismatch`; an unusable key is
`key-format-unsupported`; a bad certificate is `certificate-malformed`.

The auto-enrollment signer receives the CSR, not this private key. The private
key is generated and stored on the joining machine.

## Server trust

When an enrollment has no `ca_ref` (the normal `connect` path), the runtime
uses its bundled Mozilla roots. If `SSL_CERT_FILE` is set, that file is an
explicit override and must contain a readable PEM trust bundle; an unreadable,
empty, or certificate-free file produces `trust-anchors-unresolved` during
automatic enrollment or `ca-unresolved` while opening a stored session.

When `ca_ref` is present, the current runtime treats it as a path to a PEM trust
file and reads that file directly. It is not a secret-service selector in the
current connector. A blank or unusable path is refused rather than silently
falling back to public roots.

For the production broker described in
[`BROKER-SETUP.md`](BROKER-SETUP.md), the server certificate is issued by
Let's Encrypt, so omitting `ca_ref` is expected. The organization's private CA
belongs in Mosquitto's `cafile` to verify client certificates; it should not be
copied into client trust settings merely because it signs those clients.

## MQTT authentication

Provisioned sessions use mutual TLS only:

- the broker requires a client certificate;
- `use_identity_as_username true` makes the certificate CN the authenticated
  MQTT username;
- the runtime sends no username/password pair for a provisioned session;
- the client id is the bare enrolled instance id, because the ACL scopes origin
  writes through `%c`.

The enrollment password is used only for the HTTPS signer during automatic
enrollment. It is not the MQTT password. See
[INSTANCE-ID-CONTRACT.md](INSTANCE-ID-CONTRACT.md) and
[IDENTITY-CONTRACT.md](IDENTITY-CONTRACT.md) for the values bound into the
certificate and client id.

## Manual helper and compatibility note

[`resolve-credentials.sh`](../../deploy/mqtt-broker/resolve-credentials.sh)
can store a certificate-plus-key PEM blob in Secret Service or macOS Keychain
for manual integrations. Its `credential_ref` terminology belongs to that
helper's compatibility surface. The current Loam runtime does not look up
`credential_ref`; it reads `client.pem` and `key.pem` from the profile above.

If another integration uses the helper, preserve its exact opaque reference
string and the `service=loam-federation`/Keychain service label it documents.
Do not infer that helper's storage format into a new `loam federation connect`
descriptor.

## Failure map

| Refusal | Meaning | Repair |
| --- | --- | --- |
| `identity-required` | `client.pem` or `key.pem` is missing or cannot be read. | Check the profile path and private permissions; rerun automatic enrollment if the machine has no certificate. |
| `certificate-malformed` | The client certificate PEM or its subject cannot be parsed. | Restore a complete org-CA-signed client certificate. |
| `key-format-unsupported` | The private key is not a supported PEM encoding or cannot be loaded. | Use a PKCS#8, SEC1, or PKCS#1 private key and keep its matching certificate. |
| `key-cert-mismatch` | The two files are valid but are not a pair. | Replace them with the certificate and key created by the same enrollment. |
| `ca-unresolved` | The session could not build server trust. | Repair/remove the stale `SSL_CERT_FILE` or fix the `ca_ref` PEM path. |
| `trust-anchors-unresolved` | The signer client failed to build trust before dialing. | Read the detail for the exact file and repair it; this is a local trust-store problem, not proof that the signer is down. |
| `endpoint-malformed` | The stored endpoint is not a valid `mqtts://host:port` authority. | Reconnect with the broker FQDN and TLS port. |

The read-only `status` and `list` commands do not resolve credentials and do
not make a broker connection. A row saying that a past probe verified
authentication is historical evidence, not a current session observation.
