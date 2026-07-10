# Goal Artifact Template

The goal file is the single authoritative artifact for intent, validation,
lifecycle, and review history. It lives at `goals/<slug>.md`. The index at
`goals/INDEX.md` is a discovery table; the goal file is authoritative when
they drift.

## Front matter

```yaml
---
title: <Goal title>
slug: <goal-slug>
status: draft | active | paused | achieved | abandoned
created_at: YYYY-MM-DD HH:MM ±HH:MM
updated_at: YYYY-MM-DD HH:MM ±HH:MM
reviewed_at: null | YYYY-MM-DD HH:MM ±HH:MM
next_review_at: null | YYYY-MM-DD HH:MM ±HH:MM
---
```

- `status` transitions: `draft` → `active` → `paused`/`achieved`/`abandoned`.
  `paused` may return to `active`. `achieved` may return to `active` when
  meaning changes invalidate prior evidence. `abandoned` is terminal.
- `reviewed_at` tracks the most recent explicit review. `null` before the
  first review.
- `next_review_at` is an optional explicit review deadline for active goals.
  When set, it overrides the default 90-day cadence for lint staleness.

## Body

```md
# <Goal title>

## Intent

<One concise statement of the desired outcome and why it matters.>

## Validation contract

- Procedure: <observable check or independent-review procedure>
- Expected result: <passing state>
- Evidence required: <minimal proof to record>

## Boundaries

<Optional constraints and non-goals, or none.>

## Horizon and cadence

<Optional horizon and review cadence, or none.>

## Linked work

### Specs

- <spec path, or none>

### Plans

- <plan path, or none>

## Current state

- Next action: <one action or none>
- Blockers: <blockers or none>

## Reviews

### YYYY-MM-DD

- Result: pass | fail | blocked | changed
- Checked: <commit, environment, artifact, or other concrete state>
- Procedure: <what ran or who reviewed>
- Evidence: <concise proof or useful existing reference>
- Decision: <status/next-action decision>
```

## Lifecycle transitions

Allowed source states are listed explicitly. A transition not listed here is invalid.

| From | To | Trigger | Who |
|------|----|---------|-----|
| (none) | draft | Save incomplete goal | user confirms |
| draft | active | Intent + validation confirmed | user confirms |
| active | paused | User decision | user |
| active | achieved | Explicit review passes | skill applies after review |
| active | abandoned | User decision | user |
| paused | active | Reactivate | user confirms |
| paused | abandoned | User decision | user |
| achieved | active | Meaning change invalidates evidence | user confirms + skill |
| achieved | abandoned | User decision | user |

Invalid transitions (not exhaustive): `draft` → `achieved` (no review without activation); `abandoned` → any (terminal); `achieved` → `paused` (return to `active` via meaning change only).

## Review results

- **pass** — procedure ran and the observed result meets the expected
  result. The skill may mark the goal `achieved`.
- **fail** — procedure ran but the observed result does not meet the
  expected result. The goal stays `active`; record one next action.
- **blocked** — procedure could not run (tool unavailable, reviewer
  absent). Lifecycle status is unchanged; do not report as fail or
  achieved.
- **changed** — a meaning change was applied; the review records the old
  and new contract summary. An achieved goal returns to `active` when
  prior evidence no longer proves the revised contract.

## INDEX.md format

```md
# Goals Index

| Status | Goal | Path | Updated | Next review |
|--------|------|------|---------|-------------|
| active | <title> | goals/<slug>.md | YYYY-MM-DD | YYYY-MM-DD or — |
```

The goal file is authoritative when the index drifts.
