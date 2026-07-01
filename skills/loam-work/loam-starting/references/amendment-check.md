# Amendment Check For `loam::starting`

Use this reference when `loam::starting` finds `[>]` tasks within or upstream of the target set during the Pre-flight: amendment check. The hub stops here and asks the user to choose before any execution.

## Scan scope

- **Full run:** check all `[>]` tasks in the plan.
- **Targeted run:** check only `[>]` tasks inside the target set or in its transitive dependencies.

## If there are no relevant `[>]` tasks

Proceed directly to Orientation in the parent skill.

## If there are relevant `[>]` tasks

Before touching code:

1. Map downstream tasks in the target set for each relevant `[>]` task — every `[ ]`, `[~]`, `[h]`, or `[>]` task that depends on it directly or transitively.
2. Classify each `[>]` task:
   - **Blocking** — the `[>]` task is in the dependency chain of the next runnable task in the target set. Cannot proceed without resolving this first.
   - **Non-blocking** — the `[>]` task is not in the dependency chain of the next runnable task (parallel branch, or later). Execution could proceed without it, but it may cause problems later.

STOP. Do not execute any tasks yet. Present to the user:

```text
Amendment check: plans/<slug>.md has [>] tasks that need re-running.

[>] BLOCKING (must resolve before continuing):
  Tx — <title>
    Re-run reason: <from task notes>
    Blocks: Ty, Tz (downstream tasks)

[>] NON-BLOCKING (parallel or later — can defer):
  Tx — <title>
    Re-run reason: <from task notes>
    Would affect: Ty (downstream, but not the immediate next task)

Next runnable task (ignoring [>]): Tx — <title>

What would you like to do?
  a) Re-run all [>] tasks first (recommended — ensures consistency)
  b) Re-run blocking [>] tasks only, then continue
  c) Skip all [>] tasks for now and continue to the next [ ] task
     (WARNING: downstream tasks may need re-running again later)
  d) Tell me which specific [>] tasks to address
```

Wait for the user's choice before proceeding. Then:

- **Choice a** — prepend all `[>]` tasks to the front of the target set queue, in dependency order.
- **Choice b** — prepend only blocking `[>]` tasks to the front of the queue.
- **Choice c** — remove all `[>]` tasks from the queue entirely. Note in the Decisions log: `User chose to defer [>] tasks (Tx, Ty) — these re-runs are still pending.`
- **Choice d** — follow the user's specific direction.

After the user's choice is applied, return to the parent skill's Orientation step.