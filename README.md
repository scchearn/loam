# loam

<p align="center">
  <img src="loam.svg" alt="loam" width="120">
</p>

loam is a collection of workflow skills for AI coding agents.
It gives an agent a structured way to plan work, research questions,
execute plans, and maintain a persistent knowledge base — so
sessions build on each other instead of starting from scratch.

## Install

### Step 1 — all harnesses (skill discovery)

```bash
npx skills add scchearn/loam
```

Skills install into your agent's skills directory
(`~/.agents/skills/loam-*`, symlinked from `~/.claude/skills/`,
`~/.config/opencode/skills/`, etc.) and are available the next time you
start a session. This is the single source of truth for skill content —
the plugin bootstrap (step 2) reads from here.

### Step 2 — injection harnesses only (optional, auto-injection)

On OpenCode, Claude Code, and Cursor, you can optionally register loam as a
plugin to auto-inject the `loam::using` router into every session start. The
plugin reads skill content from the `npx skills` install — no second copy.

### OpenCode (auto-injection)

Register loam as an opencode plugin in your `opencode.json`:

```json
{ "plugin": ["loam@git+https://github.com/scchearn/loam.git"] }
```

Restart OpenCode. The plugin injects `loam::using` into the first user
message of each session. See [`.opencode/INSTALL.md`](./.opencode/INSTALL.md)
for details.

### Claude Code (auto-injection)

Register the loam marketplace and install the plugin:

```bash
/plugin marketplace add scchearn/loam
/plugin install loam@loam
```

The `hooks/hooks.json` SessionStart hook injects `loam::using` at session start
(startup, clear, compact).

### Cursor (auto-injection)

Install loam as a Cursor plugin. The `hooks/hooks-cursor.json` sessionStart hook
injects `loam::using` at session start. `.cursor-plugin/plugin.json` drives
plugin discovery.

### Codex (skill discovery only)

Codex has no session-start injection. Clone and symlink for skill discovery:

```bash
git clone https://github.com/scchearn/loam.git ~/.codex/loam
mkdir -p ~/.agents/skills
ln -s ~/.codex/loam/skills ~/.agents/skills/loam
```

See [`.codex/INSTALL.md`](./.codex/INSTALL.md) for details. Invoke
`loam::using` on demand at session start.

### Antigravity (skill discovery only)

```bash
npx skills add scchearn/loam
```

No auto-injection. Skills are discovered via `~/.agents/skills/loam-*`;
invoke `loam::using` on demand.

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

## Skill metrics

Each skill's `description` and `SKILL.md` body have size budgets defined by the
[agentskills.io spec](https://agentskills.io/specification): descriptions cap at
1,024 characters (~100 tokens, loaded into context at session startup), and skill
bodies are recommended to stay under 5,000 tokens and 500 lines. The table below
shows where each loam skill sits against those budgets. Token counts are real
counts from [tiktoken](https://github.com/openai/tiktoken)'s `cl100k_base` encoding,
not char-divided-by-four estimates — `description` is what the agent pays for at
startup, while the body is only loaded when the skill activates.

<!-- BEGIN skill-metrics -->
<!-- Auto-generated by bin/skill-metrics.sh via tiktoken cl100k_base. Do not edit by hand; run `bin/skill-metrics.sh --update` to refresh. -->

| Skill | Desc chars (max 1,024) | Desc tokens (~100) | Body lines (max 500) | Body tokens (< 5,000) |
|-------|---:|---:|---:|---:|
| loam::initializing-vault | 206 | 51 | 9 | 73 |
| loam::scaffolding-wiki | 379 | 78 | 196 | 2,147 |
| loam::adding-to-memory | 514 | 103 | 215 | 2,197 |
| loam::amending-memory | 505 | 114 | 180 | 1,888 |
| loam::auditing-guidance | 410 | 85 | 252 | 2,528 |
| loam::ingesting-codebase | 329 | 76 | 251 | 3,048 |
| loam::learning-from-session | 432 | 88 | 361 | 4,073 |
| loam::linting-memory | 423 | 93 | 274 | 4,384 |
| loam::normalizing-memory | 421 | 94 | 260 | 2,634 |
| loam::querying-memory | 463 | 91 | 173 | 1,539 |
| loam::reviewing-memory | 459 | 104 | 136 | 1,664 |
| loam::syncing-code-graph | 363 | 84 | 221 | 2,944 |
| loam::using | 365 | 76 | 121 | 2,381 |
| loam::amending-plan | 340 | 68 | 268 | 2,920 |
| loam::checkpointing | 365 | 69 | 179 | 2,108 |
| loam::configuring-agents | 459 | 91 | 225 | 3,176 |
| loam::planning | 267 | 48 | 313 | 4,035 |
| loam::resuming | 288 | 60 | 142 | 1,786 |
| loam::starting | 166 | 34 | 353 | 4,915 |
| loam::writing-spec | 261 | 48 | 243 | 2,596 |
<!-- END skill-metrics -->

## Documentation

- [Why loam](./WHY.md) — why this exists: the rediscovery-cost problem and the substrate bet

## License

MIT — see [LICENSE](./LICENSE).

































