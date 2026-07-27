# Installing loam for Codex

## Installation

Run the global setup wizard:

```bash
npx @scchearn/loam setup
```

Setup installs global skills through Skills CLI and verifies the private native
runtime. Codex can discover the global skills under `~/.agents/skills/`. Setup
also installs a user-scope Stop hook for optional background ingestion; it does
not write repository hooks or Codex TOML.

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

Invoke `loam::using` at session start or whenever a Loam task appears;
runtime-dependent skills use the injected absolute native runtime command and
stop with setup guidance when it is unavailable.

## Optional background ingestion

Background code ingestion is disabled by default. Set
`LOAM_INGEST_BACKGROUND=1` or `background_ingest.enabled` in
`~/.agents/loam/config.json` to allow a bounded same-harness worker at Stop.
`LOAM_INGEST_BACKGROUND=0` always disables it. The adapter reads only the
documented stop payload (`cwd`, `session_id`, and `stop_hook_active`), closes
handoff stdin, and writes `{}` to stdout. It never reads a transcript.

Before launching, the worker takes a per-workspace lease, runs full native
state, requires `code_ingest_pending`, resolves installed exclusions, and
computes a complete actionable fingerprint. Live or uncertain workers,
missing exclusions, unreadable/racing files, and unknown process identity fail
closed. Check `ingest-status --workspace <path> --json` through the installed
integration. It never modifies source files, commits, or pushes.
