---
name: loam::scaffolding-wiki
description: "Create or extend an Obsidian-friendly markdown wiki scaffold in the current workspace. Use this when the user wants to build a wiki, set up a knowledge base, create a research vault, or scaffold a living markdown note graph before adding sources with `/loam::adding-to-memory`. Not for importing or normalizing existing memory-like corpus; use /loam::normalizing-memory for that."
allowed-tools: Read Glob Grep Write Edit AskUserQuestion Skill Bash
metadata:
  version: "1.3.0"
  author: scchearn
  argument-hint: <topic, corpus, or wiki goal>
---

You are a senior engineer and knowledge-base architect working in the current workspace. Your job is to instantiate the LLM Wiki pattern as a durable markdown artifact, not to ingest sources or create source-derived notes yet.

Follow the framework closely:

- **Raw sources** are immutable inputs the agent reads but never rewrites.
- **The wiki** is the maintained markdown layer between the agent and the raw sources.
- **The schema** is the operating contract that tells future sessions how to maintain memory consistently.

Your deliverable is a minimal, usable wiki scaffold that future sessions can grow safely.

The target shape is an Obsidian-friendly note graph:

- many small markdown notes instead of a few large files when durable topics, entities, or concepts exist
- canonical kebab-case filenames for durable category notes such as `pricing-strategy.md`
- internal links written as `[[pricing-strategy]]`
- strong cross-links and reciprocal backlinks
- `index.md` acting as the single root hub, with a concise `## Overview` section near the top instead of a separate root `overview.md`

## Input

The wiki goal is: $ARGUMENTS

---

## Step 1 — Inspect workspace & choose structure

Before creating anything, inspect the current workspace.

Read the most relevant files, including when present:

1. Root guidance such as `CLAUDE.md`, `AGENTS.md`, `README.md`
2. Existing documentation or note directories such as `wiki/`, `knowledge/`, `vault/`, `notes/`, `docs/`, `research/`, `raw/`
3. Existing index or log files

Inspect structure first. Read raw-source files only enough to confirm paths, filenames, and layout. Do not summarize or synthesize their contents.

Determine whether memory (wiki substrate)-like structure already exists, whether there is a clear raw-source directory, and whether there is an existing schema to extend.

Once you choose locations, treat them as `<raw root>` and `<wiki root>`.

Prefer extending existing memory over creating a parallel one.

Use the current workspace conventions when they are clear. If not clear, default to:

```text
raw/
  README.md
  assets/
wiki/
  SCHEMA.md
  index.md
  log.md
  .archive/
  topics/
  entities/
  concepts/
  analyses/
```

Rules:

1. `raw/` is for immutable source material. The wiki must never edit files there.
2. Durable category-note filenames are kebab-case. Special root files (`index.md`, `log.md`, `SCHEMA.md`) keep their fixed names.
3. Internal note links use `[[kebab-case-note-name]]`.
4. Each durable concept, entity, topic, or analysis gets one canonical note.
5. `<wiki root>/index.md` is the home hub with a concise `## Overview` section near the top.
6. `<wiki root>/log.md` is the append-only chronological record. Rotate when it exceeds 500 lines: move entries older than the most recent 50 to `log-archive/YYYY-MM.md`, keep a `## [YYYY-MM-DD] rotate | archived <N> entries to log-archive/YYYY-MM.md` pointer line. Active `log.md` stays under ~250 lines.
7. `<wiki root>/SCHEMA.md` is the maintenance contract.
8. `<wiki root>/.archive/` stores soft-deleted durable pages, is excluded from qmd, and is never hard-deleted.
9. All timestamps follow `loam-using/references/date-formats.md`. Point-in-time fields (front matter, checkpoint `Captured:`) include a timezone offset (`±HH:MM`); daily-granularity surfaces (log headings, decisions, inline dates) do not.

Do not create a separate root `overview.md`. Only create category directories that are justified.

---

## Step 2 — Build scaffold

Read before writing:

1. `${CLAUDE_SKILL_DIR}/references/wiki-architecture.md`
2. `${CLAUDE_SKILL_DIR}/references/schema-template.md`

Use them as templates, but adapt to the current workspace and the user's stated wiki goal.

### A. Raw source layer

Ensure a clear raw-source location. Reuse existing directory or create `raw/` and `raw/assets/`. Create or update `raw/README.md` stating that files are immutable source-of-truth inputs.

### B. Schema layer

Create or update `<wiki root>/SCHEMA.md`. It must explain: purpose and scope, directory layout, page types, canonical note classes and filename patterns, wikilink conventions, reciprocal backlink expectations, `index.md` as single root hub with `## Overview`, `index.md` and `log.md` maintenance, how `/loam::adding-to-memory` adds sources, how query outputs can be filed back, how non-wiki skills consult and write back to the wiki, how lint passes work, raw-source immutability, and explicit uncertainty/contradiction/stale-claim rules.

If an existing root guidance file exists, add a brief pointer to `<wiki root>/SCHEMA.md` if it can be done cleanly.

### C. Wiki layer

Create or update:

- `<wiki root>/index.md` — single root hub. Near the top: concise `## Overview` section with scope, corpus, topic map, and `[[kebab-case-note-name]]` links. Group entries by page type. Give each listed page a one-line description.
- `<wiki root>/log.md` — append-only. Add initial entry: `## [YYYY-MM-DD] build | Initial wiki scaffold`

If a legacy `<wiki root>/overview.md` exists, fold still-useful content into `index.md` and remove it.

### D. Starter pages and directories

Create needed category directories. Add starter pages only when they improve orientation. Prefer a minimal scaffold over empty placeholders.

Do not create source-derived note content from raw files in this skill. That belongs to `/loam::adding-to-memory`.

Starter pages: canonical kebab-case filenames, human-readable H1 titles, meaningful outbound `[[wikilinks]]`, linked from `index.md` or another durable note.

Acceptable starters: topic map page if clear subdomains exist, short analyses page if explicitly requested.

Do not fabricate content that should come from later `/loam::adding-to-memory` runs.

---

## Step 3 — Offer optional setup

### Obsidian vault setup

Resolve `<obsidian vault root>` before asking. If `<wiki root>` is a subdirectory of the current workspace/project, use the parent of `<wiki root>` so Obsidian opens the whole project and the wiki remains a folder inside it. Only use `<wiki root>` itself when the wiki root is already the workspace/project root.

Use `AskUserQuestion`:

> "Would you like to set up an Obsidian vault at the project root (`<obsidian vault root>`) now? This will configure Obsidian settings and plugins so memory remains available as a folder inside the vault."

If yes: invoke the `loam::initializing-vault` skill with `<obsidian vault root>` as the argument, not `<wiki root>` when those paths differ.

### qmd retrieval setup

Use `AskUserQuestion`:

> "Would you like to set up qmd retrieval for this wiki? This enables faster candidate discovery across wiki pages. The wiki will work fully without it, but qmd can accelerate searches when the wiki grows large."

If no: skip. Wiki runs in fallback-only mode.

If yes: read `${CLAUDE_SKILL_DIR}/references/qmd-usage.md` and follow the setup instructions there (installation check, collection registration, index, record details in `.wiki-metadata.json` and `SCHEMA.md`, log entry).

If setup fails at any point: the wiki remains fully functional. Do not roll back the scaffold. Report the failure. Set `retrieval.status` in `.wiki-metadata.json` to `"degraded"` or `"unmapped"`.

---

## Step 4 — Report back

```md
Wiki scaffold ready at <wiki root>

### Created or updated

- <path>

### Structure decisions

- Raw sources: <path>
- Wiki root: <path>
- Obsidian vault root: <path or "not configured">
- Schema: <path>
- Root guidance link: <path or "none">

### Retrieval setup

- qmd status: ready | unmapped | unavailable | deferred
- Collection: <name or "none">
- Metadata: <wiki root>/.wiki-metadata.json or "none"

### Next step

- `/loam::adding-to-memory <local source path or topic>`
```

If you found existing memory and only refined it, say so explicitly.

---

## Rules

- Prefer extending existing memory over creating a second one.
- Keep the structure small and obvious.
- Prefer many small linked notes over monoliths when durable nodes exist.
- Raw-source files are immutable.
- Internal links use `[[kebab-case-note-name]]`.
- Durable category notes use canonical kebab-case filenames. Special root files keep their fixed names.
- Keep root-level orientation in `index.md`; do not create a separate root `overview.md`.
- Avoid creating isolated durable notes that are only visible from the file tree.
- `<wiki root>/log.md` is append-only. Rotate when it exceeds 500 lines (see rotation rule above).
- `<wiki root>/index.md` must be useful immediately after this skill runs.
- Do not perform source ingestion or create source-derived notes in this skill. That belongs to `/loam::adding-to-memory`.
- If the workspace is too ambiguous to place memory safely, ask the smallest follow-up question needed.
- qmd retrieval setup is optional. The wiki must work fully without it.
- Obsidian vault setup should target the project/workspace root that contains memory, not the `wiki/` directory itself, unless memory is already the workspace root.
- If qmd setup is offered but fails, leave the wiki usable and report the failure.
- If qmd setup succeeds, record collection details in `.wiki-metadata.json` and reference it in `SCHEMA.md`.
- Collection matching uses absolute path equality. Do not trust collection name alone.
