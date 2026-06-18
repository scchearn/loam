# Agent Topologies

## Default recommendation

Start with one agent unless the task clearly benefits from separation of concerns, review gates, or sandbox isolation.

## Single agent

Use when:

- the task is small or medium
- the work does not need parallelism
- the user values simplicity over throughput

Tradeoffs:

- lowest coordination cost
- weakest review isolation

## Worker-reviewer

Use when:

- implementation quality matters
- one agent writes and one agent validates
- the user wants review on every round before merge

Tradeoffs:

- simple quality gate
- moderate coordination overhead

Recommended output shape:

- one worker
- one reviewer
- explicit `APPROVED` or `FIX` contract
- same workflow thread for each round

## Planner-executor-reviewer (`planner-executor-reviewer`)

Use when:

- the task is risky enough to justify both planning and review isolation
- execution should be sandboxed or tightly constrained
- the user asked for implementation plus a quality gate

Tradeoffs:

- stronger safety and coherence than worker-reviewer
- more coordination overhead than a two-agent loop

Recommended output shape:

- one planner/coordinator
- one executor, often sandboxed
- one reviewer or evaluator
- one shared workflow thread
- explicit handoff order from planner to executor to reviewer

## Hub-spoke

Use when:

- one coordinator should route work
- several specialist agents have distinct roles
- you need clear observability and central state

Tradeoffs:

- strong coordination clarity
- hub is a single point of failure

Recommended output shape:

- one coordinator
- 2+ workers or specialists
- tag-based group routing
- explicit bundle handoff rules

## Sequential cascade (`sequential-cascade`)

Use when:

- planning must happen before execution
- later agents need the earlier transcript
- staged refinement matters more than speed

Tradeoffs:

- high handoff clarity
- slower than parallel work

Recommended output shape:

- planner -> executor -> reviewer
- transcript or bundle transfer at each stage
- narrow responsibilities per stage

## Ensemble with judge (`ensemble-with-judge`)

Use when:

- multiple independent answers are valuable
- bias reduction matters
- a judge can synthesize the final result

Tradeoffs:

- expensive
- best for ambiguous or high-judgment tasks

Recommended output shape:

- N workers
- one judge
- common prompt and thread strategy
- evidence synthesis by the judge

## Executor-isolation exceptions

Use when:

- risky execution should be isolated
- the default headless OpenCode executor is not enough
- another agent should coordinate or review

Recommended default:

- coordinator: OpenCode
- executor: headless OpenCode by default
- reviewer: separate strong reviewer when quality gates matter

Exception:

- use Codex or another non-default runtime only when the user explicitly asks for it or a real isolation or capability requirement justifies it

## Quick chooser

| Situation | Default topology |
|---|---|
| quick low-risk work | single agent |
| code changes with review on every round | worker-reviewer |
| risky implementation with planning and review | planner-executor-reviewer |
| many specialist roles | hub-spoke |
| competing independent answers | ensemble with judge |
