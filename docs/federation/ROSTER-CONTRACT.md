# Peer-roster contract (seam B / T13)

Authority for the per-project peer-roster file the connector's session builder reads
as its injected `PeerRoster` (D-T1). The **provisioning side** (`provision-peer-roster.sh`)
writes it; the **connector side** (tula) reads it. It is a **separate provisioned
file**, NOT an enrollment-descriptor field — no change to C's registry schema.

## B.6 — File location + naming (byte-exact; any mismatch = silent empty roster)

```
${LOAM_FEDERATION_ROSTER_DIR}/{org_id}/{project_id}.json
```

- `LOAM_FEDERATION_ROSTER_DIR` default: `${HOME}/.agents/loam/federation/rosters`
  (under the documented global loam root `~/.agents/loam`). Overridable by the
  `LOAM_FEDERATION_ROSTER_DIR` env var for tests.
- `{org_id}` and `{project_id}` are the enrollment's exact `org_id` / `project_id`
  strings, used verbatim as path segments.
- **One file per (org, project).** Not a single file with a project map.

> **CONFIRMED (tame, via galu 2026-08-09):** the default root IS the production global
> root — `discovery.mjs` computes `~/.agents/loam`; the connector registry is
> `<global-root>/loam.sqlite3`; rosters live at
> `~/.agents/loam/federation/rosters/{org_id}/{project_id}.json`. The connector reads
> via a 3-rung ladder — `LOAM_FEDERATION_ROSTER_DIR` (tests) → `$LOAM_HOME` → the
> default — identical bytes. This deployment writes to the default path; nothing more
> to match.

## B.7 — Schema (exact JSON field names)

```json
{
  "version": 1,
  "org_id": "<org_id>",
  "project_id": "<project_id>",
  "principals": ["<principal_id>", "..."],
  "origins": ["<instance_id>", "..."]
}
```

- `principals`: list of **bare `principal_id`** strings → maps onto B's
  `allowed_claims`.
- `origins`: **FORM PINNED — BARE instance ids** (e.g. `01K6Q4…`), NOT
  `urn:loam:instance:` URIs → maps onto B's `allowed_origins`. This matches the
  topic's delivery-origin segment, which is a bare id, so **neither side transforms**
  (tula strips nothing). The bare id is the `<id>` portion of the envelope `source`
  `urn:loam:instance:<id>` — see INSTANCE-ID-CONTRACT.md, which keeps the full URN
  form for CloudEvents `source`/`data.from.instance_id`. Roster origin = bare;
  envelope source = urn; the two differ only by the `urn:loam:instance:` prefix.
- `version` pins the schema; `org_id`/`project_id` must equal the enclosing path
  segments (the validator checks this).

## B.8 — Absent vs empty vs wildcard

Confirmed aligned with tula's intent:

| State | Meaning |
| ----- | ------- |
| File **absent** | `no-peer-roster` — connector does not build a delivering session. |
| File present, `principals` AND `origins` both empty | **Refused** — invalid roster, never a session. |
| Any entry is a wildcard (`*`, `**`, empty string) | **Refused** — never a session. |
| File present with ≥1 concrete principal and/or origin | Populated — session admits exactly those. |

`provision-peer-roster.sh --validate` enforces exactly this: it refuses to write an
empty or wildcard roster, so "populated" means the same thing on both halves.

## Two-instance run (laptop + MacBook)

For the bigboss run, one project, two instances. The roster for that project names
**both** nodes so each admits the other's frames — see `peer-roster.example.json`.
