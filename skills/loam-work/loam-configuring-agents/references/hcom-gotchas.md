# hcom Gotchas

## Hard rules

- Always use `--thread` for workflow isolation on `hcom send`, `hcom events`, `hcom listen`, and related workflow commands.
- Do not pass `--thread` to `hcom opencode` spawn commands.
- Never use `sleep` in scripts; use `hcom events --wait` or `hcom listen`.
- Never hardcode generated agent names.
- Always parse launch output for the actual names.
- Always use `hcom kill` for cleanup, not `stop`.
- Always use `--go` on launch and kill in scripted flows.
- For OpenCode launch examples, prefer `HCOM_OPENCODE_ARGS="--model <provider/model>"` and `--headless` unless the user asked for another runtime or a visible terminal.
- Always resolve models from `opencode models` and record provider-qualified IDs.
- If the same family appears under multiple providers, surface the top exact IDs and choose one explicit assumption.
- Always choose an explicit intent on sends.
- Only use valid hcom intents: `request`, `inform`, `ack`.
- Create the workflow thread once and reuse it; do not regenerate it per launch command.
- If you use group routing, use the real `@<tag>-` syntax rather than a made-up alias.

## Thread strategy

Each workflow needs one thread ID shared by:

- sends
- waits
- listeners
- follow-up automation

## Name strategy

Prefer:

- stable tags for groups
- exact names for direct replies
- a documented roster in the generated artifacts

## Risk reminders

Mention collision detection when multiple agents may edit the same files.
Mention stronger execution isolation when proposing a non-default runtime such as Codex.

## Failure patterns to prevent

- leaving thread strategy implicit
- putting `--thread` on `hcom opencode` spawn commands
- returning launch commands without cleanup expectations
- treating group routing as obvious instead of documenting it
- proposing risky execution without a separate reviewer when the task justifies one
- using bare model family names instead of provider-qualified IDs
- silently choosing one provider when several expose the same family
