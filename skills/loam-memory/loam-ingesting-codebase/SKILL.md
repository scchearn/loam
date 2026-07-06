---
name: loam::ingesting-codebase
description: "Ingest a codebase into memory as code pages connected by wiki links. Walks the tree, classifies each code file by role, applies a role template, and writes a code page per meaningful unit under <wiki root>/code/. Resumable: skips files already ingested and current. Not for prose documents; use /loam::adding-to-memory for those."
allowed-tools: Read Glob Grep Write Edit Bash
metadata:
  version: "1.5.1"
  author: scchearn
  argument-hint: <codebase root path>
---

You are a senior engineer and disciplined wiki maintainer ingesting a codebase into the persistent wiki so future sessions inherit a compressed semantic map of the code instead of re-reading source.

The codebase becomes a graph of code pages under `<wiki root>/code/`, connected by wiki links. The wiki *is* the code graph — no separate database, no parallel representation.

## Input

The codebase root is: $ARGUMENTS

---

## Step 1 — Resolve wiki, codebase, and build the index

### Wiki resolution and qmd readiness

Run `loamstate` to probe the wiki and qmd in one shot:

```bash
bash "${CLAUDE_SKILL_DIR}/../loam-using/scripts/loamstate.sh" "$(pwd)" 2>/dev/null \
  || powershell "${CLAUDE_SKILL_DIR}/../loam-using/scripts/loamstate.ps1" "$(pwd)" 2>/dev/null
```

Parse the JSON output. If `exists` is false, stop and recommend:

```text
/loam::scaffolding-wiki <topic or wiki goal>
```

Use `wiki_root` from `loamstate` as the resolved wiki root. Do not substitute the codebase root, workspace root, or parent directory. If `qmd_ready` is true, note the `collection` name for later refresh (`qmd update -c <collection>`). The skill works fully without qmd; it only accelerates post-ingest discovery. Runtime guard: if `loamstate` fails or returns invalid JSON, fall back to Globbing for `SCHEMA.md`, `index.md`, or `log.md`.

### Codebase resolution

Resolve `$ARGUMENTS` to an absolute path. If it does not exist or is not a directory, stop and report the error. Treat it as the codebase root for `source_path` front-matter values (paths are relative to this root).

### Build the existing index

Run the index subcommand to get every code-ingested page already in the wiki:

```bash
"${CLAUDE_SKILL_DIR}/scripts/codegraph.sh" index <wiki-root> --codebase-root <codebase-root>
```

Parse the JSON output into an in-memory map: `{source_path → {slug, ingested_at, mtime, exists}}`. Pages without `source_path:` front matter are prose entity pages and are skipped silently. This map is the set of already-ingested code nodes. The index scans both `code/` and `entities/` (for legacy stranded `source_path:` pages during the transition to the `code/` namespace).

If the script is missing or fails, fall back to Globbing `code/*.md` and `entities/*.md` and parsing front matter with Read. The script is an optimization, not a hard dependency.

If `codegraph.sh index` or `codegraph.sh diff` reports `wiki root contract not found` or `did you mean: .../wiki`, stop and rerun the command with the actual `wiki_root`. Do not proceed from an empty index caused by a bad wiki-root path.

### Optional preflight summary

For a quick size check before ingesting, run:

```bash
"${CLAUDE_SKILL_DIR}/scripts/codegraph.sh" walk <codebase-root> --summary \
  --exclusions "${CLAUDE_SKILL_DIR}/references/ingestion-exclusions.md"
```

This reports candidate counts by extension plus excluded low-signal counts (`pattern`, `gitignore`, `empty`, `large`, `generated_header`, `binary`). Use it to decide whether the run is likely to hit the cap; it is not required for correctness.

If the script is missing or fails, fall back to Globbing the tree and applying the exclusion list manually (Read `references/ingestion-exclusions.md` for the patterns).

---

## Step 2 — Diff to find the work set

Run the diff subcommand to get the files that need ingestion or re-summarization:

```bash
"${CLAUDE_SKILL_DIR}/scripts/codegraph.sh" diff <codebase-root> <wiki-root> \
  --exclusions "${CLAUDE_SKILL_DIR}/references/ingestion-exclusions.md"
```

Parse the JSON output: `{path, mtime, reason, slug?}` where `mtime` is the source file's Unix epoch mtime and `reason` is `new` or `stale`. Legacy pages with date-only `ingested_at` are stale once so they migrate to epoch precision.

- **`reason: "new"`** → new ingest (create code page)
- **`reason: "stale"`** → re-summarize (overwrite the same code page; `slug` is provided)
- current files are omitted

**Cap the work set at 200 files.** If more remain, stop after 200 and report the pending count. The user re-invokes to continue; resumability (Step 1's index rebuild) means the next run picks up exactly where this one stopped.

If the work set is empty, report that the codebase is fully ingested and current, then stop.

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

## Step 6 — Update index and log

### Update index.md

Add new code pages to `index.md` under the `## Code` group (create the group if absent) with one-line descriptions. Update descriptions for re-summarized pages if the summary changed meaningfully.

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
- Index: <path>
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
- If the script (`codegraph.sh` / `codegraph.ps1`) is missing or fails, fall back to Glob/Read/stat. The skill must work fully without the script.
