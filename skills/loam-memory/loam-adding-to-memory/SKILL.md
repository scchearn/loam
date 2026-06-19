---
name: loam::adding-to-memory
description: "Read a local source file or synthesize conversation context, then integrate the content directly into topic, entity, concept, and analysis pages in existing memory (the wiki substrate). Use this when the user wants to add a source to the wiki, add a document, ingest a local note, transcript, article, report, or PDF, or explicitly preserve the current conversation as a topic note. Not for proposal-first session learnings that directly update existing pages; use /loam::learning-from-session."
allowed-tools: Read Glob Grep Write Edit Bash
metadata:
  version: "1.0.0"
  author: scchearn
  argument-hint: <local source path | topic or summary from chat>
---

You are a senior engineer and disciplined wiki maintainer working in the current workspace. Your job is to compile content into the persistent wiki so future sessions inherit the knowledge instead of rediscovering it.

The target output is an Obsidian-friendly note graph:

- canonical kebab-case filenames for durable category notes
- internal links written as `[[kebab-case-note-name]]`
- small linked topic, entity, concept, and analysis notes
- reciprocal backlinks where the relationship is materially useful

This skill supports two modes: **file mode** (local file ingestion) and **chat-context mode** (synthesize from the current conversation into topic/entity/concept pages). Do not fetch URLs directly in this skill.

If the user wants a **proposal-first review of session learnings** that directly updates existing pages, use `/loam::learning-from-session` instead.

## Input

The content to add is: $ARGUMENTS

---

## Step 1 — Resolve source, wiki & discover related notes

### Source resolution

Use the argument plus the current workspace to identify the source file or a very small intended batch of related files.

Rules:

1. Prefer an exact local path when the user provided one.
2. If the user gave a title or description instead of a path, search for the best candidate under likely raw-source locations such as `raw/`, `research/`, `docs/`, `notes/`, or similar directories.
3. If more than one candidate is plausible, ask the smallest follow-up question needed.
4. If the user clearly requested a large batch, stop and ask them to narrow it or to run repeated adds. This skill is optimized for one source at a time.

### Wiki resolution

Find the existing wiki by looking for files such as:

- `wiki/SCHEMA.md`
- `wiki/index.md`
- `wiki/log.md`
- `wiki/overview.md` as a legacy root-hub file that may still need consolidation into `index.md`

If the workspace uses a different but clearly established wiki root, reuse it.

If multiple wiki roots are present and the target is ambiguous, ask the smallest possible follow-up question.

If no existing wiki can be found, stop and recommend:

```text
/loam::scaffolding-wiki <topic or wiki goal>
```

### Mode detection

1. If `$ARGUMENTS` resolves to an existing local file (or is clearly a file path with an extension), use **file mode**.
2. If `$ARGUMENTS` does not resolve to a file and instead looks like a topic, question, or natural-language summary, use **chat-context mode**.
3. If ambiguous, ask the user: "Did you mean a local file path, or should I synthesize from our conversation?"

### Check qmd readiness

1. Glob for `.wiki-metadata.json`. If found, **read it immediately**. If `retrieval.status` is `"ready"`, qmd is ready — use `retrieval.collection_name` and **skip to discovery below**. Do not run fallback checks.
2. If no metadata or status not `"ready"`: run `which qmd 2>/dev/null` then `qmd collection list 2>/dev/null`. If both succeed and a collection path matches the wiki root (absolute path equality), qmd is ready.
3. If qmd is still not ready: use Grep/Glob to find existing notes.
4. Runtime guard: if any qmd command fails or returns stale results, treat as degraded — fall back to Grep/Glob.

If qmd is ready, read `${CLAUDE_SKILL_DIR}/references/qmd-usage.md` for finding existing related notes.

### Read the wiki contract & discover related notes

Read before editing:

1. `<wiki root>/SCHEMA.md`
2. `<wiki root>/index.md`
3. the most recent relevant parts of `<wiki root>/log.md`
4. any existing topic/entity/concept pages that look directly related to the source
5. `${CLAUDE_SKILL_DIR}/references/ingest-checklist.md`
6. if chat-context mode: `${CLAUDE_SKILL_DIR}/references/chat-context-ingest.md`

**If qmd is ready**, follow `references/qmd-usage.md` to find existing related notes before editing.

**If qmd is not ready**, use Grep and Glob to find existing notes that mention the same entities, topics, or concepts.

Always read the actual candidate pages before editing.

### Read the source

**File mode**: Read the source file carefully. Distinguish facts, claims/interpretations, open questions, and entities/concepts/topics to link. When the source is large, read enough for a faithful summary and use targeted follow-up reads for the most relevant sections.

**Chat-context mode**: Synthesize the current conversation context. Distinguish decisions/facts, claims not fully settled, open questions, and entities/concepts/topics to link. Err toward uncertainty. Prefer "X was discussed as a likely approach" over "X is the approach."

---

## Step 2 — Update memory incrementally

Treat memory as a compiled artifact that must stay internally coherent.

### A. Related pages

Synthesize the source's content directly into the most relevant topic, entity, concept, or analysis pages. Graph fan-out is required, not optional.

1. Only touch pages materially affected by the source.
2. If an important entity, concept, or topic lacks a dedicated page, create a minimal canonical note.
3. Prefer small linked updates over copying the same summary text into many pages.
4. If the new source contradicts an existing page, make the conflict explicit.
5. If the new source supersedes an old claim, say so clearly.
6. Update reciprocal links under `Related pages` or `Mentioned in` when materially useful.
7. Avoid isolated durable notes. Every new note should be reachable from `index.md` or another durable note.
8. For file mode: note the source path in relevant pages where it materially aids retrieval (e.g., in a `## References` or `## Source material` section).
9. For chat-context mode: mark conversation-sourced claims with appropriate uncertainty (discussed, suggested, agreed in conversation).

Minimal new-note shape:

```md
# Human Readable Title

## Summary

<short durable summary>

## Related pages

- [[another-note]]
```

### B. Index

Update `index.md` so every durable page touched remains discoverable with a one-line description. Use grouped `[[wikilinks]]` where appropriate.

### C. Log

Append a new entry to `log.md`:

**File mode:**
```md
## [YYYY-MM-DD] add (file) | <source title>
```

**Chat-context mode:**
```md
## [YYYY-MM-DD] add (chat) | <topic>
```

Capture: the source path or conversation topic, pages created or updated, important contradictions, gaps, or follow-up leads.

### D. Refresh qmd after writes

If qmd was ready and you wrote to the wiki, run `qmd update -c <collection> 2>/dev/null`. If refresh fails, report it but do not roll back wiki edits.

---

## Step 3 — Preserve uncertainty

- For file mode: note the source path in relevant pages where it materially aids retrieval.
- For chat-context mode: note the conversation date in relevant pages where appropriate.
- Make uncertainty explicit.
- Separate the source's claims from memory (wiki substrate)'s cross-source synthesis when they differ.
- Avoid overstating weak evidence.
- Never rewrite raw-source files.

---

## Step 4 — Report back

```md
Wiki updated from <source path or conversation topic>

### Mode

- file | chat

### Touched pages

- <path>

### New pages

- <path>

### Index and log

- Index: <path>
- Log: <path>

### Open questions

- <question or "none">

### Next useful command

- `/loam::adding-to-memory <another local source path or topic>`
```

If the source was already represented and you refreshed it, say so explicitly.

---

## Rules

- File mode: local files only. Do not fetch URLs.
- Chat-context mode: synthesize from the current conversation context only.
- Prefer one source per run.
- Read the wiki schema before editing.
- If qmd is ready, use it for finding existing related notes; otherwise fall back to Grep/Glob.
- Never edit a wiki page based only on qmd output. Always read the actual wiki files first.
- Durable category notes use canonical kebab-case filenames.
- Internal note links use `[[kebab-case-note-name]]`.
- Update `index.md` and `log.md` on every add.
- Raw-source files are immutable.
- Do not leave avoidable broken wikilinks after the add pass.
- Strengthen reciprocal links when the relationship is materially useful.
- Make contradictions and stale claims explicit.
- Prefer incremental linked updates over large rewrites.
- In chat-context mode, conversation-sourced claims carry less authority. Mark uncertainty explicitly.
- After wiki writes, refresh qmd if the collection is ready. If refresh fails, report it but do not roll back.