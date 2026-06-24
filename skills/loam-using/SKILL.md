---
name: loam::using
description: "The always-on protocol for the loam skill namespace. Use at session start and whenever a loam task appears. Routes to the right skill, explains the memory model (memory = umbrella; wiki, guidance, and checkpoints are substrates), and lists the cross-cutting rules. This is a routing/meta skill — delegate to a specific loam skill rather than performing work itself."
metadata:
  version: "1.1.0"
  author: scchearn
---

# Using loam

This is the router for the loam skill namespace. It does not perform work itself. It tells you which loam skill to invoke for a given intent, and the rules that apply across all of them.

**This is a routing/meta skill.** When you recognize the matching skill, invoke it via the host harness's skill loader. Do not perform the work here.

## Non-negotiables

Three rules with no exceptions. Violating any produces work that looks right but drifts from the loam model.

1. **Invoke the relevant skill before any loam action.** Not just tool calls. Before planning, researching, starting, resuming, checkpointing, configuring agents, amending a plan, adding to memory, querying memory, amending memory, linting memory, normalizing memory, reviewing memory, capturing learnings, auditing guidance, scaffolding the wiki, or initializing a vault: invoke the matching skill. This document is a router. The skill body has the actual rules. The responsibility is yours on everything else. Err on the side of invoking.
2. **Memory first, substrate second.** Talk about "memory" first. Use "wiki" only when distinguishing the markdown substrate from the guidance or checkpoint substrates. Never say "the wiki" when you mean "memory" — the wiki is one substrate of memory, not the whole thing.
3. **Proposal-first on memory writes.** Never edit memory without showing a proposal and getting explicit user approval. Applies to every skill that writes: `loam-adding-to-memory`, `loam-amending-memory`, `loam-learning-from-session`, `loam-normalizing-memory`, `loam-linting-memory` (for its safe-local-fixes path). Direct edits without a proposal shown to the user are forbidden.

## The memory model

**Memory** is the umbrella concept for everything loam captures. Three substrates exist:

| Substrate | What it is | Who maintains it |
|---|---|---|
| **wiki** | Durable Obsidian-friendly markdown notes under `wiki/`. Topic, entity, concept, analysis pages. | loam-memory group (most skills) |
| **guidance** | `AGENTS.md` is the canonical guidance file. `CLAUDE.md` is a thin import shim (`@AGENTS.md` only). `.claude.local.md` for personal overrides. | loam-memory group (`auditing-guidance`, `learning-from-session` guidance path) |
| **checkpoints** | Transient work-state under `wiki/checkpoints/`. Restart notes, not durable knowledge. | loam-work group (`checkpointing` writes, `resuming` reads) |

The wiki substrate is what `qmd` indexes. The guidance substrate is what harnesses load as prompt context. Checkpoints are work-state, not knowledge — they refuse to touch `index.md` or `log.md` and should not become durable claims.

**Talk about "memory" first. Use "wiki" only when the substrate distinction matters.** A skill that operates on the markdown notes works on the wiki substrate of memory. A skill that edits `AGENTS.md` works on the guidance substrate of memory. Both are memory.

## Red flags: thoughts that mean STOP and invoke

These rationalizations cause skills to be skipped. If you catch yourself thinking any of them, invoke the relevant skill **even if you "already read it" earlier in the session**.

| Thought | Action |
|---|---|
| "I'll just write this to the wiki" | Invoke `loam-adding-to-memory`. Memory writes are proposal-first. |
| "This is simple, I'll just plan it" | Invoke `loam-writing-spec`. Specs are required before `loam-planning`. |
| "I'll just edit this plan directly" | Invoke `loam-amending-plan`. Plan changes cascade; the skill walks the impact. |
| "I'll skip the proposal, just write it" | STOP. All memory writes are proposal-first. No exceptions. |
| "The wiki is out of date, I'll fix it inline" | Invoke `loam-amending-memory`. Corrections are proposal-first. |
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
4. **Propose before memory writes.** Every memory write is proposal-first. Show the user what will change, get approval, then apply.
5. **Update plans / checkpoints / guidance only via the matching skill.** Don't edit a plan inline — use `loam-amending-plan`. Don't write a checkpoint inline — use `loam-checkpointing`. Don't edit `AGENTS.md` inline — use `loam-learning-from-session` or `loam-auditing-guidance`.

### Conflict rules (when two skills seem relevant)

- **Process / current-work skills win before memory-maintenance skills.** If you're mid-execution and discover a memory issue, finish the current-work step first (or checkpoint it), then invoke the memory skill.
- **Memory writes are always proposal-first.** Even if the current-work skill produces a learning, route it through `loam-learning-from-session` rather than editing memory inline.
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
│  └─ set up agent team ─────────────── /loam::configuring-agents
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

## Compact skill reference

One line per skill. The decision graph is the primary router; this is for quick lookup.

- `loam-writing-spec` — research a question, produce `specs/<slug>.md`
- `loam-planning` — compile approved spec into execution-ready plan
- `loam-starting` — begin or resume execution of a plan
- `loam-resuming` — resume from checkpoint notes
- `loam-checkpointing` — write a resumable checkpoint before pausing
- `loam-configuring-agents` — plan or reuse an agent team config (hcom backend; general advice when hcom unavailable)
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
- **Not sure if something is durable enough for memory?** If it's about how to work here (command, pattern, quirk), it's guidance. If it's about what is true here (fact, decision, architecture), it's wiki. If it's transient work-state, it's a checkpoint — use `loam-checkpointing`, not a memory write.
- **User wants /checkpoint or /resume shortcuts?** Read `references/commands-install.md` and follow the protocol. Ask permission before copying. Command files are in `assets/commands/`.
- **If no skill fits and the task is non-trivial, ask before guessing.**