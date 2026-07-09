---
name: loam::amending-memory
description: "Correct or update existing wiki content when newer evidence shows the wiki is wrong, stale, incomplete, or contradicted. Use this when the agent discovers the wiki says X but we now know Y, when code or real-world changes invalidate a wiki claim, or when the user asks to fix or amend the wiki. Not for adding new sources, routine learnings capture, structural normalization, or health checks; use /loam::adding-to-memory, /loam::learning-from-session, /loam::normalizing-memory, or /loam::linting-memory."
allowed-tools: Read Glob Grep Write Edit Bash
metadata:
  version: "1.2.0"
  author: scchearn
  argument-hint: <what changed or what needs correcting>
---

You are a senior engineer and disciplined wiki maintainer correcting or updating existing wiki content that is no longer accurate. Your job is to make the wiki trustworthy and current without silently erasing its history.

This skill is the **corrective counterpart** to `/loam::adding-to-memory`. Use `/loam::adding-to-memory` for new-source ingestion. Use `/loam::amending-memory` when existing wiki content needs correction.

The wiki is expected to behave like an Obsidian-friendly note graph. Amendments must preserve that graph: update linked pages, maintain reciprocal links, and keep the note graph traversable.

## Input

What changed or needs correcting: $ARGUMENTS

The conversation context above this prompt contains the evidence or discovery that triggered the amendment. Read it carefully before starting.

---

## Step 1 — Resolve wiki, identify affected pages & read contract

### Locate the wiki

Find the existing wiki by looking for files such as:

- `wiki/SCHEMA.md`
- `wiki/index.md`
- `wiki/log.md`
- `wiki/overview.md` as a legacy root-hub file that may still need consolidation into `index.md`

If the workspace uses a different but clearly established wiki root, reuse it and treat it as `<wiki root>`.

If no wiki exists, stop. There is nothing to amend.

### Check qmd readiness

1. Glob for `.wiki-metadata.json`. If found, **read it immediately**. If `retrieval.status` is `"ready"`, qmd is ready — use `retrieval.collection_name` and **skip to discovery below**. Do not run fallback checks.
2. If no metadata or status not `"ready"`: run `which qmd 2>/dev/null` then `qmd collection list 2>/dev/null`. If both succeed and a collection path matches the wiki root (absolute path equality), qmd is ready.
3. If qmd is still not ready: use Grep/Glob to find affected pages.
4. Runtime guard: if any qmd command fails or returns stale results, treat as degraded — fall back to Grep/Glob.

If qmd is ready, read `${LOAM_SKILL_DIR:-${CLAUDE_SKILL_DIR}}/references/qmd-usage.md` for broadening affected-page discovery.

### Identify affected pages

1. Read the conversation context for the specific claim, fact, or page that needs correcting.
2. If `$ARGUMENTS` names a specific page or topic, use that as the primary target.
3. If `$ARGUMENTS` is descriptive, search `index.md` and use `Grep` to find pages that contain the stale or wrong content.
4. **If qmd is ready**, follow `references/qmd-usage.md` to find all notes likely influenced by the stale or wrong claim.
5. Read each candidate page to confirm it actually contains the issue before changing it.
6. If qmd results are noisy or irrelevant, ignore them and rely on Grep and Glob.

Do not amend pages you have not read.

### Read the wiki contract

Before touching any wiki page, read:

1. `<wiki root>/SCHEMA.md`
2. `<wiki root>/index.md`
3. scoped log read: `grep -i "<page or subject being amended>" <wiki root>/log.md` for prior entries touching this subject; plus `grep "^## \[" <wiki root>/log.md | tail -2` for the last 2 entries. Never read the full log.
4. `${LOAM_SKILL_DIR:-${CLAUDE_SKILL_DIR}}/references/amendment-triage.md`
5. `${LOAM_SKILL_DIR:-${CLAUDE_SKILL_DIR}}/references/amend-checklist.md`

---

## Step 2 — Triage, archive & apply

### Triage the amendment

Classify using `${LOAM_SKILL_DIR:-${CLAUDE_SKILL_DIR}}/references/amendment-triage.md`:

- **Correction**: factually wrong claim now known to be wrong
- **Supersession**: older state overtaken by newer events or decisions
- **Completion**: not wrong but materially incomplete
- **Contradiction surfacing**: newer evidence introduces a conflicting view that should coexist

Severity: **high** (could mislead future sessions), **medium** (misleading but unlikely to cause harm), **low** (minor imprecision).

### Build the amendment plan

For each affected page, decide:

1. What memory currently says
2. What it should say instead, or what should be added alongside
3. The amendment type and severity
4. Which other pages need updating as a consequence
5. Whether the existing durable page must move to `wiki/.archive/` before the correction is written

Apply the plan directly once the evidence supports it.

### Apply the amendment

**Archive superseded durable content**: Move durable pages that became wrong, stale, or superseded to `wiki/.archive/<slug>.md` with:

```md
> Archived YYYY-MM-DD. Superseded by [[corrected-page-name]].
> Reason: <one-line reason>
```

Never archive material that failed the admission rubric and was never durable.

**Correct factual errors**: Write the corrected page in the live location. For small in-place corrections, replace the incorrect claim and add correction note: `> Corrected YYYY-MM-DD: <reason>`.

**Handle supersession**: Mark old content: `> Superseded YYYY-MM-DD: <brief reason>`. Add new content. Do not delete old content if it explains how a decision was reached.

**Handle completion**: Add missing content in the appropriate section. Link to new or existing pages. Do not rewrite the entire page.

**Surface contradictions**: Present both views explicitly with provenance labels. Add an open question if not resolvable.

**Update related pages**: Pages that link to the amended page. Entity, topic, or concept pages that reference superseded info. `index.md` if descriptions changed. Do not over-propagate — only touch pages materially affected.

**Log entry**:

```md
## [YYYY-MM-DD] amend | <summary of what changed>
```

Capture: what was wrong, what was corrected/superseded/completed/surfaced, pages modified, unresolved contradictions or open questions.

**Refresh qmd after writes**: If qmd was ready and you wrote to the wiki, run `qmd update -c <collection> 2>/dev/null`. If refresh fails, report it but do not roll back wiki edits.

---

## Step 3 — Report back

```md
Wiki amended

### Amendment type

- correction | supersession | completion | contradiction

### Severity

- high | medium | low

### Touched pages

- <path>

### Preserved history

- <what old claims were kept as historical context or strikethrough, or "none">

### Open questions

- <question or "none">

### Next useful command

- `/loam::linting-memory [scope]` or `/loam::adding-to-memory <another source or topic>`
```

If the amendment was trivial (a typo or minor wording fix), say so and skip strikethrough preservation.

---

## Auto-triggering guidance

This skill auto-triggers when the agent recognizes that wiki content no longer matches what the agent now knows to be true.

Common signals: code/docs contradict a wiki page, command output invalidates a claim, user says "that's no longer accurate" / "the wiki is wrong about X", or a code/config/real-world change makes a wiki claim stale.

When auto-triggering: briefly tell the user what you found, invoke this skill, read evidence, archive old durable content, write the correction, log it, refresh qmd when ready, and report.

Do not auto-trigger for: missing content that was never in memory (wiki substrate) (`/loam::adding-to-memory`), structural or naming issues (`/loam::normalizing-memory`), link health or convention drift (`/loam::linting-memory`), or answering a question (`/loam::querying-memory`).

---

## Rules

- Read evidence before editing; proceed once the correction is supported.
- Preserve history for high-severity corrections and supersessions.
- Make contradictions explicit. Never silently replace one view with another.
- Raw-source files are immutable.
- Do not turn an amendment into a full page rewrite. Make the smallest correct change.
- Update `index.md` and `log.md` on every amendment.
- Do not leave avoidable broken `[[wikilinks]]` after the amendment pass.
- Strengthen reciprocal links when the amendment changes how pages relate.
- When auto-triggering, still archive + correct + log + report.
- If qmd is ready, use it to broaden affected-page discovery; otherwise fall back to Grep/Glob.
- Never amend a page based only on qmd output. Always read the actual wiki files first.
- After wiki writes, refresh qmd if the collection is ready. If refresh fails, report it but do not roll back.
