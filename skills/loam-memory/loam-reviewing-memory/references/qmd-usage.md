# qmd usage for loam::reviewing-memory

This reference applies only when qmd is **ready** (`.wiki-metadata.json` reports `retrieval.status: "ready"`). If qmd is not ready, ignore this file entirely.

The active qmd collection should exclude archived pages with `ignore: [".archive/**"]` in its per-collection config. If archived files appear in results, report the qmd config drift and treat those paths as historical context only.

## Expanding from discovered issues

In this skill, qmd is **secondary only**. Do not let qmd drive the primary scan. The scan uses Grep and Glob. Use qmd only to expand from a discovered issue into nearby related notes.

Use `--files` to get candidate file paths only (no snippets), then Read the actual pages to verify.

For each significant signal found during the scan:

1. Run `qmd search "<topic or entity from the signal>" --files -n 3 -c <collection>` to find nearby notes that may amplify or share the same issue.
2. Strip the `qmd://<collection>/` prefix from file paths to get relative wiki paths.
3. Ignore `.archive/` paths for active issue expansion; they are historical.
4. Read the candidate files to confirm they are actually related.
5. If they are related, include them in the classification step.

Do not let qmd replace the primary scan. Use it only to widen the net around specific issues already discovered.

Always verify candidates by Reading the actual wiki files. qmd discovers file paths — Read confirms content.

If any qmd command fails or returns stale results, treat qmd as degraded and skip qmd entirely for the rest of this session.
