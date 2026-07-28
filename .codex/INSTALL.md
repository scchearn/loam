# Installing loam for Codex

## Installation

Run the global setup wizard:

```bash
npx @scchearn/loam setup
```

Setup installs global skills through Skills CLI and verifies the private native
runtime. Codex can discover the global skills under `~/.agents/skills/`. When
Codex is detected, setup offers to add the Loam marketplace and install its thin
plugin through the native Codex CLI. That plugin owns both `SessionStart` and
`Stop`; setup removes legacy Loam user hooks and never creates new ones.

The plugin contains no skills. Canonical skill content remains under
`~/.agents/skills/`. If the plugin is installed before setup, it stays
network-free and reports `npx @scchearn/loam setup` until the shared core exists.

Use `--yes` for automation or `--dry-run` to preview without mutation or
download. The runtime remains outside `PATH`.

## Verify and update

```bash
npx skills list --global
npx skills update --global
npx @scchearn/loam setup
```

The existing clone plus symlink path is a repository-development or migration
compatibility option, not the normal installation path. It must not create a
project-local Loam runtime or skill copy.

## Session use

The marketplace plugin injects `loam::using`, the absolute native
runtime command, and current workspace state at SessionStart. If the runtime is
unavailable, it remains network-free and reports `npx @scchearn/loam setup`.

## Background ingestion

Background code ingestion is enabled by default. To opt out, set
`LOAM_INGEST_BACKGROUND=0` as the unconditional kill switch, or set
`background_ingest.enabled` to `false` in `~/.agents/loam/config.json`.
`LOAM_INGEST_BACKGROUND=1` explicitly overrides a disabled config. The adapter
reads only the documented stop payload (`cwd`, `session_id`, and
`stop_hook_active`), closes handoff stdin, and writes `{}` to stdout. It never
reads a transcript.

Before launching, the worker takes a per-workspace lease, runs full native
state, requires `code_ingest_pending`, resolves installed exclusions, and
computes a complete actionable fingerprint. Live or uncertain workers,
missing exclusions, unreadable/racing files, and unknown process identity fail
closed. Check `ingest-status --workspace <path> --json` through the installed
integration. It never modifies source files, commits, or pushes.
