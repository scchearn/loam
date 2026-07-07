---
name: loam::using
description: "The always-on protocol for the loam skill namespace. Use at session start and whenever a loam task appears. Routes to the right skill, explains the memory model (memory = umbrella; wiki, guidance, and checkpoints are substrates), and lists the cross-cutting rules. This is a routing/meta skill — delegate to a specific loam skill rather than performing work itself."
metadata:
  version: "1.5.0"
  author: scchearn
---

# Using loam

This is the router for the loam skill namespace. It tells you which loam skill to invoke for a given intent and the rules that apply across all of them. It does not perform work itself — when you recognize the matching skill, invoke it via the host harness's skill loader.

## Non-negotiables

1. **Invoke the matching skill before any loam action** — planning, researching, starting, resuming, checkpointing, debating, amending a plan, or any memory/guidance operation. This document only routes; the skill body has the rules. Err on the side of invoking, even if you "already read it" this session.
2. **Memory first, substrate second.** Say "memory" by default; use "wiki" only to distinguish the markdown substrate from guidance or checkpoints. The wiki is one substrate of memory, not the whole thing.
3. **Agent-owned memory writes.** The agent writes, corrects, routes, and archives memory without pre-approval. A human flagging a page as wrong/stale triggers the same correction flow as an agent-found contradiction.
4. **Domain-router precedence.** In a workspace with a loam substrate (`wiki/`, `specs/`, `plans/`), `loam::using` is the domain router for loam-shaped work: memory, specs, plans, checkpoints, and agent debates. Route those tasks through it rather than any generic skill router. Non-loam work is unaffected.

## The memory model

**Memory** is the umbrella for everything loam captures. Three substrates:

| Substrate | What it is | Who maintains it |
|---|---|---|
| **wiki** | Durable Obsidian-friendly markdown notes under `wiki/` (topic, entity, concept, analysis pages). What `qmd` indexes. | loam-memory group |
| **guidance** | `AGENTS.md` (canonical); `CLAUDE.md` is a thin `@AGENTS.md` shim; `.claude.local.md` for personal overrides. What harnesses load as prompt context. | `auditing-guidance`, `learning-from-session` |
| **checkpoints** | Transient work-state under `wiki/checkpoints/`. Restart notes, not durable knowledge — they never touch `index.md`/`log.md`. | `checkpointing` writes, `resuming` reads |

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

## Disambiguation (two skills seem to fit)

- **Process/current-work skills win before memory-maintenance skills.** Mid-execution and you spot a memory issue? Finish or checkpoint the current step first, then invoke the memory skill.
- **Amend vs lint:** specific wrong claim → `amending-memory`; whole-graph check → `linting-memory`.
- **Query vs review:** want an answer → `querying-memory`; want open gaps → `reviewing-memory`.
- **Add vs learn:** have a source → `adding-to-memory`; session produced insight → `learning-from-session` (it classifies wiki vs guidance).
- **Durable enough?** How-to-work-here (command, pattern, quirk) = guidance; what-is-true-here (fact, decision, architecture) = wiki; transient work-state = checkpoint. When still unsure, ask before guessing.

## loamstate hints

`loamstate.sh <workspace-root>` may emit advisory `hints[]` with `maintenance` or `workflow` signals. Hints point at the relevant loam skill; they never authorize bypassing that skill or auto-running commands. Missing hints mean "no cheap signal," not "nothing to do." For schema and kinds, inspect `loamstate.sh` output or the script header.

## Installing slash commands

To install `/checkpoint` and `/resume` shortcuts, read `references/commands-install.md` and follow it. Detect the harness (don't guess), default to project-local scope, and ask before copying.
