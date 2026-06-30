# qmd usage for loam::adding-to-memory

This reference applies only when qmd is **ready** (`.wiki-metadata.json` reports `retrieval.status: "ready"`). If qmd is not ready, ignore this file entirely.

The active qmd collection should exclude archived pages with `ignore: [".archive/**"]` in its per-collection config. If archived files appear in results, report the qmd config drift and fall back to direct wiki-file reads.

## Finding existing related notes

Before editing, use qmd to find existing wiki pages that may need updates or could create duplicates. Use `--files` to get candidate file paths only (no snippets), then Read the actual pages to verify.

1. Derive 2-4 search terms from the source content or conversation topic.
2. Run `qmd search "<terms>" --files -n 8 -c <collection>` to find existing topic, entity, concept, and analysis notes.
3. For ambiguous or natural-language queries, use `qmd query "<question>" --files -n 8 -c <collection>`.
4. Strip the `qmd://<collection>/` prefix from file paths to get relative wiki paths.
5. Use scores to prioritize which files to Read first.
6. Read the actual candidate pages before editing.
7. If qmd returns no useful results or noisy output, fall back to Grep and Glob.
8. Ignore any returned `.archive/` paths; they are historical, not active memory.

Always verify candidates by Reading the actual wiki files. qmd discovers file paths — Read confirms content.

## Refresh after writes

If this skill wrote to memory (wiki substrate):

1. Run `qmd update -c <collection> 2>/dev/null` to reindex changed files.
2. If refresh fails, report it but do not roll back successful wiki edits.
