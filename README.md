# loam

loam is a collection of workflow skills for AI coding agents.
It gives an agent a structured way to plan work, research questions,
execute plans, and maintain a persistent knowledge base — so
sessions build on each other instead of starting from scratch.

## Install

```bash
npx skills add scchearn/loam
```

Skills install into your agent's skills directory
(`~/.claude/skills/`, `~/.config/opencode/skills/`, etc.)
and are available the next time you start a session.

## What you get

20 skills, grouped by what they're for:

### Planning and execution

- **Planning** — turn an approved spec into an execution-ready plan
- **Writing-spec** — research a question and produce a spec
- **Starting** — begin executing a plan
- **Resuming** — pick up work after a pause, using saved checkpoints
- **Checkpointing** — save a restart point before pausing or handing off
- **Amending-plan** — update an in-flight plan when scope changes
- **Configuring-agents** — run a structured debate or conference between agents to reach consensus on a goal

### Memory

- **Adding-to-memory** — save a source or document into the knowledge base
- **Querying-memory** — ask the knowledge base a question
- **Amending-memory** — fix a wrong or stale claim in the knowledge base
- **Linting-memory** — health-check the knowledge base for orphans, broken links, drift
- **Normalizing-memory** — retrofit structure onto a messy notes corpus
- **Reviewing-memory** — surface what's unresolved or gaps in the knowledge base
- **Learning-from-session** — capture learnings from a session into memory or agent guidance
- **Auditing-guidance** — review and improve AGENTS.md / CLAUDE.md files

### Setup

- **Scaffolding-wiki** — set up the knowledge base structure
- **Initializing-vault** — configure an Obsidian vault

## How it works

loam skills maintain a persistent **memory** layer with three parts:

- **Wiki notes** — durable knowledge about the project (Obsidian-friendly markdown)
- **Agent guidance** — `AGENTS.md`, `CLAUDE.md` files that tell future agent sessions how to work here
- **Checkpoints** — transient restart notes for pausing and resuming work

When you start a session, the agent loads **Using** — a router skill that
recognizes what you're trying to do and invokes the right skill for it.
You don't need to memorize the list above; the agent routes itself.

loam works fully on its own. If your wiki grows large, [qmd](https://github.com/tobilu/qmd) (`npm install -g @tobilu/qmd`) speeds up search across memory — the skills detect it automatically and fall back to built-in search when it's absent.

Agents can install `/checkpoint` and `/resume` slash-command shortcuts — the `loam::using` skill bundles the command files and an install reference.

## Ways to use loam

You don't need to memorize skill names. Say what you want in plain
language — the **Using** router matches it to the right skill.

- "Write a spec for what we discussed" — researches and produces a spec
- "Plan the work from that spec" — turns an approved spec into a plan
- "Run the plan" — begins executing, task by task
- "Have agents debate this decision" — runs an approval-gated consensus debate
- "Stopping work" / "I need to step away" — saves a restart checkpoint
- "Resume where I left off" — picks up from the last checkpoint
- "The scope changed, update the plan" — walks the impact, proposes plan changes
- "Add to memory" / "capture all into loam" — ingests a source or conversation
- "What does memory say about X?" — answers from the knowledge base
- "Memory is wrong about X" — corrects stale claims (proposal-first)
- "What is unresolved" — surfaces open questions and gaps in memory
- "Health-check the wiki" — finds orphans, broken links, drift
- "This notes corpus is messy" — retrofits structure onto existing notes
- "Save what we learned this session" — routes learnings to wiki or AGENTS.md
- "Audit the AGENTS.md" — scores, prunes stale content, adds missing commands
- "Set up loam" / "scaffold a knowledge base" — creates the wiki structure

## License

MIT — see [LICENSE](./LICENSE).
