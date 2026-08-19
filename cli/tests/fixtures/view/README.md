# View fixture matrix

Miniature workspaces for the Loam View schema/validator/producer test
suites (Rust and Node). Each condition below is actually present in the
fixture's files, not just implied by the directory name.

| Fixture | Condition |
| --- | --- |
| `sparse/` | No `wiki/`, `goals/`, `specs/`, or `plans/` at all -- only `AGENTS.md` and a source file. Expect `status: not-configured`. |
| `healthy/` | Wiki index + topic + code page (hash-matched to its source, i.e. current), goal, spec, plan, and checkpoint, all cross-linked. Expect `status: ready` with every capability `ready`. |
| `code-drift/` | `wiki/code/stale-module.md`'s `content_hash` front matter does not match `src/stale-module.js` (stale); `src/new-module.js` has no code page (new); `wiki/code/orphan-module.md` points at a `source_path` that does not exist (orphan). |
| `broken-links/` | One page links `[[does-not-exist]]` (broken), `[[overview]]` (ambiguous -- matches both `wiki/topics/overview.md` and `wiki/entities/overview.md`), and `[[setup]]` (case-drift -- target is `wiki/topics/Setup.md`). |
| `malformed/` | `wiki/topics/bad-frontmatter.md` has unterminated YAML in its front matter; `goals/bad-timestamps.md` has an unparseable `created_at` and an invalid `updated_at`. Both artifacts must still appear in inventory with `parse_errors`, not be dropped. |
| `degraded/` | `wiki/code/corrupt.md` contains invalid UTF-8 bytes. The rest of the workspace (`wiki/index.md`, `wiki/topics/status.md`) stays readable, so a probe reading that one file should fail/degrade without corrupting the rest of the snapshot. |
