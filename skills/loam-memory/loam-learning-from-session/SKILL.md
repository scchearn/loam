---
name: loam::learning-from-session
description: "Review the current session for durable learnings, then route each one through the five-way matrix: wiki page, guidance file, checkpoint, task annotation/plan, or discard. Use when the session uncovered decisions, architecture facts, commands, conventions, gotchas, or open questions that future sessions should inherit. Not for source ingestion or correcting stale wiki claims; use /loam::adding-to-memory or /loam::amending-memory."
allowed-tools: Read Glob Grep Write Edit Bash
metadata:
  version: "1.3.0"
  author: scchearn
  argument-hint: [topic or session summary]
---

You are a senior engineer reviewing the current session for durable learnings. Five destinations are available, and routing is itself a feature of this skill:

- **wiki-page path** — for learnings that pass the canonical `loam-using` Durable-memory admission rubric and belong in the durable Obsidian-friendly wiki.
- **guidance-file path** — for learnings about how to work in this repo that belong in `AGENTS.md`, `CLAUDE.md`, or `.claude.local.md` as concise one-liners that future agent sessions will inherit as prompt context.
- **checkpoint path** — for resumable session state that belongs under `wiki/checkpoints/` via `/loam::checkpointing`.
- **task annotation / plan path** — for per-task context that belongs on the active unit of work.
- **discard path** — for build output, branch state, one-offs, unverifiable claims, and rubric failures.

This skill routes and writes directly. The agent owns classification and does not ask for pre-approval.

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

## Step 1.5 — Durability test

Before classifying, test every candidate learning. Failing any test means the
learning is not durable as written — either strip it to a reusable pattern
(Step 2) or drop it.

1. **Time test** — Would a fresh session 6 months from now benefit from this,
   in a similar but not identical situation? If only "today's session"
   benefits → not durable.
2. **Identity test** — Does this reference session-specific identifiers
   (agent names of the day, today's broken worktree, a version already
   shipped, a symlink already chmod'd, a specific thread ID)? If yes → strip
   them. If nothing durable remains after stripping → not durable.
3. **Incident vs pattern** — Is this "what happened today and how we fixed
   it" (incident) or "what is generally true and how to handle it" (pattern)?
   Pure incidents → route to `/loam::checkpointing`, not memory or guidance.
4. **Already-applied test** — Was the fix already applied and shipped? The
   fix itself is done; only the *generalizable lesson* is durable.

Reusable gotchas have a stricter capture bar: they must be written as
`Trigger → Mistake → Fix`, be reproducible from the description by a future
agent, and not be environment-specific. Operational session reports,
host/PATH/toolchain incidents, and one-off local failures do not enter durable
capture; checkpoint them if they matter for resumption. When a gotcha passes,
route the candidate gotcha to the skill that tripped the agent, either its `Common
Mistakes` section or `references/gotchas.md`.

A learning that passes only after stripping is still durable — the stripped
form is what gets routed.

## Step 2 — Classify each learning (the router)

For each candidate learning, choose the destination surface. Classification is itself a feature: the right surface depends on **who consumes the learning** and **what shape it takes**.

### Strip to reusable pattern (before routing)

For every learning that passed the durability test by stripping, do the strip
now, before choosing wiki vs guidance. Remove: dates, specific file paths,
specific agent names, specific version numbers, specific error strings,
specific thread or task IDs. Keep the reusable shape.

Worked example — incident wrapped around a durable kernel:

- ❌ as extracted: "On 2026-06-22 the integration broke; package x@1.19.2
  fixed it — `dist/**/index.js` must use explicit `.js` extensions in
  relative re-exports; Node native ESM rejects extensionless re-exports
  with `ERR_MODULE_NOT_FOUND`."
- ✅ after strip: "Node native ESM rejects extensionless relative
  re-exports (`ERR_MODULE_NOT_FOUND`); published `dist` barrels must use
  explicit `.js` extensions."

The date, the integration, the package, and the version are incident. The
Node ESM rule is the durable kernel.

Worked example — already-durable, no strip needed:

- ✅ "Provider list endpoints return bare arrays with no total-count
  envelope; exact record-level progress is not realistic."
- ✅ "Rate limits are not uniform across surfaces: the stats POST endpoint
  has a much lower per-minute budget than the GET surfaces; retry/backoff
  visibility matters for both UX and worker pacing."
- ✅ "Real testing requires an admin-enabled API token; there is no
  trustworthy sandbox path for end-to-end validation."

These are facts about the system, not about today. They pass the durability
test as written.

If stripping leaves only a date and a version, the learning was an incident,
not a pattern → route to `/loam::checkpointing` or drop.

### What fails the durability test (examples)

- ❌ "the `.worktrees/<feature>/.git/` directory is empty today" →
  transient workspace state. Not durable. Checkpoint, not memory.
- ❌ "agent foo and agent bar are general-purpose helpers" → agent names
  rotate per session. Not durable. If the *routing rule* is durable, strip
  the names: "general-purpose helpers vs repo-tagged workers — route repo
  code to the tagged worker." That stripped form may pass.
- ❌ "ran `sudo chmod 755` on the symlink at `/path/...`" → already applied.
  The durable kernel is the general rule (e.g. "OpenSSH rejects
  world-writable `Include`-d ssh_config files"); the specific chmod is done.
- ❌ "today's push thread hit `attempt to write a readonly database`" →
  incident. The durable kernel is the workaround pattern ("`hcom send` to a
  new thread can fail with `readonly database`; fall back to
  `hcom-mcp thread_seed`"), not the specific thread.

### Route to the wiki-page path when:

- The learning passes the canonical `loam-using` Durable-memory admission rubric.
- The learning is a fact, decision, architecture note, or analysis that any future session (agent or human) reading memory would benefit from.
- The learning deserves a topic, entity, concept, or analysis page (or an update to one).
- The learning is worth cross-linking into the wiki graph.
- The learning is durable knowledge about the project, not about how to instruct an agent.

### Route to the guidance-file path when:

- The learning is a command, pattern, test approach, environment quirk, or gotcha that future **agent sessions** in this repo need to know.
- The right shape is a concise one-liner that lives in prompt context, not a wiki paragraph.
- The destination is `AGENTS.md` (team-shared, harness-agnostic), `CLAUDE.md` (team-shared Claude-specific), or `.claude.local.md` (personal/local, gitignored).
- The learning is about **how to work here**, not about **what is true here**.
- The candidate gotcha passes the `Trigger → Mistake → Fix` capture bar but no tripped skill is available to update directly.

### Route to the checkpoint path when:

- The learning is session state needed for resume or handoff.
- The learning is a pure incident report with no generalizable pattern, but future resumption needs it.

### Route to the task annotation / plan path when:

- The learning is per-task context attached to an active unit of work.
- The information is useful only while executing or reviewing that task.

### Route to discard when:

- The learning is build output, branch state, one-off, unverifiable, reconstructable with one command or file read, or fails the admission rubric.

### Mixed routing is allowed and expected

A single session may produce multiple destination types. Keep destinations separate in the report. A learning that would fit both wiki and guidance should default to guidance only if it is short and instruction-shaped; otherwise route to memory (wiki substrate) if it passes the admission rubric.

### Defer or route elsewhere when:

- The learning is not durable enough — discard, checkpoint, or attach to the active task if useful there.
- The learning reveals memory is wrong, stale, or contradicted — route to `/loam::amending-memory`.
- The learning is a new source to ingest (a file, article, transcript) — route to `/loam::adding-to-memory`.
- The learning is an unresolved open question — preserve as such, do not settle it silently.
- The learning is a pure incident report (what happened today, how we fixed
  it, no generalizable pattern) — route to `/loam::checkpointing`, not
  memory or guidance.
- The learning is an environment-specific failure — keep it in the operational
  report or checkpoint only; do not add it to `Common Mistakes` or gotchas.

---

## Step 3 — Per-path workflow

### 3A — Wiki-page path

If the session produced any wiki-bound learnings, run the wiki workflow below. If none, skip 3A entirely.

#### Find the wiki and probe state

Run `loamstate` to probe the wiki and qmd in one shot:

```bash
bash "${LOAM_SKILL_DIR:-${CLAUDE_SKILL_DIR}}/../loam-using/scripts/loamstate.sh" "$(pwd)" 2>/dev/null \
  || powershell "${LOAM_SKILL_DIR:-${CLAUDE_SKILL_DIR}}/../loam-using/scripts/loamstate.ps1" "$(pwd)" 2>/dev/null
```

Parse the JSON output. If `exists` is false, stop and recommend:

```text
/loam::scaffolding-wiki <topic or wiki goal>
```

Use `wiki_root` as the resolved wiki root. If `has_overview` is true, note it as a legacy root-hub file. Use `qmd_ready` + `collection` for qmd state. Runtime guard: if `loamstate` fails or returns invalid JSON, fall back to Globbing for `SCHEMA.md`, `index.md`, or `log.md` and manual qmd checks.

If qmd is ready, read `${LOAM_SKILL_DIR:-${CLAUDE_SKILL_DIR}}/references/qmd-usage.md` for finding existing destination notes. If qmd is not ready, use Grep/Glob to find existing pages.

#### Read the wiki contract & discover destinations

Read before editing:

1. `<wiki root>/SCHEMA.md`
2. `<wiki root>/index.md`
3. scoped log read: `grep -i "<session focus keywords>" <wiki root>/log.md` for prior entries touching the session focus; plus `grep "^## \[" <wiki root>/log.md | tail -2` for the last 2 entries. Never read the full log.
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
5. If a learning contradicts an existing memory claim, archive the superseded durable page, write the correction, log it, and continue.
6. If a claim came from discussion but was not validated, label it as discussed, suggested, or pending.

### 3B — Guidance-file path

If the session produced any guidance-bound learnings, run the concise one-liner workflow below. If none, skip 3B entirely.

#### Find the guidance files

```bash
find . \( -name "AGENTS.md" -o -name "CLAUDE.md" -o -name ".claude.local.md" \) 2>/dev/null | head -20
```

**Where to write:**

- `AGENTS.md` — the canonical guidance file. All shared guidance goes here. This is where guidance-path learnings are written.
- `CLAUDE.md` — an import shim containing only `@AGENTS.md`. Never write content to `CLAUDE.md`. If it has content beyond the import, flag it as drift.
- `.claude.local.md` — personal/local only (gitignored). For preferences, not shared guidance.

If no `AGENTS.md` exists but a `CLAUDE.md` does, check if `CLAUDE.md` has real content (beyond `@AGENTS.md`). If it does, propose moving that content to a new `AGENTS.md` and collapsing `CLAUDE.md` to `@AGENTS.md`. Write guidance learnings to `AGENTS.md`.

#### Draft additions

**Keep it concise** — one line per concept. Guidance files are part of the prompt, so brevity matters.

Format: `<command or pattern>` — `<brief description>`

Avoid:

- Verbose explanations
- Obvious information
- One-off fixes unlikely to recur

---

## Step 4 — Apply routed learnings

Apply each routed learning to its destination.

### 4A — Apply wiki-page path

1. Update the existing relevant pages.
2. Create any admitted new durable page using a canonical kebab-case filename.
3. Update `index.md` if durable pages changed or discoverability improved materially.
4. Append `<wiki root>/log.md`:

```md
## [YYYY-MM-DD] learnings | <session focus>
```

Capture: session focus, pages updated or created, important learnings preserved, contradictions corrected, open questions left unresolved.

5. **Refresh qmd**: If qmd was ready and you wrote to the wiki, run `qmd update -c <collection> 2>/dev/null`. If refresh fails, report it but do not roll back wiki edits.

Do not create a conversation-source note in this skill.

### 4B — Apply guidance-file path

Write guidance additions to `AGENTS.md` only. Never write to `CLAUDE.md` (it is an `@AGENTS.md` import shim). Keep additions to one line per concept.

### 4C — Apply checkpoint path

Invoke `/loam::checkpointing` for resumable state. Do not write checkpoint content into durable wiki pages.

### 4D — Apply task annotation / plan path

Attach per-task context to the active task, plan, or task tracker entry using that tool's native format.

### 4E — Discard

Discard material that fails routing. Optionally mention discarded classes in the report when helpful.

---

## Step 5 — Report back

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

### Routed elsewhere or discarded
- Checkpoint: <count or "none">
- Task annotation / plan: <count or "none">
- Discarded: <count or "none">

### Open questions
- <question or "none">

### Next useful command
- `/loam::adding-to-memory <local source path>` or `/loam::amending-memory <what changed>`
```

If the review found nothing durable enough to add, say so explicitly and do not make any edits.

---

## Rules

- Review the current session before scanning either surface.
- Read the wiki schema before wiki edits.
- Read existing guidance files before guidance edits.
- Route through the five-way matrix before writing.
- The classification is a feature. Do not collapse the destinations into one.
- Wiki path: direct page updates only. Never create a conversation-source note in this skill.
- Wiki path: prefer existing pages over new pages. Use qmd to find existing destination notes when ready; fall back to Grep/Glob when not ready.
- Wiki path: never edit a wiki page based only on qmd output. Always read the actual wiki files first.
- Wiki path: keep additions concise, durable, and attributable to the session.
- Wiki path: corrections and supersessions use archive + correct + log; do not silently replace stale claims.
- Guidance path: keep it concise — one line per concept. Guidance files are part of the prompt.
- Guidance path: avoid verbose explanations, obvious information, and one-off fixes unlikely to recur.
- Do not fetch external sources in this skill.
- Do not modify raw-source files.
- Update `<wiki root>/log.md` on every wiki learnings pass.
- After wiki writes, refresh qmd if the collection is ready. If refresh fails, report it but do not roll back.
- Run the durability test (Step 1.5) on every candidate before classifying.
  Incidents belong in `/loam::checkpointing`, not memory or guidance.
- Capture durable gotchas only as `Trigger → Mistake → Fix` entries in the
  tripped skill's `Common Mistakes` or `references/gotchas.md`; see
  `references/gotchas.md` for examples.
- Strip session-specific identifiers (dates, paths, agent names, versions,
  thread IDs) before routing. If stripping leaves nothing durable, drop it.
