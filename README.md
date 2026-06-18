# loam

Packaged agent workflow skills under a single flat namespace, designed for agent coding harnesses compatible with Anthropic-style skill layout (Claude Code, OpenCode, Copilot CLI, Gemini CLI, and others via platform adapters).

Each skill is addressed as `loam::<gerund>[-<object>]` and invoked through the host harness's skill loader (slash-prefixed invocation tokens such as `/loam::planning`).

## Start here

**`/loam::using`** is the meta-skill router. Load it at session start — it routes to the right skill for any loam intent, explains the memory model, and lists the cross-cutting rules. If you're new to loam, read `loam-using` first.

## The memory model

**Memory** is the umbrella concept for everything loam captures. Three substrates exist:

| Substrate | What it is | Who maintains it |
|---|---|---|
| **wiki** | Durable Obsidian-friendly markdown notes under `wiki/`. Topic, entity, concept, analysis pages. | loam-memory group (most skills) |
| **guidance** | `AGENTS.md`, `CLAUDE.md`, `.claude.local.md` — prompt-context files for future agent sessions. | loam-memory group (`auditing-guidance`, `learning-from-session` guidance path) |
| **checkpoints** | Transient work-state under `wiki/checkpoints/`. Restart notes, not durable knowledge. | loam-work group (`checkpointing` writes, `resuming` reads) |

Talk about "memory" first. Use "wiki" only when distinguishing the markdown substrate from the guidance or checkpoint substrates. The wiki is one substrate of memory, not the whole thing.

## What loam is

loam is an **umbrella product**: a curated set of workflow skills for planning, research, memory maintenance, and substrate initialization. Skills are organized into three internal groups for maintainability and discoverability, but addressing is **flat** — the group never appears in the invocation token.

## Flat addressing

Every command is `/loam::<gerund>[-<object>]`. The internal group is never typed.

- `/loam::planning` — compile an approved spec into an execution-ready plan
- `/loam::writing-spec` — research a question and produce `specs/<slug>.md`
- `/loam::starting` — begin or resume execution of a plan
- `/loam::adding-to-memory` — ingest a source or conversation into memory (wiki substrate)
- `/loam::scaffolding-wiki` — instantiate a new wiki substrate scaffold
- ...full table below

## Internal groups (source-only)

Three groups organize skills in the source repo. On install via `npx skills`, everything is flattened into a per-agent directory keyed by skill name — the group folders are **not preserved on install**. Groups exist for source-repo discoverability and maintainability only.

| Group | Rationale |
|---|---|
| `loam-work` | Prospective / future-facing ops on work that hasn't happened yet. Plans, research, starting, resuming, checkpointing, configuring agent teams, amending plans. Reads memory for state but produces work artifacts (plans, task lists, checkpoints). |
| `loam-memory` | Settled / past-facing ops on knowledge that's already true. Maintains the wiki and guidance substrates of memory. Source of truth for everything loam captures. |
| `loam-ground` | Substrate initialization. Brings the wiki substrate (Obsidian vault, qmd indexing) into existence. Two skills, one-time foundational acts. |

The tense distinction (prospective vs settled) is the mental model, not the address axis — addressing is by domain. Tense survives as the help-output sort order.

## Naming rules (load-bearing)

1. **Flat address.** Every command is `loam::<gerund>[-<object>]`. The group name never appears.
2. **Gerund form.** `planning`, not `plan`. `amending`, not `amend`. (Anthropic skill-authoring guidance: gerund is preferred over bare imperative.)
3. **Object echoes the substrate, not the group.** `loam::amending-plan` (plan ≠ work) keeps the object; `loam::planning` drops it (work would echo).
4. **Object noun consistency within a group.** All `loam-memory` wiki skills use `memory` as the substrate noun: `adding-to-memory`, `querying-memory`, `linting-memory`, etc. Agent-md skills use `guidance`: `auditing-guidance`. `learning-from-session` is the one exception (input is the session, not the substrate).
5. **No spanning groups.** A skill that would touch two groups is two skills.

## On-disk name mapping

The CLI address `loam::<skill>` is the user-facing form and the frontmatter `name:` field value. The on-disk directory basename uses `loam-<skill>` with a hyphen — `npx skills` (vercel-labs/skills v1.5.11) sanitizes `::` to `-` on install for the directory name only. The installed SKILL.md keeps `name: loam::<skill>` verbatim.

| CLI address | Frontmatter `name` | Dir basename (source + install) |
|---|---|---|
| `loam::planning` | `loam::planning` | `loam-planning/` |
| `loam::amending-plan` | `loam::amending-plan` | `loam-amending-plan/` |
| `loam::querying-memory` | `loam::querying-memory` | `loam-querying-memory/` |
| `loam::scaffolding-wiki` | `loam::scaffolding-wiki` | `loam-scaffolding-wiki/` |

Every loam `name` is globally unique by virtue of the `loam-` / `loam::` prefix. `npx skills` accepts `::` in the frontmatter `name:` field, sanitizes to `-` for the installed directory name, and dedupes by name (silently drops duplicates).

Group folders are source-only — `npx skills add` flattens everything into per-agent dirs keyed by name. The catalog walk descends one extra level into `skills/<group>/<skill>/SKILL.md` with no manifest required. **Do not place a `SKILL.md` at the group-folder level** — it shadows everything nested below it.

## Install

With [vercel-labs/skills](https://github.com/vercel-labs/skills) (`npx skills`):

```bash
npx skills add scchearn/loam
```

This discovers all 18 skills (the router `loam-using` at the top level, plus the 17 skills under `loam-work/`, `loam-memory/`, `loam-ground/`) and installs them flat into your harness's per-agent skills directory (e.g. `~/.claude/skills/`, `~/.config/opencode/skills/`).

No `.claude-plugin/marketplace.json` is required. It is only needed if you also want Claude plugin-marketplace discovery.

## All 18 skills

| CLI address | Group | Source skill |
|---|---|---|
| `/loam::using` | (meta, top-level) | new — router for the namespace |
| `/loam::planning` | loam-work | do-plan |
| `/loam::writing-spec` | loam-work | do-research |
| `/loam::starting` | loam-work | do-start |
| `/loam::resuming` | loam-work | do-resume |
| `/loam::checkpointing` | loam-work | do-checkpoint |
| `/loam::configuring-agents` | loam-work | do-agents |
| `/loam::amending-plan` | loam-work | do-amend |
| `/loam::adding-to-memory` | loam-memory | do-wiki-add |
| `/loam::querying-memory` | loam-memory | do-wiki-query |
| `/loam::normalizing-memory` | loam-memory | do-wiki-align |
| `/loam::amending-memory` | loam-memory | do-wiki-amend |
| `/loam::linting-memory` | loam-memory | do-wiki-lint |
| `/loam::reviewing-memory` | loam-memory | do-wiki-review |
| `/loam::learning-from-session` | loam-memory | do-wiki-learnings + revise-agent-md (merged) |
| `/loam::auditing-guidance` | loam-memory | agent-md-improver |
| `/loam::scaffolding-wiki` | loam-ground | do-wiki-build |
| `/loam::initializing-vault` | loam-ground | setup-obsidian-vault |

18 source skills → 18 loam skills. One merge (`learning-from-session`), zero splits, five renames, one new meta-skill (`loam-using`).

### Notes on the merge

`/loam::learning-from-session` absorbs two source skills:

- **`do-wiki-learnings`** — proposal-first review of session learnings destined for durable wiki pages (topic, entity, concept, analysis).
- **`revise-agent-md`** — concise one-liner updates to `AGENTS.md`, `CLAUDE.md`, `.claude.local.md`.

The merged skill is a **router** that classifies each learning into one of the two writing paths. The classification is itself a feature: the right surface depends on who consumes the learning and what shape it takes. Both writing paths survive intact.

## Relationship to `agent-skills`

The legacy source repo [scchearn/agent-skills](https://github.com/scchearn/agent-skills) remains the source of the skills that were packaged here. `loam` is the packaged, addressable product. Sunset of `agent-skills` is deferred to a separate task.

## License

MIT — see [LICENSE](./LICENSE).