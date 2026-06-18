# qmd usage for loam-learning-from-session

This reference applies only when qmd is **ready** (`.wiki-metadata.json` reports `retrieval.status: "ready"`). If qmd is not ready, ignore this file entirely.

## Finding the best destination note

Before proposing a new page, use qmd to find existing pages that may already cover the learning. Use `--files` to get candidate file paths only (no snippets), then Read the actual pages to verify.

1. For each candidate learning, derive 1-3 search terms from the topic.
2. Run `qmd search "<terms>" --files -n 5 -c <collection>` to find existing pages that may already cover the learning.
3. For ambiguous or natural-language queries, use `qmd query "<learning topic>" --files -n 5 -c <collection>`.
4. Strip the `qmd://<collection>/` prefix from file paths to get relative wiki paths.
5. Use scores to prioritize which files to Read first.
6. If qmd finds a clear canonical page, prefer updating that page over creating a new one.
7. If qmd returns no useful results or noisy output, fall back to Grep and Glob.

Read the actual candidate pages before deciding. qmd discovers file paths — Read confirms content.

## Refresh after writes

If this skill wrote to memory (wiki substrate):

1. Run `qmd update -c <collection> 2>/dev/null` to reindex changed files.
2. If refresh fails, report it but do not roll back successful wiki edits.