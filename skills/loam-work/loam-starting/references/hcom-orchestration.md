# hcom Orchestration For `loam::starting`

Use this reference when `loam::starting` encounters concrete `Execution` metadata and must orchestrate headless workers from the current session.

## Core principle

The current session is the visible hub. Spawn only the worker roles you need. Keep one workflow thread per delegated group. The worker reports `DONE:` or `APPROVED:` on-thread, then the hub runs the plan's verify command locally before marking anything `[x]`.

## Structured `Execution` metadata

Preferred form:

```md
- **Execution:** topology: hub-spoke | agent: recon-del | worktree: .worktrees/admin-delete | branch: feature/admin-delete-restore | model: opencode-go/kimi-k2.6 | rules: be thorough, never ask interactive questions, use hcom send for clarification
```

Continuation shorthand:

```md
- **Execution:** same agent as T3
```

If the task only has advisory topology text and lacks the concrete fields needed to launch safely, fall back to inline hub execution for that session and log the fallback.

## Completion signal vocabulary

Require delegated workers to use one of these on the shared thread:

- `DONE: <task ids>` — implementation is ready for hub verification
- `APPROVED: <task ids>` — internal review passed; still requires hub verification
- `BLOCKED: <reason>` — worker cannot proceed safely
- `FIX:` — hub sends this back after local verification fails

`DONE:` and `APPROVED:` are handoff signals, not completion by themselves.

## Pre-flight checks

Before launching workers:

1. Check `hcom` availability. If unavailable, execute inline and log the fallback.
2. Inspect any requested worktree or branch.
3. Reuse an existing worktree only when it is on the expected branch and safe to reuse.
4. If the worktree exists on a different branch, or contains conflicting unexpected changes, stop and ask the user.

## Worktree preparation

Check existing worktrees:

```bash
git worktree list
```

Create a new worktree when needed:

```bash
git worktree add ".worktrees/admin-delete" -b "feature/admin-delete-restore"
```

If the branch already exists, create the worktree without `-b` and reuse the branch explicitly.

## Thread bootstrap

Use one workflow thread per delegated group:

```bash
export WF_THREAD="<plan-slug>-$(date +%Y%m%d%H%M%S)"
```

Reuse the same `WF_THREAD` across sends, waits, FIX loops, and cleanup. Do not regenerate it per command.

## Launch pattern

Spawn the worker headless. Do not put `--thread` on spawn commands.

```bash
AGENT_OUT=$(HCOM_OPENCODE_ARGS="--model <provider/model>" hcom opencode --tag <tag> --headless --go 2>&1)
AGENT_NAME=$(printf '%s\n' "$AGENT_OUT" | grep '^Names: ' | sed 's/^Names: //' | tr -d ' ')
```

Never hardcode the launched name. Parse it from the `Names:` line every time.

## Delegation group selection

When `loam::starting` Orientation selects runnable tasks in the same execution wave and hcom is available, build the delegation group:

1. Group only tasks whose dependencies are already satisfied and whose Files/constraints do not conflict.
2. Use `needs-independent-review`, `risk:data-destructive`, and `needs-isolation` to choose the safest delegation pattern.
3. Preserve task order inside each delegated assignment. The worker may execute sequentially, but the hub still verifies before anything is marked `[x]`.
4. If labels are insufficient to launch safely, execute inline as hub tasks for this session and log the fallback.

## Active `[h]` group handling

If Orientation selected an existing `[h]` group rather than a fresh task, wait on its workflow thread with `hcom events --wait`, accept `DONE:` or `APPROVED:` as handoff signals, and treat `BLOCKED:` as a blocker. Write a handoff note, and mark the first unresolved task `[!]` if needed.

## Assignment message

Send the worker everything it needs in one request:

```bash
hcom send @<tag>- --thread "$WF_THREAD" --intent request -- "
You are handling tasks T3-T5.
Task titles: ...
Execution order inside the group: T3 -> T4 -> T5.
Files to read: ...
Files to modify: ...
Verify commands the hub will run: ...
Rules: ...
When ready, send DONE: T3-T5 on this thread.
If you are blocked, send BLOCKED: <reason> on this thread and stop.
"
```

Always put `--` before the message body. Use `--intent request` for assignments and `--intent inform` for routine status updates.

## Waiting for signals

Wait on the workflow thread instead of sleeping:

```bash
hcom events --wait 300 --sql "type='message' AND msg_thread='${WF_THREAD}' AND (msg_text LIKE 'DONE:%' OR msg_text LIKE 'APPROVED:%' OR msg_text LIKE 'BLOCKED:%')"
```

Notes:

- Exit code `0` means a matching event was found.
- Exit code `1` means timeout.
- Exit code `2` means SQL error.

Use `BLOCKED:` as a real blocker. Do not ignore it.

## Hub verification loop

When the worker reports `DONE:` or `APPROVED:`:

1. Run every delegated task's verify command locally.
2. If everything passes, mark the delegated tasks `[x]`.
3. If verification fails, send a `FIX:` message back to the worker with the concrete error details and wait again.
4. Maximum 3 hub verify rounds.

Example FIX loop:

```bash
hcom send @<tag>- --thread "$WF_THREAD" --intent request -- "FIX: pnpm check failed with <error details>. Please fix the issue and send DONE: T3-T5 when ready."
```

Keep the delegated tasks `[h]` until hub verification passes.

5. If the worker reports `BLOCKED:`, or the hub verify loop still fails after 3 rounds, mark the first unresolved task `[!]`, revert later unresolved tasks to `[ ]` if they never truly started, append to the Decisions log, write a handoff note, and stop.

## Cleanup

Always clean up launched workers with `hcom kill` and `--go`:

```bash
hcom kill "$AGENT_NAME" --go
```

Do not use `stop` here. `kill` cleans up the headless worker reliably.

## Fallback rules

Fall back to inline hub execution for this session when any of these apply:

- `hcom` is unavailable
- `Execution` metadata is advisory-only and lacks safe launch fields
- worktree or branch state is too ambiguous to reuse safely and the user is unavailable to resolve it

When you fall back:

1. Note the reason in the Decisions log
2. Execute the tasks locally as hub tasks
3. Leave the `Execution` metadata in the plan intact; the fallback is session-specific

## Common mistakes

| Mistake | Fix |
|---|---|
| Launching a second visible hub | Keep the current session visible; spawn only workers headless |
| Putting `--thread` on spawn commands | Put `--thread` on `hcom send` and `hcom events`, not on `hcom opencode` |
| Marking delegated tasks `[x]` after a worker says `DONE:` | Keep them `[h]` until the hub verify step passes |
| Using `sleep` to wait for workers | Use `hcom events --wait` |
| Hardcoding worker names | Parse the `Names:` line from launch output |
| Reusing a dirty worktree blindly | Inspect `git worktree list` and stop if reuse is unsafe |
