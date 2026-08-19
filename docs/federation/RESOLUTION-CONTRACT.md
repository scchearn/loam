# Credential resolution contract (seam A / T12)

Authority for how the connector-side resolver turns an enrollment's `credential_ref`
and `ca_ref` into broker credentials. The **provisioning side** (this deployment,
`resolve-credentials.sh`) *stores* the material; the **connector side** (tula) *looks
it up*. Both halves MUST use the identical lookup below or they silently disagree.

## Load-bearing invariant

The secret-service lookup keys ARE the enrollment's stored `credential_ref` and
`ca_ref` values, used **verbatim** (byte-for-byte, no normalization, no re-derivation).
Provisioning stores under exactly those strings; the connector reads those strings off
`EnrolledRow` and looks them up. A normalized or re-derived key = silent
`credentials-unresolved`.

## A.1 — Material shape (auth model)

**mTLS is the SOLE authentication mechanism. There is no password.**

- The broker runs `require_certificate true` + `use_identity_as_username true`
  (see `mosquitto.conf`): the broker derives the MQTT **username from the client
  cert CN** and there is **no password check**.
- Therefore: `MqttSession.username` = the client-cert **CN** (== `principal_id`,
  see INSTANCE-ID-CONTRACT.md §C.10); `MqttSession.password` = **empty/unused**.
  The broker ignores whatever password is sent. **Recommendation to tula:** make
  `username`/`password` optional; if they must stay non-optional, set
  `username = <cert CN>` and `password = ""`. Neither is looked up from the secret
  service — the username is read from the resolved client cert's subject CN.
- The looked-up material is exactly: the **client cert + client key** (via
  `credential_ref`) and the **server-verification trust** (via `ca_ref`). See A.4.

## A.2 — Lookup mechanism (exact argv the connector reproduces)

Backend is selected by **OS at runtime** (see A.5), not by the ref's scheme. The
service label is `SECRET_SERVICE_LABEL` (default `loam-federation`). The ref string
is the lookup selector.

**Linux (libsecret / Secret Service):**
```sh
# retrieve (connector does this) — secret is written to stdout, never argv:
secret-tool lookup service loam-federation ref "<credential_ref>"
secret-tool lookup service loam-federation ref "<ca_ref>"          # only if ca_ref present
```

**macOS (Keychain):**
```sh
# retrieve (connector does this) — -w prints only the secret to stdout:
security find-generic-password -s loam-federation -a "<credential_ref>" -w
security find-generic-password -s loam-federation -a "<ca_ref>" -w   # only if ca_ref present
```

Attribute mapping is fixed: Linux `service=loam-federation`, `ref=<the ref string>`;
macOS `-s loam-federation` (service), `-a <the ref string>` (account). Retrieval on
both platforms outputs the secret to **stdout** and never places it in argv.

(Provisioning side stores with `secret-tool store --label "loam-federation:<ref>"
service loam-federation ref "<ref>"` on Linux and `security add-generic-password -U
-s loam-federation -a "<ref>" -w "<blob>"` on macOS. The macOS store passes the blob
in argv — provisioning-time only, on the operator's trusted machine; retrieval is
argv-safe.)

## A.3 — Ref syntax

`credential_ref` and `ca_ref` are **opaque verbatim lookup keys**. The resolver does
**not** parse a scheme. A `vault://…` prefix (or any prefix) is part of the opaque
key string, not a routing directive; the backend is chosen by OS, not by scheme.
Accept **any** ref string. (v1 deliberately does not implement scheme-based backend
routing; if introduced later it is additive and must keep the verbatim-key invariant.)

## A.4 — Encoding

- Format: **PEM** (text) throughout.
- `credential_ref` → **one** lookup returning a single PEM blob = the **client
  certificate PEM followed by the client private key PEM**, concatenated (cert
  first, then key). The resolver splits on the PEM boundary
  (`-----END CERTIFICATE-----` / `-----BEGIN … PRIVATE KEY-----`). One ref, one
  lookup — matches C's single `credential_ref`.
- `ca_ref` → the **server-verification** CA PEM. **Important:** this is the trust
  anchor the client uses to verify the **broker's server cert**, which is issued by
  **Let's Encrypt** (public). It is NOT the org CA (the org CA lives only on the
  broker as `cafile` to verify *clients*).
  - **`ca_ref` absent ⇒ use platform system/public roots** (correct and expected for
    the LE server cert). The connector should treat this as "verify against the
    system trust store" — populate `MqttSession.ca_certificate` as empty/None to
    signal system roots. **Recommendation to tula:** make `ca_certificate` optional,
    or interpret empty as system roots.
  - **`ca_ref` present ⇒ pin** that CA PEM for server verification (for a future
    private-CA server-cert variant). Not used in the example.org/LE deployment.
- Max two lookups per node (`credential_ref`, optional `ca_ref`).

## A.5 — Cross-platform (make-or-break: Linux laptop ↔ MacBook)

Both backends are first-class and named. The resolver and the connector select by OS:

| OS | Backend | Store | Retrieve |
| -- | ------- | ----- | -------- |
| Linux | Secret Service (libsecret) | `secret-tool store …` | `secret-tool lookup service loam-federation ref "<ref>"` |
| macOS | Keychain | `security add-generic-password -U -s loam-federation -a "<ref>" -w "<blob>"` | `security find-generic-password -s loam-federation -a "<ref>" -w` |

The MacBook node uses Keychain; the Linux laptop uses libsecret. A resolver that
implements only one is a one-node deployment — both are required. Selection is by
`uname` (`Darwin` ⇒ Keychain, else Secret Service); an override env
`LOAM_SECRET_BACKEND=secret-tool|security` is honored for testing.

## Summary for tula's struct

- `username` = client-cert CN (== principal_id); `password` unused (prefer optional).
- `client_authentication` = (cert, key) split from the single `credential_ref` PEM blob.
- `ca_certificate` = system roots when `ca_ref` absent (prefer optional/empty-means-system);
  pinned PEM when present.
- lookup keys = `credential_ref` / `ca_ref` verbatim; backend by OS (libsecret / Keychain).
