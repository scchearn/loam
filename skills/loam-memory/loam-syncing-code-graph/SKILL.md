---
name: loam::syncing-code-graph
description: "Reconcile the code graph in memory (wiki substrate) against the actual codebase. In --touched mode, re-summarizes only files a completed plan touched (cheap, post-plan gate). In --sweep mode, walks the whole repo and patches drift from out-of-band edits. Drift is accepted between gates; this skill is the only place the code graph is reconciled to the repo tree."
allowed-tools: Read Glob Grep Write Edit Bash
metadata:
  version: "1.7.1"
  author: scchearn
  argument-hint: <codebase root> [--touched <plan-path> | --sweep] [--ref <commit>]
---

You are a senior engineer reconciling the code graph in memory against the actual codebase. The wiki holds code pages (ingested by `/loam::ingesting-codebase`) under `<wiki root>/code/` with stable content identity, optional Git provenance, and retained compatibility fields. This skill patches drift — it does not do first-time ingestion (that's the ingestion skill's job) and it does not health-check the wiki's internal graph (that's `/loam::linting-memory`).

Drift detection compares `content_id` plus the fixed opaque generator key `loam-code-page-v1`. Mtime, size, and raw SHA-256 remain provenance only. Legacy pages missing stable identity are stale once and migrate incrementally.

Drift between the wiki and the codebase is inevitable and accepted. This skill is invoked at natural gates (plan completion, on-demand sweep) and touches only what changed.

## Input

Arguments: `$ARGUMENTS`

Parse the first token as the **codebase root** (absolute path). Then parse flags:

- `--touched <plan-path>` — cheap mode: reconcile only files the plan touched
- `--sweep` — thorough mode: walk the whole repo and patch all drift
- `--ref <commit>` — reconcile the selected committed projection instead of the default working tree

If neither flag is given, default to `--sweep`.

If both flags are given, error and stop.

---

## Step 1 — Resolve wiki, codebase, and load the index

### Wiki resolution and qmd readiness

First reuse the injected `Workspace state` under the reuse contract in `loam::using`. Do not rerun the integration when that block supplies wiki existence/root, qmd readiness, collection, and hints; the codegraph commands below are authoritative for current drift.

If the injected state cannot be reused, refresh native state through the injected absolute integration path:

```bash
<native-runtime-command> state --fast "$(pwd)"
```

If the native runtime reports unavailable or does not provide real state, stop and recommend `npx @scchearn/loam install`; do not fabricate state or use a project-local fallback. If `exists` is false, stop — there is nothing to sync. Use `wiki_root` from the resolved state; do not substitute the codebase root, workspace root, or parent directory. If `qmd_ready` is true, note the `collection` name for later refresh.

### Codebase resolution

Resolve the codebase root from the first argument. If it does not exist or is not a directory, stop and report the error.

### Build the existing index

Run the index subcommand from the ingestion skill's scripts:

```bash
<native-runtime-command> codegraph index <wiki-root> --codebase-root <codebase-root>
```

Parse the JSON output into an in-memory map keyed by `source_path`, retaining `slug`, compatibility fields, `content_id`, `blob_oid`, `source_commit`, `source_state`, `generator_version`, `mtime`, and `exists`. This is the current code graph in the wiki. The index scans both `code/` and `entities/` (for legacy stranded `source_path:` pages during the transition to the `code/` namespace).

If the native runtime command fails or reports an unavailable runtime, stop and report the setup recovery command. Do not fall back to a project-local launcher.

If the native codegraph command reports `wiki root contract not found` or `did you mean: .../wiki`, stop and rerun it with the actual `wiki_root`. Do not proceed from an empty index caused by a bad wiki-root path.

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

For each touched path, resolve it relative to the codebase root. Default mode checks the working tree; explicit-ref mode checks the native walk projection.

### Classify each touched path

For each touched path:

1. **File no longer exists in the selected projection** → its code page is now orphaned. Find the code page whose `source_path:` matches (from the index). Remove the page. Find all pages that link to it (`[[slug]]`) and remove those links. Record the removal.

2. **File exists** → use `codegraph diff` with `--generator-version loam-code-page-v1`, the standard exclusions, and optional `--ref`, then filter to touched paths.
   - **Diff reports stale** → re-summarize. In default mode read `<codebase-root>/<path>`; with `--ref`, read `git -C <codebase-root> show <source_commit>:./<path>`. Classify the role, apply the ingestion template, and replace the complete identity/provenance front matter from the native record while deriving compatibility size/hash from the same bytes.
   - **Diff does not report stale** → skip. Stable identity already proves the selected bytes are current.

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
<native-runtime-command> codegraph walk <codebase-root> \
  --exclusions "${LOAM_SKILL_DIR:-${CLAUDE_SKILL_DIR}}/../loam-ingesting-codebase/references/ingestion-exclusions.md" \
  --generator-version loam-code-page-v1 [--ref <commit>]
```

Omit the bracketed `--ref` pair in default mode. Parse the additive records, retaining `path`, compatibility values, `content_id`, `blob_oid`, `source_commit`, `source_state`, and `generator_version`.

You may also run the diff subcommand to get `new` and `stale` sets directly:

```bash
<native-runtime-command> codegraph diff <codebase-root> \
  --exclusions "${LOAM_SKILL_DIR:-${CLAUDE_SKILL_DIR}}/../loam-ingesting-codebase/references/ingestion-exclusions.md" \
  --generator-version loam-code-page-v1 [--ref <commit>]
```

Default mode preserves eligible tracked, modified, staged-new, and untracked working-tree files and supports non-Git roots with namespaced SHA-256. Explicit-ref mode requires Git, ignores working-tree overlays, and reads only the resolved commit. `source_state: provisional` is local working-tree provenance, not authoritative published/federated source truth.

Still use the walk output plus index to find orphaned nodes; `diff` intentionally returns only `new` and `stale` files.

If the native runtime command fails or reports an unavailable runtime, stop and report the setup recovery command. Do not fall back to a project-local launcher.

### Diff the graph against the codebase

Build three sets:

1. **Orphaned nodes** — code pages in the index whose `source_path` does NOT appear in the walk output. These correspond to deleted files.
   - For each: remove the code page. Find all pages linking to it and remove the links. Record the removal.

2. **Stale nodes** — code pages whose `source_path` IS in the walk output but stable identity or generator version differs, including legacy pages without `content_id`.
   - For each: re-summarize from the selected projection, classify, template, and write. Replace all identity/provenance fields from the native record and compatibility size/hash from the same bytes. Re-wire edges.

3. **New files** — walked files not in the index.
   - Do NOT auto-ingest. Flag them and recommend `/loam::ingesting-codebase <codebase-root>` to the user. Record the count.

4. **Current nodes** — in index, in walk, and matching stable identity plus generator version. Skip.

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

If qmd was ready and you wrote to the wiki, run `qmd update -c <collection>` then `qmd embed -c <collection>`; report both outcomes separately. If either fails, report it but do not roll back wiki edits.

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
- Code-ingested pages carry `source_path`, `ingested_at`, `source_size`, `content_hash`, `content_id`, `blob_oid`, `source_commit`, `source_state`, and `generator_version`. Legacy omissions are migration state, not errors.
- Default mode preserves current working-tree and non-Git behavior. Explicit `--ref` uses only the selected commit; never read source bytes from a different projection.
- After wiki writes, refresh qmd if ready. Failures are reported, not rolled back.
- Read the wiki schema before editing the index or log.
- When re-summarizing, reuse the ingestion skill's role templates and classification rubric — do not improvise a different node format.
- Edge links are untyped `[[slug]]`. Consistent with `/loam::ingesting-codebase`.
- If the codegraph forwarder or the native runtime is missing or fails, stop and recommend `npx @scchearn/loam install`; do not substitute a project-local fallback.
