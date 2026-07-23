# loam

<p align="center">
  <img src="loam.svg" alt="loam" width="120">
</p>

loam is a collection of workflow skills for AI coding agents.
It gives an agent a structured way to plan work, research questions,
execute plans, and maintain a persistent knowledge base — so
sessions build on each other instead of starting from scratch.

## Install

### Step 1 — global setup

```bash
npx @scchearn/loam setup
```

Setup delegates canonical global skill installation to Skills CLI, installs and
verifies the exact native runtime pinned by `CLI_VERSION`, configures detected
OpenCode, Claude Code, and Cursor integrations, and migrates recognized legacy
project Loam artifacts only from the current workspace. It does not modify
`PATH` or install a project-local runtime.

### Harness visibility

Successful setup leaves supported detected harnesses ready. Session startup is
local, read-only, and network-free. If the runtime is missing or mismatched,
the integration reports `npx @scchearn/loam setup` instead of fabricating state.

- OpenCode, Claude Code, and Cursor receive automatic integration when detected
  and configured by setup.
- Codex and other universal-discovery harnesses can discover the global skills;
  Loam makes no full integration claim without a shipped adapter.
- The existing clone plus direct `.opencode/plugins/loam.js` path remains a
  migration compatibility path and reports setup recovery when incomplete.

See [`.opencode/INSTALL.md`](./.opencode/INSTALL.md) and
[`.codex/INSTALL.md`](./.codex/INSTALL.md) for harness-specific discovery and
migration notes.

## What you get

21 skills, grouped by what they're for:

### Planning and execution

- **Planning** — turn an approved spec into an execution-ready plan
- **Writing-spec** — research a question and produce a spec
- **Starting** — begin executing a plan
- **Resuming** — pick up work after a pause, using saved checkpoints
- **Checkpointing** — save a restart point before pausing or handing off
- **Amending-plan** — update an in-flight plan when scope changes
- **Configuring-agents** — run a structured debate or conference between agents to reach consensus on a goal
- **Setting-goals** — turn a broad ambition into an externally verifiable goal

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

### Native runtime

The setup package installs the exact native `loam` executable selected by the
global `CLI_VERSION`, verifies its SHA-256 against the published runtime
manifest, and stores it under the global `.agents/loam/` root outside `PATH`.
Runtime-dependent skills invoke the injected absolute native command directly;
the shared Node integration is limited to readiness, startup context, and
harness envelopes. Supported targets are macOS (Intel and Apple Silicon),
Windows x64, and Linux x64/arm64.

Trust model: the GitHub repository plus HTTPS. The manifest SHA-256 detects
corruption, truncation, and artifact mismatch; no installer script is ever
executed and nothing is downloaded once the requested runtime is ready.

Agents can install `/checkpoint` and `/resume` slash-command shortcuts — the `loam::using` skill bundles the command files and an install reference.

## Ways to use loam

You don't need to memorize skill names. Say what you want in plain
language — the **Using** router matches it to the right skill.

- "Write a spec for what we discussed" — researches and produces a spec
- "Plan the work from that spec" — turns an approved spec into a plan
- "Run the plan" — begins executing, task by task
- "Have agents debate this decision" — runs an approval-gated consensus debate
- "Set a goal" / "I want to achieve X" — creates a verifiable goal artifact
- "Review this goal" — runs the goal's validation procedure
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
| loam::scaffolding-wiki | 445 | 90 | 198 | 2,196 |
| loam::adding-to-memory | 592 | 116 | 217 | 2,301 |
| loam::amending-memory | 505 | 114 | 180 | 1,951 |
| loam::auditing-guidance | 410 | 85 | 252 | 2,528 |
| loam::ingesting-codebase | 329 | 76 | 270 | 3,099 |
| loam::learning-from-session | 487 | 101 | 365 | 4,202 |
| loam::linting-memory | 471 | 102 | 310 | 4,875 |
| loam::normalizing-memory | 457 | 101 | 261 | 2,665 |
| loam::querying-memory | 530 | 105 | 175 | 1,586 |
| loam::reviewing-memory | 510 | 113 | 137 | 1,787 |
| loam::syncing-code-graph | 363 | 84 | 220 | 2,924 |
| loam::using | 368 | 77 | 225 | 3,778 |
| loam::amending-plan | 437 | 88 | 271 | 3,032 |
| loam::checkpointing | 365 | 69 | 180 | 2,188 |
| loam::configuring-agents | 459 | 91 | 225 | 3,176 |
| loam::planning | 327 | 62 | 323 | 4,314 |
| loam::resuming | 376 | 77 | 142 | 1,807 |
| loam::setting-goals | 473 | 101 | 184 | 1,850 |
| loam::starting | 166 | 34 | 355 | 4,983 |
| loam::writing-spec | 332 | 66 | 252 | 2,892 |
<!-- END skill-metrics -->

## Documentation

- [Why loam](./WHY.md) — why this exists: the rediscovery-cost problem and the substrate bet

## License

MIT — see [LICENSE](./LICENSE).
