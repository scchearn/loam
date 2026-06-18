# Wiki Alignment Rules

Use this reference when normalizing existing memory-like corpus to the repo's target conventions.

## Target conventions

- durable category-note filenames use kebab-case
- special root files keep their fixed names: `index.md`, `log.md`, and `SCHEMA.md`
- internal links use `[[kebab-case-note-name]]`
- H1 titles can remain human-readable
- `index.md` is the single root hub and should begin with a concise `## Overview` section
- a separate root `overview.md` is legacy drift that should be folded into `index.md` and removed when safe
- topic, entity, concept, and analysis notes should cross-link and, when appropriate, link back to each other
- relevant `AGENTS.md` / `CLAUDE.md` files stay lean operational entrypoints and point into memory (wiki substrate) for durable deep reference
- mirror files that intentionally defer to `AGENTS.md` should stay thin unless the workspace clearly intends otherwise

## Alignment goals

- reduce graph isolation
- normalize durable note identity
- make traversal easier in Obsidian
- make retrieval easier for LLMs
- preserve useful existing content instead of replacing it wholesale
- reduce stale or duplicated deep-reference content in monorepo guidance files
- keep operational guidance and durable wiki knowledge in the right layers

## Monorepo guidance scoping

- Do not assume there is only one `AGENTS.md` / `CLAUDE.md` pair in the repo.
- For repo-wide alignment, inspect root guidance and wiki-local guidance first.
- For scoped alignment, inspect root guidance plus the nearest relevant guidance files in or above the target subtree.
- Only expand to sibling app, package, or module guidance files when memory scope clearly spans them or when their content is materially duplicated and stale.
- Treat one-line mirror files like `@AGENTS.md` as mirrors, not independent deep-reference documents.

## Guidance-only mode

- `--guidance-only` is a hard scope restriction.
- In that mode, memory is read-only context only.
- Do not create, rename, move, or rewrite wiki notes.
- Do not modify `SCHEMA.md`, `index.md`, `log.md`, or any legacy root `overview.md` in that mode.
- Only propose and apply edits to relevant `AGENTS.md` / `CLAUDE.md` files.

## Safe transformations

- create missing `SCHEMA.md` or `index.md`
- add or refresh a concise `## Overview` section in `index.md`
- consolidate a legacy root `overview.md` into `index.md` and remove it when safe
- normalize obvious durable note filenames to kebab-case
- normalize obvious internal links to `[[wikilinks]]`
- add missing reciprocal backlinks when the relationship is clearly material
- move notes into clearer category directories when the destination is obvious and safe
- update hub notes so they actually help traversal
- add concise pointers from `AGENTS.md` / `CLAUDE.md` files to relevant wiki hub notes or scoped wiki pages
- shorten duplicated deep-reference sections in guidance files into concise wiki pointers when the durable detail is already or should already be in memory (wiki substrate)
- correct clearly stale guidance after verifying against current repo state and the relevant wiki scope
- keep mirror files thin when they intentionally defer to a canonical guidance file

## High-caution transformations

- merging duplicate notes
- large content rewrites
- reclassifying many notes at once when the taxonomy is not obvious
- renaming notes with many inbound references when the canonical identity is still debatable
- broad deletions or rewrites of guidance files
- trimming operational guidance before the durable knowledge clearly exists in memory (wiki substrate)
- editing unrelated scoped guidance files in a large monorepo just because they exist

When in doubt, propose the change but do not apply it silently.

## Preservation rules

- preserve note content unless a small structural rewrite materially improves the graph
- preserve contradictions and uncertainty
- when source pages exist in a `sources/` directory, absorb their content into topic/entity/concept pages and remove the source pages; flag ambiguous cases rather than guessing destinations
- do not modify raw-source files
- preserve commands, hard rules, safety constraints, scope boundaries, and canonical source-of-truth pointers in guidance files
- guidance files should answer "how do I operate safely here?" while memory answers "what durable knowledge should future sessions reuse?"

## Graph quality checks after alignment

- fewer unresolved internal links
- fewer isolated notes
- stronger hub notes
- better consistency of filenames and link targets
- improved reciprocal backlinks where relationships are material
- guidance files point to memory (wiki substrate) instead of duplicating long, fast-aging technical reference material

In `--guidance-only` mode, evaluate only the last bullet above plus stale-guidance correction. Do not treat missing graph repairs as in-scope edits.
