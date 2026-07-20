---
name: loam::syncing-code-graph
description: "Reconcile the code graph in memory (wiki substrate) against the actual codebase. In --touched mode, re-summarizes only files a completed plan touched (cheap, post-plan gate). In --sweep mode, walks the whole repo and patches drift from out-of-band edits. Drift is accepted between gates; this skill is the only place the code graph is reconciled to the repo tree."
allowed-tools: Read Glob Grep Write Edit Bash
metadata:
  version: "1.6.0"
  author: scchearn
  argument-hint: <codebase root> [--touched <plan-path>] [--sweep]
---

You are a senior engineer reconciling the code graph in memory against the actual codebase. The wiki holds code pages (ingested by `/loam::ingesting-codebase`) under `<wiki root>/code/` with `source_path:`, `ingested_at:`, `source_size:`, and `content_hash:` front matter. `ingested_at` is the source file's Unix epoch mtime, not a wall-clock ingest date. This skill patches drift — it does not do first-time ingestion (that's the ingestion skill's job) and it does not health-check the wiki's internal graph (that's `/loam::linting-memory`).

Drift detection uses mtime+size as the primary signal and content hash as a secondary check. When mtime says stale but size is unchanged and `content_hash` exists, `codegraph diff` computes the file's SHA-256 and suppresses the stale flag if the hash matches (false-stale suppression). Missing or nonnumeric `source_size` disables size comparison and falls back to mtime-only; missing `content_hash` disables secondary suppression. Treat missing fields as legacy, not errors. The `--strict` flag forces full-hash verification on every file (catches false-fresh where content changed but mtime was backdated); it is opt-in, not the default.

Drift between the wiki and the codebase is inevitable and accepted. This skill is invoked at natural gates (plan completion, on-demand sweep) and touches only what changed.

## Input

Arguments: `$ARGUMENTS`

Parse the first token as the **codebase root** (absolute path). Then parse flags:

- `--touched <plan-path>` — cheap mode: reconcile only files the plan touched
- `--sweep` — thorough mode: walk the whole repo and patch all drift

If neither flag is given, default to `--sweep`.

If both flags are given, error and stop.

---

## Step 1 — Resolve wiki, codebase, and load the index

### Wiki resolution and qmd readiness

First reuse the injected `Workspace state` under the reuse contract in `loam::using`. Do not rerun `loamstate` when that block supplies wiki existence/root, qmd readiness, collection, and hints; the codegraph commands below are authoritative for current drift.

If the injected state cannot be reused, run a fast probe:

```bash
bash "${LOAM_SKILL_DIR:-${CLAUDE_SKILL_DIR}}/../loam-using/scripts/loamstate.sh" --fast "$(pwd)" 2>/dev/null \
  || powershell "${LOAM_SKILL_DIR:-${CLAUDE_SKILL_DIR}}/../loam-using/scripts/loamstate.ps1" "$(pwd)" 2>/dev/null
```

If `exists` is false, stop — there is nothing to sync. Use `wiki_root` from the resolved state; do not substitute the codebase root, workspace root, or parent directory. If `qmd_ready` is true, note the `collection` name for later refresh. Runtime guard: if a required probe fails or returns invalid JSON, fall back to Globbing for `SCHEMA.md`, `index.md`, or `log.md`.

### Codebase resolution

Resolve the codebase root from the first argument. If it does not exist or is not a directory, stop and report the error.

### Build the existing index

Run the index subcommand from the ingestion skill's scripts:

```bash
"${LOAM_SKILL_DIR:-${CLAUDE_SKILL_DIR}}/../loam-ingesting-codebase/scripts/codegraph.sh" index <wiki-root> --codebase-root <codebase-root>
```

Parse the JSON output into an in-memory map: `{source_path → {slug, ingested_at, source_size, content_hash, mtime, exists}}`. This is the current code graph in the wiki. The index scans both `code/` and `entities/` (for legacy stranded `source_path:` pages during the transition to the `code/` namespace).

If the script is missing or fails, fall back to Globbing `code/*.md` and `entities/*.md` and parsing front matter with Read.

If `codegraph.sh index` or `codegraph.sh diff` reports `wiki root contract not found` or `did you mean: .../wiki`, stop and rerun the command with the actual `wiki_root`. Do not proceed from an empty index caused by a bad wiki-root path.

### Resolve the ingestion skill's references

Note the path to the ingestion skill's references for later use:
- Exclusions: `${LOAM_SKILL_DIR:-${CLAUDE_SKILL_DIR}}/../loam-ingesting-codebase/references/ingestion-exclusions.md`
- Role classification: `${LOAM_SKILL_DIR:-${CLAUDE_SKILL_DIR}}/../loam-ingesting-codebase/references/role-classification.md`
- Role templates: `${LOAM_SKILL_DIR:-${CLAUDE_SKILL_DIR}}/../loam-ingesting-codebase/references/templates/`

---

## Step 2A — Touched mode (`--touched <plan-path>`)

### Read the plan's touched files

Read the plan file. Locate the `## Touched files` section. If the section is absent or empty, stop and tell the user:

```text
No touched files recorded in <plan-path>. The plan may not have been executed yet,
or /loam::starting did not populate the section. Nothing to sync.
```

Parse the table rows. Filter to rows where `Marker` is `edit` (files that were modified). Read-only files (`read`) are excluded — they were not changed.

For each touched path, resolve it to an absolute path under the codebase root.

### Classify each touched path

For each touched path:

1. **File no longer exists** → its code page is now orphaned. Find the code page whose `source_path:` matches (from the index). Remove the page. Find all pages that link to it (`[[slug]]`) and remove those links. Record the removal.

2. **File exists** → prefer `codegraph diff` filtered to touched paths, so touched mode uses the same mtime+size/hash decision as sweep mode. If the script is unavailable, apply the same logic locally: missing/nonnumeric `source_size` uses mtime-only; matching size plus matching `content_hash` suppresses a false-stale.
   - **Diff reports stale** → re-summarize. Read the file, classify role (using the ingestion skill's `role-classification.md` rubric), load the matching role template, extract fields, generate the code page markdown, and overwrite the page. Update `ingested_at:`, `source_size:`, and `content_hash:` to the file's current values.
   - **Diff does not report stale** → skip. No re-summarization needed. To catch false-fresh (content changed, mtime backdated), run `codegraph diff --strict` and filter to touched paths.
   - Use `codegraph diff --strict` when timestamp integrity is suspect (tar restore, Syncthing conflict, manual `touch -d`).

3. **File exists but not in the index** → new file created by the plan. Flag it for ingestion. Do NOT auto-ingest in touched mode — recommend `/loam::ingesting-codebase <codebase-root>` to the user. Record the new file.

### Re-wire edges

For each re-summarized node, re-resolve dependencies to wiki links (using the updated in-memory index). Update the `## Dependencies` section. Patch reciprocal links on pages that gained or lost a dependency link from this sync.

### Update the log

Append to `log.md`:

```md
## [YYYY-MM-DD] sync-code (touched) | <plan basename>
```

Capture: plan path, files re-summarized (count), files removed (count), files skipped (count), new files flagged for ingestion (count), edges patched.

---

## Step 2B — Sweep mode (`--sweep`)

### Walk the codebase

Run the walk subcommand from the ingestion skill:

```bash
"${LOAM_SKILL_DIR:-${CLAUDE_SKILL_DIR}}/../loam-ingesting-codebase/scripts/codegraph.sh" walk <codebase-root> \
  --exclusions "${LOAM_SKILL_DIR:-${CLAUDE_SKILL_DIR}}/../loam-ingesting-codebase/references/ingestion-exclusions.md"
```

Parse the JSON output: a list of `{path, mtime, size}` for candidate code files, where `mtime` is Unix epoch seconds and `size` is the file's byte size.

You may also run the diff subcommand to get `new` and `stale` sets directly:

```bash
"${LOAM_SKILL_DIR:-${CLAUDE_SKILL_DIR}}/../loam-ingesting-codebase/scripts/codegraph.sh" diff <codebase-root> <wiki-root> \
  --exclusions "${LOAM_SKILL_DIR:-${CLAUDE_SKILL_DIR}}/../loam-ingesting-codebase/references/ingestion-exclusions.md" [--strict]
```

The diff uses mtime+size as primary and content hash as secondary: when mtime says stale but size is unchanged and `content_hash` exists, it computes the file's SHA-256 and suppresses the stale flag if the hash matches (false-stale suppression). Add `--strict` to force full-hash verification on every file (catches false-fresh where content changed but mtime was backdated). Missing or nonnumeric `source_size` disables size comparison and uses mtime-only fallback; missing `content_hash` only disables secondary suppression.

Still use the walk output plus index to find orphaned nodes; `diff` intentionally returns only `new` and `stale` files.

If the script is missing or fails, fall back to Globbing and manual exclusion filtering.

### Diff the graph against the codebase

Build three sets:

1. **Orphaned nodes** — code pages in the index whose `source_path` does NOT appear in the walk output. These correspond to deleted files.
   - For each: remove the code page. Find all pages linking to it and remove the links. Record the removal.

2. **Stale nodes** — code pages whose `source_path` IS in the walk output but `codegraph diff` reports as stale (mtime newer than `ingested_at` and hash check did not suppress). Includes legacy pages whose `ingested_at` is not numeric.
   - For each: re-summarize (read, classify, template, write). Update `ingested_at:`, `source_size:`, and `content_hash:` to the file's current values. Re-wire edges.

3. **New files** — walked files not in the index.
   - Do NOT auto-ingest. Flag them and recommend `/loam::ingesting-codebase <codebase-root>` to the user. Record the count.

4. **Current nodes** — in index, in walk, not stale (mtime not newer, or hash suppressed). Skip.

### Apply removals and re-summarizations

Apply orphan removals first (so stale-node re-summarization doesn't try to link to removed pages). Then apply stale-node re-summarizations. Re-wire edges after all writes.

### Update the log

Append to `log.md`:

```md
## [YYYY-MM-DD] sync-code (sweep) | <codebase root basename>
```

Capture: nodes removed (count), nodes re-summarized (count), new files flagged for ingestion (count), edges patched, files skipped (count).

---

## Step 3 — Reconcile the code hub and refresh qmd

After either mode, apply `/loam::ingesting-codebase` Step 6: rebuild `code/_index.md` from every active ordinary code page, keep exactly one root `[[code/_index|Code graph]]` link, and remove direct root entries. Do this even when no nodes changed. Do not add individual code pages to root `index.md`.

If qmd was ready and you wrote to the wiki, run `qmd update -c <collection> 2>/dev/null`. If refresh fails, report it but do not roll back wiki edits.

---

## Step 4 — Report back

```md
Code graph synced from <codebase root>

### Mode
- touched | sweep

### Changes
- Re-summarized: <count>
- Removed (orphaned): <count>
- Skipped (current): <count>
- New files flagged: <count or "none">

### Touched pages
- <path>

### Removed pages
- <path or "none">

### Edges patched
- <count or "none">

### Index and log
- Hub: <wiki root>/code/_index.md (root: [[code/_index|Code graph]])
- Log: <path>

### Open questions
- <question or "none">

### Next useful command
- `/loam::ingesting-codebase <codebase root>` (if new files were flagged)
- `/loam::querying-memory <question>` (to verify graph traversal)
```

---

## Rules

- Never auto-ingest new files. Sweep reconciles drift; first-time ingest is `/loam::ingesting-codebase`'s job.
- `--touched` mode requires a valid plan path with a populated `## Touched files` section. If absent or empty, stop and tell the user.
- Drift is accepted. Do not attempt to prevent it; only patch it when invoked.
- Code-ingested pages carry `source_path:`, `ingested_at:`, `source_size:`, and `content_hash:` front matter. Use them; do not recompute from the body. Legacy pages may lack `source_size` and `content_hash` — treat as legacy, not errors.
- After wiki writes, refresh qmd if ready. Failures are reported, not rolled back.
- Read the wiki schema before editing the index or log.
- When re-summarizing, reuse the ingestion skill's role templates and classification rubric — do not improvise a different node format.
- Edge links are untyped `[[slug]]`. Consistent with `/loam::ingesting-codebase`.
- If the script (`codegraph.sh` / `codegraph.ps1`) is missing or fails, fall back to Glob/Read/stat. The skill must work fully without the script.
