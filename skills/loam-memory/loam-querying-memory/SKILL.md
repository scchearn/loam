---
name: loam::querying-memory
description: "Answer questions against existing memory (the wiki substrate). Use this whenever the user is asking what is happening in the project, directory, codebase, architecture, workflow, decisions, or current state and the wiki likely contains the answer, even if they do not explicitly mention the wiki. Also use it for summaries, comparisons, and reusable analyses grounded in current wiki pages. Routes authoritative goal-state questions to /loam::setting-goals. Not for surfacing unresolved gaps; use /loam::reviewing-memory for that."
allowed-tools: Read Glob Grep Write Edit Bash mcp__hcom-mcp__transcript
metadata:
  version: "1.3.0"
  author: scchearn
  argument-hint: <question>
---

You are a senior engineer and wiki maintainer answering a question from a persistent markdown wiki. Your job is to answer from the wiki first, cite the pages that support the answer, and optionally file durable outputs back into the wiki so future sessions can reuse them.

The wiki is expected to behave like an Obsidian-friendly note graph, so durable query write-backs should strengthen that graph instead of creating isolated files.

This is a **wiki-first query** skill, not a general web research skill. Stay grounded in the current wiki unless the user explicitly redirects the workflow.

## Input

The question is: $ARGUMENTS

---

## Step 1 — Discover candidates

First reuse the injected `Workspace state` under the reuse contract in `loam::using`. For a non-code query, do not rerun native state when that block supplies wiki existence/root, qmd readiness, collection, and hints.

If the injected state cannot be reused, refresh native state through the injected absolute integration path:

```bash
<native-runtime-command> state --fast "$(pwd)"
```

If the question needs the code graph, run the required native check through `<native-runtime-command> ...` before trusting the injected snapshot alone. If the native runtime reports unavailable or does not provide real state, stop and recommend `npx @scchearn/loam install`; do not fabricate state or use a project-local fallback. If `exists` is false, stop and recommend `/loam::scaffolding-wiki <topic>`. Use `wiki_root` as the resolved wiki root and `qmd_ready` + `collection` for qmd state.

Classify the question internally (do not expose unless it helps the answer): **lookup** (answer from one or a few pages), **comparison** (differences/tradeoffs across pages), **synthesis** (higher-level explanation combining multiple parts), **gap check** (whether memory can answer something yet). Derive 3-8 search terms.

### qmd search (when ready)

Use `--files` to get candidate file paths only (no snippets). Then Read the actual wiki files to verify.

- **Lookup**: `qmd search "<keywords>" --files -n 8 -c <collection>`
- **Comparison/synthesis**: `qmd query "<natural language question>" --files -n 8 -c <collection>`
- Skip the `qmd://<collection>/` prefix in file paths to get the relative wiki path (e.g. `code/validate-token.md`)
- Noisy results: retry with different terms or add `intent:` to disambiguate
- Use scores to prioritize which files to Read first

### Grep/Glob search (when qmd not ready)

1. Locate wiki root by Glob for `SCHEMA.md`, `index.md`, or `log.md`. If no wiki exists, stop and recommend `/loam::scaffolding-wiki <topic>`. If multiple roots are ambiguous, ask a minimal follow-up.
2. Derive 3-8 search terms from the question.
3. Search immediately with Grep and Glob on memory directories.
4. Read SCHEMA.md/index.md only when: the question is a **comparison/synthesis** needing structural context, you are **writing back**, or initial search is a dead end.

### Verification (always required)

Read the actual wiki files for top candidates. Follow `[[wikilinks]]`, `Related pages`, `Sources` outward from relevant notes. Expand outward only if the neighborhood is insufficient. Do not read the entire wiki unless the question truly requires it.

If qmd results and the wiki disagree, trust the wiki files. Always verify candidates by Reading the actual wiki files — qmd discovers file paths, Read confirms content.

### Transcript search (secondary source)

Transcripts are a legitimate secondary source, ranked below the wiki: the wiki
is curated and durable, transcripts are raw and noisy. Consult them when the
question concerns recent or in-flight work, what an agent did or said, or when
wiki evidence is thin and a session might corroborate or date it. Do not lead
with them. When a transcript and a wiki page disagree, report both and say
which is more recent; a newer transcript often means the wiki is stale, so
route the disagreement to /loam::amending-memory rather than picking a side.

Availability check, in order; stop at the first that works, skip the whole
subsection silently if none do:

1. The injected `Workspace state` block says `hcom: ready`. Prefer the `mcp__hcom-mcp__transcript` tool when it is present; otherwise use the `hcom` CLI via Bash.
2. If the block is absent, the `mcp__hcom-mcp__transcript` tool being present is sufficient.

If neither is available, do not mention hcom, do not install anything, and
answer from the wiki alone.

Search: MCP `transcript` with `mode: search`, `pattern`, `exclude_self: true`,
`limit: 10`; or `hcom transcript search "<pattern>" --exclude-self --limit 10 --json`.
Then read the matching exchange with `mode: read` (or `hcom transcript <name> N`)
before citing it. Snippets are truncated; never cite from a snippet alone.

Discard results whose `text` is `binary file matches` — those agents store
transcripts in a database the search cannot read.

Cite transcript evidence as `<agent name>, exchange N`. Do not cite or write
back the transcript file path.

---

## Step 2 — Answer with citations

Answer the question using the current wiki as the evidence base.

Rules:

1. Cite the specific wiki page paths that support the answer.
2. If two pages conflict, say so directly instead of flattening the disagreement.
3. Distinguish between:
   - what memory clearly supports
   - what memory only suggests indirectly
   - what only a session transcript supports (not yet in memory)
   - what memory does not yet establish
4. If the question cannot be answered well from the current wiki, say that explicitly and identify the missing source, page, or ingest work that would help.

Keep the answer concise but complete. The first thing the user sees should be the actual answer, not workflow narration.

---

## Step 3 — Write back (when durable)

Some query outputs are durable and should become part of memory. Others are one-off answers and should stay in chat.

### Write back when

- non-trivial comparison across multiple pages
- cross-source synthesis that future sessions will likely reuse
- taxonomy, framework, or summary that improves navigation
- recurring explanation that clearly belongs in the knowledge base
- the user explicitly asked to save, file, preserve, or turn it into a page

### Do not write back when

- simple lookup from one page
- narrow, ephemeral, or operationally trivial
- would create an isolated page with weak graph connections
- when in doubt, prefer fewer new pages

### How to write back

Create or update a page under `<wiki root>/analyses/` or the most appropriate existing wiki page.

Analysis page structure:

```md
# <Analysis Title>

- Query: <original question>
- Created: YYYY-MM-DD
- Scope: <what this covers>

## Short answer

<2-5 sentence answer>

## Evidence from the wiki

- [[note-name]] — <why it matters>

## Synthesis

<combined explanation or comparison>

## Caveats and uncertainty

- ...

## Related pages

- [[index]]
- [[related-page-slug]]
```

When writing back:

1. cite supporting wiki pages inside the analysis
2. include `Related pages` wikilinks so the analysis is connected to the surrounding graph
3. update at least one clearly relevant topic, entity, concept, or hub note when that relationship is materially useful
4. update `index.md`
5. append a parseable entry to `<wiki root>/log.md`:

```md
## [YYYY-MM-DD] query | <question summary>
```

If modifying an existing page rather than creating a new analysis, adapt to that page's existing structure instead of forcing the analysis template.

### Refresh qmd after writes

If you wrote to the wiki and qmd was ready, run `qmd update -c <collection>` then `qmd embed -c <collection>`; report both outcomes separately. If either fails, report it but do not roll back wiki edits.

---

## Step 4 — Report back

```md
### Answer

<direct answer with citations>

### Gaps or uncertainty

- <gap or "none">

### Filed back into wiki

- <path>
- Index: <path or "unchanged">
- Log: <path or "unchanged">
```

If nothing was written back, say `none` under `Filed back into wiki`.

---

## Rules

- Cite supporting wiki page paths in the answer.
- Always verify qmd candidates by Reading the actual wiki files. qmd discovers file paths — Read confirms content.
- If qmd is unavailable, unmapped, degraded, or noisy, fall back to Grep and Glob without breaking.
- Do not fetch web or external sources in this skill. Agent transcripts via hcom are a permitted secondary source, ranked below the wiki for curation, not for recency; surface disagreements instead of resolving them here.
- Do not modify raw-source files.
- Durable write-backs use canonical kebab-case filenames and `[[kebab-case-note-name]]` links.
- Route authoritative goal-state and readiness questions to `/loam::setting-goals`. The wiki may reference goals, but the goal document is authoritative for lifecycle, validation, and review history.
