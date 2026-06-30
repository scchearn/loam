# qmd usage for loam::scaffolding-wiki

This reference applies only when qmd is being set up during a wiki build. If qmd is not ready, ignore this file entirely.

Note: consuming wiki skills read `.wiki-metadata.json` first as the fast path to determine qmd readiness. The metadata file you create here is their primary check — they only fall back to `which qmd` + `qmd collection list` when metadata is missing or stale.

## Installation check

1. Run `which qmd 2>/dev/null`. If it fails, qmd is not installed.
2. If qmd is not installed, tell the user that setup requires `npm install -g @tobilu/qmd` followed by collection setup. Ask whether to proceed now or defer.

## Collection registration

If qmd is installed:

1. Run `qmd collection list 2>/dev/null` to see existing collections.
2. For each collection, run `qmd collection show <name> 2>/dev/null` and compare the `Path:` field to the resolved wiki root using absolute path equality.
3. If a collection path already matches the wiki root, confirm readiness.
4. If no collection matches, offer to create one. Recommended name: `<workspace-slug>-<wiki-root-name>`.

## Create and index

If the user wants a new collection:

1. Choose a collection name. Recommended default: `<workspace-slug>-<wiki-root-name>`.
2. Run `qmd collection add <wiki root> --name <collection-name>`.
3. Add `ignore: [".archive/**"]` to the per-collection qmd config so archived pages stay out of retrieval.
4. Run `qmd update -c <collection-name>` to index the wiki files.
5. Run `qmd embed` if vector search is desired (requires a local embedding model).

Example collection config:

```yaml
collections:
  <collection-name>:
    path: <wiki root>
    pattern: "**/*.md"
    ignore:
      - ".archive/**"
```

## Record collection details

After the collection is ready:

1. Create or update `<wiki root>/.wiki-metadata.json` with a `retrieval` section:

```json
{
  "retrieval": {
    "tool": "qmd",
    "collection_name": "<collection-name>",
    "collection_path": "<absolute-path-to-wiki-root>",
    "status": "ready",
    "last_verified": "<YYYY-MM-DD>"
  }
}
```

2. Add a `## Retrieval Tooling` section to `<wiki root>/SCHEMA.md`:

```md
## Retrieval Tooling

This wiki optionally uses qmd for candidate discovery during wiki skill operations. qmd accelerates finding relevant pages but is never the authority layer — the wiki files remain authoritative.

- Configuration: `.wiki-metadata.json` at the wiki root
- When qmd is ready, wiki skills use it for candidate discovery, then verify against real wiki files
- When qmd is unavailable, unmapped, or degraded, all skills fall back to Grep and Glob without breaking
- SCHEMA.md and index.md are always direct reads
- After wiki edits, skills refresh qmd if the collection is ready
- `log.md` is deprioritized in factual retrieval; it records maintenance history, not primary evidence. It rotates to `log-archive/` at 500 lines, so the active file stays small.
- The qmd collection config excludes archived pages with `ignore: [".archive/**"]`
```

3. Append a setup entry to `<wiki root>/log.md`:

```md
## [YYYY-MM-DD] build | qmd retrieval setup — collection <collection-name>
```

## If setup fails

If collection creation or indexing fails:

1. The wiki remains fully functional in fallback-only mode.
2. Do not roll back the wiki scaffold.
3. Report the failure in the build report.
4. Set the `retrieval.status` in `.wiki-metadata.json` to `"degraded"` or `"unmapped"` as appropriate.

## Consuming skills use `--files`

All consuming wiki skills use `qmd search` and `qmd query` with the `--files` flag for candidate discovery. This returns only file paths and scores (no content snippets), keeping token usage minimal. The skills then Read the actual wiki files to verify candidates.
