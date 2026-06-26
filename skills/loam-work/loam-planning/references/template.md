---
title: <Feature Name>
slug: <feature-slug>
spec: <spec-slug>
description: <Short plan summary, 70 tokens max>
status: pending # pending | in-progress | done | abandoned
task_count: 0
created_at: YYYY-MM-DD HH:MM ±HH:MM
started_at: null
completed_at: null
---

# <Feature Name>

## Spec

[<Spec Title>](../specs/<spec-slug>.md) — <one-line summary of the consumed spec, including status/date>

## Goal

<One sentence: what does done look like? What is the observable end state?>

## Acceptance criteria

- [ ] <Acceptance criterion copied or directly derived from the spec>
- [ ] Relevant automated tests or validations covering the changed behavior pass
- [ ] Relevant workspace-native validation commands pass

## Tasks

### T1 — <title>

- **Status:** [ ]
- **Depends on:** none
- **Outcome:** <one concrete observable result this task must leave behind>
- **Steps:**
  - [ ] <first ordered action>
  - [ ] <second ordered action>
  - [ ] <third ordered action>
- **Constraints:** <non-goals, invariants, and labels such as needs-isolation or needs-independent-review; omit when empty>
- **Watch for:** <known pitfalls, stale assumptions, or gotchas; omit when empty>
- **Files:** <!-- paths with role markers -->
  - path/to/file.ext (read+edit)
- **Verify:** `<focused workspace-native automated command>`
- **Passes when:** <plain-language passing state that proves this task is complete>

### T2 — <title>

- **Status:** [ ]
- **Depends on:** T1
- **Outcome:** <one concrete observable result this task must leave behind>
- **Steps:**
  - [ ] <first ordered action>
  - [ ] <second ordered action>
  - [ ] <third ordered action>
- **Constraints:** <non-goals, invariants, and labels; omit when empty>
- **Watch for:** <known pitfalls, stale assumptions, or gotchas; omit when empty>
- **Files:** <!-- paths with role markers -->
  - path/to/file.ext (read)
  - path/to/other.ext (edit)
- **Verify:** `<focused workspace-native automated command>`
- **Passes when:** <plain-language passing state that proves this task is complete>

---

## Execution groups

<!-- derived from Depends on fields -->

| Wave | Tasks | Notes |
| ---- | ----- | ----- |
| 1 | T1 | <runs first> |
| 2 | T2, T3 | <independent tasks may run concurrently> |

## Learning checkpoints

<!-- omit when empty -->

| After | Wiki target | What to capture |
| ----- | ----------- | --------------- |
| Tn | wiki/topics/<topic>.md | <durable fact, command, gotcha, or open question to evaluate after the task> |

## Execution disciplines

| Skill | Scope | Fetch-fail fallback |
| ----- | ----- | ------------------- |
| superpowers:<skill-name> | <task IDs or plan phase> | <one-line fallback discipline> |

## Decisions log

---

## Touched files

<!-- Populated by /loam::starting on task completion. Deduplicated, edit-marked.
     The planner leaves this section empty. /loam::starting fills it during execution. -->

| Path | Marker | Tasks |
| ---- | ------ | ----- |

---

## Handoff notes

_No handoff yet._
