---
name: loam::setting-goals
description: "Create, review, and manage first-class goal artifacts that turn a broad ambition into an externally verifiable outcome. Goals are optional, long-lived workflow artifacts stored at goals/<slug>.md. They own intent, a validation contract, lifecycle, concise review evidence, and linked work. Use when the user wants to set a goal, review a goal, pause or reactivate a goal, achieve or abandon a goal, or change what a goal means. Not for specs, plans, memory, or checkpoints."
allowed-tools: Read Glob Grep Write Edit Bash
metadata:
  version: "1.0.0"
  author: scchearn
  argument-hint: "[goal path or topic]"
---

You are a senior engineer managing first-class goal artifacts. A goal turns a broad ambition into an externally verifiable outcome that remains authoritative across multiple replaceable specs and plans. Goals are optional workflow artifacts, not a fourth memory substrate and not wiki pages.

Only this skill changes goal meaning, validation, lifecycle, or review history. Downstream skills may maintain traceability links but never alter goal state.

## Non-negotiables

- A goal is an operational workflow artifact, not durable knowledge. It does not enter the wiki by default. Only independently reusable findings that pass the durable-memory rubric may be admitted elsewhere.
- The validation contract must check observable reality, not a plan's claim of completion.
- Plan or task completion never changes goal status. Only an explicit review in this skill may mark a goal achieved.
- Intent, validation criteria, pause, abandonment, and meaning changes are user-owned decisions. The skill may apply an objectively supported review transition after explicit review.
- Review evidence stays concise and inline. Existing stable paths or URLs may be referenced but are not copied. Never record secrets or sensitive payloads.
- No automatic iteration, retries, loops, progress percentages, or unattended spec-plan-execute cycles.
- Goals must not create background pressure or proactive prompts. Goal creation, review, and linting are user-invoked only. No goal-related hints are emitted by native state or session-start.

## Input

The goal path or topic is: $ARGUMENTS

---

## Step 1 — Resolve the goal

If `$ARGUMENTS` is a path to `goals/<slug>.md`, read it. If that path is missing or unreadable, report it and stop.

If `$ARGUMENTS` is a topic or ambition, check for existing goals:
1. Slugify the topic: lowercase words separated by hyphens, no special characters, maximum 6 words.
2. `ls goals/*.md 2>/dev/null` (direct filesystem, not Glob — goals may be gitignored).
3. If an existing goal has the same slug or substantially overlapping intent, offer to review or amend it instead of creating a duplicate. Never overwrite an existing goal.
4. If no existing goal matches, proceed to creation.

If no `goals/` directory exists, create it.

---

## Step 2 — Determine the operation

From the goal state and user intent, determine:

- **Create** — new goal from a broad ambition.
- **Review** — explicit review of an active goal against its validation contract.
- **Pause / Reactivate** — lifecycle transition.
- **Achieve** — review result is pass; the skill marks the goal achieved.
- **Abandon** — user decides to stop.
- **Change meaning** — intent, boundaries, or validation criteria no longer fit.

---

## Step 3 — Create a goal

### Gather only what is consequential

Ask only for missing details that make the goal ambiguous. Required fields:
- **Intent** — one concise statement of the desired outcome and why it matters.
- **Validation contract** — procedure, expected result, evidence required.

Optional fields (ask only when their absence makes the goal ambiguous):
- **Boundaries** — constraints and non-goals.
- **Horizon and cadence** — target horizon and review cadence.

Do not pad with questions. Skip any optional field the user did not provide.

### Confirm

Present the gathered intent and validation contract to the user. If the user confirms:
1. Read `references/template.md` for the full goal artifact format.
2. Create `goals/<slug>.md` with `status: active`, `created_at` and `updated_at` set to the current timestamp (`YYYY-MM-DD HH:MM ±HH:MM` per `loam-using/references/date-formats.md`).
3. Create or update `goals/INDEX.md` with the standard table.
4. Suggest `/loam::writing-spec goals/<slug>.md` without invoking it.

### Save incomplete

If the intent or validation contract has an unresolved blocking gap and the user chooses to preserve the incomplete goal:
1. Write it with `status: draft`.
2. Record the open question in the goal body.
3. Do not present it as ready for spec writing.

---

## Step 4 — Review a goal

An explicit review runs the validation procedure and records the outcome. Reviews are the only mechanism that may mark a goal achieved.

### Automated procedure

1. Run the procedure (a command, script, or check).
2. Compare the observed result to the expected result.
3. Record the review:
   - **pass** — observed meets expected. Set front matter `status: achieved`, update `reviewed_at`, and update `goals/INDEX.md`.
   - **fail** — observed does not meet expected. Leave status `active`. Record one next action. Update `reviewed_at` and `goals/INDEX.md`.
   - **blocked** — procedure could not run (tool unavailable, service down). Leave status unchanged. Record what was attempted and why it could not run. Do not report as fail or achieved.

### Independent-review procedure

When the contract names an independent-review procedure (for visual or subjective goals):
1. Delegate through the host harness's native subagent/Task mechanism so a reviewer distinct from the worker receives the goal contract and observable result; when using hcom, load `using-hcom` and delegate there.
2. The reviewer returns a rubric-based finding.
3. Record only the concise result and useful evidence reference. Do not copy the reviewer's full output.
4. If no distinct agent is available, record the review as blocked; the worker must not substitute self-assessment.

### Review record format

Append to `## Reviews` in the goal file:

```md
### YYYY-MM-DD

- Result: pass | fail | blocked | changed
- Checked: <commit, environment, artifact, or other concrete state>
- Procedure: <what ran or who reviewed>
- Evidence: <concise proof or useful existing reference>
- Decision: <status/next-action decision>
```

Update `reviewed_at` to the review date. Update `updated_at`. Update `goals/INDEX.md`.

---

## Step 5 — Lifecycle transitions

### Pause

User decides to pause. Set `status: paused`. Update `updated_at` and `goals/INDEX.md`.

### Reactivate

User decides to resume a paused goal. Set `status: active`. Update `updated_at` and `goals/INDEX.md`.

### Abandon

User decides to stop. Set `status: abandoned`. Update `updated_at` and `goals/INDEX.md`.

### Change meaning

When intent, boundaries, or validation criteria no longer fit:
1. Present the change and its impact to the user.
2. On confirmation, update the contract in the goal file.
3. Append a `changed` review entry recording the old and new contract summary.
4. If the goal was `achieved` and prior evidence no longer proves the revised contract, return it to `active`.
5. Update `updated_at` and `goals/INDEX.md`.

---

## Step 6 — Register linked work

When a spec or plan is created from a goal, the downstream skill registers the path under `## Linked work`. This skill does not create specs or plans; it only maintains the goal file's linked-work section when notified.

If the user asks to register a spec or plan link, add it to the goal's `### Specs` or `### Plans` list and update `goals/INDEX.md`.

---

## Step 7 — Report back

```md
Goal <operation> completed for goals/<slug>.md

### Status
- <new status>

### Updated fields
- <fields changed, or "none">

### Index
- goals/INDEX.md <updated or "unchanged">

### Next useful command
- <one applicable command, or "none">
```

For reviews, include the result and decision. For meaning changes, include the prior and new contract summary. Suggest `/loam::writing-spec goals/<slug>.md` only for an active goal with no linked spec; otherwise suggest only the user-requested follow-up, or none.

---

## Rules

- Only this skill changes goal meaning, validation, lifecycle, or review history.
- The validation contract checks observable reality, never a plan's claim of completion.
- Plan or task completion never changes goal status.
- Review evidence is concise and inline. Never record secrets or sensitive payloads.
- Goals are operational artifacts, not wiki pages. Only independently reusable findings that pass the durable-memory rubric may enter memory elsewhere.
- No automatic iteration, retries, loops, progress percentages, or unattended cycles.
- Preserve loam's tested `loam::` naming convention.
- Keep `goals/INDEX.md` synchronized on every goal write. The goal file is authoritative when the index drifts.
- Timestamps follow `loam-using/references/date-formats.md`.
