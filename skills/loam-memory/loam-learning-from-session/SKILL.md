---
name: loam-learning-from-session
description: "Review the current session for durable learnings, then route each one to the right durable surface: a wiki page (proposal-first, via the wiki-page workflow) or an agent guidance file (concise one-liner edits, via the guidance-file workflow). Use when the session uncovered decisions, architecture facts, commands, conventions, gotchas, or open questions that future sessions should inherit. Not for source ingestion or correcting stale wiki claims; use /loam::adding-to-memory or /loam::amending-memory."
allowed-tools: Read Glob Grep Write Edit Bash
metadata:
  version: "1.0.0"
  author: scchearn
  argument-hint: [topic or session summary]
  disable-model-invocation: true
---

You are a senior engineer reviewing the current session for durable learnings. Two writing paths are available, and routing is itself a feature of this skill:

- **wiki-page path** — for learnings that belong in the durable Obsidian-friendly wiki (topic, entity, concept, analysis pages). Proposal-first; reads the wiki schema, qmd graph, index, and log before editing.
- **guidance-file path** — for learnings about how to work in this repo that belong in `AGENTS.md`, `CLAUDE.md`, or `.claude.local.md` as concise one-liners that future agent sessions will inherit as prompt context.

This skill is a **proposal-first** router. It does not write until the user approves.

## Input

The session focus is: $ARGUMENTS

If no explicit focus is provided, derive the narrowest useful focus from the current conversation context and say what you chose.

---

## Step 1 — Reflect on the session

Review the conversation context. Extract only durable learnings:

- commands or workflows that worked
- architecture facts or repo structure discovered
- decisions made and constraints clarified
- conventions, gotchas, or recurring pitfalls
- durable open questions worth preserving
- environment or configuration quirks about this repo that an agent would benefit from

Ignore transient chatter, one-off dead ends, and speculative guesses that were never validated.

Distinguish: **facts** (supported by code, files, commands, or explicit user confirmation), **inferred conventions** (likely patterns not fully confirmed), **open questions** (unresolved items for future sessions).

## Step 2 — Classify each learning (the router)

For each candidate learning, choose the destination surface. Classification is itself a feature: the right surface depends on **who consumes the learning** and **what shape it takes**.

### Route to the wiki-page path when:

- The learning is a fact, decision, architecture note, or analysis that any future session (agent or human) reading memory would benefit from.
- The learning deserves a topic, entity, concept, or analysis page (or an update to one).
- The learning is worth cross-linking into the wiki graph.
- The learning is durable knowledge about the project, not about how to instruct an agent.

### Route to the guidance-file path when:

- The learning is a command, pattern, test approach, environment quirk, or gotcha that future **agent sessions** in this repo need to know.
- The right shape is a concise one-liner that lives in prompt context, not a wiki paragraph.
- The destination is `AGENTS.md` (team-shared, harness-agnostic), `CLAUDE.md` (team-shared Claude-specific), or `.claude.local.md` (personal/local, gitignored).
- The learning is about **how to work here**, not about **what is true here**.

### Mixed routing is allowed and expected

A single session may produce both kinds. Keep them separate in the proposal: wiki-bound learnings in one section, guidance-bound learnings in another. A learning that would fit both surfaces should default to the guidance-file path only if it is short and instruction-shaped; otherwise route to memory (wiki substrate).

### Defer or route elsewhere when:

- The learning is not durable enough — defer.
- The learning reveals memory is wrong, stale, or contradicted — route to `/loam::amending-memory`.
- The learning is a new source to ingest (a file, article, transcript) — route to `/loam::adding-to-memory`.
- The learning is an unresolved open question — preserve as such, do not settle it silently.

---

## Step 3 — Per-path workflow

### 3A — Wiki-page path

If the session produced any wiki-bound learnings, run the proposal-first wiki workflow below. If none, skip 3A entirely.

#### Find the wiki

Look for files such as:

- `wiki/SCHEMA.md`
- `wiki/index.md`
- `wiki/log.md`
- `wiki/overview.md` as a legacy root-hub file that may still need consolidation into `index.md`

If the workspace uses a different but clearly established wiki root, reuse it.

If no wiki exists yet, stop and recommend:

```text
/loam::scaffolding-wiki <topic or wiki goal>
```

#### Check qmd readiness

1. Glob for `.wiki-metadata.json`. If found, **read it immediately**. If `retrieval.status` is `"ready"`, qmd is ready — use `retrieval.collection_name` and **skip to discovery below**. Do not run fallback checks.
2. If no metadata or status not `"ready"`: run `which qmd 2>/dev/null` then `qmd collection list 2>/dev/null`. If both succeed and a collection path matches the wiki root (absolute path equality), qmd is ready.
3. If qmd is still not ready: use Grep/Glob to find destination pages.
4. Runtime guard: if any qmd command fails or returns stale results, treat as degraded — fall back to Grep/Glob.

If qmd is ready, read `${CLAUDE_SKILL_DIR}/references/qmd-usage.md` for finding existing destination notes.

#### Read the wiki contract & discover destinations

Read before proposing:

1. `<wiki root>/SCHEMA.md`
2. `<wiki root>/index.md`
3. the most recent relevant parts of `<wiki root>/log.md`
4. `<wiki root>/overview.md` when it exists and may still contain legacy root-hub context
5. the existing pages directly related to the learnings you want to capture

Resolve the scope: if `$ARGUMENTS` names a topic, use that as primary focus. Otherwise derive from the session. Use `index.md` and `Grep` to find directly related pages. Do not propose edits to a page you have not read.

**If qmd is ready**, follow `references/qmd-usage.md` to find the best existing destination note for each candidate learning.

**If qmd is not ready**, use Grep and Glob to find existing pages that may already cover the learning.

#### Decide per-learning outcome

For each wiki-bound learning, decide whether it should:

- update an existing topic, entity, concept, or analysis page
- create a small new durable page because no existing page is a good fit
- be preserved as an open question rather than a settled fact
- be deferred because it is not durable enough
- be routed to `/loam::amending-memory` because it reveals memory is wrong or stale

Rules:

1. Prefer updating existing pages over creating new ones.
2. **Direct updates only** — do not create a conversation-source note in this skill.
3. Create a new durable page only when the learning is central, reusable, and poorly served by existing notes.
4. Keep additions concise and durable. Do not dump session transcripts.
5. If a learning contradicts existing memory claim, surface it and recommend `/loam::amending-memory`.
6. If a claim came from discussion but was not validated, label it as discussed, suggested, or pending.

### 3B — Guidance-file path

If the session produced any guidance-bound learnings, run the concise one-liner workflow below. If none, skip 3B entirely.

#### Find the guidance files

```bash
find . \( -name "AGENTS.md" -o -name "CLAUDE.md" -o -name ".claude.local.md" \) 2>/dev/null | head -20
```

Decide where each addition belongs:

- `AGENTS.md` — team-shared, harness-agnostic guidance
- `CLAUDE.md` — team-shared Claude-specific guidance when the repo uses it
- `.claude.local.md` — personal/local only (gitignored)

#### Draft additions

**Keep it concise** — one line per concept. Guidance files are part of the prompt, so brevity matters.

Format: `<command or pattern>` — `<brief description>`

Avoid:

- Verbose explanations
- Obvious information
- One-off fixes unlikely to recur

---

## Step 4 — Show the proposal (do not edit yet)

Present a single unified proposal covering both paths. If only one path has entries, show only that one.

```md
## Session Learnings Proposal

### Summary
- Learnings reviewed: <count>
- Wiki-page path: <count>
- Guidance-file path: <count>
- New wiki pages: <count>
- Needs amend instead: <count or "none">
- Deferred: <count or "none">

### Wiki updates

#### Update: <page path>

**Why:** <one-line reason>

```diff
+ <concise proposed addition>
```

#### New page: <page path>

**Why:** <one-line reason>

```md
# <title>
...
```

#### Index and log
- `index.md`: <what will change, or "unchanged">
- `log.md`: append `## [YYYY-MM-DD] learnings | <session focus>`

### Guidance-file updates

#### Update: ./AGENTS.md

**Why:** <one-line reason>

```diff
+ <the addition - keep it brief>
```

### Defer or route elsewhere
- <item>: not durable enough | should use `/loam::amending-memory` | still unresolved

### Open questions
- <question or "none">
```

Then ask:

> "Does this learnings proposal look right? If yes, I'll apply it. If anything should be added, removed, or made more conservative, tell me and I'll revise it first."

Wait for explicit confirmation.

---

## Step 5 — Apply the approved learnings

### 5A — Apply wiki-page path

1. Update the existing relevant pages.
2. Create any approved new durable page using a canonical kebab-case filename.
3. Update `index.md` if durable pages changed or discoverability improved materially.
4. Append `<wiki root>/log.md`:

```md
## [YYYY-MM-DD] learnings | <session focus>
```

Capture: session focus, pages updated or created, important learnings preserved, contradictions or items deferred, open questions left unresolved.

5. **Refresh qmd**: If qmd was ready and you wrote to the wiki, run `qmd update -c <collection> 2>/dev/null`. If refresh fails, report it but do not roll back wiki edits.

Do not create a conversation-source note in this skill.

### 5B — Apply guidance-file path

Only edit the files the user approved. Keep additions to one line per concept.

---

## Step 6 — Report back

```md
Session learnings applied for <session focus>

### Wiki touched pages
- <path or "none">

### Wiki new pages
- <path or "none">

### Guidance files touched
- <path or "none">

### Index and log
- Index: <path or "unchanged">
- Log: <path or "unchanged">

### Deferred or needs amend
- <item or "none">

### Open questions
- <question or "none">

### Next useful command
- `/loam::adding-to-memory <local source path>` or `/loam::amending-memory <what changed>`
```

If the review found nothing durable enough to add, say so explicitly and do not make any edits.

---

## Rules

- Review the current session before scanning either surface.
- Read the wiki schema before proposing wiki edits.
- Read existing guidance files before proposing guidance edits.
- Proposal first, apply second — always.
- The classification (wiki vs guidance) is a feature. Do not collapse the two paths into one.
- Wiki path: direct page updates only. Never create a conversation-source note in this skill.
- Wiki path: prefer existing pages over new pages. Use qmd to find existing destination notes when ready; fall back to Grep/Glob when not ready.
- Wiki path: never edit a wiki page based only on qmd output. Always read the actual wiki files first.
- Wiki path: keep additions concise, durable, and attributable to the session.
- Wiki path: route corrections and supersessions to `/loam::amending-memory` instead of silently fixing them here.
- Guidance path: keep it concise — one line per concept. Guidance files are part of the prompt.
- Guidance path: avoid verbose explanations, obvious information, and one-off fixes unlikely to recur.
- Do not fetch external sources in this skill.
- Do not modify raw-source files.
- Update `<wiki root>/log.md` on every approved wiki learnings pass.
- After wiki writes, refresh qmd if the collection is ready. If refresh fails, report it but do not roll back.