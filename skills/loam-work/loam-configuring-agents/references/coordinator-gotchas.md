# Coordinator Gotchas

## Hard rules

- Register identities before sending or subscribing.
- Address recipients by immutable `actorId`, not display name.
- Always set an explicit intent on sends.
- Use only `request`, `inform`, or `ack` intents.
- Create one workflow `threadId` and reuse it across the debate.
- Keep task pairing in `correlationId`; do not infer it from message prose.
- Treat `startup: "failed"` as a real failure, not a running worker.
- Treat `providerSessionChanged: true` from `resume` as a context reset.
- Run local verification after `DONE:` or `APPROVED:` before reporting completion.
- Cancel workers through the coordinator after the workflow ends.

## Failure patterns to prevent

- using mutable display names as authorization or routing keys
- silently falling back to another transport when the coordinator is unavailable
- accepting a worker's handoff as proof without running the verify command
- treating a changed provider session as a transparent resume
- inventing intent values or storing routing state only in prose
- launching workers without a canonical workspace or task correlation
