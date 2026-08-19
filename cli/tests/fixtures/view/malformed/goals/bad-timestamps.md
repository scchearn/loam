---
title: Bad Timestamps
slug: bad-timestamps
status: active
created_at: not-a-real-date
updated_at: 2026-13-45 99:99 +99:99
reviewed_at: null
next_review_at: null
---

# Bad Timestamps

## Intent

Exercise the `artifact-parse` signal: this goal's `created_at` is not a
date at all, and `updated_at` is a syntactically date-shaped but
semantically invalid timestamp (month 13, day 45, hour 99). Both should be
recorded as a `parse_errors` diagnostic rather than silently accepted or
dropping the artifact.

## Validation contract

- Procedure: none
- Expected result: none
- Evidence required: none

## Boundaries

none

## Horizon and cadence

none

## Linked work

none

## Current state

- Next action: none
- Blockers: none

## Reviews

none
