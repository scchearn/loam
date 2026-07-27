# Installing loam for OpenCode

## Prerequisites

- [OpenCode.ai](https://opencode.ai)
- Node.js/npm

## Installation

Run the global setup wizard from any workspace:

```bash
npx @scchearn/loam setup
```

Setup installs the canonical global skills through Skills CLI, verifies the
exact native runtime, and configures the user-level OpenCode adapter. It does
not require a repository clone, write project configuration, or modify `PATH`.
Use `--yes` for automation and `--dry-run` to preview without mutation or
download.

## Verify

Start a fresh OpenCode session and ask: "Do you have loam?" The first context
should include `You have loam (v<plugin-version>).` and a real workspace-state
block when the native runtime is ready. If the runtime is unavailable, the
context reports `npx @scchearn/loam setup` rather than synthetic state.

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

## Troubleshooting

1. Rerun `npx @scchearn/loam setup --dry-run` to inspect readiness and paths.
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
