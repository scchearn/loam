# Roles And Reviewers

## Common roles

### Planner / coordinator

- owns topology choice
- assigns work
- consolidates outcomes
- keeps scope and stop conditions stable

### Coder / executor

- performs implementation work
- reports changed files, validation steps, and unresolved risks
- should not own final review for high-risk changes

### Researcher

- gathers outside information
- useful only when external knowledge is required
- should not silently expand project scope

### Reviewer

- checks correctness, regressions, and quality
- should be separate from the main implementer when quality matters
- should respond with a clear approval or fix contract

### Evaluator

- validates outputs against explicit criteria
- useful for plans, migrations, generated artifacts, and quality gates

## When to require a reviewer

Require a reviewer when:

- code or configuration changes are non-trivial
- there is a safety or correctness risk
- the user asked for a quality gate
- the executor is sandboxed and someone else must inspect the result

## When a single agent is enough

A single agent is enough when:

- the task is simple
- review isolation adds more overhead than value
- the user wants a quick answer or low-cost run

## Reviewer contracts

Preferred review responses:

- `APPROVED:` when the work passes review
- `FIX:` followed by concrete defects or missing evidence

Avoid vague review outcomes such as:

- "looks good"
- "maybe tighten this"
- silent acceptance without evidence

## Role split defaults

| Situation | Default role split |
|---|---|
| review loop | worker + reviewer |
| risky implementation | planner + executor + reviewer |
| broad multi-step work | coordinator + specialist workers + reviewer |
