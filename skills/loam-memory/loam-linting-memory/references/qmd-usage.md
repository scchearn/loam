# qmd usage for loam::linting-memory

This reference applies only when qmd is **ready** (`.wiki-metadata.json` reports `retrieval.status: "ready"`). If qmd is not ready, ignore this file entirely.

## Expanding structural fixes

In this skill, qmd is **secondary only**. Structural checks must remain Glob- and Grep-led. Use qmd only to find related-note neighborhoods when a structural fix might need reciprocal links or nearby canonical notes.

Use `--files` to get candidate file paths only (no snippets), then Read the actual pages to verify.

1. For orphan pages or missing cross-links, run `qmd search "<topic or entity>" --files -n 3 -c <collection>` to find notes that should link to or from the affected page.
2. For concepts mentioned repeatedly but lacking a page, run `qmd search "<concept>" --files -n 5 -c <collection>` to find the pages that mention the concept and confirm it deserves a dedicated note.
3. Strip the `qmd://<collection>/` prefix from file paths to get relative wiki paths.
4. Read the actual candidate files before proposing any fix.

Do not let qmd replace inventory checks, orphan checks, unresolved wikilink checks, or index.md integrity checks. Those remain Glob- and Grep-led.

Always verify candidates by Reading the actual wiki files. qmd discovers file paths — Read confirms content.

If any qmd command fails or returns stale results, treat qmd as degraded and skip qmd entirely for the rest of this session.

## Refresh after writes

If this skill wrote to the wiki:

1. Run `qmd update -c <collection> 2>/dev/null` to reindex changed files.
2. If refresh fails, report it but do not roll back successful wiki edits.