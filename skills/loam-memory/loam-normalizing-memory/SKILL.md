---
name: loam::normalizing-memory
description: "Inspect an existing memory corpus (wiki substrate) and align it to this repo's Obsidian-friendly note-graph conventions. Use this when the user wants to import, normalize, retrofit, or clean up existing memory, notes folder, vault, docs tree, or mixed markdown knowledge base. In monorepos, also use it to align relevant AGENTS.md and CLAUDE.md files. Excludes goals/ from normalization. Not for routine wiki maintenance; use /loam::linting-memory for that."
allowed-tools: Read Glob Grep Write Edit Bash
metadata:
  version: "1.3.0"
  author: scchearn
  argument-hint: "[--guidance-only] [wiki root or scope]"
---

You are a senior engineer and wiki maintainer retrofitting an existing memory corpus (wiki substrate) to this repo's Obsidian-friendly conventions.

This skill is broader than `/loam::linting-memory`.

- Use `/loam::linting-memory` for ongoing maintenance of memory (wiki substrate) that already largely follows the conventions.
- Use `/loam::normalizing-memory` when the structure, naming, link style, hub notes, or note graph need normalization or retrofit work.

The target output is:

- an Obsidian-friendly note graph
- durable category notes with canonical kebab-case filenames
- internal links written as `[[kebab-case-note-name]]`
- `index.md` functioning as the single root hub, with a concise `## Overview` section near the top
- stronger reciprocal links and fewer isolated notes
- an explicit `SCHEMA.md` when one is missing or materially incomplete
- lean operational `AGENTS.md` / `CLAUDE.md` files that point into memory (wiki substrate) for durable deep reference instead of duplicating long, fast-aging technical knowledge

## Input

The alignment target is: $ARGUMENTS

If no explicit target is provided, align the most plausible wiki-like root in the current workspace.

---

## Step 0 — Parse arguments

`$ARGUMENTS` may contain an optional mode flag and an optional target scope.

Examples:

- `--guidance-only`
- `--guidance-only apps/api`
- `wiki`
- `apps/web`

Parse the arguments like this:

1. If `--guidance-only` appears anywhere, set the mode to **guidance-only**.
2. Remove the flag from the remaining text.
3. Treat the remaining text, if any, as the target root or scope.
4. If no target remains, use the most plausible wiki root and its most relevant guidance scope.

### Guidance-only mode

In `--guidance-only` mode:

- inspect and update only relevant `AGENTS.md` and `CLAUDE.md` files
- read memory so those guidance files can point to the right hub notes and durable pages
- treat memory as read-only context only
- do not rename or move wiki notes
- do not normalize links across the whole wiki corpus
- do not restructure category directories
- do not create, update, or append any wiki file, including `SCHEMA.md`, `index.md`, `log.md`, or any legacy root `overview.md`

---

## Phase 1 — Resolve the wiki-like root and guidance scope

Locate the most plausible wiki-like root by checking for signals such as:

- `SCHEMA.md`
- `index.md`
- `overview.md` as a legacy root-hub signal
- `log.md`
- dense markdown trees under directories like `wiki/`, `knowledge/`, `vault/`, `notes/`, `docs/`, or `research/`
- existing source, topic, concept, entity, or analysis note clusters

Rules:

1. If the user names a root or scope explicitly, prefer it.
2. If multiple roots are plausible, ask the smallest follow-up question needed.
3. If nothing wiki-like exists yet, stop and recommend:

```text
/loam::scaffolding-wiki <topic or wiki goal>
```

Treat the chosen location as `<wiki root>` for the rest of this skill.

Then resolve the relevant guidance-file scope.

Guidance files may exist at multiple levels in a monorepo. Do not assume there is only one global pair.

Use these scoping rules:

1. Always inspect root-level `AGENTS.md` / `CLAUDE.md` when they exist.
2. Always inspect `<wiki root>/AGENTS.md` / `<wiki root>/CLAUDE.md` when they exist.
3. If the target names a subtree such as an app, package, service, or module, inspect the nearest relevant scoped guidance files in or above that subtree.
4. If no explicit subtree is named, inspect additional scoped guidance files only when memory scope clearly covers them or when they likely contain duplicated or stale deep-reference material that should point into memory (wiki substrate).
5. Treat mirror files such as `CLAUDE.md` files that only defer to `AGENTS.md` as mirrors. Keep them thin unless the workspace clearly intends otherwise.

---

## Phase 2 — Inspect the current shape

Before changing files, read:

1. `${LOAM_SKILL_DIR:-${CLAUDE_SKILL_DIR}}/references/alignment-rules.md`
2. `${LOAM_SKILL_DIR:-${CLAUDE_SKILL_DIR}}/references/proposal-template.md` as the report shape for unresolved or ambiguous work
3. `${LOAM_SKILL_DIR:-${CLAUDE_SKILL_DIR}}/references/guidance-file-triage.md`
4. `<wiki root>/SCHEMA.md` when it exists
5. `<wiki root>/index.md` when it exists
6. `<wiki root>/overview.md` when it exists, so you can assess whether it contains legacy root-hub content that should be folded into `index.md`
7. `<wiki root>/log.md` when it exists
8. the relevant `AGENTS.md` / `CLAUDE.md` files for the resolved scope
9. a representative sample of notes from the target scope

In `--guidance-only` mode, only read the minimum wiki context needed to point guidance files at the right hub or scoped notes. Do not perform a full graph audit.

Map the current state:

- current directory structure
- current filename patterns
- current internal link styles such as `[[wikilinks]]`, markdown links, plain-text references, or mixed styles
- existing hub notes and whether they are effective
- obvious duplicate note identities
- broken or unresolved internal links
- missing reciprocal backlinks where relationships are material
- notes that should likely move into `topics/`, `entities/`, `concepts/`, `analyses/`, or `code/`
- existing `sources/` directories whose pages should be absorbed into topic/entity/concept pages
- relevant `AGENTS.md` / `CLAUDE.md` files and whether each is canonical, scoped, or mirror-only
- stale or duplicated deep-reference sections in guidance files that should instead point into memory (wiki substrate)
- guidance sections that should stay local and operational, such as commands, hard rules, safety constraints, scope boundaries, and canonical source-of-truth pointers

Treat the existing corpus with care. The goal is alignment, not a destructive rewrite.

---

## Phase 3 — Classify fixes

Classify the alignment work before editing. Distinguish clearly between:

- changes that are safe and obvious, which you apply directly
- changes that are likely right but still somewhat judgmental, which you report instead of guessing
- ambiguous duplicates or restructures, which remain unresolved

Important report requirements:

1. Call out every file you expect to create or update.
2. Call out every file you expect to rename or move.
3. Call out any ambiguous duplicates you will not merge automatically.
4. State whether `SCHEMA.md` or `index.md` need creation or major revision, and whether any legacy root `overview.md` should be consolidated and removed.
5. State whether link-style normalization will be partial or broad.
6. Call out the guidance files you expect to update.
7. Call out any guidance sections you expect to shorten and replace with wiki pointers.
8. Call out any suspected stale guidance claims that need verification or correction.
9. Call out mirror files you will intentionally leave thin or untouched.
10. In `--guidance-only` mode, set all wiki-structure report sections such as create/update, rename/move, link normalization, and backlink repairs to `None` unless a guidance file itself is the item being updated.
11. Before creating any new wiki page, apply the canonical `loam-using` admission rubric by reference; route or discard material that fails it.

---

## Phase 4 — Apply the alignment

Apply the smallest safe set of changes needed to align memory.

Safe full-alignment work includes:

1. creating or updating `<wiki root>/SCHEMA.md`
2. creating or updating `<wiki root>/index.md` as the single root hub with a concise `## Overview` section near the top
3. consolidating a legacy root `<wiki root>/overview.md` into `index.md` and removing it when safe
4. normalizing obvious durable category-note filenames to kebab-case
5. normalizing obvious internal links to `[[kebab-case-note-name]]`
6. updating inbound references when filenames change
7. adding reciprocal backlinks where the relationship is clearly material
8. creating missing category directories when the current scope clearly benefits from them
9. moving notes into clearer category locations when the destination is obvious and safe
10. migrating source pages: when `<wiki root>/sources/` exists, read each source page, synthesize its claims into the most relevant topic/entity/concept pages, then remove the source page. Flag any source pages where the destination is not obvious rather than guessing.
11. auditing and updating relevant `AGENTS.md` / `CLAUDE.md` files so they point to memory (wiki substrate) and keep operational guidance concise
12. shortening duplicated deep-reference sections in guidance files into concise wiki pointers when the durable detail already belongs in memory (wiki substrate)
13. correcting clearly stale guidance after verifying against current repo state, guidance scope, and the wiki
14. keeping mirror files thin when they intentionally defer to a canonical guidance file
15. appending a parseable entry to `<wiki root>/log.md` like:

```md
## [YYYY-MM-DD] alignment | <scope>
```

In `--guidance-only` mode, restrict the work to items 10-13 above. Do not rename or move wiki notes, restructure the wiki graph, or modify any wiki file including `log.md`.

When renaming or moving notes:

- preserve the content
- preserve the durable identity
- update obvious inbound references
- avoid partial graph breakage

When updating guidance files:

- preserve commands, hard rules, safety constraints, scope boundaries, and canonical source-of-truth pointers
- prefer concise pointers into memory (wiki substrate) over long duplicated deep-reference prose
- verify stale-looking claims against current repo state before changing or removing them
- keep mirror files like `CLAUDE.md` thin when they intentionally defer to `AGENTS.md`

Do not:

- silently merge ambiguous duplicate notes
- flatten meaningful disagreements
- rewrite raw-source files
- perform broad speculative synthesis during alignment
- create wiki pages that fail the canonical `loam-using` admission rubric
- turn `AGENTS.md` / `CLAUDE.md` files into shadow wiki pages
- rewrite every scoped guidance file in a monorepo when the target scope does not require it

If a case is ambiguous, leave it unresolved and report it explicitly.

---

## Phase 5 — Report back

After applying the alignment, output:

```md
Alignment applied to <wiki root>

### Mode

- <full alignment or guidance-only>

### Guidance files updated

- <path or "none">

### Created or updated

- <path>

### Renamed or moved

- <old path> -> <new path>

### Left unresolved

- <issue or "none">

### Next useful command

- `/loam::linting-memory [scope]`
```

If the scope was already close to the target conventions and only small repairs were needed, say so explicitly.

In `--guidance-only` mode, the `Created or updated`, `Renamed or moved`, and other structural wiki sections should normally be `none` unless the item is itself an `AGENTS.md` or `CLAUDE.md` file.

---

## Rules

- Apply safe structural fixes directly and report ambiguous work instead of guessing.
- Prefer the smallest safe alignment over a broad rewrite.
- Durable category-note filenames use kebab-case. Special root files keep their fixed names.
- Internal links use `[[kebab-case-note-name]]` where the target note is a durable internal note.
- Preserve note content and durable identity when renaming or moving files.
- In monorepos, scope guidance-file edits to the root, wiki-local, and nearest relevant subtree files. Do not fan out across unrelated modules.
- Guidance files should stay operational entrypoints, not shadow wiki pages.
- In `--guidance-only` mode, edit only `AGENTS.md` / `CLAUDE.md` files. The wiki is read-only context in that mode.
- Do not silently merge ambiguous duplicates.
- Do not modify raw-source files.
- Exclude `goals/` from wiki normalization. Goals are workflow artifacts maintained by `/loam::setting-goals`, not wiki content to be normalized.
