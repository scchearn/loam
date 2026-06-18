# Model Selection

## Decision rule

Choose exact OpenCode model IDs by role and availability, not by brand loyalty or bare family names.

## Discovery first

Use:

- `opencode models` to list all available exact model IDs
- `opencode models <provider>` to narrow to one provider such as `openai`, `google`, `ollama`, or `openrouter`

Hard rules:

- record full provider-qualified IDs such as `openai/gpt-5.5`
- do not rely on bare family names such as `gpt-5.5`, `glm-5.1`, or `gemini-2.5-pro`
- assume the same family may exist under multiple providers
- if live discovery is unavailable in the current environment, label chosen provider-qualified IDs as assumptions or likely candidates rather than confirmed local availability

## Duplicate family handling

When the requested or implied family exists under multiple providers:

- surface the top 2-3 exact IDs
- briefly note why they are viable candidates
- choose one exact ID as an explicit assumption in the generated artifacts
- if provider choice materially changes cost, latency, or safety, call that out in risks or tradeoffs

If only one viable exact ID is available, use it directly.
If you could not run discovery, present the shortlist as likely candidates and make the chosen ID an explicit assumption.

## Default runtime

Prefer:

- `hcom opencode --headless`

Use when:

- the user did not ask for a visible terminal
- no other runtime is explicitly required
- the team only needs planning, review, or constrained execution guidance

## Coordinator / planner

Prefer:

- strong reasoning models available in OpenCode
- provider-qualified IDs resolved from `opencode models`

Use when:

- the agent must ask good architecture questions
- the agent must synthesize tradeoffs
- the agent must keep the whole plan coherent

## Fast spoke workers

Prefer:

- lower-cost fast OpenCode models with exact provider-qualified IDs

Use when:

- the task is narrow
- speed matters more than deep reasoning
- the worker is operating inside a clearer contract

## Default executor

Prefer:

- a headless OpenCode worker by default
- another runtime only when the user asked for it or a real capability gap exists

Use when:

- code changes should stay on the default runtime
- the workflow benefits from an execution-focused worker
- the user did not require a different tool

## Isolated executor exception

Prefer:

- Codex or another isolated runtime only when stronger sandbox or tool isolation is an explicit requirement

Use when:

- code changes must happen in a stricter execution environment
- risky shell work justifies a non-default runtime
- the team wants a distinct execution worker

## Reviewer / evaluator

Prefer:

- a stronger reasoning model than the worker when possible
- a different model family than the worker when reviewer diversity matters
- exact provider-qualified IDs rather than family aliases

Use when:

- code review or output validation matters
- the task is risky enough to justify a quality gate
- the user explicitly wants a second opinion

## Single-model strategy

Use when:

- cost predictability matters
- consistency matters more than specialization
- the team is small

## Mixed-model strategy

Use when:

- the task benefits from role specialization
- cost can be reduced by using cheaper spoke workers
- sandboxing or review isolation matters

## Recommended defaults

| Situation | Default |
|---|---|
| simple planning | one strong OpenCode planner model |
| risky implementation | strong OpenCode planner + headless OpenCode executor + separate reviewer |
| low-cost parallel workers | strong OpenCode coordinator + cheaper OpenCode spokes |
| review-heavy workflow | keep reviewer at least as strong as worker |

## Common mistakes

- using the same agent to plan, execute, and review risky work
- choosing a cheap model for the coordinator when the task is mostly tradeoff reasoning
- making the reviewer weaker than the executor on high-risk work
- writing bare family names instead of provider-qualified IDs
- assuming `openai/gpt-5.5` and `openrouter/openai/gpt-5.5` are interchangeable without naming the chosen one
