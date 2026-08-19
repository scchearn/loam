# Identity contract (LOCKED — bigboss, 2026-08-09, email-based)

How a human principal is identified, and how the trustworthy display name is bound.
Provisioning side (this deployment) issues the cert; the connector side (tula) reads
identity from the CONNACK-authenticated cert. Same pattern as instance_id: the cert is
authoritative, local config is only a consistency check.

## The model

- **`principal_id` = git `user.email`.** It is the cert **CN**, the ACL principal
  (broker `use_identity_as_username true` → username = CN = email), the dedup key, and
  `data.from.principal_id`.
- **display name = git `user.name`.** Carried so notifications read "from <name> did X"
  instead of an email.
- **Security: the display name is bound into the authenticated cert, never free-form
  sender text.** Otherwise anyone could set their name to "Samuel Hearn" and spoof the
  boss. It lives in the signed subject, so the broker-authenticated cert carries it.

## Cert subject (what `issue-client.sh <email> <instance> <name> [agent]` produces)

```
subject = CN=<git_email>, emailAddress=<git_email>, GN=<git_user_name>
subjectAltName = URI:urn:loam:instance:<instance_id>[, URI:urn:loam:agent:<agent_id>]
```

- **CN** — the email = `principal_id`. Broker username = CN.
- **emailAddress** — the same email (conventional slot; redundant with CN by design).
- **GN (givenName, OID 2.5.4.42)** — the display name. Chosen as a standard, openssl-
  supported subject attribute that reliably round-trips in the signed subject. It is a
  **carrier**, not a semantic given-name. **tula: if you'd prefer a different subject
  attribute (e.g. a `displayName` OID), say so — it's a one-line `-subj` change; flagged
  for your ack.**
- SAN — instance/agent URNs (see INSTANCE-ID-CONTRACT.md).

**GN-absent policy (agreed with tame):** a cert with no `GN` → `display_name` is empty,
NOT a refusal — identity is the CN; the name is cosmetic + sanitized. `issue-client.sh`
always sets GN (required arg), so this should not arise here, but the connector treats
absent GN as empty display_name.

**Cert structure is standard X.509** — openssl-issued, DER subject in the standard
issuer-then-subject order, CN/GN in the subject `Name`. tame reads the subject via a
position-correct DER TLV walk (second `Name`, no x509 parser dep); this deployment keeps
the structure standard, so that walk is stable.

## Connector side (tula)

- Read `principal_id` = cert **CN** (authenticated email). This settles C.10's principal
  source: from the authenticated cert, not caller text, not the enrollment row alone
  (the enrollment row's `principal_id` must equal the cert CN).
- Add `display_name` to `data.from`, sourced from the **authenticated cert GN** — NEVER
  from caller/transcript text. Render it through `sanitize_untrusted` (Slice D
  injection-safety still applies to the name string).
- **Connect-path consistency check:** read local git `user.email` + `user.name`; the
  **email MUST equal the provisioned cert CN** (cert authoritative). A mismatch is a
  typed refusal, never an override — same shape as the instance_id SAN check.

## Two-instance run (laptop + MacBook)

Both nodes are the SAME person ⇒ SAME email ⇒ SAME `principal_id` and SAME display name,
differing ONLY by `instance_id`/`client-id`. This is exactly why the ACL scopes origin
writes on `%c` (client-id = instance_id), not `%u` (= CN = shared email). The peer roster
for the run lists the one email in `principals` and both bare instance ids in `origins`.
