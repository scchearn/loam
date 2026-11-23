# Coordinator Primitives

## Core operations

- `registerIdentity` for stable actor identity
- `send` for direct or group messaging
- `spawn` for headless worker execution
- `status` for lifecycle state
- `cancel` for cleanup
- `resume` for guarded continuation of a worker session
- `subscribe` for programmatic message delivery

Use the `loam-coordinator` command for one-shot operations. It invokes the owned
interface directly and does not route through a compatibility transport.

## Launch defaults

- prefer headless workers when the user does not need a visible terminal
- record exact provider-qualified model IDs
- pass `runtime`, `workspace`, `taskId`, and `body` as structured spawn fields
- retain the returned `actorId` and `sessionId`; never parse names from prose

## Identity and routing

- route by immutable `actorId`, never by mutable display name
- keep the workspace path canonical and absolute
- use `threadId` and `correlationId` fields for workflow isolation and result pairing

## Intents

- `request`: response or action required
- `inform`: status or result, no response required
- `ack`: explicit no-reply acknowledgment

These are the only intent values. Do not invent new values such as `review`.

## Handoffs

Keep substantive handoff context in the message body or an artifact reference.
Use a stable `threadId` for the workflow and a `correlationId` for each task or
round. The coordinator preserves full message bodies and does not silently
truncate oversized content.
