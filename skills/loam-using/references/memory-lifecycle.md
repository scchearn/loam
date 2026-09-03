# Memory lifecycle

Read before creating, correcting, or demoting a wiki page. Referenced from `loam::using`.

## Durable-memory admission rubric

Use before creating a wiki page; page-creating skills reference this instead of copying it. A claim earns a page only if it passes all three:

- **R1 Reusable** — a future session on a different task would plausibly need it.
- **R2 About the project/domain** — codebase, architecture, decisions, conventions, dependencies, or durable external facts; not the conversation, the agent, or transient user state.
- **R3 Costly to reconstruct & re-checkable** — re-deriving from a live source (code, config, task list) costs more than the page costs to maintain. If one command or file read gets it back, no page. Where an external source exists, name it so freshness can re-validate. Decisions and rationale are self-sourcing.

Disqualifiers override: **D1 ephemeral** (build state, current branch, "today I ran X" → operational report); **D2 duplicate** (an existing page covers it → amend it). Tiebreaker: admit if not reconstructable from a live source, else discard. `wiki/.archive/` holds only was-durable-now-superseded content; never-durable material is routed elsewhere, never archived.

## Correction and freshness

- **Self-correction:** on reading memory that contradicts current evidence, move the superseded durable page to `wiki/.archive/` with an archival header, write the correction in place, append a `self-correct` line to `log.md`, and continue the task.
- **Human-flag:** treat "this page is wrong/stale" as a correction trigger, not an approval request — same archive + rewrite + log flow.
- **Freshness:** validation uses existing `updated_at` plus lint-time evidence (no freshness frontmatter). A page is **confirmed** (bump `updated_at`), **corrected** (archive + rewrite + log), or **demoted** (archive, "no longer applies"). `loam::linting-memory` owns the triggers and flags 90-day-old pages citing volatile surfaces.
