# Debate Topologies

## Default recommendation

Start with `position-vs-position-with-synthesizer` when bias reduction matters. Use `position-vs-position` when the tension is strictly binary and no synthesis is needed. The topology must match the tension axis — if there is no real tension, there is no debate.

## Position-vs-position

Use when:

- the tension is strictly binary (two positions, no middle ground needed)
- the deliverable is one of the two positions, refined
- no third-party synthesis is required

Tradeoffs:

- lowest coordination cost among multi-agent shapes
- no bias reduction beyond the two positions arguing
- convergence must come from one position winning or both moving

Recommended shape:

- 2 agents, distinct positions
- one shared workflow thread
- 2 rounds default
- stop rule: positions converge or stalemate after round 2

## Position-vs-position-with-synthesizer

Use when:

- 2 distinct positions plus bias reduction matter
- a neutral third agent can synthesize the final deliverable
- the user wants a convergence artifact neither position would produce alone

Tradeoffs:

- simple quality gate for bias
- moderate coordination overhead
- the synthesizer must be neutral — if it has a position, use `position-vs-position` instead

Recommended shape:

- 2 agents arguing distinct positions
- 1 synthesizer (neutral, no position)
- one shared workflow thread
- 2 rounds default, synthesizer converges after rounds end
- stop rule: rounds end, synthesizer issues the forcing-field deliverable

## Multi-position-roundtable

Use when:

- 3 or more distinct positions exist
- multi-stakeholder consensus is the goal
- no single synthesizer can represent all positions

Tradeoffs:

- strongest coverage of divergent views
- highest coordination overhead
- convergence is harder — the forcing-field deliverable must do more work

Recommended shape:

- N agents (N >= 3), each a distinct position
- hub orchestrates and converges (or a designated synthesizer role)
- one shared workflow thread
- 2 rounds default, optional 3rd when the hub states a reason
- stop rule: positions converge, or the hub forces convergence via the forcing-field deliverable

## Ensemble-with-judge (as debate topology)

Use when:

- independent answers are valuable before any cross-examination
- bias reduction matters and a judge can synthesize
- the tension is about which answer is best, not which position wins

Tradeoffs:

- expensive (N workers + 1 judge)
- best for ambiguous or high-judgment goals
- rounds are optional — the judge may converge after openings

Recommended shape:

- N agents produce independent openings (no cross-view until judge)
- 1 judge synthesizes the convergence deliverable
- common prompt and thread strategy
- stop rule: judge issues the forcing-field deliverable after reviewing openings

## Choosing agents for distinct positions

- Distinct models or distinct prompts. Never clones.
- Disagreement is mandated: each role brief must require the agent to argue its assigned position, not to find consensus prematurely.
- The hub stance (neutral / partisan-for-synthesis / participant) changes which agents argue and which synthesize. State it in the prepared plan.

## Quick chooser

| Situation | Default topology |
|---|---|
| binary tension, no synthesis needed | position-vs-position |
| binary tension + bias reduction | position-vs-position-with-synthesizer |
| 3+ positions or multi-stakeholder | multi-position-roundtable |
| independent-answer aggregation | ensemble-with-judge |