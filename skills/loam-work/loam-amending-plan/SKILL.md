---
name: loam-amending-plan
description: "Amend an existing plan file — add tasks, modify pending or delegated tasks, and mark completed tasks that are invalidated by the change as [>] (needs re-run). Walks through analysis, cascading impact, and user confirmation before touching the file. When memory (wiki substrate) exists, it may also preserve durable amendment findings there."
allowed-tools: Read Write Edit Glob Grep
metadata:
  version: "1.0.0"
  author: scchearn
  argument-hint: plans/<slug>.md # describe what to amend in your message, then invoke this skill
  disable-model-invocation: true
---

You are a senior engineer working in the current workspace. A plan is in flight and something has changed. Your job is to rigorously analyse the full impact of that change on the plan, present a proposal for the user to approve, and only then apply it. Do not modify the file until the user confirms.

## Input

The plan file to amend is: $ARGUMENTS

The amendment description is in the conversation context immediately above this prompt. Read it carefully before anything else.

---

## Phase 1 — Deep read

Read the plan file in full. Build a complete picture:

1. **Goal and acceptance criteria** — what is the plan trying to achieve?
2. **Front matter and related references** — note the current `title`, `slug`, `description`, `status`, `task_count`, `created_at`, `started_at`, `completed_at`, any legacy `updated_at` or `## Plan summary` content that should be removed, and any entries under `## Related research` and `## Related specs`
3. **All tasks** — for every task, note: ID, title, status (`[ ]`/`[~]`/`[h]`/`[x]`/`[!]`/`[>]`), dependencies, verify command, files to read, files to modify, notes, and any `Execution` metadata
4. **Dependency graph** — mentally map which tasks feed into which. A change to T2 may cascade to T4, T5, T7 even if T4 doesn't directly reference T2
5. **Decisions log** — understand the history and what has already been decided
6. **Current state** — how far along is execution? What's been done, what's locally in flight, and what's delegated externally via `[h]`?
7. **Optional wiki context** — if memory (wiki substrate) exists, read the schema and only the notes directly relevant to the plan area or amendment. Treat memory as a durable memory layer, not the authority over current repo state.

Do not modify anything yet.

---

## Phase 2 — Critique the amendment

Now reason carefully about the amendment. Work through **every single task** in the plan and ask: _does this amendment affect this task's inputs, outputs, scope, or correctness?_

Apply these lenses:

**Direct impact** — does the amendment directly change what this task does or produces?

**Upstream cascade** — if an earlier task is invalidated, does this task depend on that earlier task's output? Transitively?

**Scope drift** — does this amendment widen or narrow what a pending task needs to do? The task body may need updating even if it doesn't need re-running.

**New gaps** — does the amendment introduce requirements that no existing task covers? What new tasks are needed?

**Acceptance criteria** — does the amendment affect the plan's goal or acceptance criteria? If so, those need updating too.

**Linked research and specs** — does the amendment mean the plan should reference different or additional research or spec files? If so, update `## Related research`, `## Related specs`, and any affected task `Files to read`.

**Plan metadata** — does the amendment change the short description, task count, or overall status that the YAML front matter and `plans/INDEX.md` should reflect?

**Wiki impact** — if memory (wiki substrate) exists, does the amendment reveal a durable architecture, domain, or workflow change that should be preserved there after confirmation? Do not confuse this with task-management chatter.

**Validation quality** — does the amendment require stronger automated tests or validations so the updated behavior can be checked independently later, not just during this session?

Be critical. Do not under-scope the impact. It is better to flag a task as potentially affected than to miss a cascade.

If the amendment depends on workspace behavior or external APIs you cannot confidently establish from local context, stop and recommend `/loam::writing-spec <topic>` before applying speculative plan changes.

---

## Phase 3 — Build the proposal

Produce a clear, structured proposal. Do NOT apply any changes yet.

Format it exactly like this:

```
## Amendment proposal for plans/<slug>.md

### What's changing and why
<1-3 sentences summarising the amendment and its root cause>

### Impact analysis

**Tasks marked [>] (completed, need re-run):**
  Tx — <title>
    Why: <specific reason this task's output is now wrong or stale>
    Cascade source: <which upstream change causes this, if indirect>

  (or: None — no completed tasks are invalidated)

**Pending tasks with updated scope:**
  Tx — <title>  [currently: [ ] / [~] / [h]]
    Change: <what in this task's notes, files, verify command, or dependencies will be updated>

  (or: None)

**New tasks to add:**
  T(N+1) — <title>
    Depends on: <task IDs>
    Verify: <command>
    Rationale: <why this is needed>

  (or: None)

**Goal / acceptance criteria changes:**
  <describe any updates needed, or "None">

**Related research / specs changes:**
  <describe any links to add, remove, or replace in `## Related research` and `## Related specs`, or "None">

**Plan metadata / index changes:**
  <describe any description, task count, status, or index-row updates needed, or "None">

**Wiki impact:**
  <describe any durable wiki notes that should be updated after confirmation, or "None">

**Unaffected tasks:** Tx, Ty, Tz ...
  Reason: <brief justification that these are genuinely unaffected>

### What happens to the dependency graph
<Describe any re-ordering or new dependency edges created by this amendment>

### Anything you're uncertain about
<Flag any tasks where you're unsure whether they're affected — better to surface uncertainty than silently skip>
```

---

## Phase 4 — Confirm with user

**STOP HERE. Do not write any files.**

Present the proposal above to the user, then ask:

> "Does this proposal look right? If yes, I'll apply it. If anything should be added, removed, or changed, tell me and I'll revise the proposal before applying."

Wait for the user's response. Do not proceed to Phase 5 until the user explicitly confirms (e.g. "yes", "looks good", "apply it").

If the user asks for changes to the proposal, revise it and ask again. Repeat until confirmed.

---

## Phase 5 — Apply the amendments

Once confirmed, apply changes to the plan file in this exact order:

### A. Update goal / acceptance criteria / related research / specs / plan metadata (if needed)

Edit the Goal and Acceptance criteria sections if the amendment changes the observable end state. Keep the acceptance criteria checkable, and prefer independently re-runnable tests or validations where appropriate.

If the observable scope changes materially, also update front matter `description`. The description must stay within 70 tokens.

If the amendment changes which research memos or spec files the plan depends on, update `## Related research` and `## Related specs` so they list only the relevant paths with short reasons. If additional research is clearly needed but missing, stop and recommend `/loam::writing-spec <topic>` before applying speculative plan changes.

### B. Mark invalidated completed tasks as [>]

For each `[x]` task in the confirmed proposal:

1. Change `- **Status:** [x]` to `- **Status:** [>]`
2. Add a `- **Re-run reason:**` line immediately after Status, one sentence explaining why

```
### T3 — Update persisted contract
- **Status:** [>]
- **Re-run reason:** Amendment changes the data contract, so this task's previous output is now stale.
- **Depends on:** T2
- **Verify:** `<workspace-native automated check>`
- **Notes:** ...
```

### C. Edit pending tasks with updated scope

For each `[ ]`/`[~]`/`[h]` task in the confirmed proposal:

- Update Notes to describe the scope change
- Update Verify if needed
- Update `Files to read` and `Files to modify` if the research surface or edit surface changed
- Update Depends on if dependency order changed
- If the amendment changes behavior, update the task so it includes automated tests or validations when reasonable

If the task is currently `[h]`, treat it as active external work. Do not silently leave stale delegation in place. Either:

- keep it `[h]` only if the amendment does not materially change what the external worker is doing, or
- convert it to `[>]` / `[ ]` as appropriate if the in-flight delegated output is now stale and must be re-delegated later

Do NOT change the task ID. If the change makes the task fundamentally different, supersede it: mark the old one `[!]` with a note, and add a new task.

### D. Add new tasks

Append new tasks after the last existing task, continuing the ID sequence (last is T8 → add T9, T10, ...):

```
### Tx — <title>
- **Status:** [ ]
- **Depends on:** <task IDs or "none">
- **Execution:** <optional structured execution metadata, or omit for hub tasks>
- **Verify:** `<workspace-native automated check>`
- **Files to read:** <!-- research memos, docs, existing source files, contracts, tests, or external references to consult -->
- **Files to modify:** <!-- source files, tests, docs, or generated artifacts this task will change -->
- **Notes:** Added by amendment YYYY-MM-DD: <reason>
```

If the new task should be delegated later, use the same structured `Execution` format the plan already uses.

### E. Update downstream dependencies

If new tasks become prerequisites for existing pending tasks, update their `Depends on` fields.

### F. Append to decisions log

The log is **append-only**. Add exactly one entry:

```
YYYY-MM-DD — Plan amended: <summary of change>. Re-run required: <[>] task IDs or "none">. Added: <new task IDs or "none">.
```

### G. Sync front matter and INDEX.md

After all edits, re-evaluate the plan metadata and sync it in the YAML front matter:

- `task_count` must equal the number of `### T...` task blocks after the amendment
- Preserve `created_at`
- Preserve existing `started_at`; if execution has never started, keep it `null`
- Remove front matter `updated_at` if it exists from an older plan format
- If all tasks are still `[ ]` and execution has not started, keep `status: pending`
- If all tasks are `[x]`, set or keep `status: done`
- Otherwise, set or keep `status: in-progress`
- If the plan moves from `done` back to `pending` or `in-progress`, clear `completed_at`
- If the plan is `done`, ensure `completed_at` is populated
- Remove the entire legacy `## Plan summary` section if it is still present
- If `plans/INDEX.md` still uses legacy timestamp columns, rewrite it to the slim `Status | Title | Plan | Description | Tasks` schema
- Keep the row in `plans/INDEX.md` aligned with the front matter for `Status`, `Title`, `Plan`, `Description`, and `Tasks`
- Update or create the row in `plans/INDEX.md` if any mirrored metadata changed, and move it if the status ordering changed

This ensures a previously-completed plan that gets amended doesn't falsely show as `done` in the index.

### H. Optional wiki write-back

If memory (wiki substrate) exists and the confirmed amendment reveals durable knowledge worth preserving:

1. Prefer updating an existing relevant topic, concept, entity, or analysis note
2. If you create a new durable category note, use a canonical kebab-case filename and `[[kebab-case-note-name]]` links
3. Update `index.md` if durable pages changed
4. Append `log.md` with a parseable heading like:

```md
## [YYYY-MM-DD] amend | <plan or scope>
```

Do not write back ephemeral plan-management chatter or speculative scope notes.

---

## Phase 6 — Report

Output a concise confirmation:

```
Amendment applied to plans/<slug>.md

[>] Needs re-run:   Tx (title), Ty (title)   ← or "none"
    Modified scope: Tx (title)                ← or "none"
    New tasks:      Tx (title), Ty (title)    ← or "none"
    Wiki updates:   path/to/note.md          ← or "none"

To resume execution: /loam::starting plans/<slug>.md
Note: /loam::starting will detect the [>] tasks and will also respect any remaining [h] delegated tasks.
```

---

## Rules (non-negotiable)

- **Never delete tasks.** Completed tasks become `[>]`. Pending or delegated tasks get edited. The decisions log is append-only.
- **Never apply changes before user confirmation in Phase 4.**
- **Preserve all task IDs.** Never renumber existing tasks.
- **Do not execute any implementation work.** Your job is plan surgery only.
- **If memory (wiki substrate) exists** — you may update it only with durable amendment findings. Current repo state wins if memory is stale or wrong.
- **Cascade aggressively, apply conservatively.** Flag every possibly-affected task in the proposal. Mark `[>]` only what the user confirms truly needs re-running.
- **Prefer independently re-runnable validation.** When the amendment changes behavior, bias toward updating or adding tests that others can run later.
