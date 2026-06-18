# Wiki Architecture Reference

Use this reference when scaffolding a persistent markdown wiki from the LLM Wiki pattern.

The target output is an Obsidian-friendly note graph:

- many small notes
- canonical kebab-case filenames for durable category notes
- `[[wikilinks]]` between durable notes
- backlinks and reciprocal relationships that make graph traversal useful
- `index.md` as the single root hub for both humans and LLMs, with a concise `## Overview` section near the top

## Core layers

### Raw sources

- Immutable inputs.
- Examples: clipped articles, transcripts, PDFs, notes, datasets, images.
- The wiki may read from this layer but must never rewrite it.

### Wiki

- The maintained markdown layer between the raw sources and later answers.
- This is where summaries, topic pages, entity pages, comparison notes, and analyses live.
- The wiki is expected to compound over time.
- Prefer many small linked notes over a few monolithic summaries when durable nodes clearly exist.

### Schema

- The operating contract for future sessions.
- It defines page types, naming rules, linking rules, how `/loam::adding-to-memory` extends the wiki, and maintenance expectations.
- Without this layer, each future session has to rediscover how the wiki is supposed to work.

## Note identity and linking

- Use one canonical note per durable topic, entity, concept, or analysis.
- Durable category-note filenames should be kebab-case, such as `pricing-strategy.md`. Special root files like `index.md`, `log.md`, and `SCHEMA.md` keep their fixed names.
- Internal note links should use `[[pricing-strategy]]`.
- H1 headings can remain human-readable, such as `# Pricing Strategy`.
- When a note becomes important enough to reuse, give it a real note instead of repeating the same explanation across many files.
- Prefer reciprocal links for meaningful relationships, especially where topic, entity, and concept notes materially inform each other.
- Keep root-level orientation in `index.md` instead of creating a separate root `overview.md`.

## Default scaffold

Use the smallest structure that still makes the wiki operable. A good default is:

```text
raw/
  README.md
  assets/
wiki/
  SCHEMA.md
  index.md
  log.md
  topics/
  entities/
  concepts/
  analyses/
```

## Special files

### `index.md`

- home map-of-content, content-oriented catalog, and top-level orientation page
- begin with a concise `## Overview` section covering wiki scope and intended corpus, current state and open questions, and a starting topic map with `[[wikilinks]]` when those notes exist
- each page should have a link and one-line description
- organize by section so future sessions can quickly identify relevant pages
- keep this current on every `/loam::adding-to-memory` run and any other durable write-back that changes discoverability
- do not rely on it as the only navigation layer; the note graph should still be traversable from the notes themselves

A separate root `overview.md` is legacy drift. If one exists, fold its still-useful root-level content into `index.md` and remove it.

### `log.md`

- chronological, append-only history
- record scaffold creation, `/loam::adding-to-memory` runs, query write-backs, and lint passes
- parseable headings make this searchable with simple tools later

Recommended heading formats:

```md
## [2026-04-09] build | Initial wiki scaffold
## [2026-04-10] add (file) | Source Title
## [2026-04-10] add (chat) | Topic
```

## Page categories

These are common categories, not mandatory ones:

- `topics/` — broader subjects or themes that synthesize across raw material
- `entities/` — people, companies, products, places, projects, or systems
- `concepts/` — ideas, methods, claims, frameworks, recurring patterns
- `analyses/` — query outputs worth preserving as first-class wiki pages

Use only the categories the domain actually needs.

Each note in these categories should use a canonical kebab-case filename and participate in the graph through outbound and inbound links where appropriate.

## Authoring guidance

- Prefer links over duplicated prose.
- Prefer `[[wikilinks]]` over plain prose references when a note exists.
- Make contradictions explicit.
- Separate confirmed facts from open questions.
- Do not over-scaffold with placeholder files.
- Keep starter content structural rather than synthetic when raw sources have not been added yet.
- Avoid isolated notes: new durable notes should be reachable from a hub note or a closely related note.

## Build-skill boundary

The scaffold pass should create the lanes, not drive every future workflow.

During build:

- define the raw-source location
- define the wiki structure
- define the schema
- initialize `index.md` with a concise `## Overview` section and `log.md`
- establish the canonical link and filename rules
- create only the starter pages required for orientation
- do not create source-derived notes from raw files

During `/loam::adding-to-memory`:

- read one or more new raw sources
- update topic/entity/concept pages with synthesized content
- strengthen the note graph with wikilinks and reciprocal links
- revise the index
- append the log
- note contradictions and open questions

During research, planning, execution, and amendment:

- consult the wiki when it exists, but do not require one
- treat the wiki as a durable memory layer, not the authority over the current repo state
- if current repo state or primary docs conflict with the wiki, trust the current repo state and update the wiki if the finding is durable
- file back only durable findings such as stable architecture facts, domain clarifications, recurring debugging discoveries, reusable comparisons, or established constraints
- avoid writing back ephemeral task chatter, temporary dead ends, or narrow planning noise
