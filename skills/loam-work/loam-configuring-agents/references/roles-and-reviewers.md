# Role Briefs And Convergence

## Core principle

Each role brief must mandate disagreement. An agent that agrees with the other position by default is not a debate participant — it is a clone. Clones produce no tension and the debate is theatre.

## Role brief template

Each role brief contains:

- **Position** — the one position this agent argues. Distinct from every other position in the debate.
- **Mandate** — "Argue this position. Do not concede unless the evidence forces it. Do not search for middle ground until the convergence prompt is issued."
- **Scope facts** — the evidence, constraints, or context this agent must respect.
- **Constraints** — what the agent must NOT touch (other positions, project implementation, unrelated scope).
- **Expected output** — the opening argument, round responses, and the final position statement.

## Distinct positions

Positions are distinct when they would produce different outputs given the same scope facts. If two agents handed the same scope facts would reach the same conclusion, they are clones — change the model, change the prompt, or change the position.

Anti-patterns:

- "Agent A argues for X, Agent B argues for X but with nuance" — not a tension. Give B a real opposing position.
- "Both agents evaluate the same option" — not a debate. Use `ensemble-with-judge` if you want independent answers, not adversarial positions.
- "The agents will find the best answer together" — not a debate. That is collaboration. This skill is for when collaboration would rubber-stamp.

## Hub stance

- **Neutral** — the hub orchestrates only. It does not argue a position. It advances rounds and issues the convergence prompt. Default when the user does not specify.
- **Partisan-for-synthesis** — the hub argues a position AND produces the convergence deliverable. Use when the hub has a strong view but must still hear the opposition. State the bias explicitly in the prepared plan.
- **Participant** — the hub argues a position and a separate synthesizer role converges. Use when the hub cannot be neutral and should not synthesize.

## Synthesizer role

When the topology includes a synthesizer (`position-vs-position-with-synthesizer`, `multi-position-roundtable` with synthesis, `ensemble-with-judge`):

- the synthesizer is neutral — it has no position in the debate
- it receives all round outputs
- it issues the forcing-field deliverable after the rounds end
- it must not introduce new evidence; it synthesizes what the positions produced

## Convergence prompt

The convergence prompt is issued by the hub (or the synthesizer) after the rounds end. It must:

- require the forcing-field deliverable as the output
- not invite open-ended discussion — convergence is forced, not negotiated
- name the stop rule that fired
- name the positions that argued

## Forcing-field deliverable

The forcing field is the structural constraint in the deliverable that makes faking agreement harder than agreeing. Every debate has one. If none is visible, design one before composing the prepared plan.

Examples:

- "The deliverable must pick one position and state which evidence changed its mind."
- "The deliverable must list the two strongest objections from the losing position and why they were overruled."
- "The deliverable must be a single decision the hub can act on; a split decision is not a valid output."

A deliverable that allows both positions to claim victory has no forcing field. Redesign it.

## Reviewer / evaluator

This skill does not use a separate reviewer role by default. The forcing-field deliverable IS the quality gate. If the user asks for an external evaluator of the convergence result, route them to `loam::planning` or `loam::starting` — this skill's scope is the debate, not post-debate review.