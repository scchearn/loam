---
name: loam::ingesting-codebase
description: "Ingest a codebase into memory as code pages connected by wiki links. Walks the tree, classifies each code file by role, applies a role template, and writes a code page per meaningful unit under <wiki root>/code/. Resumable: skips files already ingested and current. Not for prose documents; use /loam::adding-to-memory for those."
allowed-tools: Read Glob Grep Write Edit Bash
metadata:
  version: "1.6.0"
  author: scchearn
  argument-hint: <codebase root path>
---

You are a senior engineer and disciplined wiki maintainer ingesting a codebase into the persistent wiki so future sessions inherit a compressed semantic map of the code instead of re-reading source.

The codebase becomes a graph of code pages under `<wiki root>/code/`, connected by wiki links and catalogued by the generated `<wiki root>/code/_index.md` hub. The wiki *is* the code graph — no separate database, no parallel representation.

## Input

The codebase root is: $ARGUMENTS

---

## Step 1 — Resolve wiki, codebase, and build the index

### Wiki resolution and qmd readiness

First reuse the injected `Workspace state` under the reuse contract in `loam::using`. Do not rerun the integration when that block supplies wiki existence/root, qmd readiness, collection, and hints; Step 2 computes the authoritative codegraph diff.

If the injected state cannot be reused, refresh native state through the injected absolute integration path:

```bash
<native-runtime-command> state --fast "$(pwd)"
```

If `exists` is false, stop and recommend:

```text
/loam::scaffolding-wiki <topic or wiki goal>
```

If the native runtime reports unavailable or does not provide real state, stop and recommend `npx @scchearn/loam setup`; do not fabricate state or use a project-local fallback. Use `wiki_root` from the resolved state as the wiki root. Do not substitute the codebase root, workspace root, or parent directory. If `qmd_ready` is true, note the `collection` name for later refresh (`qmd update -c <collection>`). The skill works fully without qmd; it only accelerates post-ingest discovery.

A `code_ingest_pending` hint, when present, previews the work set; Step 2 remains authoritative when fast injected state omits that hint.

### Codebase resolution

Resolve `$ARGUMENTS` to an absolute path. If it does not exist or is not a directory, stop and report the error. Treat it as the codebase root for `source_path` front-matter values (paths are relative to this root).

If `$ARGUMENTS` is empty, default the codebase root to `$(pwd)`. Do not ask for scope confirmation just because the workspace contains multiple subprojects; the caller can pass a narrower path when they want one.

### Build the existing index

Run the index subcommand to get every code-ingested page already in the wiki:

```bash
<native-runtime-command> codegraph index <wiki-root> --codebase-root <codebase-root>
```

Parse the JSON output into an in-memory map: `{source_path → {slug, ingested_at, mtime, exists}}`. Pages without `source_path:` front matter are prose entity pages and are skipped silently. This map is the set of already-ingested code nodes. The index scans both `code/` and `entities/` (for legacy stranded `source_path:` pages during the transition to the `code/` namespace).

If the native runtime command fails or reports an unavailable runtime, stop and report the setup recovery command. Do not fall back to a project-local launcher.

If the native codegraph command reports `wiki root contract not found` or `did you mean: .../wiki`, stop and rerun it with the actual `wiki_root`. Do not proceed from an empty index caused by a bad wiki-root path.

### Optional preflight summary

For a quick size check before ingesting, run:

```bash
<native-runtime-command> codegraph walk <codebase-root> --summary \
  --exclusions "${LOAM_SKILL_DIR:-${CLAUDE_SKILL_DIR}}/references/ingestion-exclusions.md"
```

This reports candidate counts by extension plus excluded low-signal counts (`pattern`, `gitignore`, `empty`, `large`, `generated_header`, `binary`). Use it to decide whether the run is likely to hit the cap; it is not required for correctness.

If the native runtime command fails or reports an unavailable runtime, stop and report the setup recovery command. Do not fall back to a project-local launcher.

---

## Step 2 — Diff to find the work set

Default mode is diff-guided ingest: process `new` and `stale` entries from `codegraph diff`. Do not ask the user to choose between ingest and re-ingest when the diff already classifies each file.

Run the diff subcommand to get the files that need ingestion or re-summarization:

```bash
<native-runtime-command> codegraph diff <codebase-root> \
  --exclusions "${LOAM_SKILL_DIR:-${CLAUDE_SKILL_DIR}}/references/ingestion-exclusions.md"
```

Parse the JSON output: `{path, mtime, reason, slug?}` where `mtime` is the source file's Unix epoch mtime and `reason` is `new` or `stale`. Legacy pages with date-only `ingested_at` are stale once so they migrate to epoch precision.

- **`reason: "new"`** → new ingest (create code page)
- **`reason: "stale"`** → re-summarize (overwrite the same code page; `slug` is provided)
- current files are omitted

**Cap the work set at 200 files.** If more remain, stop after 200 and report the pending count. The user re-invokes to continue; resumability (Step 1's index rebuild) means the next run picks up exactly where this one stopped.

If the work set is empty, skip page generation but still reconcile the hub in Step 6 before reporting fully current.

If the work set is non-empty, never report the codebase as fully ingested. Process the capped work set, or if a remaining file appears too low-signal to ingest, report it as a concrete decision item with its path and reason. Do not claim it will be auto-skipped unless `codegraph diff` no longer returns it.

Low-signal files are filtered before this step by `codegraph walk/diff`: files excluded by patterns or `.gitignore`, zero-byte files, whitespace-only files, binary/non-text files, likely generated files by header, and files over the default large-file guard. Do not classify or summarize filtered files.

---

## Step 3 — Classify role and load template

Before reading the file, classify its role from the path, filename, and — if needed — a quick scan of the first ~30 lines. Read `references/role-classification.md` for the full rubric. The five roles:

1. **service** — files exposing an API, endpoint, route, controller, or handler
2. **utility** — pure functions, helpers, business logic
3. **type** — type definitions, interfaces, DB models, classes, entities, schemas
4. **config** — semantic config and constants worth ingesting
5. **test** — test/spec files

One role per file. When ambiguous, pick the role matching the file's primary export or primary intent.

Load the matching role template from `references/templates/role-<role>.md`.

---

## Step 4 — Read, extract, and generate the code page

### Read the file

Read the file in full. Distinguish: the primary export or symbol (becomes the page name), its signature/shape, what it does (intent, not full implementation), what it depends on (imported names, called functions), and edge cases or failure modes.

### Derive the slug

Derive the code-page slug from the file's primary export name or primary symbol, lowercased and kebab-cased. If no clear primary export, use the filename sans extension, kebab-cased. Examples:

- `src/auth/validateToken.ts` exporting `validateToken` → `validate-token`
- `src/utils/helpers.ts` exporting multiple utilities → `helpers` (one page per file)
- `src/models/User.ts` exporting `User` class → `user`

### Resolve dependencies to wiki links

For each dependency name found in the file:

1. Check the in-memory index (built in Step 1, augmented with new ingests this run) for a matching `source_path` or slug.
2. If it resolves to a code page in the wiki → render as `[[slug]]`.
3. If it does not resolve → flag as an external dependency (plain text, not a wikilink). Note it in the `## Dependencies` section with an `(external)` marker.

Do not create broken wikilinks. Unresolved names stay as text.

### Generate the code page

Fill the loaded role template with the extracted fields. The resulting markdown is the code page. Include front matter:

```yaml
---
source_path: <relative-path-from-codebase-root>
ingested_at: <source-file-mtime-epoch>
source_size: <bytes>
content_hash: <sha256-hex>
---
```

Use the source file's Unix epoch mtime for `ingested_at`. For re-summarized files, update `ingested_at` to the file's current mtime, not today's date. Populate `source_size` with the file's byte size and `content_hash` with the lowercase SHA-256 hash (from `sha256sum` on POSIX, lowercase-normalized `Get-FileHash` on Windows).

Legacy pages (written before this version) may have only `source_path` and `ingested_at`. Treat missing `source_size` and `content_hash` as legacy, not errors. Populate all three fields on write. Do not backfill all pages in a single pass — let it happen incrementally as files drift and get re-summarized.

### Write the page

Write to `<wiki root>/code/<slug>.md`. Overwrite if re-summarizing. Create if new.

### Update the in-memory index

Add or update the entry in the in-memory index so subsequent files in this run can link to it:

```
<source_path> → {slug, ingested_at: <file mtime epoch>, source_size: <file size>, content_hash: <file hash>, mtime: <file mtime epoch>}
```

---

## Step 5 — Wire reciprocal links

After all files in the work set are written, wire reciprocal links. For each newly written or updated code page:

1. Read the `## Dependencies` or `## Relations` section to find which code pages it links to.
2. For each linked page, append a reciprocal backlink to its `## Callers` or `## Mentioned in` section if the relationship is materially useful and the link is not already present.
3. Prefer small targeted edits (append one line) over rewrites.
4. Skip reciprocal links to external dependencies (they have no page to link back from).

Do not re-write pages whose links are unchanged. Only touch pages that gained or lost a dependency link from this run.

---

## Step 6 — Rebuild the code hub and update the log

### Rebuild `<wiki root>/code/_index.md`

After every run, rebuild `<wiki root>/code/_index.md` from every active `code/*.md` page except itself, sorted by slug:

```md
# Code graph

> Generated by loam from active code pages. Do not edit by hand.

## Code pages

- [[<slug>]] — <one-line summary>
```

Use each page's `## Summary`, falling back to its source path. The hub carries no `source_path:` and is not a code node.

### Keep one root entry point

Keep exactly one `[[code/_index|Code graph]]` entry under root `index.md` → `## Code`; create the group if needed and remove direct ordinary code-page entries.

Do not add individual code pages to root `index.md`.

### Append to log.md

```md
## [YYYY-MM-DD] ingest-code | <codebase root basename>
```

Capture: root path, files ingested (count), files re-summarized (count), skipped/current files if known from the preflight summary, external dependencies flagged, pending files (count if the cap was hit), any open questions.

### Refresh qmd

If qmd was ready and you wrote to the wiki, run `qmd update -c <collection> 2>/dev/null`. If refresh fails, report it but do not roll back wiki edits.

---

## Step 7 — Report back

```md
Codebase ingested from <codebase root>

### Mode
- ingest | re-ingest

### Work set
- New pages: <count>
- Re-summarized: <count>
- Skipped (current): <count>
- Pending (cap hit): <count or "none">

### Touched pages
- <path>

### New pages
- <path>

### External dependencies flagged
- <name> (external) — <count or "none">

### Index and log
- Hub: <wiki root>/code/_index.md (root: [[code/_index|Code graph]])
- Log: <path>

### Open questions
- <question or "none">

### Next useful command
- `/loam::ingesting-codebase <codebase root>` (to continue if cap was hit)
- `/loam::syncing-code-graph <codebase root> --sweep` (to check for drift)
```

---

## Rules

- One codebase root per run.
- Read every file before summarizing. Never summarize from filename alone.
- One role per file. When ambiguous, pick the role matching the file's primary export or primary intent.
- Edge links are untyped `[[slug]]`. Do not annotate edge type in the link itself.
- Unresolved dependency names stay as plain text flagged `(external)`. Do not create broken wikilinks.
- Code-ingested pages carry `source_path:` and `ingested_at:` front matter. Prose entity pages (from `/loam::adding-to-memory`) keep their existing front-matter-less shape.
- Granularity: one code page per file, keyed by the file's primary export or primary symbol. Do not split a single file into multiple pages unless it contains multiple independently-meaningful top-level declarations.
- Respect the 200-file cap. Do not silently exceed it.
- Resumability is automatic: the next invocation rebuilds the index from disk and skips current files.
- After wiki writes, refresh qmd if ready. Failures are reported, not rolled back.
- Read the wiki schema before editing the index or log.
- Prefer incremental linked updates over large rewrites.
- Do not leave avoidable broken wikilinks after the ingest pass.
- If the codegraph forwarder or the native runtime is missing or fails, stop and recommend `npx @scchearn/loam setup`; do not substitute a project-local fallback.
