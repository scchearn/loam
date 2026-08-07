<p align="center">
  <img src="loam.svg" alt="loam" width="120">
</p>

# Why loam exists

> A short, honest answer to: *"is this just another skills pack, or does it actually move something?"*

---

## The shape of the problem

AI coding agents are powerful and amnesiac. Every session boots cold, and the cost of that cold boot is paid in tokens: the agent re-derives what last session already figured out, re-looks-up an API it already researched, re-reads a file it already summarized. The work compounds in the human's head, never in the agent's.

Three things are true at once:

1. **Rediscovery is the dominant waste.** Not lost plans — lost *learnings*. The API quirk you diagnosed Tuesday, the import path that only works with flag X, the decision to use library A over B: agents pay full token cost to discover these every single session.
2. **`AGENTS.md` alone isn't enough.** A single guidance file is the obvious first move, and it helps — but it goes stale, grows unbounded, and can't carry structured knowledge (entities, topics, code-graph pages). It's a config file stretched into a memory role.
3. **There is no substrate.** Note apps exist (Obsidian, etc.) but agents don't write to them in a shape they can later query. Wikis exist but aren't agent-authorable. The gap is a memory surface that's writable by an agent, readable by any agent, and diffable in git.

loam is the smallest useful step in front of that gap.

---

## What success looks like

Not "we shipped a skill pack." Three observable outcomes:

| Outcome | Signal | Why it matters |
|---|---|---|
| Rediscovery cost drops | A session reaches for memory instead of re-running the same lookup | Tokens and tool calls spent on novel work, not re-deriving |
| Any harness, any model, picks up a plan | A plan written under one harness resumes under another with a "loam" mention to orient | The plan is the asset, not the runtime's private state |
| The harness gets more powerful over time | Memory grows session over session; the agent's effective context expands without re-prompting | The system compounds instead of resetting |

If real sessions don't show these, the project failed and we should say so out loud.

---

## What makes this different from a generic notes integration

- **It's agent-authored in a queryable shape.** Obsidian vaults are great for humans typing notes. loam writes notes in the shape an agent will later query — wikilinks, frontmatter, topic and entity pages, cross-links. The write path and the read path are designed by the same hand.
- **It's runtime-agnostic by format.** The skills are `SKILL.md` markdown; any harness that loads skills can install and run them — Claude Code, OpenCode, Codex, Gemini. How a given model picks up and orients on the skills varies by harness, but a small "loam" mention in a question or task prompt is enough to orient the model onto the loam skill set. No runtime owns the plan or the memory; the substrate is plain markdown.
- **It separates durable from transient.** Wiki notes and `AGENTS.md` are durable. Checkpoints under `wiki/checkpoints/` are transient — one restart, then superseded. Conflating these is how memory becomes garbage.
- **It degrades gracefully.** `qmd` accelerates memory search when installed; built-in search covers the gap when it isn't. The substrate is plain markdown — readable in any editor, diffable in git, survivable past any vendor.
- **It routes itself.** The `loam::using` skill recognizes plain-language intent ("the wiki is wrong about X", "stopping work", "add to memory") and dispatches the right skill. Users never memorize skill names.
- **It has one explicit runtime boundary.** Global setup installs and verifies the private native runtime; runtime-dependent skills invoke that absolute command directly, while harness adapters only provide readiness context and the required envelope. Startup never downloads, repairs, or invents state.
- **Its code graph follows real work.** Working-tree and untracked files remain visible, non-Git projects still work, and stable content identity avoids timestamp churn. Git blob/commit provenance supports an explicit reproducible ref view without treating provisional local summaries as published truth.

---

## Principles

A change that breaks one of these is a different project, not a new feature.

- **Distributed, not centralized.** loam is not a service, and no single agent, machine, or vendor owns it. The skills are portable markdown any harness can load, and the memory lives in your git repo. Nothing central has to be running for loam to work, and there is no hub that, if it went down, would take your memory with it.
- **Harness-agnostic.** The plan and the memory are the assets, not any one runtime's private state. What one harness writes, another can pick up. loam adapts to Claude Code, Codex, OpenCode, and Cursor through thin adapters over shared logic; the substrate underneath is identical for all of them.
- **Graceful fallback to plain files.** The optional tools only accelerate: qmd speeds up search, the native runtime speeds up probes. Take either away and loam still works, because underneath everything it is just markdown files on your system that you can open, grep, and git-diff by hand. The floor is always plain files.

---

## Where this is heading

> Working notes, not a committed roadmap. We are thinking about this out loud, and it stays subordinate to the principles above.

loam started as a fix for one developer's rediscovery cost. The direction we are exploring now is bigger: **loam as the base a whole team, or a whole company, works from.**

Picture each person's loam as its own project, directory, or repo. What we are working toward is letting those separate loams see and work with each other, and letting a shared company knowledge base grow out of them, without giving up a single principle above. That means resolving an apparent contradiction on purpose:

- **A shared company base, still with no central server.** "Centralized" here means one reference a team can rely on, not a hub someone has to keep running or that owns your data. It is federation between many loams, not memory moved into someone's cloud.
- **Still distributed. Still in git. Still yours.** Every workspace keeps its own memory as plain markdown in its own repo. Sharing adds provenance, access boundaries, and explicit conflict handling on top; it never moves the source of truth off your machines, and it stays safe and self-owned.

If a team's separate loams can compound into one base that is still just markdown in git, safe and not sitting on anyone's central server, that is the version of loam a company runs on.

---

## What it explicitly does *not* try to be

- **Not a knowledge graph database.** No triples, no SPARQL, no graph engine. Wikilinks and grep cover the problems agents actually face.
- **Not an agent runtime.** It doesn't spawn agents or arbitrate between models. It gives an already-running agent a place to put what it learned and a way to find it later.
- **Not a vibe-based "AI memory" product.** No embeddings required, no re-ranking service, no monthly bill. The memory is files. You can `git log` it.

---

## The real difficulty

The hard part isn't storage — markdown and git solve that. It isn't picking between known options. The hard part is **finding the gaps**: discovering what an agent or harness needs that the substrate doesn't yet provide, then closing those gaps with the smallest useful skill.

That's why loam grows with real need, not a roadmap. A new skill appears when a recurring pain demands one. The current set reflects what's been demanded so far; it is not final.

---

## Who this is for

In rough order of who benefits most today:

1. **Developers who use AI coding agents across multi-session work** and are tired of paying token cost to rediscover what was already learned.
2. **People who already keep notes in Obsidian or markdown.** loam meets your notes where they live — same format, agent-authorable.
3. **Teams running agents on long-lived codebases** where onboarding a cold session shouldn't take a human an hour of typing.
4. **Anyone who wants a plan to outlive the session that wrote it** — and to be picked up by a different model or harness without translation.

---

## What "real impact" actually requires

A skills pack is not impact. Sessions that spend tokens on novel work instead of rediscovery is impact. So:

- Every skill needs a trigger a human can recognize in plain language. If the user has to learn the skill's name, the skill failed.
- Memory must be queryable cold, by any agent, without the original writer present. A wiki only the authoring agent can search is a trap.
- Plans must be portable. A plan that only resumes under the harness that wrote it has recreated the problem it claimed to solve. A "loam" mention should be enough to orient any model onto the skill set and pick up the plan.
- The launch isn't "we shipped a skill pack." It's "Tuesday's session learned X, Wednesday's session reached for X instead of re-deriving it, and a different harness would have found the same X." Specific. Concrete. Useful before any install.

If you read this and disagree, [open an issue](https://github.com/scchearn/loam/issues). The project gets better when users push back on what memory should carry. That's the whole point.
