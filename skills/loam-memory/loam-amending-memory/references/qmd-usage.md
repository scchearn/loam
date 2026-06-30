# qmd usage for loam::amending-memory

This reference applies only when qmd is **ready** (`.wiki-metadata.json` reports `retrieval.status: "ready"`). If qmd is not ready, ignore this file entirely.

The active qmd collection should exclude archived pages with `ignore: [".archive/**"]` in its per-collection config. If archived files appear in results, report the qmd config drift and fall back to direct wiki-file reads.

## Finding affected pages

Use qmd to broaden affected-page discovery beyond exact string matches. Use `--files` to get candidate file paths only (no snippets), then Read the actual pages to verify.

1. Run `qmd search "<relevant terms>" --files -n 8 -c <collection>` to find notes that mention the affected topic.
2. Run `qmd query "<what changed and why>" --files -n 8 -c <collection>` to find notes likely influenced by the stale or wrong claim.
3. Strip the `qmd://<collection>/` prefix from file paths to get relative wiki paths.
4. Use scores to prioritize which files to Read first.
5. This helps discover pages that mention the affected topic but do not contain the exact stale wording.
6. Read each candidate page to confirm it actually contains the issue before changing it.
7. Ignore any returned `.archive/` paths; they are historical, not active memory.
8. If qmd results are noisy or irrelevant, ignore them and rely on Grep and Glob.

Always verify candidates by Reading the actual wiki files. qmd discovers file paths — Read confirms content.

## Refresh after writes

If this skill wrote to memory (wiki substrate):

1. Run `qmd update -c <collection> 2>/dev/null` to reindex changed files.
2. If refresh fails, report it but do not roll back successful wiki edits.
