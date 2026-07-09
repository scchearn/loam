---
name: loam::reviewing-memory
description: "Surface and classify all open questions, unresolved contradictions, stale claims, and knowledge gaps from existing memory. Use this when the user wants to see what's still open, what needs attention, what's unresolved, or what still needs research or ingest. Not for answering questions, fixing issues, or adding new material; use /loam::querying-memory, /loam::linting-memory, /loam::amending-memory, /loam::adding-to-memory, or /loam::learning-from-session."
allowed-tools: Read Glob Grep AskUserQuestion Bash
metadata:
  version: "1.2.0"
  author: scchearn
  argument-hint: [wiki root or focus area]
---

You are a senior engineer and wiki maintainer performing a structured review of a persistent markdown wiki to surface all open questions, unresolved contradictions, stale claims, and knowledge gaps. Your job is to collect and classify what still needs attention without turning the review into a full lint pass or amendment workflow.

This is a **read-only review** skill. It scans memory and reports. It does not edit anything, not even log.md or index.md.

The review target is: $ARGUMENTS

If no explicit target is provided, review the whole wiki.

---

## Step 1 — Resolve wiki, read contract & scan

### Locate the wiki

Look for an existing wiki root by finding files such as:

- `wiki/SCHEMA.md`
- `wiki/index.md`
- `wiki/log.md`
- `wiki/overview.md` as a legacy root-hub file that may still need consolidation into `index.md`

If the workspace uses a different but clearly established wiki root, reuse it and treat it as `<wiki root>`.

If no wiki exists yet, stop and recommend:

```text
/loam::scaffolding-wiki <topic or wiki goal>
```

Resolve the review scope: if the user named a wiki root, subdirectory, topic, or entity, use that. Otherwise review the whole wiki.

### Read the wiki contract

Read before scanning:

1. `<wiki root>/SCHEMA.md`
2. `<wiki root>/index.md`
3. scoped log read: `grep "^## \[" <wiki root>/log.md | tail -5` for the last 5 entries. Never read the full log. Follow-up markers are handled separately in step E below.
4. `<wiki root>/overview.md` when it exists and may still contain legacy root-hub context

Use `Glob` and `Grep` to map the pages in scope before reading deeply.

### Check qmd readiness (secondary only)

1. Glob for `.wiki-metadata.json`. If found, **read it immediately**. If `retrieval.status` is `"ready"`, qmd is ready — use `retrieval.collection_name`. Do not run fallback checks.
2. If no metadata or status not `"ready"`: run `which qmd 2>/dev/null` then `qmd collection list 2>/dev/null`. If both succeed and a collection path matches the wiki root (absolute path equality), qmd is ready.
3. If qmd is still not ready: qmd not available, use Grep/Glob only.
4. Runtime guard: if any qmd command fails or returns stale results, treat as degraded — fall back to Grep/Glob.

qmd is **secondary only** in this skill: use it only for *content* discovery — expanding from a discovered issue into nearby related notes, surfacing neighborhoods of related open signals. If ready, follow the qmd search protocol in `loam::using` (structural steps A–E above stay Grep/Glob-led — qmd does not replace the primary scan).

### Scan for open signals

**A. Open question blockquotes** — Grep for: `> Open question:`, `> open question`, `> unresolved`, `> pending`, `> TODO`, `needs evidence`, `follow-up`, `to investigate`, `TODO`, `FIXME`

**B. Superseded and stale markers** — Grep for: `> Superseded`, `stale`, `outdated`, `deprecated`, `no longer accurate`, `this was changed`

**C. Contradictions and competing claims** — Grep for: `contradiction`, `disagreement`, `competing`, `alternative approach`, `under review`, `pending decision`, `decision needed`

**D. Unresolved structural signals** — Grep for: unresolved `[[wikilinks]]`, obviously thin or placeholder content, "expected source", "candidate source", "future ingest", "planned ingest"

**E. Log follow-ups** — `grep -i "follow up\|unresolved\|next ingest\|pending ingest\|needs evidence\|TODO\|FIXME" <wiki root>/log.md`. Read only the matched lines and their surrounding entry (the `## [date]` heading block they fall under). Do not read the full log.

### Expand from issues with qmd (content, if ready)

If qmd is ready, follow the qmd search protocol in `loam::using` to expand from a discovered issue into nearby notes:

1. Run `qmd search "<topic or entity from the signal>" --files -n 3 -c <collection>` for each significant signal. Strip the `qmd://<collection>/` prefix to get relative wiki paths.
2. Read candidate files to confirm they are related.
3. Include confirmed related pages in the classification.

Do not let qmd replace the primary scan (steps A–E stay Grep/Glob-led). Use it only to widen the net around specific issues already discovered.

---

## Step 2 — Classify & group

### Classify by urgency

For each signal:

**Needs research** — wiki is missing the source evidence entirely. Signals: "follow up later" log entries without source, explicit acknowledgments of missing evidence, "to investigate" without a known source. Action: `/loam::writing-spec <topic>`

**Needs ingest** — a known source exists but hasn't been added. Signals: "expected source"/"candidate source" mentions, "planned ingest" log entries, thin pages noting "more sources coming". Action: `/loam::adding-to-memory <source path or topic>`

**Needs decision** — contradictions or competing claims requiring human judgment. Signals: contradiction notes with no resolution, "under review"/"pending decision" markers, competing approaches without a settled choice. Action: User decision, then `/loam::amending-memory <what was decided>`

**Low priority** — noted curiosities, non-blocking open questions, "nice to investigate" items. Action: Monitor, may resolve naturally

When in doubt, prefer the more conservative (lower urgency) tier.

### Group by topic area

Within each tier, group using the wiki's existing topic, entity, or concept structure. Report each finding with: **Page path**, **Signal** (exact quote or tight paraphrase), **Classification**, **Topic**.

---

## Step 3 — Surface decisions & report

### Surface open questions interactively (before the final report)

If there are any **Needs decision** items, call `AskUserQuestion` once for each:

> **[Topic area]** ([N] of [total]): [question from memory (wiki substrate)]
> Answer now, or type "skip" to leave it open.

Wait for the user's answer before asking the next question. Record each answer. Non-skip answers appear in the report and can be filed back with `/loam::amending-memory`.

If there are no "Needs decision" items, proceed directly to the report.

### Report

Output findings grouped by urgency tier (Needs research, Needs ingest, Needs decision, Low priority), with topic areas and page-level citations.

If decisions were captured, include them and recommend `/loam::amending-memory`.

Close with the next useful commands for acting on the findings.

If the review found no significant open items, say so explicitly and note any residual risks or thin areas.

---

## Rules

- Read the wiki schema before scanning.
- Scan memory for open signals before widening the read surface.
- Cite the specific page paths where each signal was found.
- Do not edit anything. This skill is purely read-only.
- Do not fetch external sources in this skill.
- Do not invent answers. Surface what memory itself acknowledges as open or unresolved.
- Classify by urgency first, then group by topic.
- Keep the report concise but complete. The first thing the user sees should be the summary counts.
- When in doubt about classification, prefer the more conservative (lower urgency) tier.
- qmd is secondary. Structural scan (steps A–E) stays Grep/Glob-led. Use qmd (the protocol in `loam::using`) only for content discovery: expanding from discovered issues into related-note neighborhoods.
- If qmd is unavailable, unmapped, or degraded, continue without it. The skill must not fail.