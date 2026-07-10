---
title: <Spec Title>
slug: <spec-slug>
status: draft # draft | approved | superseded
created_at: YYYY-MM-DD HH:MM ±HH:MM
updated_at: YYYY-MM-DD HH:MM ±HH:MM
approved_at: null
research: [] # optional: plans/research/<slug>.md
goal: # optional: goals/<slug>.md provenance; omit when not goal-backed
---

# <Spec Title>

## Problem

<!-- One sentence describing the problem this spec resolves. -->

<Problem statement>

## Clarifications

<!-- Q/A/Status triples from elicitation. Use "none" when Step 2 was skipped. -->

- Q: <question asked or ambiguity identified>
  A: <user answer, repo-derived answer, or assumption>
  Status: answered | assumed | unresolved-blocking | unresolved-non-blocking

<!-- Or: -->

- none

## Scenarios

<!-- Required for behavior-changing specs. Use "none" with rationale for trivial/non-behavioral specs. -->

### Scenario: <name>

- Given <initial state/context>
- When <action/event>
- Then <observable outcome>
- Verify with: <test/command/manual check, or TBD>

### Scenario: <name> — failure/edge path

- Given <initial state>
- When <action/event — invalid, missing, or boundary input>
- Then <expected failure behavior>
- Verify with: <test/command/manual check, or TBD>

<!-- For trivial/non-behavioral specs: -->

- none — trivial docs/config-only change; acceptance criteria are sufficient.

## Scope

### In

<!-- What this spec includes. -->

- <Included behavior or area>

### Out

<!-- What is explicitly excluded. -->

- <Excluded behavior or area, or none>

## Constraints

<!-- Non-negotiable rules that flow into plan Constraints. Use none when empty. -->

- <Constraint or none>

## Acceptance criteria

<!-- Summary checkboxes derived from Scenarios. For behavior-changing specs, each criterion MUST trace to one or more scenarios. Do not introduce behavior absent from Scenarios. EARS-style phrasing (WHEN...THE SYSTEM SHALL...) is acceptable for conditional behavior. -->

- [ ] <Acceptance criterion derived from scenarios>

## Decision

<!-- Chosen approach, not a list of options. -->

<Decision>

## Rejected alternatives

<!-- Why not X. loam::planning reads this to avoid re-evaluating closed decisions. Use none when empty. -->

- <Alternative and rationale, or none>

## Key files / modules

<!-- Paths/modules that seed task Files. loam::planning verifies and extends these. Use none when unknown. -->

- <path-or-module, or none>

## Completeness checklist

<!-- Result of the internal completeness gate. Fill after analysis and before finalizing the spec. -->

| Area | Status | Notes |
| ---- | ------ | ----- |
| Behavior | pass / n/a / gap-non-blocking / gap-blocking | |
| Scenarios | pass / n/a / gap-non-blocking / gap-blocking | |
| Scope | pass / n/a / gap-non-blocking / gap-blocking | |
| Constraints | pass / n/a / gap-non-blocking / gap-blocking | |
| Interfaces / contracts | pass / n/a / gap-non-blocking / gap-blocking | |
| Data / migration | pass / n/a / gap-non-blocking / gap-blocking | |
| Errors / edge cases | pass / n/a / gap-non-blocking / gap-blocking | |
| Security / privacy | pass / n/a / gap-non-blocking / gap-blocking | |
| Integrations | pass / n/a / gap-non-blocking / gap-blocking | |
| Operations / rollout | pass / n/a / gap-non-blocking / gap-blocking | |
| Verification | pass / n/a / gap-non-blocking / gap-blocking | |
| Planning inputs | pass / n/a / gap-non-blocking / gap-blocking | |
| Open questions | pass / gap-non-blocking / gap-blocking | |

<!-- If any row is gap-blocking, the spec is not ready for /loam::planning. -->

## Open questions

<!-- Blocking unless marked non-blocking. Approved specs should use none or non-blocking items. -->

- none

## Minimal spec guidance

For trivial changes, `## Problem`, `## Acceptance criteria`, and `## Decision` are the only mandatory body sections with substantive content. For behavior-changing changes, `## Clarifications` (or `none`), `## Scenarios`, and `## Completeness checklist` are also required. Other sections may contain `none`, but keeping the headings makes loam::planning parsing reliable.