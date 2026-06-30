---
title: Agent-Owned Memory Lifecycle
slug: agent-owned-memory
status: draft
created_at: 2026-06-28 15:05 +02:00
updated_at: 2026-06-30 13:22 +02:00
approved_at: null
research: []
---

# Agent-Owned Memory Lifecycle

## Problem

loam's memory model is inconsistent: the router (`loam-using`) declares "all memory writes are proposal-first, no exceptions," but the primary ingest skill (`loam-adding-to-memory`) already writes without a gate, and the other 5 write skills gate on human approval before applying changes. This creates a split ownership model where the agent does the labor but the human gates every write. The desired model is the reverse: the agent owns the full memory lifecycle — writes freely, self-corrects when it discovers stale or wrong content, and soft-deletes to a recoverable archive. The human can flag incorrect content as a correction trigger, but does not pre-approve writes. Irreversibility (archive + git history), not pre-approval, is the safety net.

## Clarifications

- Q: Should `loam-adding-to-memory` be ungated (agent owns memory) or gated (human owns memory)?
  A: Agent owns memory. The human's role is to flag incorrect content, which triggers correction — not to pre-approve writes.
  Status: answered

- Q: When the agent discovers wrong content mid-task (not during a maintenance skill), should it self-correct inline or switch to `loam-amending-memory`?
  A: Self-correct inline. Archive the old content to `wiki/.archive/`, write the correction, log it, continue the task.
  Status: answered

- Q: When content is "deleted" (superseded, wrong, stale), should it be hard-deleted or moved to an archive?
  A: Soft-delete only. Content moves to `wiki/.archive/` with an archival header. Nothing is ever hard-deleted. Git history is the secondary safety net.
  Status: answered

- Q: Should the human have any role in memory ownership?
  A: Yes — the human can flag incorrect content ("this page is wrong," "this is stale"). The agent treats this as a correction trigger (same as a self-discovered contradiction). The human triggers, the agent executes the correction. The human does not approve the correction text.
  Status: answered

- Q: Which archive mechanism: `.archive/` directory with qmd exclusion, inline `## Archived` section, or `.trash/` with git restore?
  A: `wiki/.archive/` directory with qmd exclusion. Pages are moved (not copied) so wikilinks still resolve. qmd excludes `.archive/**` via per-collection `ignore` config.
  Status: answered

- Q: Should the agent self-correct anytime it reads a wiki page and finds a contradiction, or only during explicit maintenance skills?
  A: Always self-correct. Any task that reads wiki context and finds a contradiction triggers inline correction.
  Status: answered

- Q: Are all 6 write skills changed at once, or phased?
  A: All 6 at once.
  Status: answered

## Amendment: anti-junk ownership filter

The no-gate ownership decision stands. To avoid junk memory, the agent applies a write-time filter and routing matrix inline. This is not a human gate: the agent judges, does not add a turn, and proceeds without approval.

### Add / change / drop

| Action | Item | Reason |
|---|---|---|
| ADD | Durable-memory admission rubric, canonical in `loam-using` | Defines what earns a wiki page and avoids duplicating the rule across skills |
| ADD | Five-way routing matrix | Keeps non-durable material out of wiki pages by routing it elsewhere |
| ADD | Freshness-validation triggers | Catches drift without human review |
| ADD | Do/don't remember examples | Gives page-creating skills concrete anti-slop guidance |
| CHANGE | `loam-learning-from-session` routing | Binary wiki/guidance routing becomes the five-way matrix |
| CHANGE | all page-creating skill intake | Apply admission before creating a page; route non-durable material instead of archiving it |
| CHANGE | freshness checks | Infer volatility at lint time from existing `updated_at` and cited volatile surfaces; no new frontmatter |
| DROP | nothing structural | Gate removal remains; this adds an agent-owned filter, not approval |

### Durable-memory admission rubric

A claim earns a wiki page only if it passes all three:

- **R1 Reusable** — a future session on a different task would plausibly need it.
- **R2 About the project/domain** — codebase, architecture, decisions, conventions, external dependencies, or durable external facts; not the conversation, the agent, or transient user state.
- **R3 Costly to reconstruct & re-checkable** — re-deriving the claim from a live source (code, config, calendar, task list) would cost more than the page costs to maintain. If one command or one file read gets it back, it does not earn a page. Where an external source exists, the page names it (code path, `raw/` doc, command output, dated doc) so freshness can re-validate. Pure decisions and rationale are self-sourcing and satisfy this by stating their reasoning.

Disqualifiers override the rubric:

- **D1 ephemeral** — build state, current branch, "today I ran X" → operational report.
- **D2 duplicate** — an existing page covers it → amend the existing page instead of creating another.

Tiebreaker: admit if not reconstructable from a live source; discard if reconstructable. `wiki/.archive/` is only for *was-durable-now-superseded* content. Never-durable material is not written to the wiki and is therefore never archived.

### Routing matrix

| Material | Destination | Skill path |
|---|---|---|
| Reusable project/domain fact, passes the rubric | durable wiki page | `loam::adding-to-memory`, self-correction |
| Agent-behavior convention/command/gotcha | `AGENTS.md` / `CLAUDE.md` | `loam::learning-from-session` guidance path |
| Session state for resume/handoff | `wiki/checkpoints/<slug>.md` | `loam::checkpointing` |
| Per-task context attached to a unit of work | task annotation / plan file | `loam::planning`, `loam::starting` |
| Build output, branch state, one-off, unverifiable, or rubric failure | discard (optional `log.md` audit line) | none |

### Do / don't remember

| Do remember | Don't remember |
|---|---|
| "Calendar MCP and gws CLI use separate OAuth; both can need independent re-auth" | "We talked about auth today" |
| "Decision: soft-delete over hard-delete; git history is secondary safety net" | "I decided to use soft-delete" |
| "qmd v2.5.3 supports per-collection `ignore` (source: `cli/qmd.js:553`)" | "qmd docs look good" |
| "Convention: never hard-delete wiki content" → guidance | "User seems frustrated with the gate" |
| "Paused mid-ingest of `raw/article.md`; `topics/foo.md` half-written" → checkpoint | "Session went well" |
| "task #42 due Friday" → task annotation | A wiki copy of TaskWarrior state |

### Freshness-validation triggers

The agent re-validates rather than only correcting on accidental discovery when any of these occur:

- **F1** a cited file path changed since page `updated_at`.
- **F2** a refactor, rename, deletion, or migration touched a referenced path (`loam::syncing-code-graph --touched`).
- **F3** a pinned version, API, or external doc the page names has a newer mention.
- **F4** `updated_at` is older than 90 days and the page cites volatile surfaces (APIs, configs, versions). Lint flags this; it does not auto-archive.
- **F5** contradiction-on-read — already covered by self-correction.

Validation result is one of: **confirmed** (bump `updated_at`), **corrected** (archive + rewrite + log), or **demoted** (archive with reason "no longer applies").

## Scenarios

### Scenario: Agent ingests a new source — no gate

- Given the user invokes `/loam::adding-to-memory raw/article.md` and the wiki exists
- When the agent reads the source, synthesizes content, and discovers related pages
- Then the agent applies the admission rubric before creating pages, routes non-durable material via the routing matrix, writes admitted pages, updates existing pages (including contradiction/supersession marks), updates `index.md` and `log.md`, refreshes qmd, and reports what it did — without showing a proposal or waiting for approval
- Verify with: manual — invoke `/loam::adding-to-memory` on a test source and confirm no proposal step appears; durable content gets pages; non-durable content is routed or discarded; `log.md` has a new `add` entry

### Scenario: Agent discovers wrong content mid-task — self-correct inline

- Given the agent is working on an unrelated task (e.g., executing a plan) and reads a wiki page as context
- When the agent finds a claim in the wiki page that is contradicted by current evidence (code, source file, newer doc)
- Then the agent: (1) moves the old page to `wiki/.archive/<slug>.md` with an archival header, (2) writes the corrected page in the original location, (3) appends `## [YYYY-MM-DD] self-correct | <what was wrong>` to `log.md`, (4) continues the original task without switching to `loam-amending-memory`
- Verify with: manual — plant a wrong claim in a wiki page, start an unrelated task that reads it, confirm the page is archived and corrected inline

### Scenario: Human flags incorrect content — agent corrects without gate

- Given the user says "this page is wrong" or "this wiki page is stale" about a specific page
- When the agent receives the flag
- Then the agent: (1) reads the flagged page and current evidence, (2) moves the old page to `wiki/.archive/`, (3) writes the correction, (4) logs it as `## [YYYY-MM-DD] correct | <flagged by user>`, (5) reports what changed — without showing a proposal or waiting for approval
- Verify with: manual — flag a wiki page as wrong, confirm the agent archives + corrects without a gate step

### Scenario: Agent amends memory — no gate

- Given the user invokes `/loam::amending-memory "the wiki says X but the code now shows Y"`
- When the agent reads the affected pages and confirms the issue
- Then the agent: (1) moves superseded/wrong content to `wiki/.archive/`, (2) writes the corrected page, (3) updates related pages and `index.md`, (4) appends `## [YYYY-MM-DD] amend | <summary>` to `log.md`, (5) refreshes qmd, (6) reports — without showing a proposal or waiting for approval
- Verify with: manual — invoke `/loam::amending-memory` and confirm no "wait for confirmation" step; archived content is in `wiki/.archive/`

### Scenario: Agent captures session learnings — no gate

- Given the user invokes `/loam::learning-from-session` after a session
- When the agent identifies learnings
- Then the agent routes each learning through the five-way matrix — durable wiki page, guidance, checkpoint, task annotation/plan file, or discard — and writes only the routed surfaces that apply, without showing a proposal or waiting for approval
- Verify with: manual — invoke `/loam::learning-from-session`, confirm no proposal step; durable facts, guidance, checkpoint state, task context, and discard cases route correctly

### Scenario: Page creation applies admission everywhere

- Given any memory skill is about to create a new wiki page (`adding`, `amending`, `learning`, or `normalizing`)
- When the candidate page fails the admission rubric or hits a disqualifier
- Then the agent does not create the page, routes the material via the matrix, and continues without treating the skipped page as archived content
- Verify with: manual — attempt to create a page for reconstructable task state and confirm no wiki page or archive entry is created

### Scenario: Freshness lint flags volatile stale pages

- Given a wiki page cites a volatile surface (API, config, version, or code path) and its `updated_at` is older than 90 days
- When `loam-linting-memory` runs
- Then lint flags the page for re-validation but does not auto-archive it
- Verify with: manual — create a stale volatile page, run `/loam::linting-memory`, confirm a re-validation warning and no archive move

### Scenario: Agent normalizes memory — no gate

- Given the user invokes `/loam::normalizing-memory` on a messy wiki corpus
- When the agent identifies structural issues (orphan pages, broken links, missing index entries)
- Then the agent applies structural fixes — without waiting for confirmation
- Verify with: manual — invoke `/loam::normalizing-memory` on a test corpus, confirm fixes applied without a gate

### Scenario: Agent lints memory — no gate on safe fixes

- Given the user invokes `/loam::linting-memory` and the agent discovers safe-fixable issues (broken wikilinks, stale checkpoint filenames, log rotation needed)
- When the agent runs `fix` mode
- Then the agent applies fixes directly — without explicit approval. The safe-fix list (what counts as safe) still defines the boundary of auto-fixable issues.
- Verify with: manual — invoke `/loam::linting-memory` with `fix` mode, confirm fixes applied without approval step

### Scenario: Agent audits guidance — no gate on prune

- Given the user invokes `/loam-auditing-guidance` and the agent identifies stale/duplicate entries in `AGENTS.md`
- When the agent prunes
- Then the agent removes stale entries directly — without showing a prune diff and waiting for approval. Removed content is noted in the audit report.
- Verify with: manual — invoke `/loam-auditing-guidance`, confirm pruning without approval step

### Scenario: Archived content is excluded from search — failure/edge path

- Given a wiki with `wiki/.archive/old-claim.md` and qmd configured with `ignore: [".archive/**"]`
- When a future session runs `qmd query` or `qmd search`
- Then archived content does not appear in search results
- Verify with: `qmd query "content from archived page"` — confirm zero results from `.archive/`

### Scenario: Wikilink to archived page still resolves — edge path

- Given a page `wiki/.archive/old-claim.md` exists (moved from `wiki/topics/old-claim.md`) and another page contains `[[old-claim]]`
- When Obsidian or the agent resolves `[[old-claim]]`
- Then the link resolves to `wiki/.archive/old-claim.md` (Obsidian searches all vault folders including dot-folders). The archived page has a `> See [[corrected-claim]] for the current state.` pointer.
- Verify with: manual — open the linking page in Obsidian, click `[[old-claim]]`, confirm it navigates to the archived page

### Scenario: qmd collection lacks ignore config — failure path

- Given a wiki collection was set up before this spec and does not have `ignore: [".archive/**"]` in its qmd config
- When `loam-linting-memory` runs
- Then the lint checks that `.archive/**` is excluded and flags the missing ignore pattern as a health issue
- Verify with: manual — remove the `ignore` config from a collection, run `/loam::linting-memory`, confirm it flags the missing exclusion

## Scope

### In

- Remove proposal-first gate from all 6 write skills: `loam-adding-to-memory`, `loam-amending-memory`, `loam-learning-from-session`, `loam-normalizing-memory`, `loam-linting-memory` (safe-fixes path), `loam-auditing-guidance` (prune path)
- Rewrite `loam-using` router: replace "all memory writes are proposal-first, no exceptions" (6 sites) with the agent-ownership model
- Add `wiki/.archive/` directory as the soft-delete target with archival header format
- Add qmd exclusion for `.archive/**` via per-collection `ignore` config
- Add self-correction trigger: agent corrects inline when it discovers wrong content during any context read, without switching to `loam-amending-memory`
- Add human-flag trigger: human says "this is wrong" → agent archives + corrects without gate
- Add durable-memory admission rubric to `loam-using`; page-creating skills reference it instead of restating it
- Add five-way routing matrix and apply it to `loam-learning-from-session` and other page-creation paths
- Add freshness-validation triggers to `loam-linting-memory`, using existing `updated_at` and inferred volatile-surface citations
- Update `schema-template.md` with `.archive/` in directory layout and ownership language
- Update qmd-usage references to include `.archive/` exclusion in collection setup
- Update `loam-linting-memory` to check that qmd `.archive/**` exclusion exists
- Version bump all touched skills

### Out

- Hard-delete of any wiki content (never)
- Pre-write proposal or approval on any memory write
- Consolidation of the 6 `qmd-usage.md` copies (separate concern, see curation debate)
- Changes to `loam-work` skills (planning, starting, checkpointing, resuming, amending-plan, configuring-agents) — they are not memory-write skills
- Changes to read-only skills (`loam-querying-memory`, `loam-reviewing-memory`, `loam-resuming`) — they don't write
- External ref pinning, README count fix, AGENTS.md dogfooding (separate curation concerns)
- A top-level version manifest or CHANGELOG (separate curation concern)

## Constraints

- **Nothing is ever hard-deleted.** All "deletions" are moves to `wiki/.archive/`. Git history is the secondary safety net.
- **`.archive/` is excluded from qmd.** Archived content must not appear in search results. This is enforced via per-collection `ignore: [".archive/**"]` in `~/.config/qmd/index.yml`.
- **Wikilinks must not break.** Pages are moved (not copied) to `.archive/` so Obsidian's vault-wide search still resolves `[[old-page]]` to the archived location. Archived pages have a `> See [[corrected-page]]` pointer.
- **Self-correction is inline.** The agent does not switch to `loam-amending-memory` mid-task. It archives, corrects, logs, and continues.
- **The human can trigger but not gate.** "This page is wrong" is a trigger (same as a self-discovered contradiction). The human does not approve the correction text.
- **Admission is canonical in `loam-using`.** Page-creating skills reference the rubric; they do not copy it into separate drift sites.
- **Non-durable material is routed, not archived.** `.archive/` is only for content that was durable and later became wrong, stale, or superseded.
- **No freshness frontmatter migration.** Freshness uses existing `updated_at` plus lint-time inference from cited volatile surfaces.
- **`raw/` stays immutable.** This change affects `wiki/` ownership only. The raw-source immutability contract is unchanged.
- **The safe-fix boundary in `loam-linting-memory` stays.** The list of what counts as a "safe fix" is not expanded — only the approval step is removed.
- **Frontmatter versions bump.** Every touched SKILL.md gets a version bump per semver (minor for new behavior, patch for edits).

## Acceptance criteria

- [ ] `loam-using` SKILL.md contains no instance of "proposal-first" or "no exceptions" as a memory-write rule. The agent-ownership model (writes freely, self-corrects, soft-deletes, human flags) is stated in its place.
- [ ] `loam-using` SKILL.md contains the canonical durable-memory admission rubric with R1/R2/R3, D1/D2, and the reconstructable/discard tiebreaker.
- [ ] Page-creating skills reference the `loam-using` admission rubric instead of restating it.
- [ ] `loam-learning-from-session` routes learnings through the five-way matrix: wiki, guidance, checkpoint, task annotation/plan, discard.
- [ ] `loam-linting-memory` flags pages older than 90 days that cite volatile surfaces for re-validation, without auto-archiving them.
- [ ] No new freshness frontmatter fields (`last_validated`, `validated_against`, `volatility`) are added.
- [ ] `loam-adding-to-memory` SKILL.md has no proposal/confirmation step and includes self-correction + archive behavior in Step 2.
- [ ] `loam-amending-memory` SKILL.md has no "wait for explicit confirmation" step (lines 119-123 deleted). The flow is "read evidence → archive old → write correction → log → report."
- [ ] `loam-learning-from-session` SKILL.md has no "show proposal / wait for approval" step (lines 241-301 deleted). The agent routes and writes.
- [ ] `loam-normalizing-memory` SKILL.md has no "wait for explicit confirmation" (line 169 deleted).
- [ ] `loam-linting-memory` SKILL.md has no "explicit approval" requirement for fix mode (line 187 deleted). The safe-fix list boundary remains.
- [ ] `loam-auditing-guidance` SKILL.md has no "user approval" requirement for pruning (line 214 deleted).
- [ ] `schema-template.md` includes `wiki/.archive/` in the directory layout with the note "excluded from qmd, never hard-deleted."
- [ ] `schema-template.md` ownership language says the agent owns and maintains the files, writes without pre-approval, and soft-deletes to `.archive/`.
- [ ] qmd-usage references (scaffolding-wiki copy at minimum) instruct adding `ignore: [".archive/**"]` to the collection config during setup.
- [ ] `loam-linting-memory` checks that qmd `.archive/**` exclusion exists and flags it as a health issue if missing.
- [ ] All 8 touched SKILL.md files have bumped `metadata.version`.
- [ ] `loam-using` router describes the self-correction trigger (agent discovers wrong content mid-task → archive + correct + log + continue) and the human-flag trigger (human says "this is wrong" → same flow).

## Decision

**Agent-owned memory lifecycle.** The agent writes freely (no pre-write gate), applies the durable-memory admission rubric inline, routes non-durable material away from wiki pages, self-corrects when it discovers stale or wrong content (inline, without switching skills), and soft-deletes to `wiki/.archive/` (never hard-deleted). The human can flag incorrect content to trigger correction, but does not approve writes. Irreversibility — via archive + git history — is the safety net, not pre-approval.

This replaces the current "proposal-first, no exceptions" rule with a model where the agent is the sole writer and corrector of memory, and the human is the reviewer who reads (in Obsidian) and flags (to the agent) but does not gate.

### Archive mechanism

`wiki/.archive/` directory, qmd-excluded. Pages are moved (not copied) so wikilinks still resolve. Archived pages get a header:

```md
> Archived YYYY-MM-DD. Superseded by [[corrected-page-name]].
> Reason: <one-line reason>
```

qmd exclusion via per-collection `ignore` in `~/.config/qmd/index.yml`:

```yaml
collections:
  <collection-name>:
    path: <wiki path>
    pattern: "**/*.md"
    ignore:
      - ".archive/**"
```

Verified: qmd v2.5.3 supports per-collection `ignore` patterns (source: `cli/qmd.js:553` — `ignorePatterns: yamlCol?.ignore`).

### Self-correction trigger

When the agent reads any wiki page as context during any task and finds a claim contradicted by current evidence:

1. Move the old page to `wiki/.archive/<slug>.md` with the archival header
2. Write the corrected page in the original location (or create a new page if the topic shifted)
3. Append `## [YYYY-MM-DD] self-correct | <what was wrong>` to `log.md`
4. Continue the original task — correction is inline, not a skill switch

`loam-amending-memory` becomes the *explicit* version (user says "fix the wiki" or agent runs a lint pass). Self-correction during context reads is the *implicit* version. Both follow the same archive + correct + log flow.

### Human-flag trigger

When the human says "this page is wrong" or "this is stale," the agent treats it identically to a self-discovered contradiction. The human *triggers* correction but does not *gate* it — the agent decides the correction text and writes it.

## Rejected alternatives

- **Position A — gate all writes (flat gate, human owns memory):** Rejected because the user explicitly wants the agent to own memory. The gate model makes the agent a laborer and the human the gatekeeper, which is the opposite of the desired architecture. The debate with `mabe` concluded that a flat gate is the simplest *gate* form, but the user overruled the gate model entirely.
- **Position B (original) — leave `adding-to-memory` ungated, gate the rest:** Rejected because it's inconsistent. The router says "no exceptions" but the skill violates it. The user wants consistency in the other direction: remove all gates, not add one to `adding-to-memory`.
- **Proportional gate (minimal for new pages, full for destructive):** Rejected by `mabe` in debate — adds classification risk, gaming surface, and re-gate edge cases for a 15-second savings. The user further overruled by wanting no gate at all.
- **Inline `## Archived` section (no directory move):** Rejected because it means pages grow unboundedly and archived content stays in the active page, polluting search results and context reads. A separate directory with qmd exclusion is cleaner.
- **`.trash/` with git restore as the only safety net:** Rejected because it assumes git is the wiki's VCS and provides no search-level exclusion. `.archive/` + qmd exclusion is VCS-agnostic and keeps archived content out of search.
- **Keep `loam-amending-memory` as the only correction path (no inline self-correction):** Rejected because it means wrong content stays live and indexed until a maintenance skill runs. Inline self-correction catches issues at the moment they're discovered, minimizing the contamination window.

## Key files / modules

- `skills/loam-using/SKILL.md` — router; 6 "proposal-first / no exceptions" sites to replace (lines 17, 21, 43, 46, 47, 62, 68); add canonical admission rubric, routing matrix, freshness triggers
- `skills/loam-memory/loam-adding-to-memory/SKILL.md` — already ungated; add self-correction + archive behavior in Step 2; apply admission before page creation
- `skills/loam-memory/loam-amending-memory/SKILL.md` — remove proposal gate (lines 85-123); replace with archive + apply flow
- `skills/loam-memory/loam-learning-from-session/SKILL.md` — remove proposal gate (lines 241-301); route learnings through the five-way matrix
- `skills/loam-memory/loam-normalizing-memory/SKILL.md` — remove "wait for confirmation" (line 169); apply admission when creating new pages
- `skills/loam-memory/loam-linting-memory/SKILL.md` — remove "explicit approval" (line 187); add `.archive/` exclusion lint check; add freshness re-validation flags
- `skills/loam-memory/loam-auditing-guidance/SKILL.md` — remove "user approval" for pruning (line 214)
- `skills/loam-ground/loam-scaffolding-wiki/SKILL.md` — version bump because its schema and qmd reference files change
- `skills/loam-ground/loam-scaffolding-wiki/references/schema-template.md` — add `.archive/` to directory layout (after line 20); update ownership language (line 13)
- `skills/loam-ground/loam-scaffolding-wiki/references/qmd-usage.md` — add `.archive/` exclusion to collection setup instructions
- `skills/loam-memory/loam-amending-memory/references/amend-checklist.md` — remove "wait for user confirmation" (line 16)
- `skills/loam-memory/loam-adding-to-memory/references/qmd-usage.md` (×5 consumer copies) — note `.archive/` exclusion
- `~/.config/qmd/index.yml` — add `ignore: [".archive/**"]` to each loam-managed wiki collection (manual, per-wiki)

## Completeness checklist

| Area | Status | Notes |
| ---- | ------ | ----- |
| Behavior | pass | Agent-ownership model, admission filter, routing matrix, freshness triggers, self-correction, human-flag, soft-delete are all specified. |
| Scenarios | pass | 13 scenarios covering all 6 write skills, page admission, freshness lint, self-correction, human-flag, archive exclusion, wikilink resolution, missing-ignore failure. |
| Scope | pass | In/out explicit; separate curation concerns excluded. |
| Constraints | pass | 11 constraints: no hard-delete, qmd exclusion, wikilink integrity, inline correction, human-trigger-not-gate, canonical admission, routing-not-archive, no freshness frontmatter migration, raw immutability, safe-fix boundary, version bumps. |
| Interfaces / contracts | pass | `.archive/` directory layout, archival header format, qmd `ignore` config format, `log.md` entry formats (`self-correct`, `correct`). |
| Data / migration | pass | Existing wikis need qmd `ignore` config added manually; `loam-linting-memory` flags missing exclusion. No data migration of existing pages required — `.archive/` is populated as corrections happen. |
| Errors / edge cases | pass | Missing qmd ignore config (lint flags it), wikilink to archived page (still resolves via Obsidian vault-wide search), duplicate-link conflict (avoided by move-not-copy). |
| Security / privacy | n/a | No auth, secrets, or PII changes. |
| Integrations | pass | qmd per-collection `ignore` config verified against qmd v2.5.3 source (`cli/qmd.js:553`). Obsidian wikilink resolution to dot-folders confirmed. |
| Operations / rollout | pass | All changes ship in one commit set. No phased rollout. Existing wikis need manual qmd config update (lint catches it). Runtime copy reinstalls via `npx skills add scchearn/loam`. |
| Verification | pass | All scenarios have manual verification steps. No automated test suite exists for loam skills. |
| Planning inputs | pass | Key files with line numbers, validation commands (manual), rejected alternatives with rationale. |
| Open questions | pass | All blocking questions resolved during debate. |

## Open questions

- none

## Minimal spec guidance

This is a behavior-changing spec. All sections are substantive. The decision is the agent-ownership model; the scenarios cover all 6 write skills plus the new self-correction and human-flag triggers; the constraints define the archive mechanics and qmd exclusion.
