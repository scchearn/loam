# loam

<p align="center">
  <img src="loam.svg" alt="loam" width="120">
</p>

<p align="center">
  <a href="./WHY.md"><img alt="Why Loam exists" src="https://img.shields.io/badge/why-Loam%3F-6b5b45"></a>
  <a href="https://www.npmjs.com/package/@scchearn/loam"><img alt="npm version" src="https://img.shields.io/npm/v/@scchearn/loam?logo=npm&amp;label=npm"></a>
  <a href="https://github.com/scchearn/loam/actions/workflows/ci.yml"><img alt="CI status" src="https://img.shields.io/github/actions/workflow/status/scchearn/loam/ci.yml?branch=main&amp;label=CI&amp;logo=githubactions"></a>
  <a href="https://github.com/scchearn/loam/releases"><img alt="runtime version" src="https://img.shields.io/github/v/tag/scchearn/loam?filter=cli-v*&amp;label=runtime&amp;logo=rust"></a>
  <a href="./LICENSE"><img alt="MIT license" src="https://img.shields.io/github/license/scchearn/loam"></a>
</p>

loam gives AI coding tools a shared way to plan work, research questions,
remember project knowledge, and pick up where a previous session stopped.
The durable parts are plain Markdown, so they remain useful when you change
tools or machines.

[Why loam exists →](./WHY.md)

## What loam includes

- **Workflow skills** for specifications, plans, goals, checkpoints, memory,
  code indexing, and keeping project guidance current.
- **A private native runtime** that supplies workspace state and hook delivery.
  Downloads use HTTPS and a published SHA-256 checksum before the binary runs.
  Supported targets are macOS (Intel and Apple silicon), Windows x64, and
  Linux x64/arm64.
- **Federation** for sharing current work state between computers through a
  self-hosted MQTT broker with mutual TLS certificates. Project membership
  controls who can receive updates, and a reconnecting computer can catch up
  with the latest state.
- **Enrollment that checks its inputs before saving them.** A workspace and
  broker are validated together, and a first machine can obtain its client
  certificate with an enrollment token. Refusals name the problem instead of
  silently accepting a broken setup.
- **Connector recovery and honest status.** The background connector retries
  failed broker sessions and the session context says when collaboration is
  live or temporarily unavailable rather than presenting stale data as live.
- **Automatic session delivery** in Claude Code, Codex CLI, and OpenCode. Cursor
  gets the skills and the federation CLI, but no automatic collaboration claim
  is made there yet.
- **Independent plugin and runtime releases**, so a skills or integration
  change does not require a native-binary release. See
  [RELEASING.md](./docs/RELEASING.md).

## Install

Run this from any directory:

```bash
npx @scchearn/loam install
```

`install` installs the skills globally, downloads the private native runtime,
and configures detected coding tools for your user account. It does not add
anything to `PATH`. By default it asks which detected tools to configure; use
`--yes` for an unattended install or
`--dry-run` to preview without downloads or changes:

```bash
npx @scchearn/loam install --yes
npx @scchearn/loam install --dry-run
```

For Claude Code and Codex, it also offers their native marketplace plugin;
OpenCode and Cursor use their user-level integrations.

A normal install creates no project-local copy; a legacy-project migration can
remove obsolete project-local skills and Loam markers. Start a new coding
session after setup so the tool can load the new skills and integration.

### Install, update, and setup are different

| Command | Use it for | Changes versions? |
| --- | --- | --- |
| `install` | First install, or repair a same-version install | Yes, when installing the package's version |
| `update` | Move an existing install to this package version | Yes |
| `setup` | Configure federation, optional integrations, or selected coding tools | No |
| `doctor` | Check the installation without changing it | No |

Update an existing install with:

```bash
npx @scchearn/loam update
```

An update refreshes the skills, native runtime, integrations, marketplace
plugins, hooks, and (when enabled) the federation service definition. It keeps
your federation identity and enrollment. `update` refuses a machine with no
existing install; run `install` first. A normal coding session is read-only and
does not download an update.

`setup` only changes the parts you ask it to change. On a machine with no
install, it tells you to run `install` first:

```bash
npx @scchearn/loam setup --federation enable --yes
npx @scchearn/loam setup --federation disable --yes
npx @scchearn/loam doctor
```

Enabling federation installs and starts the connector service. Disabling it
stops and removes that service definition but preserves the identity and
enrollments, so it can be enabled again later.

## Federation

Federation lets people on different computers see current work state in their
own coding tool. Each computer joins a project through a TLS-protected broker.

### Set up a broker

For a self-hosted Mosquitto broker, start with the
[broker deployment overview](./deploy/mqtt-broker/README.md), then follow the
[broker runbook](./deploy/mqtt-broker/RUNBOOK.md). Those documents cover the
server certificate, organization CA, client certificates, enrollment signer,
MQTT configuration and access rules, service setup, backups, certificate
monitoring, and the acceptance checks. The [federation documentation](./docs/federation/)
holds the detailed contracts for identity, enrollment, credentials, and project
membership; this README intentionally does not duplicate them.

### Join a project

The native runtime exposes the federation commands as `loam federation ...`.
The npm command above manages installation; it is not a wrapper for these
runtime commands. The setup output shows the private runtime path, so use that
absolute path when `loam` is not already available in your shell.

With an installed runtime, the federation profile normally uses Loam's
platform configuration directory automatically:

```bash
loam federation connect "$PWD" mqtts://mqtt.example.org:8883
```

On a new machine, give `connect` the enrollment token. A token file avoids
putting the secret in shell history; on an installed machine the configuration
directory is inferred:

```bash
loam federation connect "$PWD" mqtts://mqtt.example.org:8883 \
  --project acme/loam \
  --token-file /secure/path/enrollment-token
```

`--token <value>` and `LOAM_FEDERATION_TOKEN` are also accepted. The broker
address must use `mqtts://host:port`. If `--project` is omitted, the project is
read from the repository's `origin` remote and the organization comes from
`LOAM_FEDERATION_ORG` or the Loam profile configuration. A connect without a
token and without `--global-root` only validates the workspace and broker; it
does not save an enrollment.

Useful entry points:

```bash
# List every project this computer has joined (read-only).
loam federation list

# Check local enrollment and service-manager state (read-only).
loam federation status

# Machine-readable output for either command.
loam federation list --json
loam federation status --json
```

`list` shows the project, workspace, broker, and last successful verification.
It does not contact the broker or claim that a project is currently live. It
returns an empty list, rather than an error, on a new machine. `status` reports
the local enrollment count, service definition, and service-manager state; its
output explicitly does not observe a live broker session. The session context
and connector logs are the surfaces for live or degraded broker state.

Both commands normally resolve Loam's platform configuration directory
themselves: `LOAM_CONFIG_DIR` when set, otherwise
`~/Library/Application Support/loam` on macOS, `%APPDATA%\loam` on Windows,
and `$XDG_CONFIG_HOME/loam` or `~/.config/loam` on Linux and other Unix systems.
Pass `--global-root <path>` only when using a runtime outside the installed
layout or explicitly reading a legacy global-root registry. Add `--json` when
another program will read the result.

Common enrollment refusals are meant to be actionable:

| Message | Meaning | Next step |
| --- | --- | --- |
| `federation_org_unconfigured` | No organization was supplied or configured | Pass `--project org/project`, set `LOAM_FEDERATION_ORG`, or configure the Loam profile |
| `bad-token` | The enrollment signer rejected the token | Obtain the current token from the broker operator and retry |
| `signer-unreachable` | The enrollment signer could not be contacted | Check the signer host, port, DNS, and firewall |
| `signer-timeout` | The signer answered too slowly | Check the signer load/network; raise `LOAM_ENROLL_TIMEOUT_SECONDS` only when a slower link is expected |
| `git-identity-required` | The machine has no Git email for the certificate | Set `git config --global user.email` (and `user.name` for the display name), then retry |

## Optional integrations

The skills work without companion tools. These integrations are off by default
and opt in per install:

```bash
npx @scchearn/loam setup --integration grep   # public code search; queries leave this machine
npx @scchearn/loam setup --integration qmd    # local Markdown search; no egress
npx @scchearn/loam setup --integration hcom   # detect hcom; Loam never installs it
```

- **grep** adds grep.app code search through a remote MCP service. Enable it
  only if sending public-code search queries to that service is acceptable.
- **qmd** installs or uses a Loam-managed local Node tool and local MCP for
  searching Markdown memory. Without it, skills use their built-in fallback.
- **hcom** is detection-only. Loam checks for the native `hcom` binary, never
  installs it, and registers no MCP for it. If it is absent, setup prints the
  install recipe for your platform. Disabling hcom removes Loam's record only;
  it does not touch `~/.hcom` or running hcom processes.

Disable an integration symmetrically:

```bash
npx @scchearn/loam setup --disable-integration qmd
npx @scchearn/loam setup --disable-integration qmd --purge
```

The second command also removes qmd's large derived cache. Loam never removes
a user-owned MCP entry or a tool it did not install.

## Uninstall and `--purge`

Remove the global installation with:

```bash
npx @scchearn/loam uninstall
```

Uninstall removes Loam's global skills, installation metadata, adapters, hooks,
marketplace plugins, and Loam-managed optional tools. It preserves the durable
Loam config directory by default, including the federation identity,
enrollments, project membership records, runtime store, and runtime ledger. A
later install can therefore resume with the same machine identity. User-owned
configuration and MCP entries are preserved.

Use `--purge` only when you also want to destroy that durable config directory:

```bash
npx @scchearn/loam uninstall --purge
```

This deletes the federation certificate and key, enrollments, rosters, member
records, configuration, runtime store, and ledger. The federation identity
cannot be reconstructed; export it first if you may return to the project.
`--yes` accepts the confirmation prompt for automation. `--purge` on an
integration disable is narrower: it removes that integration's derived cache,
not the whole Loam config directory.

## Workflow skills

21 skills, grouped by what they're for:

You can ask for an outcome in ordinary language; the Using skill routes the
request to the matching workflow:

- **Plan and execute:** write a specification, turn it into a plan, start or
  resume the plan, save a checkpoint, or amend an in-progress plan.
- **Goals and decisions:** define a measurable goal or run a structured debate
  before choosing an approach.
- **Project memory:** add a source, ask what the memory says, correct stale
  knowledge, normalize or lint a notes corpus, review open questions, and
  capture useful session learnings.
- **Code understanding:** ingest a codebase into the memory graph and reconcile
  that graph after planned changes.
- **Setup:** scaffold a wiki or initialize an Obsidian vault.

Background memory maintenance is enabled by default. To opt out, set
`LOAM_HARVEST_BACKGROUND=0` or set `background_harvest.enabled: false` in
Loam's `config.json` under the platform configuration directory.

## Skill metrics

Skills load into an agent's context window, so loam keeps each one small. A
skill's short description loads at session startup; its full body loads only when
the skill runs. The table below shows how much space each skill uses against the
[agentskills.io](https://agentskills.io/specification) size budgets.

<!-- BEGIN skill-metrics -->
<!-- Auto-generated by bin/skill-metrics.sh via tiktoken cl100k_base. Do not edit by hand; run `bin/skill-metrics.sh --update` to refresh. -->

| Skill | Desc chars (max 1,024) | Desc tokens (~100) | Body lines (max 500) | Body tokens (< 5,000) |
|-------|---:|---:|---:|---:|
| loam::initializing-vault | 206 | 51 | 9 | 73 |
| loam::scaffolding-wiki | 445 | 90 | 207 | 2,400 |
| loam::adding-to-memory | 592 | 116 | 217 | 2,312 |
| loam::amending-memory | 505 | 114 | 180 | 1,958 |
| loam::auditing-guidance | 410 | 85 | 304 | 3,256 |
| loam::ingesting-codebase | 329 | 76 | 288 | 3,660 |
| loam::learning-from-session | 487 | 101 | 365 | 4,213 |
| loam::linting-memory | 471 | 102 | 316 | 5,000 |
| loam::normalizing-memory | 457 | 101 | 261 | 2,665 |
| loam::querying-memory | 530 | 105 | 206 | 2,025 |
| loam::reviewing-memory | 510 | 113 | 137 | 1,799 |
| loam::syncing-code-graph | 363 | 84 | 223 | 2,780 |
| loam::using | 368 | 77 | 71 | 1,615 |
| loam::amending-plan | 437 | 88 | 273 | 3,059 |
| loam::checkpointing | 365 | 69 | 177 | 2,094 |
| loam::configuring-agents | 459 | 91 | 225 | 3,237 |
| loam::planning | 327 | 62 | 323 | 4,321 |
| loam::resuming | 376 | 77 | 142 | 1,850 |
| loam::setting-goals | 473 | 101 | 184 | 1,883 |
| loam::starting | 166 | 34 | 356 | 4,989 |
| loam::writing-spec | 332 | 66 | 261 | 3,323 |
<!-- END skill-metrics -->

## Documentation

- [Why loam](./WHY.md): the problem loam is designed to solve
- [Federation documentation](./docs/federation/): identity, enrollment,
  credentials, project membership, and instance identity contracts
- [Broker deployment overview](./deploy/mqtt-broker/README.md)
- [Broker deployment runbook](./deploy/mqtt-broker/RUNBOOK.md)
- [Release guide](./docs/RELEASING.md)
- [OpenCode setup notes](./.opencode/INSTALL.md)
- [Codex setup notes](./.codex/INSTALL.md)

## License

MIT, see [LICENSE](./LICENSE).
