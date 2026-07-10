---
name: loam::using
description: "The always-on protocol for the loam skill namespace. Use at session start and whenever a loam task appears. Routes goals and other loam work, explains the memory model (memory = umbrella; wiki, guidance, and checkpoints are substrates), and lists cross-cutting rules. This is a routing/meta skill — delegate to a specific loam skill rather than performing work itself."
metadata:
  version: "1.7.1"
  author: scchearn
---

# Using loam

This is the router for the loam skill namespace. It tells you which loam skill to invoke for a given intent and the rules that apply across all of them. It does not perform work itself — when you recognize the matching skill, invoke it via the host harness's skill loader.

## Non-negotiables

1. **Invoke the matching skill before any loam action** — planning, researching, starting, resuming, checkpointing, debating, amending a plan, or any memory/guidance operation. This document only routes; the skill body has the rules. Err on the side of invoking, even if you "already read it" this session.
2. **Memory first, substrate second.** Say "memory" by default; use "wiki" only to distinguish the markdown substrate from guidance or checkpoints. The wiki is one substrate of memory, not the whole thing.
3. **Agent-owned memory writes.** The agent writes, corrects, routes, and archives memory without pre-approval. A human flagging a page as wrong/stale triggers the same correction flow as an agent-found contradiction.
4. **Domain-router precedence.** In a workspace with loam artifacts (`wiki/`, `goals/`, `specs/`, `plans/`), `loam::using` routes memory, goals, specs, plans, checkpoints, and agent debates before generic skill routers. Non-loam work is unaffected.

## The memory model

**Memory** is the umbrella for everything loam captures. Three substrates:

| Substrate | What it is | Who maintains it |
|---|---|---|
| **wiki** | Durable Obsidian-friendly markdown notes under `wiki/` (topic, entity, concept, analysis pages). What `qmd` indexes. | loam-memory group |
| **guidance** | `AGENTS.md` (canonical); `CLAUDE.md` is a thin `@AGENTS.md` shim; `.claude.local.md` for personal overrides. What harnesses load as prompt context. | `auditing-guidance`, `learning-from-session` |
| **checkpoints** | Transient work-state under `wiki/checkpoints/`. Restart notes, not durable knowledge — they never touch `index.md`/`log.md`. | `checkpointing` writes, `resuming` reads |

Goals (`goals/<slug>.md`) are optional workflow artifacts, not a fourth substrate. They own intent, validation, lifecycle, and review history. `loam::setting-goals` maintains them; downstream skills maintain traceability links only.

## Durable-memory admission rubric

Use before creating a wiki page; page-creating skills reference this instead of copying it. A claim earns a page only if it passes all three:

- **R1 Reusable** — a future session on a different task would plausibly need it.
- **R2 About the project/domain** — codebase, architecture, decisions, conventions, dependencies, or durable external facts; not the conversation, the agent, or transient user state.
- **R3 Costly to reconstruct & re-checkable** — re-deriving from a live source (code, config, task list) costs more than the page costs to maintain. If one command or file read gets it back, no page. Where an external source exists, name it so freshness can re-validate. Decisions and rationale are self-sourcing.

Disqualifiers override: **D1 ephemeral** (build state, current branch, "today I ran X" → operational report); **D2 duplicate** (an existing page covers it → amend it). Tiebreaker: admit if not reconstructable from a live source, else discard. `wiki/.archive/` holds only was-durable-now-superseded content; never-durable material is routed elsewhere, never archived.

## Routing matrix

| Material | Destination | Skill |
|---|---|---|
| Reusable project/domain fact (passes rubric) | durable wiki page | `adding-to-memory`, self-correction |
| Agent-behavior convention/command/gotcha | `AGENTS.md` | `learning-from-session` (guidance path) |
| Session state for resume/handoff | `wiki/checkpoints/<slug>.md` | `checkpointing` |
| Per-task context on a unit of work | task annotation / plan file | `planning`, `starting` |
| Build output, one-off, unverifiable, rubric failure | discard (optional `log.md` audit line) | none |
| Broad ambition with verifiable outcome | `goals/<slug>.md` | `setting-goals` |

## Correction and freshness

- **Self-correction:** on reading memory that contradicts current evidence, move the superseded durable page to `wiki/.archive/` with an archival header, write the correction in place, append a `self-correct` line to `log.md`, and continue the task.
- **Human-flag:** treat "this page is wrong/stale" as a correction trigger, not an approval request — same archive + rewrite + log flow.
- **Freshness:** validation uses existing `updated_at` plus lint-time evidence (no freshness frontmatter). A page is **confirmed** (bump `updated_at`), **corrected** (archive + rewrite + log), or **demoted** (archive, "no longer applies"). `loam::linting-memory` owns the triggers and flags 90-day-old pages citing volatile surfaces.

## Red flags: "I'll just..." means you're skipping a skill

If you catch yourself rationalizing a shortcut, invoke the skill instead — even if you read it earlier this session. Highest-risk cases:

- "I'll just write this to the wiki" → `adding-to-memory` (apply the rubric first).
- "I'll just plan it, it's simple" → `writing-spec` then `planning` (specs are required first).
- "I'll just edit this plan/checkpoint/`AGENTS.md` inline" → `amending-plan` / `checkpointing` / `learning-from-session`. Never edit these substrates by hand.

## Decision graph

```
"I want to..."
├─ start something new
│  ├─ set a verifiable goal ─────────── /loam::setting-goals
│  ├─ research a question ──────────── /loam::writing-spec
│  ├─ plan approved work ────────────── /loam::planning
│  └─ debate / reach consensus ────── /loam::configuring-agents
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
├─ work with goals
│  ├─ create or review a goal ────────── /loam::setting-goals
│  ├─ pause, reactivate, achieve ────── /loam::setting-goals
│  └─ change what a goal means ──────── /loam::setting-goals
└─ set up the substrate
   ├─ scaffold the wiki ─────────────── /loam::scaffolding-wiki
   └─ init Obsidian vault ───────────── /loam::initializing-vault
```

## Disambiguation (two skills seem to fit)

- **Process/current-work skills win before memory-maintenance skills.** Mid-execution and you spot a memory issue? Finish or checkpoint the current step first, then invoke the memory skill.
- **Amend vs lint:** specific wrong claim → `amending-memory`; whole-graph check → `linting-memory`.
- **Query vs review:** want an answer → `querying-memory`; want open gaps → `reviewing-memory`.
- **Add vs learn:** have a source → `adding-to-memory`; session produced insight → `learning-from-session` (it classifies wiki vs guidance).
- **Goal vs spec:** broad ambition with verifiable outcome → `setting-goals`; research a question → `writing-spec`. A goal may produce multiple specs; a spec may optionally record goal provenance.
- **Goal vs debate:** explicit debate, conference, or consensus intent wins → `configuring-agents`; use `setting-goals` when the user wants the goal artifact created or changed.
- **Durable enough?** How-to-work-here (command, pattern, quirk) = guidance; what-is-true-here (fact, decision, architecture) = wiki; transient work-state = checkpoint; operational lifecycle = goal. When still unsure, ask before guessing.

## loamstate hints

`loamstate.sh [--fast] <workspace-root>` may emit advisory `hints[]` with `maintenance` or `workflow` signals. Hints point at the relevant loam skill; they never authorize bypassing that skill or auto-running commands. Missing hints mean "no cheap signal," not "nothing to do." For schema and kinds, inspect `loamstate.sh` output or the script header.

### Reuse before probing

The injected `## Workspace state` block is a compact `loamstate --fast` result. Reuse it when its `Workspace` matches the current workspace, it contains the fields the active skill needs, and no later operation changed the relevant wiki, qmd, checkpoint, or metadata state. Do not rerun `loamstate` merely to rediscover the same state.

Run a fresh `--fast` probe when the block is absent, belongs to another workspace, lacks required fields, or relevant state changed after injection. Run the full probe only when omitted checks such as date drift or `code_ingest_pending` are required. When a skill performs a newer authoritative check itself, use that result instead of rerunning `loamstate`.

The injected block uses these stable line forms; checkpoint and signal lines are optional:

```text
Workspace: <absolute workspace> · Probe: loamstate --fast
Wiki: <absolute wiki root> · qmd: <ready|not installed> [· collection: <name>]
Wiki: none
Checkpoints: <count> (latest: "<title>" — <captured_at>)
Signals:
- <kind> — <message> [(<evidence key>: <value>, ...)] [→ <command>]
```

The PowerShell twin is fast-equivalent and currently omits full-only checks. If bash is unavailable when a full-only signal is required, treat that signal as unknown and use the active skill's conservative fallback.

### Consuming hints

After completing the primary task of any loam skill that consumed injected or freshly probed loamstate, scan its hints and surface unsatisfied hints to the user as suggested next actions. This is mandatory — hints that go unread are signals wasted.

For each hint, emit one line in this form:

```text
loamstate also flagged: <kind> — <message> (<evidence summary>)
Suggested next: <command>
```

Rules:

- **Suppress satisfied hints.** Skip any hint whose `kind` your skill body names as one it satisfies. A skill may satisfy more than one (e.g. `linting-memory` satisfies `memory_lint_stale`, `date_drift_pending`, `log_rotation_due`, and `legacy_structure_pending`); suppress all of them.
- **Only surface hints with a non-null `command`.** Hints without a command (e.g. `retrieval_not_ready`) are informational; mention them only if the user asks for state.
- **Do not auto-run the suggested skill.** Hints are advisory; the user decides whether to act. End your turn or hand back to the user after surfacing.
- **Empty `hints[]` → say nothing.** Do not invent suggestions or pad the report.
- **`evidence` summary.** When the hint's `evidence` object carries a count (e.g. `pending_count`, `drift_count`, `log_lines`, `age_minutes`), include it parenthetically: `code_ingest_pending — 3 source file(s) new or changed (pending_count: 3)`. Omit the parenthetical when `evidence` is empty.

This makes `loamstate` a closed loop: the script signals, the skill acts, and the next-most-pressing signal surfaces to the user instead of dying in the JSON.

## qmd and code-graph discovery

When `loamstate` reports `qmd_ready: true` and `collection: <name>`, prefer qmd over Glob/Grep for content discovery in the wiki.

- **Lookup**: `qmd search "<keywords>" --files -n 8 -c <collection>`
- **Comparison/synthesis**: `qmd query "<natural-language question>" --files -n 8 -c <collection>`
- Strip the `qmd://<collection>/` prefix from paths to get the relative wiki path (e.g. `code/validate-token.md`)
- Verify candidates by Reading the actual wiki files — qmd discovers paths, Read confirms content
- Ignore `.archive/` paths (historical, not active memory)
- After wiki writes, refresh: `qmd update -c <collection> 2>/dev/null`
- On qmd degradation (command fails or returns stale/noisy output): fall back to Grep/Glob for the rest of the session

### Code graph precedence (all code discovery)

When `wiki/code/` exists and qmd is ready, prefer code pages over raw source for **all** code discovery — orientation AND exact-pattern search. The code graph maps which modules exist and where symbols live before you scan raw bytes.

1. qmd first: `qmd search "<symbol or topic>" --files -n 8 -c <collection>`, Read the returned `code/<slug>.md` pages for the compressed map
2. Source pattern search (call sites, symbol usages): prefer **ast-grep** (`ast_grep_search` MCP tool, or `ast-grep`/`sg` CLI) — AST-aware, skips comments/strings, handles formatting. Scope to the files/modules the graph flagged
3. Fall back to `rg`/`grep` on raw source when `ast-grep` is unavailable (probe once; on failure use `rg`/`grep`)
4. Skip the code-graph-first step only if `code_ingest_pending` hint is set and flagged files overlap your target — then verify against raw source directly

`grep`/`Glob` remain correct for markdown/prose structural checks (inventory, orphans, wikilinks) and as the raw-source fallback.

**Wrong assumption to reject:** "qmd is only for memory; grep is correct for concrete code call sites." qmd indexes `wiki/code/` summaries, so qmd-first applies to code too. After qmd, `ast-grep` (not `grep`) is the source-pattern tool; `grep` only wins for non-code and the ast-grep-unavailable fallback.

## Installing slash commands

To install `/checkpoint` and `/resume` shortcuts, read `references/commands-install.md` and follow it. Detect the harness (don't guess), default to project-local scope, and ask before copying.
