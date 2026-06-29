---
name: loam::configuring-agents
description: "Use when agents must debate, conference, deliberate, or reach consensus on a goal — competing positions argue and converge on one deliverable, adversarial review with synthesis, multi-stakeholder deliberation, structured disagreement with a forcing-field deliverable. Triggers: 'have agents debate X', 'reach consensus on Y', 'argue distinct positions and converge'. Not for saved team configs, agents/<slug> artifacts, implementation, or open-ended research."
compatibility: "Requires hcom for live agent coordination. The prepared plan can be returned without hcom when approval is denied or hcom is unavailable."
metadata:
  version: "2.0.0"
  author: scchearn
  phase: execution
  outputs: prepared-debate-plan-and-convergence-result
---

# loam::configuring-agents

## Overview

Run a structured debate, conference, or deliberation between agents so they reach consensus on a goal. The skill produces a prepared debate plan — hub prompt, distinct role briefs, convergence prompt, round count, stopping rule, forcing-field deliverable, and the exact first hcom action — and presents it for human approval. No agent contact happens before approval. If approval is denied or absent, the prepared plan is the deliverable and the skill stops.

Core principle: produce a runnable consensus protocol, not just advice. This skill exists to prevent five baseline failures:

- stopping at the parameter interview when safe defaults would compose a complete prepared plan
- firing any hcom send, spawn, thread seed, or launch before explicit per-debate human approval
- drifting into project implementation instead of staying in debate/consensus scope
- emitting launch guidance with invalid OpenCode spawn flags or ambiguous bare model names
- treating a one-off debate as a reusable saved team config — that contract is retired

## When To Use

Use this skill when the user wants any of the following:

- a structured debate between agents arguing distinct positions
- a conference or roundtable to work out a goal
- deliberation that converges on a single deliverable
- agents to reach consensus on a decision, design, or direction
- an adversarial review where two or more positions must argue and converge

Do not use this skill for:

- producing `agents/<slug>.toml` or `agents/<slug>.md` artifacts — that contract is retired
- loading, reusing, or running saved team configs — that behavior is retired
- installing or troubleshooting hcom itself
- open-ended research without a convergence goal
- building the target project — the skill runs a debate about the goal, it does not implement the goal

If the user asks for saved team artifacts or a saved-config loader, state that the contract is retired and offer to compose the debate inline instead. Do not regenerate the old artifacts.

## Non-negotiables

Three rules with no exceptions. Violating any produces work that looks right but bypasses the gate.

1. **Proposal-first on agent coordination.** The skill produces the complete prepared debate plan — hub prompt, role briefs for each position, convergence prompt, round count, stopping rule, forcing-field deliverable, and the exact first hcom action that would fire — and presents it to the user. No hcom send, no agent spawn, no thread seed, no launch command runs until the user explicitly approves. If approval is denied or absent, the skill returns the prepared plan and stops. The prepared plan is a valid deliverable in that case; it is not a failure state.

2. **The gate is per-debate, not per-session.** Approval covers this debate only. A new debate needs new approval. Approval is not transferable to a later run, a different tension axis, or a different agent roster.

3. **"No" means stop, not renegotiate.** If the user denies approval, the skill returns the prepared plan and stops. It does not loop back to re-open parameters. Renegotiation is a fresh invocation with new `$ARGUMENTS`, not a continuation of this one. The gate is a confirmation step, not a clarification loop.

## Check hcom availability

Before entering the hcom-specific workflow, check whether hcom is available:

```bash
which hcom 2>/dev/null
```

- **If hcom is available:** continue with the workflow below (sections 1–7 + common mistakes). This is the primary mode.
- **If hcom is not available:** see [When hcom is unavailable](#when-hcom-is-unavailable) at the bottom. The interview, gate, and mechanics are identical; only the dispatch mechanism differs.

## Input

The consensus request is: $ARGUMENTS

## Quick Reference

| Signal | Default debate topology |
|---|---|
| 2 distinct positions, no synthesis needed | position-vs-position |
| 2 distinct positions + bias reduction | position-vs-position-with-synthesizer |
| 3+ distinct positions or multi-stakeholder | multi-position-roundtable |
| independent-answer aggregation with judgment | ensemble-with-judge |
| User does not specify runtime | `hcom opencode --headless` |
| User does not specify models | resolve exact OpenCode IDs with `opencode models`; narrow with `opencode models <provider>` when needed |
| Same family appears under multiple providers | surface top exact IDs and choose one assumption |
| User does not specify round count | 2 rounds |
| User does not specify convergence rule | forcing-field deliverable; rounds end when positions converge or stop rule fires |
| User does not specify hub stance | neutral |
| Missing non-critical preference | choose recommended default and label assumption |

## Workflow

### 1. Resolve scope first

Decide whether the user is asking for:

- a structured debate or conference to reach consensus on a goal
- a one-off deliberation with a convergence deliverable
- something else (implementation, research, saved config)

This skill handles only the consensus/debate layer. If the user asks to build or run the project, do **not** pretend to execute it. Reframe the request into a debate about the goal, or route them to the matching skill (`loam::planning`, `loam::starting`, `loam::writing-spec`).

If the user asks for saved `agents/<slug>` artifacts or a saved-config loader, state that the contract is retired and offer to compose the debate inline.

### 2. Gather parameters via interview

Resolve these decisions before composing the prepared plan:

1. **Goal or deliverable** — what consensus output the debate produces. Must be concrete enough to force convergence.
2. **Tension axis** — the actual disagreement the debate explores. If there is no real tension, there is no debate; route the user elsewhere.
3. **Agents and roles** — which agents argue which positions. At least 2 distinct positions. No clones: distinct models, distinct prompts, or both. Disagreement is mandated, not optional.
4. **Hub stance** — neutral (orchestrates only), partisan-for-synthesis (argues a position and synthesizes), or participant (argues a position, no synthesis privilege).
5. **Convergence rule** — when the debate ends. Default: rounds end when positions converge or the stop rule fires.
6. **Forcing field** — the structural constraint in the deliverable that makes faking agreement harder than agreeing. Every debate has one; if none is visible, design one before proceeding.
7. **Scope facts** — the evidence, constraints, or context the debate must respect.
8. **Round count** — default 2. Override only when the user states a count.

Ask a follow-up only if the missing detail would materially change the tension axis, agent roster, or convergence rule. Do not stop for preferences that have a sensible default.

Read references only when needed:

- `references/topologies.md` for debate topology tradeoffs
- `references/model-selection.md` for cost/capability tradeoffs across positions
- `references/roles-and-reviewers.md` for role-brief design and mandate-disagreement rules
- `references/output-contract.md` immediately before composing the prepared plan
- `references/hcom-primitives.md` for messaging, transcript, events, and launch primitives
- `references/hcom-gotchas.md` before finalizing any launch or send guidance

### 3. Compose the prepared debate plan

Produce, in one block:

- the goal or deliverable
- the tension axis and the positions being argued
- the agents (by tag or name), their assigned positions, and their role briefs
- the hub stance
- the round count and the stopping rule
- the convergence prompt
- the forcing-field deliverable spec
- scope facts the debate must respect
- assumptions, if defaults were chosen
- the **exact first hcom action** that would fire if approved — a verbatim `hcom send` or spawn command, not a placeholder

The prepared plan is the proposal. It must be complete enough that approval is a yes/no decision, not a clarification round.

### 4. Present the plan and ask explicit approval

Show the prepared plan to the user in one block. Then ask:

> "Run this debate now? (y/n)"

or the host harness's explicit-confirmation equivalent. Do not phrase this as a rhetorical question, a status update, or an FYI.

- If **yes**: proceed to Step 5. The approval covers this debate only.
- If **no**, or no answer: return the prepared debate plan as the output. Do not send, spawn, seed, or launch anything. State that the plan is ready and can be run later by re-invoking the skill or by the user issuing the prepared sends manually.

The gate is per-debate, not per-session. Approval is not transferable.

### 5. Run the rounds

After approval, execute the prepared plan:

1. **Brief the agents** — send each role brief to its assigned agent on the workflow thread. Use `--intent request` for assignments.
2. **Collect openings** — each agent argues its opening position on-thread.
3. **Run rounds** — default 2 rounds. Each round: agents respond to the other positions, then the hub advances or closes.
4. **Apply the stopping rule** — when the stop condition fires, rounds end. Do not add a round without a stated reason.
5. **Converge** — issue the convergence prompt. The hub (or the synthesizer role, if the topology includes one) produces the forcing-field deliverable.

Report the convergence deliverable on-thread per loam reporting norms. Use `--intent inform` for the final outcome.

### 6. Output rules

The skill always produces the prepared debate plan. If approved, the convergence result is appended on-thread.

If assumptions were chosen, include them under `## Assumptions` in the prepared plan.

When the prepared plan includes a launch sequence:

- initialize the workflow thread once and reuse it across the debate
- do not pass `--thread` to `hcom opencode` spawn commands
- use tags as stable addresses for group routing, in the real `@<tag>-` form such as `@pro-` or `@con-`
- for OpenCode launch examples, prefer `HCOM_OPENCODE_ARGS="--model <provider/model>"` and `--headless` unless the user asked for a visible terminal
- do not use bare model family names; record full provider-qualified IDs such as `openai/gpt-5.5`
- use `--intent request` for initial assignments and handoffs that require action
- use `--intent inform` for status updates, reports, and the final convergence deliverable
- use `--intent ack` only for explicit no-reply acknowledgments
- if you could not actually run `opencode models`, label provider-qualified IDs as assumptions or likely candidates rather than confirmed local availability

Intent values are limited to `request`, `inform`, and `ack`. Do not invent new intent names.

### 7. Final response format

Return the answer in this order:

1. short summary of the chosen debate topology and why it fits the tension axis
2. short assumptions list if defaults were chosen
3. the prepared debate plan (hub prompt, role briefs, convergence prompt, round count, stopping rule, forcing-field deliverable, exact first hcom action)
4. the approval gate prompt
5. if approved: the convergence result, reported on-thread
6. short risks or follow-up notes

If approval was denied or absent, stop at step 3. The prepared plan is the deliverable.

## Common Mistakes

| Failure pattern | Counter-rule |
|---|---|
| "I'll fire the first send and confirm as we go" | No. All sends wait on the gate. Confirmation is not retroactive. |
| "The user said 'set up a debate,' that implies run it" | No. "Set up" is preparation. Running is a separate explicit approval. |
| "I got approval for debate A, so debate B is pre-approved" | No. The gate is per-debate. |
| "I'll seed the thread now and wait for approval before the first round" | No. Thread seeding is an hcom send. It waits with everything else. |
| "The prepared plan is just a draft, I don't need to show the first send" | Show the exact first send. The user is approving a concrete action, not a vibe. |
| "Clones are fine if the prompt is good" | No. Distinct models or distinct prompts. Disagreement is mandated. |
| "No explicit stop rule, we'll know when we're done" | No. State the stop rule in the prepared plan. |
| "I can put `--thread` on every `hcom` command, including agent spawn" | Use `--thread` on workflow messaging and wait commands, not on `hcom opencode` spawn lines. |
| "I can invent my own intent label like `review`" | Only use the actual hcom intent values: `request`, `inform`, or `ack`. |
| "I can write just `gpt-5.5` and the runtime will know what I mean" | Resolve and record the exact provider-qualified OpenCode model ID. |
| "I can present provider-qualified IDs as confirmed even when I did not run discovery" | If `opencode models` was not actually run, label the IDs as assumptions or likely candidates. |
| "If multiple providers expose the same family, I should silently pick one" | Surface the top exact IDs, then choose one explicit assumption in the prepared plan. |
| "The user asked for saved team configs, so I should generate them" | No. The saved-config contract is retired. Compose the debate inline. |
| "I'll emit both a prepared plan and a fresh agents/<slug>.toml just in case" | No. Do not emit `agents/<slug>` artifacts. That contract is retired. |

## When hcom is unavailable

When hcom is not installed, the skill still produces the prepared debate plan via the same interview and gate. The only difference is dispatch: compose the hub prompt, role briefs, and convergence prompt inline and dispatch via your harness's subagent mechanism (Task tool, subagent dispatch, manual handoff). The interview, gate, and mechanics are identical. No saved-config fallback exists; the saved-config contract is retired.

Output a markdown prepared plan containing:

- the goal or deliverable
- the tension axis and positions
- the role briefs (one per position, distinct, mandate disagreement)
- the hub stance
- the round count and stopping rule
- the convergence prompt
- the forcing-field deliverable spec
- scope facts
- assumptions

Present it for approval. If approved, dispatch the role briefs via your harness's subagent mechanism, collect responses, run rounds, and converge. If denied, return the prepared plan and stop.