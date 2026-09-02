# Oracle questions for judged work

Read this when a judged-oracle trigger from Step 1 fires. Ask these as Step 2 clarification questions, record answers as Q/A/Status triples, and use them to fill `## Quality anchor` and `## Verification oracle`. These questions do not count against the Step 2 question budget.

These questions exist because judged work fails silently without them: without an anchor there is no definition of "good", without an evidence contract agents claim success without proof, without convergence the loop never stops, and without a retry budget weak work eats the whole effort.

## The four questions

1. **Quality anchor** — what external reference defines "good"? A reference implementation, prior accepted output, a screenshot set, a style guide, or a named gold standard the judge can compare against. If the user names none, ask them to pick one or mark the gap `unresolved-blocking` — judged work without an anchor cannot be scored honestly.

2. **Oracle type and evidence contract** — how will a judge evaluate output, and what artifact must an agent produce before claiming done? Examples: harness screenshots at named camera/view presets plus a console/perf log; rendered document previews; generated samples written to a fixed path. The contract must be concrete enough that two different judges would request the same artifact.

3. **Convergence** — what is the stop condition? Typically: every module passes its judge at the threshold, then a whole-system gate. Ask whether a blind comparison is available (ours-vs-anchor, order shuffled); if yes, put it in the stop condition because it is the least gameable gate.

4. **Retry budget** — how many judge-and-fix rounds per unit before escalating to the user? State a number (the style this pattern comes from uses 4). Also state what happens on exhaustion: user review, scope cut, or accepted-as-is.

## Defaults and assumptions

- **Harness-first** is the default for judged work: assume the evidence contract / verification harness is itself in scope and must be built before the judged work, and record this as an `assumed` clarification. Do not ask separately unless the user volunteers a constraint.
- **Judge role**: default is a separate evaluator (agent, tool, or person) that did not produce the work, writing no code — either stated in the spec or marked as an explicit open question.
- **Hard oracles skip this bank.** If done is provable by tests, commands, type checks, or mechanical diffing, the existing Verification handling is enough; set `## Quality anchor` and `## Verification oracle` to `none`.

## Mapping to spec sections

| Answer | Where it goes |
| ------ | ------------- |
| Reference defining "good" + how comparison works | `## Quality anchor` |
| Oracle type, evidence contract, judge role, pass threshold | `## Verification oracle` |
| Stop condition and blind-gate availability | `## Verification oracle` (convergence) |
| Round budget and exhaustion behavior | `## Verification oracle` (retry budget) |
| Harness-first assumption | `## Clarifications` (assumed) plus Scope, so planning schedules the harness as early work |
