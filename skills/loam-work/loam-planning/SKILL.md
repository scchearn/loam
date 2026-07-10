---
name: loam::planning
description: "Use when an approved workspace spec needs an execution-ready implementation plan with ordered, verifiable steps. Specs are mandatory: this skill consumes design decisions, verifies that the spec still matches the codebase, and writes the repo-native plans/ artifacts. Inherits optional goal provenance; see loam::setting-goals."
allowed-tools: Read Glob Grep Bash Write Edit Skill
metadata:
  version: "2.5.0"
  author: scchearn
  argument-hint: <spec path or spec topic>
---

You are a senior engineer compiling an approved spec into a rigorous implementation plan. Your job is not to research product direction or re-decide design. Your job is to verify the spec is usable, read only the relevant current codebase context, and decompose the approved work into reliable task blocks.

## Non-negotiables

- Stay in planning mode. Do not implement source changes while running this skill.
- A spec is required for every plan. If no matching spec exists, stop and direct the user to `/loam::writing-spec <topic>`.
- Consume spec decisions. Do not re-evaluate chosen designs or reopened alternatives unless current code proves the spec is critically stale.
- Produce a plan that a smaller tool-capable model can execute: every task must have a concrete Outcome, ordered Steps, Files with role markers, Verify command, and Passes when condition.
- Keep this skill authoritative for output: write `plans/<slug>.md` and keep `plans/INDEX.md` synchronized.
- If the spec is incomplete or critically stale, stop. Do not invent missing requirements or design decisions.

## Input

The spec path or topic is: $ARGUMENTS

A `spec_ready_for_plan` hint in `loamstate` output (an approved spec with no matching plan) is the advisory signal for this skill; see the hint contract in `loam::using`.

---

## Skill authority

An explicit `/loam::planning` invocation is the controlling user instruction for this workflow. Superpowers skills may be referenced as external discipline, but they do not control this skill's artifact format or step order.

Do not invoke `superpowers:brainstorming`, `superpowers:writing-plans`, `superpowers:executing-plans`, or `superpowers:subagent-driven-development` as controlling skills while running `/loam::planning`. Never write `docs/superpowers/plans/` from this skill. Plans go to `plans/<slug>.md` and `plans/INDEX.md` only.

## Displacement rules

| External rule | loam::planning displacement |
| --- | --- |
| `using-superpowers`: "invoke relevant skills before any response" | Treat `/loam::planning` as the higher-priority explicit workflow; reference external skills only as future execution discipline. |
| `brainstorming`: "the ONLY skill you invoke after brainstorming is writing-plans" | Brainstorming output is pre-planning input only; `/loam::planning` remains the planner. |
| `writing-plans`: plans are written under `docs/superpowers/plans/` | Ignore that artifact path here; this skill writes `plans/<slug>.md`. |
| `executing-plans` / `subagent-driven-development`: required execution sub-skills | Execution is handled later by `/loam::starting`; do not redirect planning. |

## Hierarchy rules

- loam::planning format wins over writing-plans format.
- brainstorming is pre-planning; read its output like supporting research when a spec cites it.
- finishing-a-development-branch is a final execution discipline, not a planning workflow.
- loam::starting supersedes executing-plans for executing this repo's task blocks.
- writing-plans output is supporting input only, never a competing plan artifact.

---

## Step 0 — Resolve the spec

Parse `$ARGUMENTS` as either:

- a direct path such as `specs/add-audit-logging.md`
- a slug or topic that should resolve to exactly one `specs/<slug>.md`

Resolution rules:

1. If a direct path is provided, read that file.
2. Otherwise slugify the topic and look for `specs/<slug>.md`.
3. If there is no exact match, search `specs/*.md` front matter and headings for a single close match. **Use `ls specs/*.md 2>/dev/null` via Bash, not Glob** — `specs/` is commonly excluded via `.gitignore` or `.git/info/exclude` (so the agent workspace can hold local-only research artifacts), and Glob silently returns zero hits for ignored paths. Grep may then be used on the resolved filenames.
4. If no spec is found, STOP with:

```text
No spec found. Specs are required before planning. Run /loam::writing-spec <topic> to produce a spec at specs/<slug>.md.
```

5. If multiple specs could match, STOP and ask the user to choose the exact spec path.

The resolved spec path becomes the plan front matter `spec:` value and the `## Spec` body link.

### Optional goal provenance

If the spec front matter contains a `goal:` field pointing to `goals/<slug>.md`:
1. If the goal file is missing or unreadable, report the broken path and stop. Otherwise read it and verify `status: active`; for `draft`, `paused`, `achieved`, or `abandoned`, report the mismatch and stop unless the user explicitly reactivates or authorizes the goal.
2. Carry the `goal:` field into the plan front matter.
3. Keep the plan's `## Goal` sentence aligned with the goal artifact's `## Intent` while expressing this plan's observable contribution.
4. Identify task outputs that may support a later goal review (test artifacts, validation commands, observable evidence). Note them in task `Watch for` or `Passes when` fields.
5. Do not treat plan acceptance as goal validation. Only `/loam::setting-goals` may change goal status.
6. After writing the plan, register it under the goal's `## Linked work` → `### Plans` list.

---

## Step 1 — Verify the spec

Read the full spec before task decomposition.

### Required spec completeness

The spec must contain, at minimum:

- front matter with `title`, `slug`, `status`, `created_at`, and `updated_at`
- `## Problem`
- `## Acceptance criteria`
- `## Decision`

The spec may also contain `## Clarifications` (Q/A/Status triples from elicitation), `## Scenarios` (Given/When/Then behavioral scenarios), `## Completeness checklist` (domain coverage table), `## Scope`, `## Constraints`, `## Rejected alternatives`, `## Key files / modules`, `## Open questions`, and a front matter `research:` reference.

For implementation planning, open questions must be `none` or explicitly marked non-blocking. If the spec is missing acceptance criteria, lacks a decision, or contains critical unresolved open questions, STOP with a concise completeness report naming each gap and recommend `/loam::writing-spec <topic>` to fill it.

If `## Completeness checklist` is present and contains any `gap-blocking` row, STOP with a completeness report naming the gap and recommend `/loam::writing-spec <topic>` to fill it. If `## Scenarios` and `## Acceptance criteria` contradict each other, STOP and recommend `/loam::writing-spec <topic>` to reconcile the conflict. For new-format specs that contain any of `## Clarifications`, `## Scenarios`, or `## Completeness checklist`, behavior-changing work requires `## Scenarios` unless it contains `none` with a rationale for why scenarios are unnecessary.

### Spec status

Prefer `status: approved`. If the spec is `draft`, continue only when the current user/session has explicitly approved using it; otherwise STOP and ask for spec approval or an amended spec. If the spec is `superseded`, STOP and ask for the replacement spec.

### Currency check

Verify the spec against the current codebase narrowly:

1. Read paths and modules listed under `## Key files / modules`.
2. Confirm referenced files, tests, schemas, commands, and patterns still exist.
3. If a listed path moved but the decision remains valid, treat it as minor staleness: record the mapping in the plan Decisions log and proceed with the current path.
4. If the spec's core decision is invalidated by codebase drift, STOP with the stale claim, current evidence, and a recommendation to run `/loam::writing-spec <topic>` or amend the spec.

Extract these planning inputs:

- spec constraints -> plan acceptance/task Constraints
- spec acceptance criteria -> plan Acceptance criteria and task Passes when entries
- spec decision -> initial Decisions log entries
- spec key files/modules -> seed task Files entries
- spec rejected alternatives -> boundaries the plan must not reopen
- spec scenarios -> task Passes when entries as richer behavioral detail alongside acceptance criteria
- spec clarifications -> promote to task Constraints only when they contain actual invariants or limits; otherwise carry into Watch for or Decisions log
- spec completeness checklist -> carry gap-non-blocking rows into task Watch for, Learning checkpoints, or Decisions log where relevant
- spec research references -> supporting context only when the spec cites them

---

## Step 2 — Scoped codebase read

Read only the codebase context needed to turn the approved spec into tasks:

1. Files, modules, tests, schemas, generated artifacts, commands, and contracts named by the spec.
2. Adjacent files needed to understand current patterns near those references.
3. Supporting research memos only when the spec front matter or body explicitly cites them.
4. Wiki notes only when they are linked by the spec or found by a narrow query for the spec's specific domain.

This is a scoped codebase read, not broad research. Do not search `plans/research/` independently. Do not perform open-ended wiki exploration. If scoped reading reveals that the spec is incomplete or critically stale, stop and route upstream.

If the resolved spec touches UI, frontend, or visual work (scan the spec for terms like `UI`, `frontend`, `visual`, `design`, or component names), also: (i) search the repo case-insensitively for `design.md` / `DESIGN.md` (typically repo root); (ii) scan available skills in the running harness whose names or descriptions match design, frontend, or taste. Read whatever is found into planning context. If neither `design.md` nor any design skill exists, note the gap in the plan's `Decisions log` and proceed without design governance — do not block.

When a wiki exists, use the qmd search protocol in `loam::using` (with its code-graph precedence) first and Grep/Glob as fallback, for a narrowly scoped domain check keyed to the spec domain. For code-specific call sites or symbol usages in source, after qmd orientation prefer `ast-grep` (fallback `rg`/`grep`) scoped to the modules qmd flagged. Flow durable constraints, gotchas, or stale assumptions into task `Watch for:` fields when they affect execution. If the scoped wiki read reveals gaps that execution is likely to answer, add those to `## Learning checkpoints` with `After`, `Wiki target`, and `What to capture` columns. If no wiki exists, skip all wiki features entirely.

---

## Step 3 — Decompose into tasks

Apply this strategy:

1. Map spec acceptance criteria and decisions to end-state deliverables.
2. Map spec scenarios to task boundaries — state/action/outcome structure helps define task scope. If scenarios specify `Verify with:` targets, use those as task Verify commands when applicable.
3. Work backward from those deliverables to define tasks.
4. Check `## Rejected alternatives` before adding any step that could reopen a closed decision.
5. Assign IDs sequentially: T1, T2, T3 ...
6. Put foundational changes first, then dependent behavior, then validation/docs/finalization.

Each task must be:

- **Atomic** — completable in one focused session, roughly 1-20 files touched.
- **Verifiable** — has a concrete workspace-native Verify command.
- **Externally checkable** — prefer tests or validations another engineer or CI can rerun.
- **Dependency-aware** — `Depends on` lists every true prerequisite task ID, not just the previous task.
- **Spec-traced** — Outcome, Steps, Constraints, Verify, and Passes when derive from the spec or scoped codebase evidence.
- **File-annotated** — `Files` lists concrete workspace-relative paths with role markers: `(read)`, `(edit)`, `(create)`, or `(read+edit)`. Use inline lists for four or fewer short paths; use indented bullets for five or more paths or any path longer than about 50 characters. Use full workspace-relative paths by default; abbreviate only when the prefix is obvious and unambiguous. Use `none` for pure verification tasks.
- **Thorough enough for smaller models** — `Steps:` is the primary execution guide. Write 3-7 ordered actions using repo-specific names where useful, so the executor does not choose its own architecture.

### Task block fields

Use this order:

1. `Status`
2. `Depends on`
3. `Outcome`
4. `Steps`
5. `Constraints` — omit when empty. Include non-goals, invariants, compatibility rules, and labels such as `needs-isolation`, `needs-independent-review`, `risk:data-destructive`, or `needs-parallel`.
6. `Watch for` — omit when empty. Include known pitfalls, stale assumptions, or gotchas from the spec/wiki/current code.
7. `Files`
8. `Verify`
9. `Passes when`

`Passes when` describes the expected passing state in plain language. It is the primary signal a smaller model uses to decide the task is complete.

If a task touches generated artifacts, types, and runtime behavior, chain focused checks in Verify so generation freshness and consuming tests are both covered.


### Intent markers and external disciplines

Use compact markers inside `Steps:` or `Constraints:` when execution should apply an external discipline:

- `[tdd: <test-file> | <test-command>]` for implementation, bugfix, and behavior-changing refactor tasks. Absence of `[tdd]` is an approved exception for config-only, docs-only, generated artifact refresh, migrations, scaffolding, or other tasks where test-first does not apply.
- `[worktree: <branch-name>]` for tasks with `needs-isolation`.
- `[debug]` for tasks likely to surface unexpected behavior, or when systematic debugging should start after repeated verification failures.
- `[design: <skill-name> | <design-source#section>]` for tasks touching UI, frontend, or visual work. `<design-source>` is a workspace-relative path with anchor to the authoritative `design.md` (e.g. `path/DESIGN.md#buttons`). Absence of `[design]` is an approved exception for non-UI tasks. When a named design/frontend skill should drive the work, also list it under `## Execution disciplines` with its scope.

When any `superpowers:<name>` discipline is relevant, add `## Execution disciplines` with its scope and one-line fetch-fail fallback. Use canonical names only; do not put GitHub URLs in plan files.

### Persisted data / schema rule

If the spec changes persisted data, schemas, contracts, or generated artifacts, include every workspace-required step as explicit tasks: schema/model update, migration, generated artifact refresh, docs/contracts, tests, and validation commands.


### Finalization task rule

For code-changing plans when a VCS branch is present, or when `superpowers:finishing-a-development-branch` is referenced in `## Execution disciplines`, include a final task named `Finalize, verify, and close branch`. The task runs full relevant validation, confirms no regressions, and prepares the branch for user merge/PR/discard decisions. Omit this task for docs-only or research-only plans.

---

## Step 3.5 — Evaluate concurrency and isolation

Derive execution waves from the `Depends on` graph. Tasks in the same wave have no dependency path between them and may be executed concurrently by tooling that supports it. The plan must also remain valid for linear execution in listed order.

Add `## Execution groups` only when there is more than one meaningful wave or when concurrency/isolation constraints help execution. Use a compact table:

```md
| Wave | Tasks | Notes |
| ---- | ----- | ----- |
| 1 | T1 | foundation |
| 2 | T2, T3 | independent tasks may run concurrently |
```

Do not add agent names, model names, worktree paths, branch names, or launch commands to the plan.

Add constraint labels to task `Constraints:` when triggers match:

| Label | Trigger |
| --- | --- |
| `needs-isolation` | migrations, schema changes, force-push-like operations, or broad branch-risk changes |
| `needs-independent-review` | auth changes, security-sensitive code, shared APIs, or public contracts |
| `risk:data-destructive` | irreversible operations, destructive migrations, deletes, schema drops |
| `needs-parallel` | independent features or separate modules that can run in the same wave |

Resolution of these labels belongs to the execution skill. The plan states constraints and dependency waves only.

---

## Step 4 — Optional wiki write-back

If memory (wiki substrate) exists, decide whether planning surfaced durable knowledge worth preserving. Good candidates include stable architecture constraints, durable workflow requirements, reusable domain distinctions, clarified system boundaries, or spec drift findings.

Do not write back the task list, temporary sequencing choices, or speculative ideas. If the finding is durable, prefer updating existing notes, update `index.md` only when discoverability changes, and append `log.md` with:

```md
## [YYYY-MM-DD] plan | <feature>
```

If scoped wiki reading identified gaps likely to be filled during execution, populate `## Learning checkpoints` instead of writing speculative content now. Use this table format and omit the entire section when there are no wiki gaps:

```md
| After | Wiki target | What to capture |
| ----- | ----------- | --------------- |
| Tn | wiki/topics/<topic>.md | <durable fact, command, gotcha, or open question> |
```

`Wiki target` should name an existing wiki page when possible, or `[new] wiki/topics/<slug>.md` when a small new durable page is likely needed.

---

## Step 5 — Report back

After writing the plan, output:

```text
Plan written to plans/<slug>.md

Spec:
  - specs/<slug>.md (<status>, updated <date>)

Supporting research cited by spec:
  - <path or none>

Filed back into wiki:
  - <path or none>

Goal: <goal sentence>

Tasks:
  T1 — <title> [verify: <command>]
  T2 — <title> [depends: T1] [verify: <command>]

Execution groups:
  - <waves or none>

To begin execution: /loam::starting plans/<slug>.md
```

Then ask: "Does this plan look right, or should I adjust anything before you run `/loam::starting`?"

Do not begin executing tasks.

---

## Plan file requirements

Use the skill-local `references/template.md`. New plans must contain:

- YAML front matter with `title`, `slug`, `spec`, `description`, `status`, `task_count`, `created_at`, `started_at`, `completed_at`, and optional `goal`
- `## Spec` with the consumed spec link and status/date summary
- `## Goal`
- `## Acceptance criteria` copied or directly derived from the spec
- `## Tasks`
- optional `## Execution groups`
- optional `## Learning checkpoints`
- optional `## Execution disciplines`
- `## Decisions log` with initial entries for spec decisions and any minor staleness mappings
- `## Touched files` — the planner leaves this section empty. `/loam::starting` populates it during execution with deduplicated edit-marked paths. `/loam::syncing-code-graph --touched` consumes it at plan completion to reconcile the code graph.
- `## Handoff notes`

Update `plans/INDEX.md` with the slim schema:

```md
# Plans Index

<!-- Status key: pending | in-progress | done | abandoned -->
<!-- Ordered by: in-progress -> pending -> done -> abandoned -->
<!-- Updated automatically by /loam::planning, /loam::starting, /loam::amending-plan -->
<!-- Each row mirrors plan front matter: status, title, slug link, description, task_count -->

| Status | Title | Plan | Description | Tasks |
| ------ | ----- | ---- | ----------- | ----- |
```

The row format is:

```md
| `pending` | <title> | [<slug>](<slug>.md) | <description> | <task_count> |
```
