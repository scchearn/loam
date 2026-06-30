# Agent Guidance

- Work from the loam repo root; task paths are relative unless stated otherwise.
- Source layout: `skills/loam-work`, `skills/loam-memory`, `skills/loam-ground`, and router `skills/loam-using`.
- Plugin install metadata lives in `.claude-plugin/plugin.json`; each skill has one `SKILL.md` with `metadata.version`.
- Run `bash bin/check-curation.sh` after repo-hygiene or skill metadata changes.
- Run `skills/loam-work/loam-checkpointing/scripts/checkpoint-contract-test` after checkpointing changes.
- Run `skills/loam-memory/loam-ingesting-codebase/scripts/codegraph-contract-test` after codegraph changes.
- To install the local hook, create `.git/hooks/pre-commit` that runs the three commands above, then `chmod +x .git/hooks/pre-commit`.
- Windows hook users need Git Bash or WSL; no PowerShell twin is shipped.
- No new dependencies for repo hygiene; use Bash, standard Unix tools, and git.
- Do not byte-sync same-named skill reference files unless they are explicitly marked shared.
- Durable gotchas belong in the tripped skill as `Trigger → Mistake → Fix`; environment-specific incidents stay operational.
- Keep guidance edits concise; avoid session reports, changelog ceremony, and speculative scaffolding.
