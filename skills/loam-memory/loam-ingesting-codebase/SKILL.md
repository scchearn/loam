---
name: loam::ingesting-codebase
description: "Ingest a codebase into memory as entity pages connected by wiki links. Walks the tree, classifies each code file by role, applies a role template, and writes an entity page per meaningful unit under <wiki root>/entities/. Resumable: skips files already ingested and current. Not for prose documents; use /loam::adding-to-memory for those."
allowed-tools: Read Glob Grep Write Edit Bash
metadata:
  version: "1.0.0"
  author: scchearn
  argument-hint: <codebase root path>
---

You are a senior engineer and disciplined wiki maintainer ingesting a codebase into the persistent wiki so future sessions inherit a compressed semantic map of the code instead of re-reading source.

The codebase becomes a graph of entity pages under `<wiki root>/entities/`, connected by wiki links. The wiki *is* the code graph — no separate database, no parallel representation.

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

Use `wiki_root` as the resolved wiki root. If `qmd_ready` is true, note the `collection` name for later refresh (`qmd update -c <collection>`). The skill works fully without qmd; it only accelerates post-ingest discovery. Runtime guard: if `loamstate` fails or returns invalid JSON, fall back to Globbing for `SCHEMA.md`, `index.md`, or `log.md`.

### Codebase resolution

Resolve `$ARGUMENTS` to an absolute path. If it does not exist or is not a directory, stop and report the error. Treat it as the codebase root for `source_path` front-matter values (paths are relative to this root).

### Build the existing index

Run the index subcommand to get every code-ingested entity page already in the wiki:

```bash
"${CLAUDE_SKILL_DIR}/scripts/codegraph.sh" index <wiki-root>
```

Parse the JSON output into an in-memory map: `{source_path → {slug, ingested_at, mtime, exists}}`. Pages without `source_path:` front matter are prose entity pages and are skipped silently. This map is the set of already-ingested code nodes.

If the script is missing or fails, fall back to Globbing `entities/*.md` and parsing front matter with Read. The script is an optimization, not a hard dependency.

### Walk the codebase

Run the walk subcommand to get every candidate code file in the codebase:

```bash
"${CLAUDE_SKILL_DIR}/scripts/codegraph.sh" walk <codebase-root> \
  --exclusions "${CLAUDE_SKILL_DIR}/references/ingestion-exclusions.md"
```

Parse the JSON output: a list of `{path, mtime}` for candidate code files, relative to the codebase root.

If the script is missing or fails, fall back to Globbing the tree and applying the exclusion list manually (Read `references/ingestion-exclusions.md` for the patterns).

---

## Step 2 — Diff to find the work set

For each walked file, compare against the in-memory index:

- **In index AND `ingested_at >= file.mtime`** → skip (already current)
- **In index AND file newer than `ingested_at`** → re-summarize (overwrite the same entity page)
- **Not in index** → new ingest (create entity page)

**Cap the work set at 100 files.** If more remain, stop after 100 and report the pending count. The user re-invokes to continue; resumability (Step 1's index rebuild) means the next run picks up exactly where this one stopped.

If the work set is empty, report that the codebase is fully ingested and current, then stop.

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

## Step 4 — Read, extract, and generate the entity page

### Read the file

Read the file in full. Distinguish: the primary export or symbol (becomes the page name), its signature/shape, what it does (intent, not full implementation), what it depends on (imported names, called functions), and edge cases or failure modes.

### Derive the slug

Derive the entity-page slug from the file's primary export name or primary symbol, lowercased and kebab-cased. If no clear primary export, use the filename sans extension, kebab-cased. Examples:

- `src/auth/validateToken.ts` exporting `validateToken` → `validate-token`
- `src/utils/helpers.ts` exporting multiple utilities → `helpers` (one page per file)
- `src/models/User.ts` exporting `User` class → `user`

### Resolve dependencies to wiki links

For each dependency name found in the file:

1. Check the in-memory index (built in Step 1, augmented with new ingests this run) for a matching `source_path` or slug.
2. If it resolves to an entity page in the wiki → render as `[[slug]]`.
3. If it does not resolve → flag as an external dependency (plain text, not a wikilink). Note it in the `## Dependencies` section with an `(external)` marker.

Do not create broken wikilinks. Unresolved names stay as text.

### Generate the entity page

Fill the loaded role template with the extracted fields. The resulting markdown is the entity page. Include front matter:

```yaml
---
source_path: <relative-path-from-codebase-root>
ingested_at: <YYYY-MM-DD>
---
```

Use today's date for `ingested_at`. For re-summarized files, update `ingested_at` to today.

### Write the page

Write to `<wiki root>/entities/<slug>.md`. Overwrite if re-summarizing. Create if new.

### Update the in-memory index

Add or update the entry in the in-memory index so subsequent files in this run can link to it:

```
<source_path> → {slug, ingested_at: <today>, mtime: <file mtime>}
```

---

## Step 5 — Wire reciprocal links

After all files in the work set are written, wire reciprocal links. For each newly written or updated entity page:

1. Read the `## Dependencies` section to find which entity pages it links to.
2. For each linked page, append a reciprocal backlink to its `## Callers` or `## Mentioned in` section if the relationship is materially useful and the link is not already present.
3. Prefer small targeted edits (append one line) over rewrites.
4. Skip reciprocal links to external dependencies (they have no page to link back from).

Do not re-write pages whose links are unchanged. Only touch pages that gained or lost a dependency link from this run.

---

## Step 6 — Update index and log

### Update index.md

Add new entity pages to `index.md` under the `## Entities` group (create the group if absent) with one-line descriptions. Update descriptions for re-summarized pages if the summary changed meaningfully.

### Append to log.md

```md
## [YYYY-MM-DD] ingest-code | <codebase root basename>
```

Capture: root path, files ingested (count), files re-summarized (count), files skipped (count), external dependencies flagged, pending files (count if the cap was hit), any open questions.

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
- Code-ingested entity pages carry `source_path:` and `ingested_at:` front matter. Prose entity pages (from `/loam::adding-to-memory`) keep their existing front-matter-less shape.
- Granularity: one entity page per file, keyed by the file's primary export or primary symbol. Do not split a single file into multiple pages unless it contains multiple independently-meaningful top-level declarations.
- Respect the 100-file cap. Do not silently exceed it.
- Resumability is automatic: the next invocation rebuilds the index from disk and skips current files.
- After wiki writes, refresh qmd if ready. Failures are reported, not rolled back.
- Read the wiki schema before editing the index or log.
- Prefer incremental linked updates over large rewrites.
- Do not leave avoidable broken wikilinks after the ingest pass.
- If the script (`codegraph.sh` / `codegraph.ps1`) is missing or fails, fall back to Glob/Read/stat. The skill must work fully without the script.