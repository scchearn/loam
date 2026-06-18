# <Team Name>

## Goal

<One-paragraph objective>

## Assumptions

- <Only include this section when defaults were chosen>
- <If multiple providers exposed the same family, note the shortlisted exact IDs and the chosen assumption>
- <If provider narrowing would help, note the next check as `opencode models <provider>`>
- <If `opencode models` was not actually run, say the exact IDs are assumptions or likely candidates>

## Topology

- Pattern: <single-agent | worker-reviewer | planner-executor-reviewer | hub-spoke | sequential-cascade | ensemble-with-judge>
- Why this pattern fits:
- Why simpler options were rejected:

## Agent Roster

Repeat this block once per agent in the chosen topology.

### <Agent Name>
- Tool:
- Model:
- Role:
- Responsibilities:

## Model Rationale

- Why each model/tool pairing fits its role:
- Any provider duplicates considered:

## Reviewer Or Evaluator Design

- Is a reviewer/evaluator required:
- Approval or rejection contract:
- What evidence must be checked:

## Communication Strategy

- Thread strategy:
- Default intent:
- Direct routing:
- Group routing:

Concrete examples:

- Direct send example:
- Group send example using `@<tag>-`:
- Reviewer outcome example using `request`, `inform`, or `ack` only:
- Explicit intent examples for all three values:

## Bundle Strategy

- Handoff 1:
- Handoff 2:
- Handoff 3:

## Tool And Permission Notes

- Launch mode:
- Sandbox usage:
- Approval expectations:
- Any risky operations:

## Launch Sequence

```bash
export WF_THREAD="<slug>-$(date +%Y%m%d%H%M%S)"
AGENT1_OUT=$(HCOM_OPENCODE_ARGS="--model <provider/model>" hcom opencode --tag <tag> --headless --go 2>&1)
AGENT1_NAME=$(printf '%s\n' "$AGENT1_OUT" | grep '^Names: ' | sed 's/^Names: //' | tr -d ' ')
hcom send @<tag>- --thread "$WF_THREAD" --intent request -- "<kickoff or handoff message>"
hcom send "$AGENT1_NAME" --thread "$WF_THREAD" --intent inform -- "<status update>"
hcom send "$AGENT1_NAME" --thread "$WF_THREAD" --intent ack -- "<no reply needed>"
hcom events --thread "$WF_THREAD" --wait
hcom kill "$AGENT1_NAME" --go
```

## Risks And Tradeoffs

- Risk:
- Mitigation:
