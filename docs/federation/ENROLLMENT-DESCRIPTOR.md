# `loam federation connect` descriptor schema (verified against cli-v0.99.1)

Discovered empirically from the installed runtime (feeding invalid descriptors and
reading the validation errors; no real enrollment was created). Fed on stdin:

```
loam federation connect <workspace> --global-root <global-root> --json   < descriptor.json
```

## Schema

```json
{
  "schema": 1,
  "org_id": "<org-id>",
  "project_id": "<project-id>",
  "repository_id": "<repository-id>",
  "broker": {
    "profile": "<profile-name>",
    "endpoint": "mqtts://<host>:<port>",
    "tls_server_name": "<host>",
    "credential_ref": "<secret-service lookup ref>"
  },
  "git": {
    "commit": "<40- or 64-char lowercase hex>",
    "remotes": [ { "name": "<configured-remote-name>", "refs": ["refs/heads/<branch>"] } ]
  }
}
```

## Validation rules observed

- `schema` required (=1).
- `org_id`, `project_id`, `repository_id` required strings.
- `broker` required object: `profile`, `endpoint`, `tls_server_name`, `credential_ref` all
  required. `endpoint` must be `mqtts://` with **no userinfo, query, or fragment**.
  **`ca_ref` is OPTIONAL** — absent ⇒ verify the server cert against system roots (our
  Let's-Encrypt case). Matches RESOLUTION-CONTRACT.md.
- `git` required object: `commit` (40/64 lowercase hex), `remotes` (≥1 item). Each remote
  is `{ name, refs }` — referenced **by name** (must already be configured in the
  workspace), NOT by URL, and `refs` is the allow-list. `url`/`allowed_refs` are rejected.
- Post-schema checks are real: e.g. `remote_not_configured` if the named remote isn't in
  the workspace's git config.

## Identity is NOT in the descriptor — resolved (verified against `provisioning::resolve`)

The descriptor has no `instance_id`/`principal_id`/`agent_id`. Verified in the source
(`provisioning::resolve`, the production enrolled-session builder — NOT the
`orchestrate_cli` **stub** path, whose `connector-{instance_id}` is a dry-run
placeholder the source comments call out as "the stub reports this derived identity"):

- `instance_id` ⇐ **per-machine value at `<global-root>/instance_id`** (`ensure_instance_id`),
  read at `connect` and pinned into the enrolled row. The single source; the connector
  never mints or defaults it. It is NOT in the cert and NOT pre-minted by the deployment.
- `principal_id` ⇐ **cert CN**; `display_name` ⇐ **cert GN**; the cert subject is validated
  against the workspace's local git `user.email` (`match_local_identity`).
- `agent_id` ⇐ the same `instance_id` (one workspace = one agent).
- **MQTT client-id ⇐ the bare `instance_id`** — the ACL's `%c` origin scoping keys on it;
  a wrong client-id is accepted at auth but every publish is *silently denied*.

**So the client cert only needs `CN = git email`, `GN = display name`, org-CA-signed** —
the `urn:loam:instance:` SAN is **not load-bearing** (resolve takes `instance_id` from the
enrolled row, not the cert). The earlier `IDENTITY-CONTRACT.md`/`RESOLUTION-CONTRACT.md`
and the `%c` ACL are correct as written.
