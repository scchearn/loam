# Installing loam for Codex

## Installation

Run the global setup wizard:

```bash
npx @scchearn/loam setup
```

Setup installs global skills through Skills CLI and verifies the private native
runtime. Codex can discover the global skills under `~/.agents/skills/`, but
Loam does not claim full session-start integration without a shipped Codex
adapter.

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

Codex has no Loam session-start adapter in this release. Invoke `loam::using`
at session start or whenever a Loam task appears; runtime-dependent skills use
the injected absolute native runtime command and stop with setup guidance when
it is unavailable.
