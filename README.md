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

18 skills, grouped by what they're for:

### Planning and execution

- **Planning** — turn an approved spec into an execution-ready plan
- **Writing-spec** — research a question and produce a spec
- **Starting** — begin executing a plan
- **Resuming** — pick up work after a pause, using saved checkpoints
- **Checkpointing** — save a restart point before pausing or handing off
- **Amending-plan** — update an in-flight plan when scope changes
- **Configuring-agents** — set up a team of AI agents for a task

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

Agents can install `/checkpoint` and `/resume` slash-command shortcuts — see `docs/commands-install.md`.

## Usage

Skills are invoked through your agent's skill loader. For example,
to research a question and produce a spec:

```
/loam::writing-spec <your question>
```

Or to add a document to the knowledge base:

```
/loam::adding-to-memory <path-to-file>
```

The agent loads the skill, follows its workflow, and produces the
artifact (a spec, a plan, a memory entry, etc.).

## License

MIT — see [LICENSE](./LICENSE).