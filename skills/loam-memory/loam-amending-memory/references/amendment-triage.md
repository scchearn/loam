# Amendment Triage Guide

Use this guide to classify what kind of amendment you are making and how aggressively to apply it.

## Amendment types

### Correction

The wiki contains a factually wrong claim that is now known to be wrong.

Examples:

- the wiki says the API uses Basic auth but it now uses Bearer tokens
- the wiki says the cache store is Redis but it was changed to SQLite
- the wiki lists a deprecated endpoint as active

### Supersession

The wiki captures an older state that has been overtaken by newer events or decisions.

Examples:

- the wiki describes a previous architecture that was replaced by a new one
- the wiki records a decision that was later reversed
- the wiki lists a milestone that has been rescheduled or redefined

### Completion

The wiki is not wrong but is materially incomplete.

Examples:

- the wiki describes the read path but omits the write path
- the wiki lists three deployment environments but a fourth was recently added
- the wiki mentions a concept but never defines it or links to a dedicated page

### Contradiction surfacing

The wiki contains one view but newer evidence introduces a conflicting view that should coexist, not replace.

Examples:

- the wiki says approach A is preferred but a recent benchmark suggests approach B
- two pages in the wiki disagree and the contradiction was never made explicit
- the wiki states a constraint that a newer source claims no longer applies

## Severity levels

### High

The inaccuracy could mislead future sessions into wrong decisions or broken code.

Apply aggressively:
- use strikethrough preservation for the old claim
- add a correction note with date and reason
- propagate the fix to all materially affected pages

### Medium

The inaccuracy is misleading but unlikely to cause direct harm.

Apply moderately:
- a clear correction is still warranted
- strikethrough preservation is optional — use judgment
- propagate only to clearly dependent pages

### Low

Minor imprecision or missing nuance that rarely matters.

Apply conservatively:
- a simple replacement or addition is fine
- no need for strikethrough or correction notes
- propagation is optional

## When not to amend

Do not use `/loam::amending-memory` for:

- **New content** that has never been in the wiki → use `/loam::adding-to-memory`
- **Structural or naming issues** → use `/loam::normalizing-memory`
- **Link health or convention drift** → use `/loam::linting-memory`
- **Answering a question** → use `/loam::querying-memory`
- **Speculative updates** where you are not confident the wiki is actually wrong → flag it in the page or log as an open question instead