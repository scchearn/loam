# hcom Primitives

## Core operations

- `hcom send` for direct or group messaging
- `hcom listen` for blocking message receipt
- `hcom events` for querying and waiting on workflow events
- `hcom bundle` for structured context handoff
- `hcom transcript` for reading another agent's conversation
- `hcom term` for terminal visibility and injection
- `hcom opencode` as the preferred default for spawned agents
- `hcom claude|gemini|codex` when the user explicitly wants another runtime or a real exception is needed
- `hcom config` for global and per-agent settings

## OpenCode-first launch defaults

- prefer `--headless` when the user does not need a visible terminal
- prefer `HCOM_OPENCODE_ARGS="--model <provider/model>"` for model selection in launch examples
- use `opencode models` to resolve exact provider-qualified IDs before writing launch commands

## Identity and routing

- direct agents by exact name when precision matters
- use `@<tag>-` routing for group broadcast, for example `@plan-` or `@review-`
- keep routing strategy explicit in the final plan

## Intents

- `request`: response required
- `inform`: respond only if useful
- `ack`: no response expected

These are the intent values to use. Do not invent new intent names.

## Bundles

Document:

- what context is passed
- when to use `normal`, `full`, or `detailed`
- which handoffs need transcript context

Default guidance:

- `normal` for lightweight summaries
- `full` for substantive execution handoffs
- `detailed` for transcript-heavy review or debugging

## Launch planning

A good launch sequence answers:

- which agent launches first
- which agent waits for whom
- which prompts or hints each role needs
- which tags are used for routing
- how cleanup will happen

When showing threaded examples, create the thread once and reuse it. Do not regenerate it per command.
Apply `--thread` on workflow messaging and wait commands, not on `hcom opencode` spawn lines.
