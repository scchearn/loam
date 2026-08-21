---
title: Greeting
slug: greeting-spec
status: approved
created_at: 2026-07-02 09:00 +02:00
updated_at: 2026-08-05 09:00 +02:00
approved_at: 2026-08-05 09:00 +02:00
research: []
goal: goals/improve-greeting.md
---

# Greeting

## Problem

The fixture needs one small, real spec so goal -> spec -> plan ->
checkpoint traceability tests have something concrete to walk.

## Clarifications

none

## Scenarios

none

## Scope

### In

- `greet(name)` returning a friendly greeting string.

### Out

- Localization, pluralization, or any formatting beyond the fixed template.

## Constraints

none

## Acceptance criteria

- [ ] `greet('Ada')` returns `"Hello, Ada!"`.

## Decision

Keep `greet` a pure, dependency-free function, as implemented in
`src/greeter.js` and documented at `wiki/code/greeter.md`.

## Rejected alternatives

none

## Key files / modules

- `src/greeter.js`
- `wiki/code/greeter.md`

## Completeness checklist

none

## Open questions

none
