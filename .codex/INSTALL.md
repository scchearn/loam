# Installing loam for Codex

## Installation

Run the global setup wizard:

```bash
npx @scchearn/loam setup
```

Setup installs global skills through Skills CLI and verifies the private native
runtime. Codex can discover the global skills under `~/.agents/skills/`. Setup
also installs user-scope SessionStart and Stop hooks; it does not write
repository hooks or Codex TOML.

For an auto-updating SessionStart adapter, install the thin marketplace plugin:

```bash
codex plugin marketplace add scchearn/loam
codex plugin add loam@loam
npx @scchearn/loam setup
```

Codex refreshes configured Git marketplaces on startup. Setup detects the
installed and enabled plugin, removes only its direct SessionStart fallback,
and retains the Stop hook used by background ingestion. The marketplace plugin
contains no skills; canonical skill content remains under
`~/.agents/skills/`.

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

The marketplace or setup fallback injects `loam::using`, the absolute native
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
