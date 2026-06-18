# Loading Existing Configs

## Trigger

Use this path when the user asks to load, start, run, resume, or reuse an existing `agents/<slug>.toml`.

Treat that as config loading, not team redesign.

## Source Of Truth

Read the saved config first.

Preserve these values unless the user explicitly asks to change them:

- runtime and launch mode
- provider-qualified model IDs
- stable tags
- thread strategy and thread bootstrap behavior
- intent strategy
- reporting model
- browser automation, session, and safety rules

If the config is missing non-critical detail, use a safe default and state the assumption.
If the config is missing blocking detail, ask only for that detail.

## Generic Loader Contract

Preferred generic interface:

```bash
hcom run agent-config <slug> "<task>"
```

Expected behavior:

1. Locate `agents/<slug>.toml`.
2. Parse team metadata, agent roster, launch commands, thread strategy, and initial assignment guidance.
3. Spawn agents with the provider-qualified model IDs from the config.
4. Use the stable tags from the config as the durable routing addresses.
5. Use the single workflow thread from the config, or generate one only if the config explicitly allows generated threads.
6. Send the user's task according to the config's communication model.
7. Preserve report-flow rules such as "agents report to the spawning agent" when the config defines them.
8. Do not rely on generated hcom names as stable addresses.

## hcom Safety Rules

- Do not pass `--thread` to `hcom opencode` spawn commands.
- Use `HCOM_OPENCODE_ARGS="--model <provider/model>"` for OpenCode model selection.
- Use `--intent request` for initial assignments and work handoffs that require action.
- Use `--intent inform` for agent reports, status updates, and final outcomes.
- Use `--intent ack` only for explicit no-reply acknowledgments.
- Use `--thread` on messages, waits, listeners, and other workflow coordination commands.
- Use tags as the stable addresses.
- Do not hardcode generated hcom names.
- Do not invent intent values beyond `request`, `inform`, and `ack`.

## Response Contract

When the user names a specific saved config, return the exact loader command for that slug.

When the user asks generically, return the generic loader invocation with `<slug>` and `<task>` placeholders.

Only generate an executable wrapper script when the user explicitly asks for automation.
