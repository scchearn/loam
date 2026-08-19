# instance_id unification contract (seam C / T14)

Authority for keeping a node's enrolled `instance_id` and its broker-session
`instance_id` a single identical value, so Slice A accepts the envelope. A divergence
is refused by Slice A as `SourceInstanceMismatch` and surfaces as `connector_refused`
— never silently accepted.

## C.9 — Single source is the enrollment registry (CONFIRMED — no sidecar)

Confirmed in tula's direction:

- **`EnrolledRow.instance_id` is THE single source.** `provision_session` builds
  `MqttSession.claimed_identity.instance_id` from it; the connector **reads, never
  mints**.
- **This deployment does NOT create a sidecar** for the connector to consult. A
  sidecar would re-create the two-source defect T8 found.
- Provisioning's role is to **pin the value INTO enrollment**: at enrollment time
  (`loam federation connect`, C's path), the canonical `instance_id` this deployment
  mints is supplied so C writes it into `EnrolledRow.instance_id`. Everything
  downstream (session, envelope `source`) reads that one field.
- `provision-instance-id.sh` therefore **mints + emits** a stable id and hands it to
  the enrollment step (T9); it writes no store the connector reads. Its self-check is
  pure-logic (a matching pair passes; a mismatched pair is flagged, mirroring
  `SourceInstanceMismatch`).

Stability: the `instance_id` is pinned once per node and is **stable across
reconnects**. The connector MUST NOT regenerate it per session — it reads the pinned
`EnrolledRow.instance_id` every time.

## C.10 — Where principal_id / agent_id come from, and the `source` form

`SessionIdentity { principal_id, agent_id, instance_id, allowed_claims }`:

- **`instance_id`** — from `EnrolledRow.instance_id` (C.9). Single source.
- **`principal_id`** — provisioned in the enrollment row (single source) AND carried
  as the **client-cert CN**. Because the broker runs `use_identity_as_username true`,
  the broker's authenticated username **is** the cert CN, so `cert CN` MUST equal
  `EnrolledRow.principal_id`. The connector reads `principal_id` from enrollment; the
  broker independently enforces it via the cert — defense in depth. Neither the
  connector nor the resolver invents it.
  - **Identity model LOCKED (bigboss, 2026-08-09) — see IDENTITY-CONTRACT.md.**
    `principal_id` = git `user.email` = cert CN; display name = git `user.name`, bound
    in the signed cert subject (`GN`), read from the authenticated cert (not caller
    text). Connect path consistency-checks local git email against the cert CN (cert
    authoritative). `issue-client.sh` takes `(email, instance, name, [agent])`.
- **`agent_id`** — provisioned in the enrollment row. Not minted by the connector.
- **`allowed_claims`** — from the peer roster's `principals` for the project (see
  ROSTER-CONTRACT.md), not from the cert.

The client cert (issued by T4's org CA) is issued to **bind** these:

```
subject CN = <principal_id>
subjectAltName = URI:urn:loam:instance:<instance_id>[, URI:urn:loam:agent:<agent_id>]
```

so the cert is self-describing and the broker-authenticated CN == `principal_id`.
The cert SAN instance URI is a **consistency check** that must equal
`EnrolledRow.instance_id`; the connector's runtime source of `instance_id` remains
`EnrolledRow`, not the cert.

**`source` mapping form (exact):** `urn:loam:instance:<instance_id>` — the same
`<instance_id>` string as `EnrolledRow.instance_id`, verbatim. This is what appears in
CloudEvents `source` and must equal `data.from.instance_id`.

**Two forms, by context (do not conflate):**
- Envelope `source` / `data.from.instance_id` / cert SAN → **full URN**
  `urn:loam:instance:<instance_id>`.
- Topic delivery-origin segment / peer-roster `origins` → **bare** `<instance_id>`
  (see ROSTER-CONTRACT.md B.7). They differ only by the `urn:loam:instance:` prefix;
  `<instance_id>` is identical in both.

## C.11 — MQTT client-id MUST equal the bare instance_id (broker ACL requirement)

The broker's ACL (`acl`, T3) scopes origin-prefix **writes** by `%c` (the MQTT
client-id), NOT by `%u` (username=CN=principal_id). Reason: the two nodes may share
one `principal_id` (same person, laptop + MacBook), so `%u` cannot isolate their
origins — only the instance can. Therefore:

> **The connector MUST set the MQTT `client-id` to the node's BARE `instance_id`**
> (the same `<instance_id>` as `EnrolledRow.instance_id`). If the connector sets a
> random or different client-id, every publish is denied by the ACL
> (`origin != %c`).

This is a hard requirement on the connection code. `%u` (principal) still authorizes
`data.from.principal_id`; `%c` (instance) enforces origin isolation. Both are needed.

## Provisioning order (per node)

1. `provision-instance-id.sh` mints the canonical `instance_id` (ULID form) + confirms
   `principal_id`, `agent_id`.
2. T4 issues the client cert with `CN=principal_id`, SAN `urn:loam:instance:<instance_id>`.
3. T12 stores that cert/key under the enrollment's `credential_ref`.
4. Enrollment (`loam federation connect`, T9) writes `instance_id`/`principal_id`/
   `agent_id` into `EnrolledRow` — the single source the connector reads.
