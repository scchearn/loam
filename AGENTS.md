# Agent Guidance

- Work from the loam repo root; task paths are relative unless stated otherwise.
- Source layout: `skills/loam-work`, `skills/loam-memory`, `skills/loam-ground`, and router `skills/loam-using`.
- Plugin install metadata lives in `.claude-plugin/plugin.json`; each skill has one `SKILL.md` with `metadata.version`.
- Plugin bootstrap files live in `.opencode/`, `hooks/`, `.codex-plugin/`, `.cursor-plugin/`, `package.json`, and `.claude-plugin/marketplace.json`.
- Plugin injection reads skill content from the `npx skills` install path (`<cwd>/.agents/skills/loam-using/SKILL.md` → `~/.agents/skills/loam-using/SKILL.md`), never from the plugin repo. `npx skills add` is the single source of truth.
- Bump version in **both** `package.json` **and** `.claude-plugin/marketplace.json` when releasing. Claude Code reads version from `marketplace.json`; OpenCode reads from `package.json`. Missing one → harness reports stale version after `/reload-plugins`.
- After plugin bootstrap or install-doc changes, verify `hooks/session-start` emits valid JSON in all 3 formats: `CLAUDE_PLUGIN_ROOT=… bash hooks/session-start`, `CURSOR_PLUGIN_ROOT=… bash hooks/session-start`, `bash hooks/session-start`.
- Run `bash bin/check-curation.sh` after repo-hygiene or skill metadata changes.
- Run `skills/loam-work/loam-checkpointing/scripts/checkpoint-contract-test` after checkpointing changes.
- Run `skills/loam-memory/loam-ingesting-codebase/scripts/codegraph-contract-test` after codegraph changes.
- To install the local hook, create `.git/hooks/pre-commit` that runs the three commands above, then `chmod +x .git/hooks/pre-commit`.
- Windows hook users need Git Bash or WSL; no PowerShell twin is shipped.
- No new dependencies for repo hygiene; use Bash, standard Unix tools, and git.
- Skills locate sibling scripts via `${LOAM_SKILL_DIR:-${CLAUDE_SKILL_DIR}}`; prefer `LOAM_SKILL_DIR` when adding new references. `CLAUDE_SKILL_DIR` is the Claude Code fallback only.
- Do not byte-sync same-named skill reference files unless they are explicitly marked shared.
- Durable gotchas belong in the tripped skill as `Trigger → Mistake → Fix`; environment-specific incidents stay operational.
- Keep guidance edits concise; avoid session reports, changelog ceremony, and speculative scaffolding.
