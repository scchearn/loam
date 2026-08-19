# Installing loam for OpenCode

## Prerequisites

- [OpenCode.ai](https://opencode.ai)
- Node.js/npm

## Installation

Run the global setup wizard from any workspace:

```bash
npx @scchearn/loam setup
```

Setup installs the canonical universal skills through Skills CLI, verifies the
exact native runtime, and configures the user-level OpenCode adapter. It does
not require a repository clone, write project configuration, or modify `PATH`.
Use `--yes` for automation and `--dry-run` to preview without mutation or
download.

## Verify

Start a fresh OpenCode session and ask: "Do you have loam?" The first context
should include `You have loam (v<plugin-version>).` and a real workspace-state
block when the native runtime is ready. If the runtime is unavailable, the
context reports `npx @scchearn/loam setup` rather than synthetic state.

The plugin invokes the absolute private native runtime directly; setup writes
that path in when the plugin is staged and rewrites it on update. There is no
shared Node integration in the session path.

**OpenCode collaboration compatibility is advertised — observed on OpenCode
1.18.15.** The plugin's `experimental.chat.messages.transform` prepends the
context to the first user message of a session, so it reaches the model without
being written to the session store. That is worth knowing if you go looking for
it: `opencode export` will not show the injected text, because the transform
runs on the outbound message list rather than on the persisted session. Ask the
model instead — that is the surface the context is delivered to.

## Updating

Update global skill content through Skills CLI, then reconcile the runtime and
adapter with setup:

```bash
npx skills update --global
npx @scchearn/loam setup
```

The existing clone plus direct `.opencode/plugins/loam.js` path remains a
migration compatibility path. If it is retained, update it with `git pull` and
restart OpenCode; it does not poll for updates at session start.

## Background ingestion

Background code ingestion is enabled by default. To opt out, set
`LOAM_INGEST_BACKGROUND=0` as the unconditional kill switch, or set
`background_ingest.enabled` to `false` in `~/.agents/loam/config.json`.
`LOAM_INGEST_BACKGROUND=1` explicitly overrides a disabled config. The adapter
acts only on `session.idle`, creates a child session for the existing
`loam::ingesting-codebase` skill, and ignores child idle events.

The worker takes a per-workspace lease, requires full native state with a
`code_ingest_pending` hint, resolves installed exclusions, and computes a
complete actionable fingerprint before spending model tokens. Missing or
unreadable exclusions, live/uncertain workers, and race/deadline fingerprints
fail closed. Use `ingest-status --workspace <path> --json` through the
installed integration to inspect the lease, intent, fingerprint, and last
outcome. The worker never edits source files, commits, or pushes.

## Background session harvest

Background session-learning harvest is enabled by default. On every turn end
the hook measures what has been said since the last harvest; when enough new
conversation exists, a detached agent reviews the window and routes durable
learnings through `loam::learning-from-session`. To opt out, set
`LOAM_HARVEST_BACKGROUND=0` as the unconditional kill switch, or set
`background_harvest.enabled` to `false` in `~/.agents/loam/config.json`.

Each session keeps its own cursor and harvests into its workspace's memory.
Harvest shares the per-workspace memory-writer lease with code ingestion and
never runs while another worker holds it. The harvest agent's own turn ends
are never re-harvested. Check `harvest-status --workspace <path> --json`
through the installed integration to inspect per-session cursors, the wiki
cache, last run, and the shared lease.

## Troubleshooting

1. Rerun `npx @scchearn/loam install --dry-run` to inspect readiness and paths.
2. Confirm global skill inventory with `npx skills list --global`.
3. Confirm the user-level OpenCode plugin path is writable and restart OpenCode.
4. If an existing clone is incomplete, remove its registration after setup or
   follow the setup recovery message.

## Tool mapping

When skills reference Claude Code tools:

- `TodoWrite` → `todowrite`
- `Task` with subagents → `@mention` syntax
- `Skill` tool → OpenCode's native `skill` tool
- File operations → your native tools

## Optional integrations

Loam skills are better with companion tools, but never require them (soft
dependency — a skill degrades gracefully when a tool is absent). Enable them
per install with the configurator, off by default:

```bash
npx @scchearn/loam setup --integration grep    # grep.app code search (remote MCP; queries egress to a public-repo index)
npx @scchearn/loam setup --integration qmd     # QMD markdown search (local Node tool + local MCP; no egress)
npx @scchearn/loam setup --integration hcom    # hcom agent messaging (detected, never installed; no MCP)
```

`setup` installs any needed tool into a loam-managed prefix, verifies it, then
registers the MCP into each configured harness using the tool's absolute path.
Disable is symmetric and complete:

```bash
npx @scchearn/loam setup --disable-integration qmd            # deregister everywhere + remove the loam-managed tool
npx @scchearn/loam setup --disable-integration qmd --purge    # also remove large derived caches (e.g. QMD's ~2–3GB model cache)
```

hcom is the exception to the install half: it ships as a native binary through
its own installers, so Loam looks for it (on `PATH`, under `HCOM_INSTALL_DIR`,
then in `~/.local/bin`), registers no MCP for it, and never installs it. When it
is missing, enabling prints the recipe for your platform:

```bash
brew install aannoo/hcom/hcom                                                              # macOS (Homebrew)
curl -fsSL https://github.com/aannoo/hcom/releases/latest/download/hcom-installer.sh | sh   # macOS, Linux, Termux, WSL
irm https://github.com/aannoo/hcom/releases/latest/download/hcom-installer.ps1 | iex        # Windows (PowerShell)
uv tool install hcom                                                                       # or: pip install hcom
```

Once installed, each session's opening briefing carries an `hcom: ready` line
and the skills that can delegate use it; without it they take their normal
single-session path. Disabling hcom removes Loam's record of it and nothing
else — `~/.hcom` and any running agents are never touched, not even with
`--purge`.

Loam never installs a tool or registers an MCP you did not select, and never
removes a user-owned MCP entry or a tool it did not install. `doctor` reports
per-integration state without failing.
