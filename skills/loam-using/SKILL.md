---
name: loam::using
description: "The always-on protocol for the loam skill namespace. Use at session start and whenever a loam task appears. Routes to the right skill, explains the memory model (memory = umbrella; wiki, guidance, and checkpoints are substrates), and lists the cross-cutting rules. This is a routing/meta skill — delegate to a specific loam skill rather than performing work itself."
metadata:
  version: "1.4.0"
  author: scchearn
---

# Using loam

This is the router for the loam skill namespace. It does not perform work itself. It tells you which loam skill to invoke for a given intent, and the rules that apply across all of them.

**This is a routing/meta skill.** When you recognize the matching skill, invoke it via the host harness's skill loader. Do not perform the work here.

## Non-negotiables

Three rules keep loam work aligned with the memory model.

1. **Invoke the relevant skill before any loam action.** Not just tool calls. Before planning, researching, starting, resuming, checkpointing, debating or reaching consensus, amending a plan, adding to memory, querying memory, amending memory, linting memory, normalizing memory, reviewing memory, capturing learnings, auditing guidance, scaffolding the wiki, or initializing a vault: invoke the matching skill. This document is a router. The skill body has the actual rules. The responsibility is yours on everything else. Err on the side of invoking.
2. **Memory first, substrate second.** Talk about "memory" first. Use "wiki" only when distinguishing the markdown substrate from the guidance or checkpoint substrates. Never say "the wiki" when you mean "memory" — the wiki is one substrate of memory, not the whole thing.
3. **Agent-owned memory writes.** The agent writes, corrects, routes, and archives memory without pre-approval. The human may flag stale or wrong content, which triggers the same correction flow as an agent-discovered contradiction.

## The memory model

**Memory** is the umbrella concept for everything loam captures. Three substrates exist:

| Substrate | What it is | Who maintains it |
|---|---|---|
| **wiki** | Durable Obsidian-friendly markdown notes under `wiki/`. Topic, entity, concept, analysis pages. | loam-memory group (most skills) |
| **guidance** | `AGENTS.md` is the canonical guidance file. `CLAUDE.md` is a thin import shim (`@AGENTS.md` only). `.claude.local.md` for personal overrides. | loam-memory group (`auditing-guidance`, `learning-from-session` guidance path) |
| **checkpoints** | Transient work-state under `wiki/checkpoints/`. Restart notes, not durable knowledge. | loam-work group (`checkpointing` writes, `resuming` reads) |

The wiki substrate is what `qmd` indexes. The guidance substrate is what harnesses load as prompt context. Checkpoints are work-state, not knowledge — they refuse to touch `index.md` or `log.md` and should not become durable claims.

**Talk about "memory" first. Use "wiki" only when the substrate distinction matters.** A skill that operates on the markdown notes works on the wiki substrate of memory. A skill that edits `AGENTS.md` works on the guidance substrate of memory. Both are memory.

## Durable-memory admission rubric

Use this rubric before creating a wiki page. Page-creating skills reference this section instead of copying it.

A claim earns a wiki page only if it passes all three:

- **R1 Reusable** — a future session on a different task would plausibly need it.
- **R2 About the project/domain** — codebase, architecture, decisions, conventions, external dependencies, or durable external facts; not the conversation, the agent, or transient user state.
- **R3 Costly to reconstruct & re-checkable** — re-deriving the claim from a live source (code, config, calendar, task list) would cost more than the page costs to maintain. If one command or one file read gets it back, it does not earn a page. Where an external source exists, the page names it so freshness can re-validate. Pure decisions and rationale are self-sourcing and satisfy this by stating their reasoning.

Disqualifiers override the rubric:

- **D1 ephemeral** — build state, current branch, "today I ran X" -> operational report.
- **D2 duplicate** — an existing page covers it -> amend the existing page instead of creating another.

Tiebreaker: admit if not reconstructable from a live source; discard if reconstructable. `wiki/.archive/` is only for was-durable-now-superseded content. Never-durable material is not written to the wiki and is never archived.

## Routing matrix

| Material | Destination | Skill path |
|---|---|---|
| Reusable project/domain fact, passes the rubric | durable wiki page | `loam::adding-to-memory`, self-correction |
| Agent-behavior convention/command/gotcha | `AGENTS.md` / `CLAUDE.md` | `loam::learning-from-session` guidance path |
| Session state for resume/handoff | `wiki/checkpoints/<slug>.md` | `loam::checkpointing` |
| Per-task context attached to a unit of work | task annotation / plan file | `loam::planning`, `loam::starting` |
| Build output, branch state, one-off, unverifiable, or rubric failure | discard, optionally with a `log.md` audit line | none |

## Self-correction and Human-flag triggers

- **Self-correction:** when the agent reads memory and finds a contradiction with current evidence, move the superseded durable page to `wiki/.archive/` with an archival header, write the correction in the live location, append a `self-correct` entry to `log.md`, and continue the original task.
- **Human-flag:** when the human says a memory page is wrong or stale, treat it as a correction trigger, not an approval request. Read the flagged page and current evidence, archive the superseded durable page, write the correction, log it, and report what changed.
- **Archive scope:** `.archive/` is only for pages that were durable and later became wrong, stale, or superseded. Never-durable material is routed elsewhere or discarded.

## Freshness-validation triggers

Use existing `updated_at` plus lint-time evidence; do not add freshness frontmatter.

- **F1** a cited file path changed since page `updated_at`.
- **F2** a refactor, rename, deletion, or migration touched a referenced path (`loam::syncing-code-graph --touched`).
- **F3** a pinned version, API, or external doc the page names has a newer mention.
- **F4** `updated_at` is older than 90 days and the page cites volatile surfaces such as APIs, configs, versions, or code paths. Lint flags this for re-validation and does not auto-archive it.
- **F5** contradiction-on-read, handled by self-correction.

Validation result is one of: **confirmed** (bump `updated_at`), **corrected** (archive + rewrite + log), or **demoted** (archive with reason "no longer applies").

## Red flags: thoughts that mean STOP and invoke

These rationalizations cause skills to be skipped. If you catch yourself thinking any of them, invoke the relevant skill **even if you "already read it" earlier in the session**.

| Thought | Action |
|---|---|
| "I'll just write this to the wiki" | Invoke `loam-adding-to-memory`. Apply the admission rubric, then write or route. |
| "I'll just plan it, this is simple" | Invoke `loam-writing-spec`. Specs are required before `loam-planning`. |
| "I'll just run a quick debate between agents" | Invoke `loam-configuring-agents`. Debates are approval-gated; no sends before the gate. |
| "I'll just edit this plan directly" | Invoke `loam-amending-plan`. Plan changes cascade; the skill walks the impact. |
| "I'll write this memory page because it might help later" | Apply the admission rubric first. If it fails, route or discard it. |
| "The wiki is out of date, I'll fix it inline" | Archive the superseded durable page, write the correction, log it, and continue. |
| "I'll just capture this learning as a note" | Invoke `loam-learning-from-session`. It routes to wiki or guidance, not you. |
| "I'll just check the wiki real quick" | Invoke `loam-querying-memory`. Stay grounded in memory, not stale context. |
| "I'll just add this command to AGENTS.md" | Invoke `loam-learning-from-session` (guidance path) or `loam-auditing-guidance` for a full audit. |
| "I'll just pause and pick up later" | Invoke `loam-checkpointing`. A checkpoint is a resumable artifact, not a mental note. |
| "I'll resume where I left off" | Invoke `loam-resuming`. Verify live state before acting. |
| "I'll set up the wiki" | Invoke `loam-scaffolding-wiki` first, then `loam-initializing-vault` for Obsidian config. |

## The loam lifecycle

For any loam task:

1. **Orient.** Recognize the matching skill from the decision graph below. If two skills seem relevant, apply the precedence rules.
2. **Choose skill.** Invoke it via the host harness's skill loader.
3. **Preserve substrate boundaries.** Do not write checkpoints into durable pages. Do not write guidance into wiki pages. Do not write wiki content into `AGENTS.md`. Each substrate has its skill.
4. **Apply admission and routing before memory writes.** Durable wiki pages must pass the rubric; everything else goes to guidance, checkpoints, task annotations, or discard.
5. **Update plans / checkpoints / guidance only via the matching skill.** Don't edit a plan inline — use `loam-amending-plan`. Don't write a checkpoint inline — use `loam-checkpointing`. Don't edit `AGENTS.md` inline — use `loam-learning-from-session` or `loam-auditing-guidance`.

### Conflict rules (when two skills seem relevant)

- **Process / current-work skills win before memory-maintenance skills.** If you're mid-execution and discover a memory issue, finish the current-work step first (or checkpoint it), then invoke the memory skill.
- **Memory writes are agent-owned.** If current work produces a learning, route it through the matrix; if it exposes stale memory, correct it inline with archive + log.
- **Checkpoints are transient.** They live under `wiki/checkpoints/` but are not durable claims. They refuse to update `index.md` or `log.md`. Don't promote a checkpoint into a durable note without going through `loam-adding-to-memory`.
- **Amend vs lint.** `loam-amending-memory` fixes specific wrong claims. `loam-linting-memory` health-checks the whole graph. If you found a specific error, amend. If you want a general check, lint.
- **Query vs review.** `loam-querying-memory` answers a question. `loam-reviewing-memory` surfaces gaps. If you want an answer, query. If you want to know what's unresolved, review.
- **Add vs learn.** `loam-adding-to-memory` ingests a source (file, article, conversation-as-note). `loam-learning-from-session` captures session-derived insights and routes them to wiki or guidance. If you have a source, add. If the session produced insight, learn.

## Decision graph

```
"I want to..."
├─ start something new
│  ├─ research a question ──────────── /loam::writing-spec
│  ├─ plan approved work ────────────── /loam::planning
│  └─ debate / reach consensus ─────── /loam::configuring-agents
├─ execute work
│  ├─ begin a plan ──────────────────── /loam::starting
│  ├─ pause / hand off ──────────────── /loam::checkpointing
│  ├─ resume after pause ────────────── /loam::resuming
│  └─ change an in-flight plan ──────── /loam::amending-plan
├─ work with memory
│  ├─ add a source ──────────────────── /loam::adding-to-memory
│  ├─ ingest a codebase ─────────────── /loam::ingesting-codebase
│  ├─ sync code graph drift ────────── /loam::syncing-code-graph
│  ├─ ask a question ─────────────────── /loam::querying-memory
│  ├─ fix what's wrong ─────────────── /loam::amending-memory
│  ├─ health-check ──────────────────── /loam::linting-memory
│  ├─ normalize messy corpus ────────── /loam::normalizing-memory
│  ├─ see what's unresolved ─────────── /loam::reviewing-memory
│  ├─ capture session learnings ─────── /loam::learning-from-session
│  └─ audit agent guidance ──────────── /loam::auditing-guidance
└─ set up the substrate
   ├─ scaffold the wiki ─────────────── /loam::scaffolding-wiki
   └─ init Obsidian vault ───────────── /loam::initializing-vault
```

### Precedence note for ambiguous cases

If an intent maps to two skills, the tree above is the primary router. When the tree doesn't disambiguate, apply the conflict rules in the lifecycle section. When still uncertain: ask the user which intent is closer.

## loamstate hints

`loamstate.sh <workspace-root>` (the orientation probe every loam skill runs at startup) emits a top-level `hints` array of advisory routing signals derived from the same cheap probes. This section is the canonical contract; other skills do not restate it.

Each hint is a JSON object:

```json
{"kind":"checkpoint_stale","group":"maintenance","severity":"info",
 "message":"...","command":"/loam::checkpointing","evidence":{"age_minutes":34}}
```

- `group` — `maintenance` (upkeep drift) or `workflow` (a next step is available).
- `severity` — `info`, `warn`, or `action`.
- `command` — a literal loam skill invocation with `<placeholders>` (e.g. `<workspace-root>`, `plans/<file>`) you substitute from `evidence` before acting; `null` when there is no single safe command.
- `evidence` — the cheap facts that fired the hint.

**Hints are advisory. They never authorize bypassing a skill.** A hint points you at the relevant loam skill; you still invoke that skill and follow its contract. Do not auto-run a hinted command as a side effect. Absent or omitted hints mean "no cheap signal", not "nothing to do".

v1 kinds — maintenance: `memory_missing`, `checkpoint_stale`, `code_ingest_pending`, `memory_lint_stale`, `date_drift_pending`, `log_rotation_due`, `legacy_structure_pending`, `retrieval_not_ready`; workflow: `resume_available`, `resume_stale`, `spec_ready_for_plan`, `plan_ready_to_start`, `plan_in_progress`. Deferred (not emitted): `code_graph_orphans`, `session_learning_candidate`, `code_sync_after_plan`.

## Compact skill reference

One line per skill. The decision graph is the primary router; this is for quick lookup.

- `loam-writing-spec` — research a question, produce `specs/<slug>.md`
- `loam-planning` — compile approved spec into execution-ready plan
- `loam-starting` — begin or resume execution of a plan
- `loam-resuming` — resume from checkpoint notes
- `loam-checkpointing` — write a resumable checkpoint before pausing
- `loam-configuring-agents` — run an approval-gated structured debate or conference between agents with distinct positions and a convergence deliverable
- `loam-amending-plan` — update an in-flight plan after scope change
- `loam-adding-to-memory` — ingest a source into memory (wiki substrate)
- `loam-ingesting-codebase` — ingest codebase as entity pages with wikilink edges
- `loam-syncing-code-graph` — reconcile code graph to repo at plan gate or on-demand
- `loam-querying-memory` — answer a question from memory
- `loam-amending-memory` — fix specific wrong or stale memory claims
- `loam-linting-memory` — health-check the memory graph
- `loam-normalizing-memory` — retrofit structure onto a messy memory corpus
- `loam-reviewing-memory` — surface unresolved gaps in memory
- `loam-learning-from-session` — capture session learnings, route to wiki or guidance
- `loam-auditing-guidance` — audit and improve `AGENTS.md` / `CLAUDE.md` / `.claude.local.md`
- `loam-scaffolding-wiki` — instantiate a new wiki substrate scaffold
- `loam-initializing-vault` — set up an Obsidian vault with config

## Installing slash commands

If the user wants `/checkpoint` and `/resume` as shortcut commands (instead of typing `/loam::checkpointing` and `/loam::resuming`), you can install them. The command files ship in the loam repo under `commands/`.

**Read `references/commands-install.md` for the full protocol.** It covers:
- Which harness you are running in (detection signals)
- Which command format your harness expects (markdown vs TOML)
- Where to install (global vs project-local)
- The ask-permission-before-copying protocol

The command files are bundled as assets under `assets/commands/` (markdown) and `assets/commands/gemini/` (TOML). They travel with the skill on install.

Do not install commands without asking the user first. Do not guess the harness — detect it, and if ambiguous, ask. Default to project-local scope; global changes are surprising.

## When in doubt

- **Can't find the wiki?** If no `wiki/SCHEMA.md` or `wiki/index.md` exists, the workspace has no memory substrate yet. Recommend `/loam::scaffolding-wiki <topic>`.
- **Two skills seem relevant?** Apply the conflict rules above. Process/work skills win before memory-maintenance skills.
- **A learning could go to wiki OR guidance?** Route through `loam-learning-from-session` — the classification is itself a feature of that skill.
- **Capturing a gotcha?** Route through `loam-learning-from-session`; only reusable, non-environment-specific `Trigger → Mistake → Fix` entries belong in the tripped skill.
- **Not sure if something is durable enough for memory?** If it's about how to work here (command, pattern, quirk), it's guidance. If it's about what is true here (fact, decision, architecture), it's wiki. If it's transient work-state, it's a checkpoint — use `loam-checkpointing`, not a memory write.
- **User wants /checkpoint or /resume shortcuts?** Read `references/commands-install.md` and follow the protocol. Ask permission before copying. Command files are in `assets/commands/`.
- **If no skill fits and the task is non-trivial, ask before guessing.**
