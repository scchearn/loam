# AGENTS.md — loam development notes

Concise, durable gotchas for agents and humans working in this repo.

## Testing

- `node --test tests/*.mjs` runs the suite. On Windows under Git Bash / MSYS,
  `tests/package.test.mjs` and `tests/compatibility.test.mjs` fail because they
  shell out to `tar -xzf <C:\...>.tgz`, and MSYS/GNU `tar` reads the `C:` drive
  prefix as a remote host (`tar: Cannot connect to C: resolve failed`). Run the
  packaging tests on Linux/CI, or with a non-MSYS `tar`; the rest of the suite
  passes on Windows.

## Install / doctor

- Install metadata lives at `<globalRoot>/install.json` and was introduced in
  **v0.8.4**. Upgrading from an earlier build leaves no metadata, so
  `loam doctor` reports `not ready` (`install_metadata_missing` across install
  metadata, native runtime, and every harness). The fix is `loam setup` — it
  writes the metadata and re-wires the harness integrations.
- The loam version shown in the session-start hook can be **stale**: it may come
  from an older plugin still sitting in the harness plugin cache
  (`~/.claude/plugins/cache/loam/loam/<oldver>/`), not from the installed
  runtime. The authoritative version is `loam --version`
  (or `node bin/loam.mjs --version`).
