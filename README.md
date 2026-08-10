# loam

<p align="center">
  <img src="loam.svg" alt="loam" width="120">
</p>

<p align="center">
  <a href="./WHY.md"><img alt="Why Loam exists" src="https://img.shields.io/badge/why-Loam%3F-6b5b45"></a>
  <a href="https://www.npmjs.com/package/@scchearn/loam"><img alt="npm version" src="https://img.shields.io/npm/v/@scchearn/loam?logo=npm&amp;label=npm"></a>
  <a href="https://github.com/scchearn/loam/actions/workflows/ci.yml"><img alt="CI status" src="https://img.shields.io/github/actions/workflow/status/scchearn/loam/ci.yml?branch=main&amp;label=CI&amp;logo=githubactions"></a>
  <a href="https://github.com/scchearn/loam/releases"><img alt="runtime version" src="https://img.shields.io/github/v/tag/scchearn/loam?filter=cli-v*&amp;label=runtime&amp;logo=rust"></a>
  <a href="./LICENSE"><img alt="MIT license" src="https://img.shields.io/github/license/scchearn/loam"></a>
</p>

loam is a collection of workflow skills for AI coding agents.
It gives an agent a structured way to plan work, research questions,
execute plans, and maintain a persistent knowledge base, so
sessions build on each other instead of starting from scratch.

[Why Loam exists →](./WHY.md)

## Install

```bash
npx @scchearn/loam setup
```

That's it. Setup installs the skills and a small native helper globally (once,
not per project), automatically configures detected OpenCode and Cursor
integrations, and offers to install the Loam marketplace plugin for detected
Claude Code and Codex installations. Setup points each harness's session hooks
straight at the private native helper and owns that registration; it never adds
duplicate user hooks.

### Collaboration compatibility

Loam's federated collaboration features are advertised per harness, and only
after every row of the compatibility matrix has been observed passing against
that harness's released version.

| Harness | Collaboration state in a session |
| ------- | -------------------------------- |
| Claude Code | **automatically compatible** — injected at session start and refreshed at the next prompt |
| Codex CLI | **automatically compatible** — injected on the first turn of a session, once Codex's one-time hook-trust review has approved the Loam hook |
| OpenCode | **automatically compatible** — prepended to the first user message of a session by the in-process plugin |
| Cursor | **withheld — not installed/evaluated**; CLI retrieval only |

Withheld means exactly that: the claim is not made, and no shim, bridge, or
polling fallback was added to simulate one. A withheld harness still gets the
full skill set and the baseline Loam context — only the automatic collaboration
claim is withheld, and collaboration state remains reachable from the CLI.

Codex runs its `SessionStart` hooks on the first turn rather than before it, so
the context arrives with your first message rather than ahead of it. Codex also
gates every hook behind a one-time trust review and truncates long hook output
in model context; the framing, the workspace state, and the federation section
survive, and part of the skill body may be elided.

Use `--yes` to configure every detected harness without prompting, or
`--dry-run` to preview without downloads or mutation. Nothing is added to your
`PATH` and nothing is installed per-project.

To update Loam later:

```bash
npx @scchearn/loam update
```

This refreshes only Loam's skills, updates the private helper, and repairs each
agent's integration and marketplace plugin. Starting a coding session remains
read-only and never downloads updates.

To remove everything loam installed:

```bash
npx @scchearn/loam uninstall
```

`install` is an alias for `setup`; `doctor` checks the installation without
changing it. `uninstall` also removes Loam's globally installed skills.

For agent-specific setup notes, see [`.opencode/INSTALL.md`](./.opencode/INSTALL.md)
and [`.codex/INSTALL.md`](./.codex/INSTALL.md).

### Background session harvest

Background session-learning harvest is enabled by default: on every turn end
the hook measures what has been said since the last harvest, and when enough
new conversation exists a detached agent reviews the window and routes durable
learnings through `loam::learning-from-session`. Each session keeps its own
cursor and harvests into its workspace's memory; harvest shares the
per-workspace memory-writer lease with code ingestion and never runs while
another worker holds it. To opt out, set `LOAM_HARVEST_BACKGROUND=0` or
`background_harvest.enabled: false` in `~/.agents/loam/config.json`.

## What you get

21 skills, grouped by what they're for:

### Planning and execution

- **Planning**: turn an approved spec into an execution-ready plan
- **Writing-spec**: research a question and produce a spec
- **Starting**: begin executing a plan
- **Resuming**: pick up work after a pause, using saved checkpoints
- **Checkpointing**: save a restart point before pausing or handing off
- **Amending-plan**: update an in-flight plan when scope changes
- **Configuring-agents**: run a structured debate or conference between agents to reach consensus on a goal
- **Setting-goals**: turn a broad ambition into an externally verifiable goal

### Memory

- **Adding-to-memory**: save a source or document into the knowledge base
- **Querying-memory**: ask the knowledge base a question
- **Amending-memory**: fix a wrong or stale claim in the knowledge base
- **Linting-memory**: health-check the knowledge base for orphans, broken links, drift
- **Normalizing-memory**: retrofit structure onto a messy notes corpus
- **Reviewing-memory**: surface what's unresolved or gaps in the knowledge base
- **Learning-from-session**: capture learnings from a session into memory or agent guidance
- **Auditing-guidance**: review and improve AGENTS.md / CLAUDE.md files
- **Ingesting-codebase**: build resumable semantic pages from source files
- **Syncing-code-graph**: reconcile those pages after code changes

### Setup

- **Scaffolding-wiki**: set up the knowledge base structure
- **Initializing-vault**: configure an Obsidian vault

## How it works

loam skills maintain a persistent **memory** layer with three parts:

- **Wiki notes**: durable knowledge about the project (Obsidian-friendly markdown)
- **Agent guidance**: `AGENTS.md`, `CLAUDE.md` files that tell future agent sessions how to work here
- **Checkpoints**: transient restart notes for pausing and resuming work

When you start a session, the agent loads **Using**: a router skill that
recognizes what you're trying to do and invokes the right skill for it.
You don't need to memorize the list above; the agent routes itself.

loam works fully on its own. If your wiki grows large, [qmd](https://github.com/tobi/qmd) (`npm install -g @tobilu/qmd`) speeds up search across memory. The skills detect it automatically and fall back to built-in search when it's absent.

### Code graph

The code graph follows the code you can currently edit: tracked, modified, and
untracked working-tree files by default, with the same behavior in non-Git
directories. Stable content identity prevents mtime-only churn. Git-backed
records also carry blob/commit provenance; uncommitted records remain local and
provisional. Use the optional `--ref <commit>` projection when a reproducible,
committed-only graph is required.

### Native runtime

Setup also installs a small native `loam` binary that some skills use to run
faster. It's downloaded from GitHub over HTTPS and checked against a published
checksum before anything runs. No install script is ever executed. Supported
platforms are macOS (Intel and Apple Silicon), Windows x64, and Linux
x64/arm64.

## Ways to use loam

You don't need to memorize skill names. Say what you want in plain
language, and the **Using** router matches it to the right skill.

- "Write a spec for what we discussed": researches and produces a spec
- "Plan the work from that spec": turns an approved spec into a plan
- "Run the plan": begins executing, task by task
- "Have agents debate this decision": runs an approval-gated consensus debate
- "Set a goal" / "I want to achieve X": creates a verifiable goal artifact
- "Review this goal": runs the goal's validation procedure
- "Stopping work" / "I need to step away": saves a restart checkpoint
- "Resume where I left off": picks up from the last checkpoint
- "The scope changed, update the plan": walks the impact, proposes plan changes
- "Add to memory" / "capture all into loam": ingests a source or conversation
- "What does memory say about X?": answers from the knowledge base
- "Memory is wrong about X": corrects stale claims (proposal-first)
- "What is unresolved": surfaces open questions and gaps in memory
- "Health-check the wiki": finds orphans, broken links, drift
- "This notes corpus is messy": retrofits structure onto existing notes
- "Save what we learned this session": routes learnings to wiki or AGENTS.md
- "Audit the AGENTS.md": scores, prunes stale content, adds missing commands
- "Set up loam" / "scaffold a knowledge base": creates the wiki structure

## Skill metrics

Skills load into an agent's context window, so loam keeps each one small. A
skill's short description loads at session startup; its full body loads only when
the skill runs. The table below shows how much space each skill uses against the
[agentskills.io](https://agentskills.io/specification) size budgets.

<!-- BEGIN skill-metrics -->
<!-- Auto-generated by bin/skill-metrics.sh via tiktoken cl100k_base. Do not edit by hand; run `bin/skill-metrics.sh --update` to refresh. -->

| Skill | Desc chars (max 1,024) | Desc tokens (~100) | Body lines (max 500) | Body tokens (< 5,000) |
|-------|---:|---:|---:|---:|
| loam::initializing-vault | 206 | 51 | 9 | 73 |
| loam::scaffolding-wiki | 445 | 90 | 198 | 2,196 |
| loam::adding-to-memory | 592 | 116 | 217 | 2,301 |
| loam::amending-memory | 505 | 114 | 180 | 1,951 |
| loam::auditing-guidance | 410 | 85 | 252 | 2,528 |
| loam::ingesting-codebase | 329 | 76 | 288 | 3,640 |
| loam::learning-from-session | 487 | 101 | 365 | 4,202 |
| loam::linting-memory | 471 | 102 | 312 | 4,985 |
| loam::normalizing-memory | 457 | 101 | 261 | 2,665 |
| loam::querying-memory | 530 | 105 | 175 | 1,586 |
| loam::reviewing-memory | 510 | 113 | 137 | 1,787 |
| loam::syncing-code-graph | 363 | 84 | 223 | 2,770 |
| loam::using | 368 | 77 | 244 | 4,063 |
| loam::amending-plan | 437 | 88 | 273 | 3,059 |
| loam::checkpointing | 365 | 69 | 180 | 2,188 |
| loam::configuring-agents | 459 | 91 | 225 | 3,176 |
| loam::planning | 327 | 62 | 323 | 4,314 |
| loam::resuming | 376 | 77 | 142 | 1,807 |
| loam::setting-goals | 473 | 101 | 184 | 1,850 |
| loam::starting | 166 | 34 | 357 | 4,991 |
| loam::writing-spec | 332 | 66 | 252 | 2,892 |
<!-- END skill-metrics -->

## Documentation

- [Why loam](./WHY.md): why this exists, the rediscovery-cost problem and the substrate bet

## License

MIT, see [LICENSE](./LICENSE).
