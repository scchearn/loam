---
name: loam::syncing-code-graph
description: "Reconcile the code graph in memory (wiki substrate) against the actual codebase. In --touched mode, re-summarizes only files a completed plan touched (cheap, post-plan gate). In --sweep mode, walks the whole repo and patches drift from out-of-band edits. Drift is accepted between gates; this skill is the only place the code graph is reconciled to the repo tree."
allowed-tools: Read Glob Grep Write Edit Bash
metadata:
  version: "1.0.0"
  author: scchearn
  argument-hint: <codebase root> [--touched <plan-path>] [--sweep]
---

You are a senior engineer reconciling the code graph in memory against the actual codebase. The wiki holds entity pages (ingested by `/loam::ingesting-codebase`) with `source_path:` and `ingested_at:` front matter. This skill patches drift — it does not do first-time ingestion (that's the ingestion skill's job) and it does not health-check the wiki's internal graph (that's `/loam::linting-memory`).

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

Run `loamstate` to probe the wiki and qmd in one shot:

```bash
bash "${CLAUDE_SKILL_DIR}/../loam-using/scripts/loamstate.sh" "$(pwd)" 2>/dev/null \
  || powershell "${CLAUDE_SKILL_DIR}/../loam-using/scripts/loamstate.ps1" "$(pwd)" 2>/dev/null
```

Parse the JSON output. If `exists` is false, stop — there is nothing to sync. Use `wiki_root` as the resolved wiki root. If `qmd_ready` is true, note the `collection` name for later refresh. Runtime guard: if `loamstate` fails or returns invalid JSON, fall back to Globbing for `SCHEMA.md`, `index.md`, or `log.md`.

### Codebase resolution

Resolve the codebase root from the first argument. If it does not exist or is not a directory, stop and report the error.

### Build the existing index

Run the index subcommand from the ingestion skill's scripts:

```bash
"${CLAUDE_SKILL_DIR}/../loam-ingesting-codebase/scripts/codegraph.sh" index <wiki-root>
```

Parse the JSON output into an in-memory map: `{source_path → {slug, ingested_at, mtime, exists}}`. This is the current code graph in the wiki.

If the script is missing or fails, fall back to Globbing `entities/*.md` and parsing front matter with Read.

### Resolve the ingestion skill's references

Note the path to the ingestion skill's references for later use:
- Exclusions: `${CLAUDE_SKILL_DIR}/../loam-ingesting-codebase/references/ingestion-exclusions.md`
- Role classification: `${CLAUDE_SKILL_DIR}/../loam-ingesting-codebase/references/role-classification.md`
- Role templates: `${CLAUDE_SKILL_DIR}/../loam-ingesting-codebase/references/templates/`

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

1. **File no longer exists** → its entity page is now orphaned. Find the entity page whose `source_path:` matches (from the index). Remove the page. Find all pages that link to it (`[[slug]]`) and remove those links. Record the removal.

2. **File exists** → compare its `mtime` (from `stat`) against the entity page's `ingested_at`:
   - **File newer** → re-summarize. Read the file, classify role (using the ingestion skill's `role-classification.md` rubric), load the matching role template, extract fields, generate the entity page markdown, and overwrite the page. Update `ingested_at:` to today. Update `source_path:` if the path changed (rename).
   - **File unchanged or older** → skip. No re-summarization needed.

3. **File exists but not in the index** → new file created by the plan. Flag it for ingestion. Do NOT auto-ingest in touched mode — recommend `/loam::ingesting-codebase <codebase-root>` to the user. Record the new file.

### Re-wire edges

For each re-summarized node, re-resolve dependencies to wiki links (using the updated in-memory index). Update the `## Dependencies` section. Patch reciprocal links on pages that gained or lost a dependency link from this sync.

### Update index and log

Update `index.md` if any entity pages were added, removed, or had their descriptions change meaningfully.

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
"${CLAUDE_SKILL_DIR}/../loam-ingesting-codebase/scripts/codegraph.sh" walk <codebase-root> \
  --exclusions "${CLAUDE_SKILL_DIR}/../loam-ingesting-codebase/references/ingestion-exclusions.md"
```

Parse the JSON output: a list of `{path, mtime}` for candidate code files.

If the script is missing or fails, fall back to Globbing and manual exclusion filtering.

### Diff the graph against the codebase

Build three sets:

1. **Orphaned nodes** — entity pages in the index whose `source_path` does NOT appear in the walk output. These correspond to deleted files.
   - For each: remove the entity page. Find all pages linking to it and remove the links. Record the removal.

2. **Stale nodes** — entity pages whose `source_path` IS in the walk output but the file's `mtime` is newer than the page's `ingested_at`.
   - For each: re-summarize (read, classify, template, write). Update `ingested_at:` to today. Re-wire edges.

3. **New files** — walked files not in the index.
   - Do NOT auto-ingest. Flag them and recommend `/loam::ingesting-codebase <codebase-root>` to the user. Record the count.

4. **Current nodes** — in index, in walk, `mtime <= ingested_at`. Skip.

### Apply removals and re-summarizations

Apply orphan removals first (so stale-node re-summarization doesn't try to link to removed pages). Then apply stale-node re-summarizations. Re-wire edges after all writes.

### Update index and log

Update `index.md` for removed and re-summarized pages.

Append to `log.md`:

```md
## [YYYY-MM-DD] sync-code (sweep) | <codebase root basename>
```

Capture: nodes removed (count), nodes re-summarized (count), new files flagged for ingestion (count), edges patched, files skipped (count).

---

## Step 3 — Refresh qmd

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
- Index: <path>
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
- Code-ingested entity pages carry `source_path:` and `ingested_at:` front matter. Use them; do not recompute from the body.
- After wiki writes, refresh qmd if ready. Failures are reported, not rolled back.
- Read the wiki schema before editing the index or log.
- When re-summarizing, reuse the ingestion skill's role templates and classification rubric — do not improvise a different node format.
- Edge links are untyped `[[slug]]`. Consistent with `/loam::ingesting-codebase`.
- If the script (`codegraph.sh` / `codegraph.ps1`) is missing or fails, fall back to Glob/Read/stat. The skill must work fully without the script.