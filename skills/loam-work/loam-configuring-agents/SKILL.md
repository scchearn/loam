---
name: loam-configuring-agents
description: "Use when planning or configuring an hcom-based AI agent team for a real task, or when loading, starting, resuming, or reusing an existing `agents/<slug>.toml` with preserved runtime, model, routing, and reporting settings."
compatibility: Repo-local OpenCode skill for hcom planning and configuration. Assumes markdown references, hcom concepts, and local model discovery with `opencode models`.
metadata:
  version: "1.0.0"
  author: scchearn
  phase: planning
  system: hcom
  outputs: agents-slug-artifacts-or-loader
---

# loam::configuring-agents

## Overview

Plan an hcom team or reuse a saved config, and emit concrete outputs that match real hcom and OpenCode launch behavior.

Core principle: produce a usable agent architecture package or loader path, not just advice. This skill exists to prevent five baseline failures:

- stopping at design discussion or a clarifying question when safe defaults would work
- returning launch commands or repo changes without the requested `agents/<slug>` artifacts
- drifting into actual project implementation instead of staying in planning/configuration scope
- emitting launch guidance with invalid OpenCode spawn flags or ambiguous bare model names
- treating a saved-config load request like a brand new team design instead of reusing the existing config and loader path

## When To Use

Use this skill when the user wants any of the following:

- an hcom team design
- agent topology selection
- model or role assignment
- a reviewer or evaluator loop
- sandboxed execution planning
- launch commands for a multi-agent workflow
- generated `agents/<slug>.toml` and `agents/<slug>.md` outputs
- loading, starting, running, resuming, or reusing an existing `agents/<slug>.toml`

Do not use this skill for:

- installing or troubleshooting hcom itself
- open-ended research
- building the target project
- executing the planned workflow

If the user needs hcom installation or delivery troubleshooting, prefer the dedicated hcom messaging skill when it is available. If it is not available, say that installation or troubleshooting is out of scope for this skill and keep the answer in planning/configuration scope.

## Input

The planning request is: $ARGUMENTS

## Quick Reference

| Signal | Default |
|---|---|
| Small, low-risk task | single agent |
| Code change with mandatory review | worker-reviewer |
| Risky implementation or mixed roles | planner-executor-reviewer |
| 3+ distinct responsibilities | hub-spoke |
| User does not specify runtime | `hcom opencode --headless` |
| User does not specify models | resolve exact OpenCode IDs with `opencode models`; narrow with `opencode models <provider>` when needed |
| Same family appears under multiple providers | surface top exact IDs and choose one assumption |
| User does not specify thread strategy | one unique thread per workflow |
| Missing non-critical preference | choose recommended default and label assumption |
| User asks to load/start/reuse a saved config | read the existing `agents/<slug>.toml` first and keep it as the source of truth |
| User asks for executable automation | provide a script only if they explicitly request automation; otherwise give the loader invocation |

## Workflow

### 1. Resolve scope first

Decide whether the user is asking for:

- a new single-agent plan
- a new multi-agent team plan
- generated planning artifacts
- loading or reusing an existing config
- immediate implementation

This skill only handles the planning/configuration layer and the config-loading layer.

If the user asks to build or run the project, do **not** pretend to execute it. Reframe the request into either:

- the hcom planning deliverable for a new design, or
- the loader path for a saved config that would be used for execution

### 2. Branch by request type

If the user says any of these, treat the request as config loading instead of team redesign:

- `load`
- `start`
- `run`
- `resume`
- `reuse`
- `existing config`
- a direct reference to `agents/<slug>.toml`

For config-loading requests:

- read the existing config first
- keep the saved config as the source of truth for runtime, launch mode, provider-qualified model IDs, tags, thread strategy, intent strategy, and reporting model
- preserve browser automation, session, and safety rules from the config when they exist
- only redesign the config if the user explicitly asks to change the team or values inside it
- ask only for blocking information that the saved config and user request do not supply
- otherwise use safe defaults, state the assumptions, and keep the existing config intact
- read `references/loading-configs.md` and `references/hcom-gotchas.md` before composing loader guidance

For new design requests, continue with the planning path below.

### 3. Ask only when truly blocked

Ask a follow-up question only if the missing detail would materially change topology or safety.

Do **not** stop for preferences that have a sensible default.

If a user does not specify a tool pairing or reviewer model, choose a recommended default and record it as an assumption.

Default assumptions:

- code change with review gate: `worker-reviewer`
- risky implementation: `planner-executor-reviewer`
- mixed-model risky work: strong OpenCode planner, headless OpenCode executor, separate reviewer
- runtime: `hcom opencode --headless`
- exact model IDs: resolve with `opencode models`, narrow with `opencode models <provider>` when needed, and record provider-qualified IDs; if live discovery is unavailable, label chosen IDs as assumptions
- duplicate provider families: surface top candidates and choose one explicit assumption
- group routing: stable tags
- workflow isolation: one unique thread per workflow

### 4. Gather the architecture decisions for new designs

Always resolve these decisions before producing the artifacts:

1. task complexity
2. topology
3. runtime and launch mode
4. model strategy
5. exact model resolution
6. role assignments
7. reviewer/evaluator requirement
8. sandbox requirement
9. communication and routing strategy
10. bundle depth and handoff points
11. launch sequence

Read references only when needed:

- `references/topologies.md` for topology tradeoffs
- `references/model-selection.md` for cost/capability tradeoffs
- `references/roles-and-reviewers.md` for role boundaries and quality gates
- `references/loading-configs.md` for saved-config loader behavior
- `references/hcom-primitives.md` for messaging, bundles, transcript, and launch primitives
- `references/hcom-gotchas.md` before finalizing any launch guidance
- `references/output-contract.md` immediately before generating outputs

### 5. Produce the right output for the request type

For new design or redesign requests, you must generate both outputs:

- `agents/<slug>.toml`
- `agents/<slug>.md`

Use these templates:

- `assets/agents-template.toml`
- `references/agents-template.md`

For config-loading requests, do **not** replace the saved config with freshly generated artifacts unless the user explicitly asked to redesign or update them.

Instead, return config-loading instructions:

- the exact command to load the saved config, or the generic loader invocation when the user did not name a slug
- the preserved values that the loader must keep from the config
- any assumptions or blocking gaps that still matter

Optional script generation is allowed only when the user explicitly asks for executable automation.

Do not substitute any of these with:

- a prose-only answer
- launch commands alone for new design requests
- a `.hcom/` directory design
- a shell script unless the user explicitly asked for automation
- repo documentation

If the user asked for a plan, return the plan **and** the design artifacts.

Derive `<slug>` from the task as short kebab-case, for example:

- `review-loop`
- `risky-implementation`
- `migration-squad`

### 6. Output rules

In design mode, the TOML must be machine-readable and the markdown must explain the rationale.

In loading mode, the response must explain how to load the saved config without redesigning it.

If assumptions were needed:

- include them under `## Assumptions` in the markdown artifact
- reflect them in the TOML values instead of leaving blanks
- for config loading, state them next to the loader instructions and keep the saved config as the source of truth

Always include the relevant equivalents of:

- runtime choice and launch mode
- provider-qualified model IDs
- thread strategy
- intent strategy
- launch commands or loader invocation
- reviewer/evaluator guidance when quality matters
- sandbox notes when risky execution is proposed
- concrete routing examples when tags are part of the design
- reporting path when the config routes reports back to the spawning agent

Intent values are limited to:

- `request`
- `inform`
- `ack`

Do not invent new intent names.

When you show a launch sequence:

- initialize the workflow thread once, or reuse the configured thread when the saved config already defines it
- reuse the same thread value across the workflow
- only generate a new thread when the config explicitly allows generated threads
- do not regenerate the thread separately per launch command
- do not pass `--thread` to `hcom opencode` spawn commands
- if you use tag routing, show it in the real `@<tag>-` form such as `@plan-`
- for OpenCode launch examples, prefer `HCOM_OPENCODE_ARGS="--model <provider/model>"` and `--headless` unless the user asked for another runtime or a visible terminal
- do not use bare model family names like `gpt-5.5`; record full provider-qualified IDs such as `openai/gpt-5.5`
- use `--intent request` for initial assignments and handoffs that require action
- use `--intent inform` for status updates, reports, and final outcomes
- use `--intent ack` only for explicit no-reply acknowledgments
- use `--thread` on messages, waits, listeners, and other workflow coordination commands
- use tags as the stable addresses for group routing
- do not hardcode generated hcom agent names as stable addresses
- when multiple providers offer similar IDs, mention `opencode models <provider>` as the narrowing step
- if you could not actually run `opencode models`, say the provider-qualified IDs are assumptions or likely candidates rather than confirmed local availability
- if browser automation is configured, preserve the config's browser, session, and safety rules

### 7. Final response format

For a new design or redesign request, return the answer in this order:

1. short summary of the chosen topology and why it fits
2. short assumptions list if defaults were chosen
3. a literal heading line containing `agents/<slug>.toml`, followed by a fenced `toml` block
4. a literal heading line containing `agents/<slug>.md`, followed by a fenced `md` block
5. short risks or follow-up notes

For a config-loading request, return the answer in this order:

1. short summary of which saved config is being loaded and why redesign is unnecessary
2. short preserved-values list covering runtime, model IDs, tags, thread strategy, intent strategy, and reporting model, plus browser/session/safety rules when present
3. a short `Config loading instructions` section with the exact command or generic loader invocation, such as `hcom run agent-config <slug> "<task>"`
4. concise loader notes covering thread handling, routing, intents, and report flow
5. short assumptions or blocking gaps
6. an optional script only if the user explicitly asked for executable automation

## Common Mistakes

| Failure pattern | Counter-rule |
|---|---|
| "I need the exact tool pairing before I can generate artifacts" | Choose the recommended pairing and label it as an assumption unless safety truly depends on the choice. |
| "I will give launch guidance but not the artifacts yet" | In design mode, always emit both `agents/<slug>` artifacts in the same answer. |
| Treating "load this config" as a new team design | Read the existing config and provide or run the loader path instead. |
| "I should set up `.hcom/` scripts and docs instead" | This skill produces planning/configuration artifacts, not repo changes. |
| "The user asked to build the project, so I should start inspecting it" | Stay in planning scope and provide the hcom team/config that would enable execution. |
| "Threading is obvious, I can omit it" | Always include explicit thread and intent strategy. |
| "I can invent my own intent label like `review`" | Only use the actual hcom intent values: `request`, `inform`, or `ack`. |
| "I can put `--thread` on every `hcom` command, including agent spawn" | Use `--thread` on workflow messaging and wait commands, not on `hcom opencode` spawn lines. |
| "I can create a fresh thread inside every launch command" | Create the workflow thread once, then reuse it across launches, sends, waits, and cleanup. |
| "The tag syntax is obvious, I can write `@tag-plan` or another made-up form" | Use the real hcom group-routing syntax: `@<tag>-`, such as `@plan-` or `@review-`. |
| "I can use generated hcom names as stable addresses in the loader" | Use tags as stable addresses and only resolve generated names ephemerally when absolutely necessary. |
| "I can write just `gpt-5.5` or `glm-5.1` and the runtime will know what I mean" | Resolve and record the exact provider-qualified OpenCode model ID. |
| "If multiple providers expose the same family, I should silently pick one" | Surface the top exact IDs, then choose one explicit assumption in the artifacts. |
| "I can present provider-qualified IDs as confirmed even when I did not run discovery" | If `opencode models` was not actually run, label the IDs as assumptions or likely candidates. |

## Example Default

For a risky implementation request with no model preferences:

- planner: strong OpenCode reasoning model resolved from `opencode models`
- executor: headless OpenCode worker by default
- reviewer: separate strong reviewer
- if the requested family appears under multiple providers, surface the top exact IDs and choose one assumption

That should normally become a `planner-executor-reviewer` plan with a unique workflow thread, headless OpenCode launch commands, tag-based routing, and reviewer signoff before completion.
