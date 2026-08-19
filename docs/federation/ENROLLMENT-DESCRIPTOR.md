# Enrollment and connect contract

The newcomer-facing enrollment command is:

```sh
loam federation connect <workspace> <broker> \
  [--project <org>/<project>] [--token-file <path>] [--global-root <path>]
```

The broker must be an `mqtts://host:port` authority with no user info, path,
query, or fragment. `connect` validates the workspace and broker inputs first.
Without a token and without `--global-root`, it stops there, prints a
`validated` result, and does not write an enrollment. Supplying `--global-root`
permits the full connect path: an existing client identity can use that path
without a token, while a machine without one still needs a token for automatic
enrollment. Installed runtimes normally discover the platform configuration
root; use an explicit `--global-root` only when running outside that installed
layout.

## Scope resolution

The command chooses the organization and project in this order:

1. `--project org/project`, when supplied, provides both values;
2. otherwise `LOAM_FEDERATION_ORG` provides the organization;
3. otherwise `org` in the Loam config `config.json` provides the organization
   (for example, `$HOME/.config/loam/config.json` on Linux);
4. otherwise the command refuses with `federation_org_unconfigured`.

Without `--project`, the project is the last path component of the workspace's
`origin` remote. The organization is intentionally never guessed from the
remote host account. This prevents a repository hosted under one account from
being silently assigned to an unrelated broker organization.

Examples:

```sh
export LOAM_FEDERATION_ORG=example-org
loam federation connect /work/loam mqtts://mqtt.example.org:8883 \
  --token-file "$HOME/.config/loam/enroll-token"

loam federation connect /work/loam mqtts://mqtt.example.org:8883 \
  --project example-org/loam \
  --token-file "$HOME/.config/loam/enroll-token"
```

## What the command verifies

Before touching the registry or service manager, the command:

- resolves the Git top-level directory and its physical identity;
- checks the workspace's `origin` remote and stores only a SHA-256 digest of
  its URL;
- requires at least one configured remote/ref binding;
- rejects remote URLs that embed HTTP credentials;
- validates the broker endpoint and bounded project identifiers.

It does not fetch Git, mutate the workspace, or prove that a recorded commit is
reachable from the remote. A commit in a low-level descriptor is descriptive
provenance, not a reachability gate.

## Compatible descriptor shape

The runtime still validates this bounded shape for low-level/compatibility
callers. Normal operators do not need to construct it or pipe JSON to
`connect`; the positional command above builds the equivalent validated object.

```json
{
  "schema": 1,
  "org_id": "example-org",
  "project_id": "loam",
  "repository_id": "example-org/loam",
  "broker": {
    "profile": "default",
    "endpoint": "mqtts://mqtt.example.org:8883",
    "tls_server_name": "mqtt.example.org",
    "ca_ref": null
  },
  "git": {
    "commit": null,
    "remotes": [
      { "name": "origin", "refs": ["refs/heads/main"] }
    ]
  }
}
```

Rules:

- `schema` is exactly `1`.
- `org_id`, `project_id`, `repository_id`, `profile`, and
  `tls_server_name` are bounded non-empty strings without control characters.
- `endpoint` is `mqtts://host[:port]` with no user info, query, fragment, or
  path. Plain `mqtt://` is refused.
- `ca_ref` is optional. When present in the current runtime it names a PEM
  trust file for verifying the broker's server certificate; it is not a
  credential or secret-service lookup key.
- `git.commit` is optional. If present, it is 40 or 64 lowercase hexadecimal
  characters.
- `git.remotes` has one to eight entries. Each entry names a remote already
  configured in the workspace and contains one or more complete `refs/...`
  values. The raw remote URL is never stored in the validated projection.
- Secret-shaped or authority-shaped fields such as `password`, `token`,
  `credential_ref`, `private_key`, `principal`, and `instance_id` are rejected.

The current `connect` command creates the machine identity separately. The
descriptor does not carry a principal, agent, or instance id. The certificate
and the local Git identity establish those values; see
[IDENTITY-CONTRACT.md](IDENTITY-CONTRACT.md) and
[INSTANCE-ID-CONTRACT.md](INSTANCE-ID-CONTRACT.md).

## Enrollment transaction

With a token and an installed runtime, `connect` performs the following ordered
work:

1. resolve and validate the workspace/project/broker inputs;
2. generate a local keypair and CSR when no client identity exists;
3. POST the password and CSR to the broker-host signer over HTTPS;
4. store the returned certificate and generated key under the local profile;
5. authenticate to Mosquitto with mTLS and run the capability probe;
6. commit the enrollment row;
7. install and enable the local connector service.

If the same physical workspace has the same binding already, the operation is
idempotent and repairs local service drift without publishing another probe. A
different binding produces `enrollment_conflict`; disconnect deliberately before
changing a workspace's broker or project.

The capability probe checks authentication, required subscriptions, a
non-retained publish, and receipt of the exact self-event. That evidence records
what worked at enrollment time. It does not turn `status` or `list` into a live
broker check; both commands remain read-only and explicitly do not observe a
current session.

## Common typed refusals

| Code | Meaning |
| --- | --- |
| `federation_org_unconfigured` | No organization was configured; use the recipe in the message or pass `--project`. |
| `descriptor_invalid_endpoint` | The broker is not a valid TLS endpoint. |
| `workspace_not_git` | The path is not inside a Git workspace. |
| `remote_not_configured` | `origin` or a named descriptor remote is missing. |
| `credential_bearing_remote` | A remote URL embeds credentials. |
| `descriptor_invalid_field` / `descriptor_invalid_commit` | A bounded id, ref, or optional commit has the wrong shape. |
| `enrollment_conflict` | The same physical workspace has a different existing binding. |
| `connect_registry_error` | The local enrollment database could not be read or written. |
| `connect_probe_failed` | The broker probe failed; inspect the detail for authentication, TLS, transport, ACL, or self-receive stage. |
| `connect_activation_failed` | The probe succeeded but the local service manager did not start the connector. |
| `rollback_incomplete` | Connect could not remove all local state after a later failure; inspect before retrying. |

For signer-specific refusals such as `bad-token`, `signer-timeout`, and
`trust-anchors-unresolved`, use the detailed [broker setup failure
guide](BROKER-SETUP.md#failure-and-refusal-guide).
