---
name: loam::linting-memory
description: "Run a health check on existing memory (the wiki substrate). Use this when the user wants to lint the wiki, health-check the knowledge base, find orphan pages, spot broken or missing cross-links, clean up stale claims and unresolved wikilinks with safe local fixes, or consolidate a legacy root `overview.md` into `index.md`. Not for adding new material; use /loam::adding-to-memory or /loam::learning-from-session for that."
allowed-tools: Read Glob Grep Write Edit Bash
metadata:
  version: "1.0.0"
  author: scchearn
  argument-hint: [wiki root or focus area]
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

Apply safe, local fixes directly. For issues that need new evidence or substantive judgment, annotate and report them instead of guessing.

## Input

The lint target is: $ARGUMENTS

If no explicit target is provided, lint the whole wiki.

---

## Step 1 — Resolve wiki, read contract & audit

### Locate the wiki and probe state

Run `loamstate` to probe the wiki and qmd in one shot:

```bash
bash "${CLAUDE_SKILL_DIR}/../loam-using/scripts/loamstate.sh" "$(pwd)" 2>/dev/null \
  || powershell "${CLAUDE_SKILL_DIR}/../loam-using/scripts/loamstate.ps1" "$(pwd)" 2>/dev/null
```

Parse the JSON output. If `exists` is false, stop and recommend:

```text
/loam::scaffolding-wiki <topic or wiki goal>
```

Use `wiki_root` as the resolved wiki root (resolved from on-disk contract files, not qmd metadata). If `has_overview` is true, note it as a legacy root-hub file to fold into `index.md`. Then resolve the lint scope: if the user named a wiki root, subdirectory, topic, or entity, use that. If no scope given, lint the whole wiki.

If multiple candidate wiki roots exist and the target is ambiguous, ask the smallest possible follow-up question.

Runtime guard: if `loamstate` fails or returns invalid JSON, fall back to Globbing for `SCHEMA.md`, `index.md`, or `log.md` and manual qmd checks.

### Read the wiki contract

Before editing, read:

1. `<wiki root>/SCHEMA.md`
2. `<wiki root>/index.md`
3. the most recent relevant parts of `<wiki root>/log.md`
4. `<wiki root>/overview.md` when it exists, so you can fold its useful root-hub content into `index.md` and remove it
5. the files inside the lint scope most relevant to the current health check
6. `${CLAUDE_SKILL_DIR}/references/lint-checklist.md`
7. `${CLAUDE_SKILL_DIR}/references/finding-triage.md`

Use `Glob` and `Grep` to map the pages in scope before reading deeply.

Treat `index.md` as the authoritative root hub. The desired steady state is a single root-hub file: `index.md` with a concise `## Overview` section before the grouped page catalog.

### qmd metadata health (secondary only)

The `loamstate` probe already resolved `qmd_ready`, `collection`, `metadata_status`, and `metadata_path`. If `metadata_path` is non-empty, compare its `retrieval.collection_path` to the actual resolved `<wiki root>` using absolute path equality. If they differ, mark qmd metadata as stale and plan a safe metadata reconciliation during fixes.

If metadata is stale and qmd is available, validate the recorded `collection` with `qmd collection show <collection> 2>/dev/null` when supported. If another collection is already registered for the actual `<wiki root>`, plan to update `collection_name` to that collection. If no collection points at the actual `<wiki root>`, do not rename or move wiki directories; set `retrieval.status` to `"degraded"`, keep the actual `<wiki root>` in `retrieval.collection_path`, and report the collection/path mismatch.

If `qmd_ready` is false, qmd is not available — use Grep/Glob only.

qmd is **secondary only** in this skill: use it only to find related-note neighborhoods when a structural fix might need reciprocal links or nearby canonical notes. If ready, read `${CLAUDE_SKILL_DIR}/references/qmd-usage.md`.

### Audit for health issues

**A. Structure and inventory** — Check for: `index.md` missing `## Overview` section; legacy `overview.md` still present; content stranded in `overview.md` instead of `index.md`; duplicated root-hub content; pages on disk but missing from index; index entries pointing to non-existent pages; duplicate note identities; filename convention drift; near-duplicate pages; empty or placeholder pages.

**B. Link health** — Check for: unresolved `[[wikilinks]]`; orphan pages; pages with no meaningful inbound or outbound links; pages only discoverable from `index.md`; pages that should link but don't; missing reciprocal backlinks; repeated entity/concept mentions without a dedicated page.

**C. Knowledge integrity** — Check for: contradictory statements; stale claims superseded by later ingests; broad synthesis pages out of date; under-sourced claims.

**D. Maintenance signals** — Check for: recent ingests not in index; lint-worthy gaps never reconciled in log; missing follow-up notes.

**E. Obsidian config placement** — Check whether `<wiki root>/.obsidian/` exists. The desired layout is for `.obsidian/` to live at the parent directory root that contains the wiki, not nested inside the wiki directory, unless `<wiki root>` is itself the project/workspace root.

**F. qmd metadata integrity** — Check whether `<wiki root>/.wiki-metadata.json` reflects the actual resolved `<wiki root>`. Lint reconciles metadata to the on-disk wiki; it must never rename, move, or recreate the wiki directory to match stale metadata.

Distinguish: **fix now** (safe from existing wiki evidence) vs **annotate now** (mark but don't resolve) vs **follow-up** (needs future evidence/research/user direction).

**Expand with qmd (secondary, if ready)**: Follow `references/qmd-usage.md` to find related-note neighborhoods for orphan pages or missing cross-links.

---

## Step 2 — Apply safe fixes, record & refresh

### Apply safe fixes

Make the smallest correct edits that improve wiki health.

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

Allowed direct fixes:

1. updating `index.md` to match actual durable pages with `## Overview`
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

Do not: ingest new raw sources, invent facts, silently merge/rename notes, silently delete disagreement/uncertainty, leave redundant `overview.md`, overwrite or merge an existing parent `.obsidian/`, move or rename `<wiki root>` or any wiki content directory, perform broad rewrites, or modify raw-source files.

### Record the lint pass

Append to `<wiki root>/log.md`:

```md
## [YYYY-MM-DD] lint | <scope>
```

Capture: scope, pages created/updated/removed, issues fixed, unresolved contradictions/gaps, legacy root-hub consolidation, `.obsidian/` placement fixes or unresolved conflicts, qmd metadata reconciliation or degraded state, suggested next ingests.

Keep `log.md` append-only. Ensure `index.md` reflects the final state before finishing.

### Refresh qmd after writes

If qmd was ready and you wrote to the wiki, run `qmd update -c <collection> 2>/dev/null`. If refresh fails, report it but do not roll back wiki edits.

---

## Step 3 — Report back

```md
Wiki lint completed for <scope>

### Fixed now

- <issue>

### Annotated but unresolved

- <issue or "none">

### Touched pages

- <path>

### Next useful command

- `/loam::adding-to-memory <local source path or topic>`
```

If the pass found no significant issues, say so explicitly and still note any residual risks or thin areas.

---

## Rules

- Read the wiki schema before editing.
- Prefer direct fixes for objective structural drift.
- Maintain `index.md` as the single authoritative root hub with a concise `## Overview` section near the top.
- Prefer canonical `[[kebab-case-note-name]]` links for durable internal references.
- Preserve contradictions and uncertainty unless existing wiki evidence genuinely settles them.
- Do not modify raw-source files.
- Do not turn lint into source ingestion.
- Treat a separate root `overview.md` as legacy drift. Consolidate into `index.md` and remove during lint.
- Treat `<wiki root>/.obsidian/` as misplaced Obsidian config when `<wiki root>` is a subdirectory. Move only `.obsidian/` to the parent directory root when that destination has no `.obsidian/` directory.
- Reconcile stale `.wiki-metadata.json` to the actual resolved wiki root. Lint updates metadata to match the on-disk wiki; it never moves the on-disk wiki to match metadata.
- Never move or rename `<wiki root>` or any wiki content directory as part of `.obsidian/` placement or qmd metadata repair.
- Update `<wiki root>/log.md` on every lint pass.
- Keep the note graph traversable, not just the index accurate.
- Keep `index.md` aligned with the durable pages that exist after the pass.
- qmd is secondary. Structural checks remain Glob- and Grep-led. Use qmd only to find related-note neighborhoods.
- After wiki edits, refresh qmd if the collection is ready. If refresh fails, report it but do not roll back.
- If qmd is unavailable, unmapped, or degraded, continue without it. The skill must not fail.
