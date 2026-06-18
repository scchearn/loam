# Output Contract

## Required outputs

The required output depends on the request type.

For a new design or redesign request, the skill must produce:

- `agents/<slug>.toml`
- `agents/<slug>.md`

If the user requested a plan, the answer still must contain both design artifacts.

For a config-loading request, the skill must produce:

- config-loading instructions
- an exact loader command or a generic loader invocation
- a preserved-values summary covering the saved config fields that remain authoritative

Do not replace a saved-config load request with freshly generated artifacts unless the user explicitly asked to redesign or update the config.

## Defaulting rule

If one preference is missing but the topology and safety model are still clear, choose the recommended default and label it as an assumption.

If the same model family is available from multiple providers, surface the top exact provider-qualified IDs and choose one explicit assumption.
If `opencode models` was not actually run, label provider-qualified IDs as assumptions or likely candidates instead of confirmed local availability.

Ask a follow-up only when the missing information would materially change topology, risk, or feasibility.

## Design mode: `agents/<slug>.toml`

Must include:

- team slug
- objective
- topology
- agent roster
- per-agent tool choice
- per-agent exact provider-qualified model choice
- runtime choice and launch mode
- role definitions
- communication defaults
- thread strategy
- explicit group-routing syntax when tags are used
- direct-routing strategy when exact names matter
- intent examples using only `request`, `inform`, or `ack`
- launch commands

Thread strategy must use one workflow thread per run, not a different thread expression on each launch line.
Intent strategy must use only `request`, `inform`, or `ack`.
OpenCode launch examples should use `HCOM_OPENCODE_ARGS="--model <provider/model>"` and should not put `--thread` on spawn commands.

## Design mode: `agents/<slug>.md`

Must include:

- goal
- assumptions when defaults were chosen
- assumptions when live model discovery could not be confirmed
- why this topology was chosen
- role-by-role rationale
- model rationale
- exact provider-qualified model IDs, including shortlisted candidates when one family appears under multiple providers
- reviewer or evaluator design
- communication plan
- runtime choice and launch mode
- explicit group-routing syntax when tags are used
- direct-routing strategy when exact names matter
- intent examples using only `request`, `inform`, or `ack`
- bundle strategy
- launch sequence
- risks and tradeoffs

## Scope control

Design outputs are planning/configuration artifacts.
Loading outputs are loader instructions that reuse an existing config.
Neither mode may claim the project work has already been done.

## Loading mode

When the user asks to load, start, run, resume, or reuse an existing `agents/<slug>.toml`:

- read the existing config first
- preserve runtime, launch mode, provider-qualified model IDs, tags, thread strategy, intent strategy, reporting model, and browser/session/safety rules when present
- only redesign the config if the user explicitly asks to change it
- provide `hcom run agent-config <slug> "<task>"` when a generic loader invocation is appropriate
- provide the exact loader command when the user named a specific slug
- provide optional executable automation only if the user explicitly asked for a script
- keep tags as stable routing addresses
- do not rely on generated hcom names as stable addresses
- use `--intent request` for initial assignments, `--intent inform` for agent reports, and `--intent ack` only for explicit no-reply acknowledgments
- use `--thread` on workflow messages and waits, but never on `hcom opencode` spawn commands

## Extraction-friendly formatting

In design mode, place each artifact name on its own line before the fenced block:

- `agents/<slug>.toml`
- `agents/<slug>.md`

This makes the outputs easier to scan and extract.

In loading mode, include a short `Config loading instructions` heading before the loader invocation.

## Forbidden substitutions

Do not replace the required artifacts with:

- prose-only design notes
- shell scripts only in design mode
- repo setup tasks
- build output
- implementation claims

Do not replace a config-loading answer with regenerated design artifacts unless the user explicitly asked for redesign or artifact refresh.
