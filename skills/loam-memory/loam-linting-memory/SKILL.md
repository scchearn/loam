---
name: loam::linting-memory
description: "Run a health check on existing memory (the wiki substrate) and goal artifacts. Use this when the user wants to lint the wiki, health-check the knowledge base, find orphan pages, spot broken or missing cross-links, clean up stale claims and unresolved wikilinks with safe local fixes, consolidate a legacy root `overview.md` into `index.md`, or health-check goal artifacts. Not for adding new material; use /loam::adding-to-memory or /loam::learning-from-session for that."
allowed-tools: Read Glob Grep Write Edit Bash
metadata:
  version: "1.9.0"
  author: scchearn
  argument-hint: [wiki root, goal path, or focus area]
---

You are a senior engineer and wiki maintainer performing a structured health-check on a persistent markdown wiki. Your job is to improve wiki integrity without turning the lint pass into a full ingest or a speculative rewrite.

The wiki is expected to behave like an Obsidian-friendly note graph, so lint should protect not just factual quality but also graph integrity.

The preferred root-hub pattern is a single `index.md` with a concise `## Overview` section near the top. A separate root `overview.md` is a legacy pattern that lint should consolidate into `index.md` and remove.

Use the LLM Wiki maintenance model:

- detect contradictions instead of flattening them
- surface stale claims that newer sources may have superseded
- find orphan pages and missing cross-references
- identify concepts or entities repeatedly mentioned but lacking their own page
- preserve unresolved gaps so future sessions know what still needs evidence

Apply safe fixes directly. For issues that need new evidence or substantive judgment, annotate and report them instead of guessing.

## Input

The lint target is: $ARGUMENTS

If no explicit target is provided, lint the whole wiki and any goals directory.

If the target is a `goals/<slug>.md` path, run only the goal-health pass (Step 2G) for that single goal file. Skip all wiki-only steps.

---

## Step 1 — Resolve wiki, read contract & audit

### Locate the wiki and probe state

The injected summary omits `metadata_status` and `metadata_path`, and lint must inspect current state. Run a fresh fast probe rather than reusing it:

```bash
bash "${LOAM_SKILL_DIR:-${CLAUDE_SKILL_DIR}}/../loam-using/scripts/loamstate.sh" --fast "$(pwd)" 2>/dev/null \
  || powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "${LOAM_SKILL_DIR:-${CLAUDE_SKILL_DIR}}/../loam-using/scripts/loamstate.ps1" "$(pwd)" 2>/dev/null
```

If `exists` is false, check whether `goals/` exists (`ls goals/*.md 2>/dev/null`). If `goals/` exists, skip all wiki-only steps (Steps 1 "Read the wiki contract" through Step 2 "Apply safe fixes") and jump directly to the goal-health pass (Step 2G). If neither a wiki nor goals exist, stop and recommend:

```text
/loam::scaffolding-wiki <topic or wiki goal>
```

If the target was a single `goals/<slug>.md` path, skip all wiki-only steps and run only the goal-health pass for that file.

Use `wiki_root` as the resolved wiki root (resolved from on-disk contract files, not qmd metadata). If `has_overview` is true, note it as a legacy root-hub file to fold into `index.md`. Then resolve the lint scope: if the user named a wiki root, subdirectory, topic, or entity, use that. If no scope given, lint the whole wiki.

This skill satisfies the `memory_lint_stale`, `date_drift_pending`, `log_rotation_due`, and `legacy_structure_pending` hints (see the hint contract in `loam::using`); treat them as advisory scope, not extra mandatory work. Fast state omits date drift, which the lint pass checks directly later.

### Read the wiki contract

Before editing, read:

1. `<wiki root>/SCHEMA.md`
2. `<wiki root>/index.md`
3. scoped log read: `grep "^## \[" <wiki root>/log.md | tail -5` for the last 5 entries (recent maintenance context). If a specific lint scope is named, also `grep -i "<scope keywords>" <wiki root>/log.md`. Never read the full log.
4. `<wiki root>/overview.md` when it exists, so you can fold its useful root-hub content into `index.md` and remove it
5. the files inside the lint scope most relevant to the current health check
6. `${LOAM_SKILL_DIR:-${CLAUDE_SKILL_DIR}}/references/lint-checklist.md`
7. `${LOAM_SKILL_DIR:-${CLAUDE_SKILL_DIR}}/references/finding-triage.md`

Use `Glob` and `Grep` to map the pages in scope before reading deeply.

When ordinary `code/*.md` pages exist or migration will create them, read and apply `references/code-hub.md`.

Treat `index.md` as the authoritative root hub for durable prose, with a concise `## Overview` and one code-hub link. The desired steady state is a single root-hub file: `index.md` with a concise `## Overview` section before the grouped page catalog.

### Run the native structural check first

Structural link and heading integrity is machine-checkable. Run it before
reading pages, so manual review is spent on judgement rather than link chasing:

```bash
bash "${LOAM_SKILL_DIR:-${CLAUDE_SKILL_DIR}}/../loam-using/scripts/loam.sh" lint "$WORKSPACE_ROOT" 2>/dev/null \
  || powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "${LOAM_SKILL_DIR:-${CLAUDE_SKILL_DIR}}/../loam-using/scripts/loam.ps1" lint "$WORKSPACE_ROOT" 2>/dev/null
```

Each line is one JSON finding with `rule`, `file`, `line`, `description`,
`target`, and `candidates`. Exit 0 is clean, 2 means findings, 1 means bad
arguments or an unreadable input. `--only markdown|memory|work` narrows the
scan. The command is read-only and never fixes anything.

If it exits 75 or 78 the native runtime is not ready — fall through to the
manual `Glob`/`Grep` checks below. The linter reports; **you** still classify
findings and apply safe fixes through the normal proposal/approval path.


### qmd metadata health (secondary only)

The `loamstate` probe already resolved `qmd_ready`, `collection`, `metadata_status`, and `metadata_path`. If `metadata_path` is non-empty, compare its `retrieval.collection_path` to the actual resolved `<wiki root>` using absolute path equality. If they differ, mark qmd metadata as stale and plan a safe metadata reconciliation during fixes.

If metadata is stale and qmd is available, validate the recorded `collection` with `qmd collection show <collection> 2>/dev/null` when supported. If another collection is already registered for the actual `<wiki root>`, plan to update `collection_name` to that collection. If no collection points at the actual `<wiki root>`, do not rename or move wiki directories; set `retrieval.status` to `"degraded"`, keep the actual `<wiki root>` in `retrieval.collection_path`, and report the collection/path mismatch.

If `qmd_ready` is false, qmd is not available — use Grep/Glob only.

qmd is **secondary only** in this skill: use it only for *content* discovery — finding related-note neighborhoods when a structural fix might need reciprocal links or nearby canonical notes, or surfacing stale claims and contradictions. If ready, follow the qmd search protocol in `loam::using` (structural checks stay Glob/Grep-led — qmd does not replace inventory, orphan, or wikilink checks).

### Audit for health issues

**A. Structure and inventory** — Check for: `index.md` missing `## Overview` section; legacy `overview.md` still present; content stranded in `overview.md` instead of `index.md`; duplicated root-hub content; durable prose missing from the root index; generated code-hub drift; index entries pointing to non-existent pages; duplicate note identities; filename convention drift; legacy checkpoint filenames like `checkpoint-YYYY-MM-DD-HHMM-<slug>.md` that should become `checkpoint-YYYY-MM-DD-HHMM.md`; near-duplicate pages; empty or placeholder pages.

**B. Link health** — Check for: unresolved `[[wikilinks]]`; orphan pages; pages with no meaningful inbound or outbound links; pages only discoverable from `index.md`; pages that should link but don't; missing reciprocal backlinks; repeated entity/concept mentions without a dedicated page.

**C. Knowledge integrity** — Check for: contradictory statements; stale claims superseded by later ingests; broad synthesis pages out of date; under-sourced claims.

**D. Maintenance signals** — Check for: recent ingests not in index; lint-worthy gaps never reconciled in log; missing follow-up notes.

**E. Obsidian config placement** — Check whether `<wiki root>/.obsidian/` exists. The desired layout is for `.obsidian/` to live at the parent directory root that contains the wiki, not nested inside the wiki directory, unless `<wiki root>` is itself the project/workspace root.

**F. qmd metadata integrity** — Check whether `<wiki root>/.wiki-metadata.json` reflects the actual resolved `<wiki root>`. Lint reconciles metadata to the on-disk wiki; it must never rename, move, or recreate the wiki directory to match stale metadata.

**G. qmd archive exclusion** — Check whether the active qmd collection config excludes archived pages with `ignore: [".archive/**"]` or an equivalent list item. If `.archive/**` is missing, flag it as a health issue; archived pages must not appear in qmd retrieval.

**H. Freshness re-validation** — Flag pages whose `updated_at` is older than 90 days and that cite volatile surfaces such as APIs, configs, versions, external docs, or code paths. This is a re-validation warning only; lint does not auto-archive stale volatile pages.

**I. Stranded code pages in `entities/`** — Check whether `entities/` contains pages with `source_path:` front matter. These are code-graph pages that belong in `code/`, not `entities/`. Prose entity pages never carry `source_path:`. See Step 2 for the migration procedure.

**J. Legacy code hash fields** — Check code-graph pages under `code/` and legacy `entities/` pages with `source_path:` front matter for missing `source_size:` or `content_hash:`. Flag as `legacy-hash-fields` (low/informational). Missing fields mean hash-secondary false-stale suppression will not fire for that page yet. Do not backfill during lint; ingest/sync will populate the fields on the next re-summarize.

Distinguish: **fix now** (safe from existing wiki evidence) vs **annotate now** (mark but don't resolve) vs **follow-up** (needs future evidence/research/user direction).

**Expand with qmd (content, if ready)**: Follow the qmd search protocol in `loam::using` to find related-note neighborhoods for orphan pages, missing cross-links, stale claims, and contradictions (structural steps A, B, E above stay Glob/Grep-led).

---

## Step 2 — Apply safe fixes, record & refresh

### Apply safe fixes

Make the smallest correct edits that improve wiki health.

Safe fixes apply directly inside the boundaries below. Missing `.archive/**` qmd exclusion and 90-day volatile-surface staleness are reported health issues unless the current wiki-local metadata can be corrected safely without changing external qmd config.

When a legacy `<wiki root>/overview.md` exists:

1. extract durable orientation content (scope, corpus boundaries, major topic links, evidenced open questions)
2. fold or compress into a concise `## Overview` section near the top of `index.md`
3. treat `index.md` as authoritative when the two files differ
4. delete `overview.md` before finishing the pass

When `<wiki root>/.obsidian/` exists and `<wiki root>` is a subdirectory:

1. resolve `<parent directory root>` as the parent directory that contains `<wiki root>`
2. if `<parent directory root>/.obsidian/` does not exist, move only `<wiki root>/.obsidian/` to `<parent directory root>/.obsidian/`
3. if `<parent directory root>/.obsidian/` already exists, do not overwrite or merge it; report the nested `.obsidian/` as unresolved and explain that manual reconciliation is needed
4. if Obsidian's global vault registry is available at `$HOME/.config/obsidian/obsidian.json` or `$HOME/Library/Application Support/obsidian/obsidian.json`, update entries that point exactly at `<wiki root>` to point at `<parent directory root>` after a successful move
5. record the vault-placement fix in `<wiki root>/log.md` because the wiki path remains unchanged

When `<wiki root>/.wiki-metadata.json` has a stale `retrieval.collection_path`:

1. update `retrieval.collection_path` to the actual resolved absolute `<wiki root>`
2. preserve `retrieval.collection_name` when it validates against the actual `<wiki root>`
3. if a different qmd collection is already registered for the actual `<wiki root>`, update `retrieval.collection_name` to that collection
4. if the recorded or corrected qmd collection validates against the actual `<wiki root>`, keep or set `retrieval.status` to `"ready"` and update `retrieval.last_verified` to `YYYY-MM-DD`
5. if no qmd collection points at the actual `<wiki root>`, or validation cannot be completed, set `retrieval.status` to `"degraded"`, keep the corrected `retrieval.collection_path`, and report the qmd collection repair needed
6. record the metadata reconciliation in `<wiki root>/log.md`

When `<wiki root>/checkpoints/` contains legacy slugged checkpoint filenames:

1. identify files matching `checkpoint-YYYY-MM-DD-HHMM-<slug>.md`
2. propose renames to `checkpoint-YYYY-MM-DD-HHMM.md`, using the smallest suffix only when a collision exists
3. update checkpoint wikilinks that reference renamed notes
4. apply the migration only through lint's normal proposal/approval path; do not rename checkpoint files silently
5. record the checkpoint filename migration in `<wiki root>/log.md`

When `<wiki root>/entities/` contains stranded code pages (pages with `source_path:` front matter):

1. identify all `entities/*.md` files that have `source_path:` front matter (these are code-graph pages, not prose entity pages)
2. create `<wiki root>/code/` if it does not exist
3. for each stranded page, check if `<wiki root>/code/<slug>.md` already exists:
   - **No collision** → move `entities/<slug>.md` to `code/<slug>.md`
   - **Collision** → do NOT overwrite; report the collision as an unresolved finding and leave `entities/<slug>.md` in place for manual resolution
4. wikilinks stay `[[slug]]` — Obsidian resolves by filename, not path, so no link rewrites are needed
5. rebuild the generated code hub; remove affected direct root-index entries
6. append a migration entry to `<wiki root>/log.md`:
   ```md
   ## [YYYY-MM-DD] migrate | code entities → code/
   ```
   Capture: count of pages moved, count of collisions reported as unresolved.
7. refresh qmd if ready (the page paths changed)

Allowed direct fixes:

1. updating `index.md` to match actual durable prose with `## Overview`
2. consolidating safe structural content from legacy `overview.md` into `index.md`
3. deleting legacy `overview.md` after useful content preserved
4. resolving obvious broken `[[wikilinks]]`
5. adding missing cross-links and reciprocal backlinks
6. creating minimal entity/concept/topic pages when strongly justified
7. adding contradiction or stale-claim notes when wiki already contains the evidence
8. improving headings or descriptions for index navigability
9. normalizing internal links to canonical `[[kebab-case-note-name]]` form
10. moving only a misplaced nested `<wiki root>/.obsidian/` directory to the parent directory root when the destination has no `.obsidian/` directory
11. reconciling stale `<wiki root>/.wiki-metadata.json` paths to the actual resolved wiki root
12. renaming legacy checkpoint files from `checkpoint-YYYY-MM-DD-HHMM-<slug>.md` to `checkpoint-YYYY-MM-DD-HHMM.md` and updating their checkpoint wikilinks when the mapping is collision-free and local
13. moving stranded code pages from `entities/` to `code/`, rebuilding the hub, and logging the migration

Do not: ingest new raw sources, invent facts, silently merge/rename notes, silently delete disagreement/uncertainty, leave redundant `overview.md`, overwrite or merge an existing parent `.obsidian/`, move or rename `<wiki root>` or any wiki content directory, perform broad rewrites, or modify raw-source files.

### Rotate log.md if needed

Check `<wiki root>/log.md` line count. If it exceeds 500 lines:

1. Move entries older than the most recent 50 to `<wiki root>/log-archive/YYYY-MM.md` (create the directory if missing).
2. Replace the moved content in `log.md` with a single pointer line: `## [YYYY-MM-DD] rotate | archived <N> entries to log-archive/YYYY-MM.md`
3. The active `log.md` should stay under ~250 lines after rotation.

Beyond rotation, the only per-pass `log.md` write lint performs is the completion marker below. Lint is otherwise read-only with respect to `log.md`. Structural exceptions already defined above (code-page migration, metadata reconciliation, vault placement, checkpoint filename migration) may append structural log entries.

### Record the lint-check marker

On successful completion of the pass, append one line to `<wiki root>/log.md`:

```md
## [YYYY-MM-DD] lint-check | <scope>
```

This is the stable evidence `loamstate` reads for the `memory_lint_stale` hint (stale after 7 days). Write it even when the pass found nothing to fix — the marker records that the check ran.

### Check date format drift

Run `datecheck` to scan all markdown files for date-format drift:

```bash
bash "${LOAM_SKILL_DIR:-${CLAUDE_SKILL_DIR}}/../loam-using/scripts/loam.sh" datecheck check "$WIKI_ROOT" 2>/dev/null \
  || powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "${LOAM_SKILL_DIR:-${CLAUDE_SKILL_DIR}}/../loam-using/scripts/loam.ps1" datecheck check "$WIKI_ROOT" 2>/dev/null
```

The script reports drift as JSON: front matter point-in-time fields missing timezone offsets, legacy TZ labels (`SAST`, `GMT+N`, `UTC`), and decisions-log entries using non-em-dash separators.

Canonical formats are defined in `loam-using/references/date-formats.md`.

If drift is found:
1. Report the findings to the user.
2. After approval, run `loam.sh datecheck fix "$WIKI_ROOT" --offset <local-offset>` to apply normalizations.
3. Re-run `loam.sh datecheck check` to confirm zero drift.

This check is read-only — `check` mode never writes. `fix` mode is only run after explicit approval, same as checkpoint filename migration.

### Refresh qmd after writes

If qmd was ready and you wrote to the wiki, run `qmd update -c <collection> 2>/dev/null`. If refresh fails, report it but do not roll back wiki edits.

---

## Step 2G — Goal-health pass (report-only)

The goal pass runs when `goals/` exists (`ls goals/*.md 2>/dev/null`), even if `loamstate` reports no wiki. It is structurally separate from wiki linting, report-only, and does not alter files or append a wiki lint log entry solely for goal checks.

Read `${LOAM_SKILL_DIR:-${CLAUDE_SKILL_DIR}}/references/lint-checklist.md` and apply the **Goal health** section. It checks: missing required front matter or sections, invalid lifecycle status, stale drafts (30 days), overdue or stale active goals (`next_review_at`, otherwise `reviewed_at` or fallback `updated_at` at 90 days), broken linked spec/plan paths, inconsistent `goals/INDEX.md` rows, achieved without a passing review, and `reviewed_at` inconsistent with the newest review entry (compare date portion only).

Paused, achieved, and abandoned goals are exempt from staleness checks. Report goal findings in a separate section. Do not alter goal files. Route corrections through `/loam::setting-goals`.

---

## Step 3 — Report back

```md
<Wiki lint | Goal lint> completed for <scope>

### Fixed now

- <issue or "none">

### Annotated but unresolved

- <issue or "none">

### Goal health findings

- <goal finding or "none — no goals directory" or "none — goals healthy">

### Touched pages

- <path or "none — goals-only workspace">

### Next useful command

- `/loam::adding-to-memory <local source path or topic>` or `/loam::setting-goals <goal path>` when goal findings exist
```

If the pass found no significant issues, say so explicitly and still note any residual risks or thin areas. In a goals-only workspace, report as "Goal lint completed for <scope>" with wiki-only sections set to "none."

---

## Rules

- Read the wiki schema before editing.
- Prefer direct fixes for objective structural drift.
- Maintain `index.md` as the root hub for durable prose, with a concise `## Overview` near the top and one generated code-hub link.
- Prefer canonical `[[kebab-case-note-name]]` links for durable internal references.
- Preserve contradictions and uncertainty unless existing wiki evidence genuinely settles them.
- Do not modify raw-source files.
- Do not turn lint into source ingestion.
- Treat a separate root `overview.md` as legacy drift. Consolidate into `index.md` and remove during lint.
- Treat `<wiki root>/.obsidian/` as misplaced Obsidian config when `<wiki root>` is a subdirectory. Move only `.obsidian/` to the parent directory root when that destination has no `.obsidian/` directory.
- Reconcile stale `.wiki-metadata.json` to the actual resolved wiki root. Lint updates metadata to match the on-disk wiki; it never moves the on-disk wiki to match metadata.
- Own checkpoint filename migration. New checkpoints should be named `checkpoint-YYYY-MM-DD-HHMM.md`; lint may rename legacy slugged checkpoint files and update checkpoint wikilinks when the mapping is collision-free and local.
- Own code-page migration. Lint may move `entities/*.md` pages with `source_path:` front matter to `code/` and rebuild the generated hub. Wikilinks need no change (Obsidian resolves by filename). On collision with an existing `code/<slug>.md`, do not overwrite; report unresolved. Append a `## [YYYY-MM-DD] migrate | code entities → code/` log entry (structural change exception to the no-per-pass-entry rule).
- Never move or rename `<wiki root>` or any wiki content directory as part of `.obsidian/` placement or qmd metadata repair.
- Rotate `<wiki root>/log.md` when it exceeds 500 lines; lint does not append per-pass entries to `log.md`.
- Check date format drift with `loam.sh datecheck check`; canonical formats are in `loam-using/references/date-formats.md`. Apply only unambiguous local normalizations and report ambiguous drift.
- Check that qmd excludes `.archive/**`; flag missing archive exclusion as a health issue.
- Flag pages older than 90 days that cite volatile surfaces for re-validation; do not auto-archive them.
- Keep the note graph traversable, not just the index accurate.
- qmd is secondary. Structural checks (inventory, orphans, wikilinks, .obsidian placement, checkpoint filenames) remain Glob- and Grep-led. Use qmd (the protocol in `loam::using`) only for content discovery: stale claims, contradictions, and related-note neighborhoods.
- After wiki edits, refresh qmd if the collection is ready. If refresh fails, report it but do not roll back.
- If qmd is unavailable, unmapped, or degraded, continue without it. The skill must not fail.
- Goal lint is report-only. It runs when `goals/` exists, even without a wiki. It does not alter goal files or append a wiki lint log entry. Route corrections through `/loam::setting-goals`.
