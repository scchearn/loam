---
name: loam::using
description: "The always-on protocol for the loam skill namespace. Use at session start and whenever a loam task appears. Routes goals and other loam work, explains the memory model (memory = umbrella; wiki, guidance, and checkpoints are substrates), and lists cross-cutting rules. This is a routing/meta skill — delegate to a specific loam skill rather than performing work itself."
metadata:
  version: "2.0.0"
  author: scchearn
---

# Using loam

This is the router for the loam skill namespace: which skill to invoke for a given intent, and the rules that apply across all of them. It performs no work itself. Protocol detail lives in `references/` under the injected `Skill root:`; read a reference when its trigger applies.

## Non-negotiables

1. **Invoke the matching skill before any loam action.** This document only routes; the skill body has the rules. Err on the side of invoking, even if you read it earlier this session.
2. **Memory first.** "Memory" is the umbrella; "wiki" names only the markdown substrate. Consult memory before raw source or recall (see Discovery order).
3. **Agent-owned memory writes.** Write, correct, route, and archive memory without pre-approval. A human flagging a page as wrong triggers the same correction flow as an agent-found contradiction.
4. **Domain-router precedence.** In a workspace with `wiki/`, `goals/`, `specs/`, or `plans/`, this router wins over generic skill routers for memory, goals, specs, plans, checkpoints, and debates.
5. **Global install only.** Skills live under `<home>/.agents/skills/`; the runtime is the injected `Native runtime command:` and nothing else. Never install, probe for, or run a project-local loam, and never `which loam`/`which hcom` — the injected state lines are the availability answer.

## Memory model

- **wiki** — durable Obsidian-friendly notes under `wiki/`; what `qmd` indexes. Maintained by the loam-memory skills.
- **guidance** — `AGENTS.md` (canonical; `CLAUDE.md` is an `@AGENTS.md` shim; `.claude.local.md` for personal overrides). Maintained by `auditing-guidance`, `learning-from-session`.
- **checkpoints** — transient work-state under `wiki/checkpoints/`; never touch `index.md`/`log.md`. `checkpointing` writes, `resuming` reads.
- **goals** (`goals/<slug>.md`) are workflow artifacts, not a substrate. `setting-goals` owns them; other skills keep traceability links only.

## Where material goes

| Material | Destination | Skill |
|---|---|---|
| Reusable project/domain fact | wiki page | `adding-to-memory` |
| How-to-work-here convention, command, gotcha | `AGENTS.md` | `learning-from-session` |
| Session state for resume/handoff | `wiki/checkpoints/` | `checkpointing` |
| Per-task context | plan file / task annotation | `planning`, `starting` |
| Broad ambition with verifiable outcome | `goals/<slug>.md` | `setting-goals` |
| Build output, one-off, unverifiable | discard | none |

A wiki page must be reusable, about the project/domain, and costly to reconstruct; ephemeral or duplicate claims fail. Full rubric, correction, and freshness rules: `references/memory-lifecycle.md` — read before creating, correcting, or demoting a page.

## Routing

- **Start:** set a goal → `setting-goals` · research a question → `writing-spec` · plan approved work → `planning` · debate/consensus → `configuring-agents`
- **Execute:** begin a plan → `starting` · pause/hand off → `checkpointing` · resume → `resuming` · change an in-flight plan → `amending-plan`
- **Memory:** add a source → `adding-to-memory` · ingest a codebase → `ingesting-codebase` · sync code-graph drift → `syncing-code-graph` · ask a question → `querying-memory` · fix a wrong claim → `amending-memory` · health-check → `linting-memory` · normalize a messy corpus → `normalizing-memory` · see what's unresolved → `reviewing-memory` · capture session learnings → `learning-from-session` · audit guidance → `auditing-guidance`
- **Goals:** create, review, pause, reactivate, achieve, or redefine → `setting-goals`
- **Substrate:** scaffold the wiki → `scaffolding-wiki` · init an Obsidian vault → `initializing-vault`
- **Shortcuts:** to install `/checkpoint` and `/resume`, read `references/commands-install.md`; detect the harness, default to project-local scope, ask before copying.

## When two skills fit

- Current-work skills before memory-maintenance skills: finish or checkpoint the step, then fix the memory issue.
- Wrong claim → `amending-memory`; whole-graph check → `linting-memory`.
- Want an answer → `querying-memory`; want open gaps → `reviewing-memory`.
- Have a source → `adding-to-memory`; session produced insight → `learning-from-session`.
- Verifiable ambition → `setting-goals`; research question → `writing-spec` (a goal may yield several specs; a spec may record goal provenance). Explicit debate intent → `configuring-agents`.
- Still unsure whether it's guidance, wiki, checkpoint, or goal? Ask before guessing.

## Red flags — "I'll just…" means you're skipping a skill

- "…write this to the wiki" → `adding-to-memory`.
- "…plan it, it's simple" → `writing-spec` then `planning`; specs are required.
- "…edit this plan/checkpoint/`AGENTS.md` inline" → `amending-plan` / `checkpointing` / `learning-from-session`. Never by hand.
- "…answer from memory" or "…grep the repo" → `querying-memory`, in the discovery order below.

## Discovery order

1. **Wiki via qmd** when the state block says qmd is ready (`qmd search "<terms>" --files -n 8 -c <collection>`; `qmd query` for natural-language questions); Grep/Glob otherwise. Read the files qmd returns — it finds paths, Read confirms content. Ignore `.archive/`.
2. **hcom transcripts** when the state block says `hcom: ready` — raw and often newer than the wiki. Read the exchange before citing. `not installed` skips silently.
3. **Raw source and recall**, last. For code, prefer `wiki/code/` pages, then `ast-grep`, then `rg`.

Transcript and wiki disagree → report both with dates, route to `amending-memory`. Full qmd, code-graph, and refresh protocol: `references/discovery.md` — read when a skill searches or writes the wiki.

## Workspace state

The injected `## Workspace state` block (workspace, wiki root, qmd, hcom, checkpoints, hints) is authoritative for this turn. Reuse it; rerun `<native-runtime-command> state --fast "$(pwd)"` only when it is absent, for another workspace, or stale after a write. The `## Federation` line is likewise authoritative; never probe the broker. If the runtime reports unavailable, run `npx @scchearn/loam install` once, retry once, then report and stop.

**Hints:** after any loam skill completes, list each unsatisfied hint that carries a command as `- [loam:hint] <kind> — <message> [→ <command>]`, then hand back. Never auto-run a hint; say nothing for none. Hint kinds, native subcommands, background ingestion, and federation operations: `references/runtime.md` — read when a skill needs a native command or must satisfy a hint.
