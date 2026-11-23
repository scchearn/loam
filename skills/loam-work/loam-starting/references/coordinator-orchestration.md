# Coordinator Orchestration For `loam::starting`

Use this reference when `loam::starting` encounters concrete `Execution` metadata
and must orchestrate workers from the current session.

## Core principle

The current session is the visible hub. Spawn only the worker roles you need. A
worker reports `DONE:` or `APPROVED:` through the coordinator message path, then
the hub runs the plan's verify command locally before marking anything `[x]`.

## Direct interface

The coordinator is the only agent-work transport. Do not invoke a legacy
transport, parse terminal output, or create a compatibility facade. The direct command accepts a
JSON payload and calls the owned `Coordinator` interface:

```bash
loam-coordinator registerIdentity --json '{"runtime":"opencode","workspace":"<absolute-path>","displayName":"<stable-label>"}'
loam-coordinator send --json '{"sender":"<actor-id>","recipients":["<recipient-id>"],"body":"...","intent":"request","threadId":"<workflow-id>"}'
loam-coordinator spawn --json '{"runtime":"opencode","workspace":"<absolute-path>","taskId":"T3-T5","body":"...","model":"<provider/model>","profile":"<profile>"}'
loam-coordinator status --json '{"actorId":"<actor-id>"}'
loam-coordinator cancel --json '{"actorId":"<actor-id>"}'
```

Set `LOAM_COORDINATOR_MODULE` to the absolute path or `file://` URL of a module
exporting a ready `coordinator` object. The command fails closed when the module
is absent or does not implement the complete interface.

## Completion signal vocabulary

Require delegated workers to send one of these through `send`:

- `DONE: <task ids>` — implementation is ready for hub verification
- `APPROVED: <task ids>` — internal review passed; still requires hub verification
- `BLOCKED: <reason>` — worker cannot proceed safely
- `FIX:` — hub sends this after local verification fails

These are message bodies, not coordinator intent values. Use `request` for work
and fixes, `inform` for status and outcomes, and `ack` only when no reply is
needed.

## Pre-flight checks

Before launching workers:

1. Confirm `LOAM_COORDINATOR_MODULE` is configured in the injected session environment.
2. Inspect any requested worktree or branch.
3. Reuse an existing worktree only when it is on the expected branch and safe to reuse.
4. If the worktree contains conflicting unexpected changes, stop and ask the user.

## Delegation group selection

Group only tasks whose dependencies are already satisfied and whose files and
constraints do not conflict. Preserve task order inside a delegated assignment.
The hub still verifies before anything is marked `[x]`.

## Assignment message

Send the worker everything it needs in one `send` request:

```json
{
  "sender": "<hub-actor-id>",
  "recipients": ["<worker-actor-id>"],
  "intent": "request",
  "threadId": "<workflow-id>",
  "body": "You are handling tasks T3-T5.\nExecution order: T3 -> T4 -> T5.\nFiles to read: ...\nFiles to modify: ...\nVerify commands: ...\nRules: ...\nWhen ready, send DONE: T3-T5. If blocked, send BLOCKED: <reason>."
}
```

Create one workflow ID per delegated group and reuse it for assignments, replies,
and fixes. Do not put routing metadata in the prose body when an interface field
exists.

## Hub verification loop

When a worker reports `DONE:` or `APPROVED:`:

1. Run every delegated task's verify command locally.
2. If everything passes, mark the delegated tasks `[x]`.
3. If verification fails, send a `FIX:` request with the concrete error details.
4. Keep tasks pending until verification passes; allow at most 3 fix rounds.

Use `status` polling for lifecycle checks. A worker that exits, reports an error,
or changes provider session is not silently treated as successful.

## Cleanup

Cancel workers with the coordinator's `cancel` operation after verification or
when the workflow is abandoned. Do not terminate runtime processes directly.

## Fallback rules

If the coordinator is unavailable or the execution metadata lacks safe launch
fields, execute inline as the hub and record the fallback. Never silently use a
second transport.
