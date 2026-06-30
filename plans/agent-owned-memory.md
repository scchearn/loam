---
title: Agent-Owned Memory Lifecycle
slug: agent-owned-memory
spec: agent-owned-memory
description: Remove human gates from memory writes while adding agent-owned admission, routing, archive, and freshness rules.
status: done
task_count: 6
created_at: 2026-06-30 14:17 +0200
started_at: 2026-06-30 14:57 +0200
completed_at: 2026-06-30 15:10 +0200
---

# Agent-Owned Memory Lifecycle

## Spec

[Agent-Owned Memory Lifecycle](../specs/agent-owned-memory.md) — draft, updated 2026-06-30 13:22 +02:00. User explicitly requested planning from this draft after duno/libo review passed with the count nit reconciled.

## Goal

loam memory-writing skills write without human pre-approval, apply a canonical agent-owned admission filter, route non-durable material away from wiki pages, soft-delete superseded durable pages to `.archive/`, and lint stale volatile claims without adding freshness frontmatter.

## Acceptance criteria

- [ ] `loam-using` SKILL.md contains no instance of "proposal-first" or "no exceptions" as a memory-write rule. The agent-ownership model (writes freely, self-corrects, soft-deletes, human flags) is stated in its place.
- [ ] `loam-using` SKILL.md contains the canonical durable-memory admission rubric with R1/R2/R3, D1/D2, and the reconstructable/discard tiebreaker.
- [ ] Page-creating skills reference the `loam-using` admission rubric instead of restating it.
- [ ] `loam-learning-from-session` routes learnings through the five-way matrix: wiki, guidance, checkpoint, task annotation/plan, discard.
- [ ] `loam-linting-memory` flags pages older than 90 days that cite volatile surfaces for re-validation, without auto-archiving them.
- [ ] No new freshness frontmatter fields (`last_validated`, `validated_against`, `volatility`) are added.
- [ ] `loam-adding-to-memory` SKILL.md has no proposal/confirmation step and includes self-correction + archive behavior in Step 2.
- [ ] `loam-amending-memory` SKILL.md has no "wait for explicit confirmation" step. The flow is "read evidence → archive old → write correction → log → report."
- [ ] `loam-learning-from-session` SKILL.md has no "show proposal / wait for approval" step. The agent routes and writes.
- [ ] `loam-normalizing-memory` SKILL.md has no "wait for explicit confirmation" gate.
- [ ] `loam-linting-memory` SKILL.md has no "explicit approval" requirement for fix mode. The safe-fix list boundary remains.
- [ ] `loam-auditing-guidance` SKILL.md has no "user approval" requirement for pruning.
- [ ] `schema-template.md` includes `wiki/.archive/` in the directory layout with the note "excluded from qmd, never hard-deleted."
- [ ] `schema-template.md` ownership language says the agent owns and maintains the files, writes without pre-approval, and soft-deletes to `.archive/`.
- [ ] qmd-usage references instruct adding `ignore: [".archive/**"]` to collection config during setup.
- [ ] `loam-linting-memory` checks that qmd `.archive/**` exclusion exists and flags it as a health issue if missing.
- [ ] All 8 touched SKILL.md files have bumped `metadata.version`.
- [ ] `loam-using` router describes the self-correction trigger and the human-flag trigger.
- [ ] `bash bin/check-curation.sh` passes.

## Tasks

### T1 — Rewrite the loam router ownership contract

- **Status:** [x]
- **Depends on:** none
- **Outcome:** `loam-using` becomes the canonical source for agent-owned memory writes, admission, routing, archive semantics, freshness triggers, self-correction, and human-flag behavior.
- **Steps:**
  - [ ] In `skills/loam-using/SKILL.md`, replace proposal-first memory-write rules and red-flag wording with the agent-owned model from the spec.
  - [ ] Add a canonical `Durable-memory admission rubric` section with R1, R2, R3 "Costly to reconstruct & re-checkable", D1/D2, and the reconstructable/discard tiebreaker.
  - [ ] Add the five-way routing matrix and state that page-creating skills reference this rubric instead of copying it.
  - [ ] Add self-correction and human-flag triggers: archive durable stale/wrong pages, write corrections, log, and continue without approval.
  - [ ] Add metadata-free freshness triggers F1-F5 using existing `updated_at`; do not introduce `last_validated`, `validated_against`, or `volatility`.
  - [ ] Bump `metadata.version` for `loam-using`.
- **Constraints:** No human gate, no copied rubric in downstream skills, no new freshness frontmatter.
- **Watch for:** Existing wording says "proposal-first" in several sections; remove it as a memory-write rule, not necessarily from unrelated historical examples if any remain.
- **Files:** `skills/loam-using/SKILL.md` (read+edit)
- **Verify:** `grep -n "Durable-memory admission rubric\|R3 Costly to reconstruct\|Routing matrix\|Freshness-validation triggers\|Human-flag" skills/loam-using/SKILL.md && ! grep -ni "proposal-first\|no exceptions" skills/loam-using/SKILL.md`
- **Passes when:** `loam-using` clearly states agent-owned writes and all canonical rubric/routing/freshness rules, and no old proposal-first memory-write rule remains.

### T2 — Update core wiki write skills to use admission and archive flows

- **Status:** [x]
- **Depends on:** T1
- **Outcome:** Add, amend, and learning flows no longer wait for user approval; they either write durable memory directly or route material through the matrix.
- **Steps:**
  - [ ] Update `loam-adding-to-memory` Step 2 so new-page creation applies the `loam-using` admission rubric, routes non-durable material away from wiki pages, and handles contradictions/supersessions through archive + correction.
  - [ ] Update `loam-amending-memory` to remove proposal/confirmation phases and make the flow read evidence → archive old durable content → write correction → update related pages/index/log → refresh qmd → report.
  - [ ] Update `amend-checklist.md` to remove the "wait for user confirmation" checklist item and align completion checks with archive + correct + log.
  - [ ] Update `loam-learning-from-session` so it routes each learning through the five-way matrix and writes routed outputs directly without proposal/approval.
  - [ ] Bump `metadata.version` for the three touched SKILL.md files.
- **Constraints:** Do not restate the full admission rubric; reference the canonical `loam-using` section. `raw/` stays immutable. Never-durable material is routed or discarded, not archived.
- **Watch for:** `loam-learning-from-session` currently describes itself as proposal-first in front matter and body; update both.
- **Files:**
  - `skills/loam-memory/loam-adding-to-memory/SKILL.md` (read+edit)
  - `skills/loam-memory/loam-amending-memory/SKILL.md` (read+edit)
  - `skills/loam-memory/loam-amending-memory/references/amend-checklist.md` (read+edit)
  - `skills/loam-memory/loam-learning-from-session/SKILL.md` (read+edit)
- **Verify:** `grep -n "admission rubric\|archive\|five-way\|discard" skills/loam-memory/loam-adding-to-memory/SKILL.md skills/loam-memory/loam-amending-memory/SKILL.md skills/loam-memory/loam-learning-from-session/SKILL.md && ! grep -n "Wait for explicit confirmation\|Wait for user confirmation\|wait for approval\|proposal-first router" skills/loam-memory/loam-amending-memory/SKILL.md skills/loam-memory/loam-amending-memory/references/amend-checklist.md skills/loam-memory/loam-learning-from-session/SKILL.md`
- **Passes when:** Core write skills reference admission, route or discard non-durable material, archive superseded durable content, log changes, and do not pause for approval.

### T3 — Update normalization, linting, and guidance pruning behavior

- **Status:** [x]
- **Depends on:** T1
- **Outcome:** Remaining memory/guidance maintenance skills match no-gate ownership while preserving their safe-fix boundaries.
- **Steps:**
  - [ ] Update `loam-normalizing-memory` so structural fixes proceed without confirmation and any new wiki page creation applies the canonical admission rubric by reference.
  - [ ] Update `loam-linting-memory` so safe fixes apply directly, `.archive/**` qmd exclusion is checked as a health issue, and volatile stale pages (`updated_at` older than 90 days plus cited API/config/version/code surface) are flagged for re-validation without auto-archive.
  - [ ] Keep lint's existing safe-fix boundary: do not expand into speculative ingestion, broad rewrites, or destructive changes.
  - [ ] Update `loam-auditing-guidance` so stale/duplicate pruning no longer requires user approval, while additions or ambiguous changes still stay bounded by the skill's own quality-report flow if retained.
  - [ ] Bump `metadata.version` for the three touched SKILL.md files.
- **Constraints:** Safe-fix boundary remains; lint flags freshness but does not auto-archive; no new freshness frontmatter.
- **Watch for:** Some existing lint operations still require approval for risky renames/date fixes; only remove the memory-write gate where the spec requires it, not safety boundaries that are outside the spec.
- **Files:**
  - `skills/loam-memory/loam-normalizing-memory/SKILL.md` (read+edit)
  - `skills/loam-memory/loam-linting-memory/SKILL.md` (read+edit)
  - `skills/loam-memory/loam-auditing-guidance/SKILL.md` (read+edit)
- **Verify:** `grep -n "admission rubric\|\.archive/\*\*\|90 days\|volatile\|safe fixes" skills/loam-memory/loam-normalizing-memory/SKILL.md skills/loam-memory/loam-linting-memory/SKILL.md skills/loam-memory/loam-auditing-guidance/SKILL.md && ! grep -n "explicit approval.*fix mode\|Do not remove without showing the prune diff\|wait for confirmation" skills/loam-memory/loam-normalizing-memory/SKILL.md skills/loam-memory/loam-linting-memory/SKILL.md skills/loam-memory/loam-auditing-guidance/SKILL.md`
- **Passes when:** Normalization, linting, and guidance pruning reflect no-gate behavior, lint detects archive exclusion and volatile stale pages, and risky/non-spec safety boundaries remain intact.

### T4 — Update scaffold schema and qmd reference contracts

- **Status:** [x]
- **Depends on:** T1
- **Outcome:** New wiki scaffolds and qmd setup references include `.archive/` ownership, exclusion, and no-hard-delete semantics from day one.
- **Steps:**
  - [ ] Update `schema-template.md` directory layout to include `<wiki path>/.archive/` as qmd-excluded and never hard-deleted.
  - [ ] Update schema ownership language so the wiki layer is agent-maintained, written without pre-approval, and corrected by soft-deleting superseded durable pages to `.archive/`.
  - [ ] Update scaffolding qmd setup instructions to include per-collection `ignore: [".archive/**"]` where qmd collection configuration is described.
  - [ ] Update the consumer `qmd-usage.md` copies listed in the spec to instruct adding `ignore: [".archive/**"]` to per-collection qmd config during setup, matching the scaffolding-wiki copy.
  - [ ] Bump `metadata.version` for `loam-scaffolding-wiki` because its references change.
- **Constraints:** Do not consolidate same-named qmd reference files; AGENTS.md explicitly says not to byte-sync references unless marked shared.
- **Watch for:** qmd usage files are intentionally per-skill; update only the minimum relevant wording in each copy.
- **Files:**
  - `skills/loam-ground/loam-scaffolding-wiki/SKILL.md` (read+edit)
  - `skills/loam-ground/loam-scaffolding-wiki/references/schema-template.md` (read+edit)
  - `skills/loam-ground/loam-scaffolding-wiki/references/qmd-usage.md` (read+edit)
  - `skills/loam-memory/loam-adding-to-memory/references/qmd-usage.md` (read+edit)
  - `skills/loam-memory/loam-amending-memory/references/qmd-usage.md` (read+edit)
  - `skills/loam-memory/loam-learning-from-session/references/qmd-usage.md` (read+edit)
  - `skills/loam-memory/loam-linting-memory/references/qmd-usage.md` (read+edit)
  - `skills/loam-memory/loam-reviewing-memory/references/qmd-usage.md` (read+edit)
- **Verify:** `grep -R -n "\.archive/\|\.archive/\*\*\|ignore:" skills/loam-ground/loam-scaffolding-wiki/references/schema-template.md skills/loam-ground/loam-scaffolding-wiki/references/qmd-usage.md skills/loam-memory/*/references/qmd-usage.md`
- **Passes when:** Schema and qmd references document `.archive/`, qmd exclusion, and active retrieval behavior without introducing reference-file consolidation.

### T5 — Validate metadata, forbidden fields, and spec scenarios

- **Status:** [x]
- **Depends on:** T2, T3, T4
- **Outcome:** Cross-file curation and spec-level invariants are verified before final review.
- **Steps:**
  - [ ] Run the repo-native curation check.
  - [ ] Confirm all eight touched SKILL.md files have bumped `metadata.version` and no touched SKILL.md is missing a version.
  - [ ] Confirm no new freshness frontmatter fields (`last_validated`, `validated_against`, `volatility`) appear under `skills/`.
  - [ ] Confirm the 13 scenarios from the spec are represented in skill behavior, especially admission-on-page-creation and volatile-stale lint flags.
  - [ ] Record any manual verification gaps in the task notes for `/loam::starting` to carry forward.
- **Constraints:** This task validates only; do not make new behavior changes except small typo/metadata fixes discovered by validation.
- **Files:** none
- **Verify:** `bash bin/check-curation.sh && ! grep -RnE "^(last_validated|validated_against|volatility):" skills`
- **Passes when:** Curation passes, all 8 version bumps exist, forbidden freshness fields are absent, and spec scenarios map to current skill text.

### T6 — Finalize, verify, and prepare local install

- **Status:** [x]
- **Depends on:** T5
- **Outcome:** The repo is ready for the user's merge/install decision with a concise summary of changed surfaces and validation evidence.
- **Steps:**
  - [ ] Re-run `bash bin/check-curation.sh` from the repo root.
  - [ ] Review the final diff for accidental gates, copied rubric drift, hard-delete language, freshness frontmatter, or qmd reference consolidation.
  - [ ] Check `git status --short` and list touched files for handoff.
  - [ ] If the user wants to dogfood the runtime copy, recommend reinstalling with `npx skills add scchearn/loam` after merge/push; do not modify the installed runtime copy directly.
  - [ ] Update this plan's touched files during `/loam::starting` completion and prepare `/loam::syncing-code-graph --touched` if a code graph exists.
- **Constraints:** Do not commit, push, or reinstall unless explicitly requested.
- **Files:** none
- **Verify:** `bash bin/check-curation.sh && git status --short`
- **Passes when:** Final validation output is clean, status shows only intended files, and the handoff clearly states how to install/dogfood the updated skills.

---

## Execution groups

| Wave | Tasks | Notes |
| ---- | ----- | ----- |
| 1 | T1 | canonical router contract first |
| 2 | T2, T3, T4 | independent downstream edits after router wording exists; may run concurrently by file group |
| 3 | T5 | cross-file validation |
| 4 | T6 | final verification and handoff |

## Execution disciplines

| Skill | Scope | Fetch-fail fallback |
| ----- | ----- | ------------------- |
| superpowers:verification-before-completion | T5, T6 | Run and read the validation commands before claiming pass. |

## Decisions log

- 2026-06-30 — Consumed spec decision: agent owns memory writes; human flags corrections but does not approve writes.
- 2026-06-30 — Consumed spec decision: `.archive/` is for was-durable-now-superseded content only; never-durable content routes elsewhere or is discarded.
- 2026-06-30 — Consumed amendment: admission rubric is canonical in `loam-using`; downstream page-creating skills reference it rather than copying it.
- 2026-06-30 — Consumed amendment: freshness remains metadata-free; use existing `updated_at` and lint-time volatile-surface inference.
- 2026-06-30 — Minor staleness noted: spec is `draft`, but current user explicitly requested this plan after the debate review passed.
- 2026-06-30 — Scoped read found no `plans/INDEX.md`; created the plan index as part of planning.
- 2026-06-30 — Scoped read found no `wiki/SCHEMA.md` or `wiki/index.md` in the loam repo; no wiki write-back or learning checkpoints were added.

---

## Touched files

<!-- Populated by /loam::starting on task completion. Deduplicated, edit-marked.
     The planner leaves this section empty. /loam::starting fills it during execution. -->

| Path | Marker | Tasks |
| ---- | ------ | ----- |
| skills/loam-using/SKILL.md | edit | T1 |
| skills/loam-memory/loam-adding-to-memory/SKILL.md | edit | T2 |
| skills/loam-memory/loam-adding-to-memory/references/chat-context-ingest.md | edit | T2 |
| skills/loam-memory/loam-amending-memory/SKILL.md | edit | T2 |
| skills/loam-memory/loam-amending-memory/references/amend-checklist.md | edit | T2 |
| skills/loam-memory/loam-learning-from-session/SKILL.md | edit | T2 |
| skills/loam-memory/loam-normalizing-memory/SKILL.md | edit | T3 |
| skills/loam-memory/loam-linting-memory/SKILL.md | edit | T3 |
| skills/loam-memory/loam-auditing-guidance/SKILL.md | edit | T3 |
| skills/loam-ground/loam-scaffolding-wiki/SKILL.md | edit | T4 |
| skills/loam-ground/loam-scaffolding-wiki/references/schema-template.md | edit | T4 |
| skills/loam-ground/loam-scaffolding-wiki/references/qmd-usage.md | edit | T4 |
| skills/loam-memory/loam-adding-to-memory/references/qmd-usage.md | edit | T4 |
| skills/loam-memory/loam-amending-memory/references/qmd-usage.md | edit | T4 |
| skills/loam-memory/loam-learning-from-session/references/qmd-usage.md | edit | T4 |
| skills/loam-memory/loam-linting-memory/references/qmd-usage.md | edit | T4 |
| skills/loam-memory/loam-reviewing-memory/references/qmd-usage.md | edit | T4 |

---

## Handoff notes

**Completed this session:** T1 (Rewrite the loam router ownership contract), T2 (Update core wiki write skills to use admission and archive flows), T3 (Update normalization, linting, and guidance pruning behavior), T4 (Update scaffold schema and qmd reference contracts), T5 (Validate metadata, forbidden fields, and spec scenarios), T6 (Finalize, verify, and prepare local install)
**Wiki updates:** none
**Next task:** none
**Open questions / blockers:** none
**Completion:** 6 of 6 tasks done (100%) — [>] and [h] tasks count as pending until verified [x]
**Active hcom threads:** agent-owned-memory-debate-20260630
**Active hcom agents:** none
**Status reconciliation:** Plan-owned changes are the files listed in `## Touched files` plus `plans/agent-owned-memory.md` and `plans/INDEX.md`. Pre-existing/unowned status entries left untouched: `skills/loam-ground/loam-initializing-vault/scripts/setup.sh`, `skills/loam-work/loam-starting/SKILL.md`, `AGENTS.md`, `bin/check-curation.sh`, `skills/loam-memory/loam-learning-from-session/references/gotchas.md`, `specs/INDEX.md`, `specs/agent-owned-memory.md`. The tracked unowned diffs are pinned-reference edits outside this plan's source scope; they were not reverted because they are unrelated workspace changes. Post-review fix added `skills/loam-memory/loam-adding-to-memory/references/chat-context-ingest.md` to remove a stale proposal-first reference.
