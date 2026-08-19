---
title: Greeting Plan
slug: greeting-plan
spec: specs/greeting-spec.md
description: Implement and verify greet(name) per the greeting spec.
status: in-progress
task_count: 1
created_at: 2026-08-05 10:00 +02:00
started_at: 2026-08-10 09:00 +02:00
completed_at: null
goal: goals/improve-greeting.md
---

## Spec

specs/greeting-spec.md

## Goal

goals/improve-greeting.md

## Acceptance criteria

- [x] `greet('Ada')` returns `"Hello, Ada!"`.

## Tasks

### T1 — Implement greet(name)

- Status: done
- Depends on: none
- Outcome: `src/greeter.js` exports `greet(name)`.
- Steps: Write the function; document it in `wiki/code/greeter.md`.
- Constraints: none
- Watch for: none
- Files: src/greeter.js, wiki/code/greeter.md
- Verify: `node -e "console.log(require('./src/greeter.js'))"`
- Passes when: `greet('Ada')` returns `"Hello, Ada!"`.

## Execution groups

| Group | Tasks |
| --- | --- |
| 1 | T1 |

## Learning checkpoints

| After | Checkpoint |
| --- | --- |
| T1 | wiki/checkpoints/checkpoint-2026-08-10-0900.md |

## Execution disciplines

| Discipline | Note |
| --- | --- |
| none | none |

## Decisions log

none

## Touched files

| Path | Task |
| --- | --- |
| src/greeter.js | T1 |

## Handoff notes

none
