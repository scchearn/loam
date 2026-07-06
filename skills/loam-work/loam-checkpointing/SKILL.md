---
name: loam::checkpointing
description: "Use when pausing, shutting down, handing off, or context-switching active work and future sessions need a compact resumable checkpoint derived from the current session context. Writes a small checkpoint note under wiki/checkpoints/ and then optionally records the user's intended return step. Not for durable learnings capture, wiki correction, or source ingestion."
allowed-tools: Read Glob Grep Write Edit Bash
metadata:
  version: "1.1.2"
  author: scchearn
  argument-hint: "[optional intended return]"
---

You are a senior engineer capturing a durable restart checkpoint for the current workspace. Your job is to write a compact, self-sufficient note that lets a future session pick up the work without reconstructing the whole conversation.

A checkpoint is an operational restart artifact, not a transcript, diary, or durable knowledge note.

## Non-negotiables

- Always write the checkpoint note **before** any further reads or questions.
- The note must be resumable even if the user closes the app and never answers the follow-up.
- Write exactly one new checkpoint note per invocation.
- Never append to an older checkpoint note as the primary artifact.
- Never update `wiki/log.md` from this skill.
- Never update `wiki/index.md` by default from this skill.
- Keep the note small but powerful: every line must change what a cold session does first. Inline only state that exists nowhere but this session; point to everything else. Stop when each workstream has one concrete `Next` and its pointers.
- Use pointers, not transcript dumps or chronology.
- Scripts in `scripts/` are accelerators; the prose path is the contract. A broken or missing helper must never block a save.

## Input

Optional intended return: $ARGUMENTS

If an argument is given, treat it as guidance for what the user expects to do first when they return. If no argument is given, derive the reason from the current situation.

---

## Step 1 — Resolve memory checkpoint lane

1. Locate the existing wiki root by looking for files such as `wiki/SCHEMA.md`, `wiki/index.md`, or `wiki/log.md`.
2. If the workspace uses a different but clearly established wiki root, reuse it.
3. If no wiki exists, create `wiki/` and `wiki/checkpoints/` (or fall back to `notes/checkpoints/` if wiki is undesired), then write the note and surface the gap in the report. The skill must never refuse to save.

4. Read `<wiki root>/SCHEMA.md` when present so naming and wiki conventions stay aligned.
5. Ensure `<wiki root>/checkpoints/` exists. Create it if the wiki root exists but the checkpoints directory does not.
6. If the workspace is a git repo and `<wiki root>/checkpoints/` is not already gitignored, add `checkpoints/` (relative to the wiki root) to the workspace `.gitignore`. Checkpoints are session state for the current machine, not durable project history; committing them pollutes `git log` with pause/resume noise and floods `git status` every session. A user who wants cross-machine resume via git can remove the entry — but the default is local-only.

Treat `<wiki root>/checkpoints/` as the checkpoint lane for this skill.

---

## Step 2 — Derive the checkpoint from current session context

Checkpoint from the **current session first**:

- current conversation state
- files, plans, notes, or threads already in play
- the optional intended-return argument, if provided

Do not browse widely. Read only what is immediately relevant to produce a trustworthy restart artifact.

### Derive the note header

Derive these fields:

- `Captured` — current local timestamp with timezone
- `Reason` — `shutdown`, `pause`, `handoff`, or `context-switch`
- `Scope` — one short line naming the dominant in-flight work lane

`Scope` should be semantic and human-readable, not a rigid key. Prefer phrases like:

- `loam::checkpointing and loam::resuming skill design`

If the session covers multiple lanes, choose the dominant lane and preserve the others as separate workstreams inside the same note.

If no single lane is dominant, write the most honest short scope you can. Do not stop only because the scope is imperfect.

### Build the workstreams

Create `1-3` workstreams. If the session has more than 3 lanes, keep the top 3 workstreams and add one header line `- Dropped lanes: <comma-separated names>` so resume knows state was truncated, not absent. Each workstream must contain:

- `Status` — `active`, `blocked`, `waiting`, `ready-to-resume`, or `done`
- `Next` — one concrete restart action
- `Pointers` — up to `3` files, notes, threads, tasks, or artifacts
- `Blocker` — only when needed

Rules:

- max `4` bullets per workstream
- `Next` may be one action sequence, as concrete as needed; all other bullets one sentence
- no diary text
- no repeated context already covered by a pointer
- if a detail does not change restart behavior, cut it

When relevant, capture as pointers inside the workstream:

- `hcom thread <name> (pending: #<id>)` — never agent names; they are regenerated each session
- delegated work not yet returned, described by task ("a reviewer was auditing X") — never by delegate name
- TaskWarrior **UUIDs** or a `project:` filter — never bare IDs (they renumber on completion)
- files edited but unfinished this session
- flag session-local or `/tmp` pointers `(volatile)`
- open `hcom listen` / `events sub` state goes in `Status: waiting` + `Blocker` (record the awaited condition, not the subscription)
- run `scripts/checkpoint-state [--window MIN]` to get a pre-filtered digest of hcom threads, TaskWarrior items, and recently-touched files; select from its output rather than re-querying inline
- relative pointer paths must be unambiguous from the workspace root; when the workspace has multiple top-level subprojects with similar directory structure, prefix with the subproject name (e.g., `aenon-local-business-website-pipeline/workflows/x.ts`, not just `workflows/x.ts`)

If `Next` is vague, the checkpoint failed.

---

## Step 3 — Write the checkpoint note immediately

Write the checkpoint note **before** any further reads or questions.

Filename pattern:

```text
<wiki root>/checkpoints/checkpoint-YYYY-MM-DD-HHMM.md
```

If that filename already exists, append the smallest suffix needed rather than overwriting, for example `checkpoint-YYYY-MM-DD-HHMM-2.md`.

Use this shape:

```md
# Checkpoint

- Captured: YYYY-MM-DD HH:MM ±HH:MM
- Reason: shutdown | pause | handoff | context-switch
- Scope: <short scope>
- Format: v1
- Previous: [[checkpoint-...]]        <!-- omit Previous/Supersedes when unused; never write 'none' -->
- Supersedes: [[checkpoint-...]]      <!-- optional -->

## Workstreams

### <title>
- Status: <active | blocked | waiting | ready-to-resume | done>
- Next: <single concrete restart action>
- Pointers: <up to 3 links/paths>
- Blocker: <optional>
```

Do not add `Intended return` yet. That comes only after the note is safely written.

- After writing, optionally run `scripts/checkpoint-verify <note>` as a non-blocking self-check. Treat its output as orientation, not as a gate.

---

## Step 4 — Link to a recent related checkpoint when justified

Glob the newest 1-2 existing checkpoints by filename (`checkpoint-YYYY-MM-DD-HHMM.md`, `checkpoint-YYYY-MM-DD-HHMM-2.md`, and legacy `checkpoint-YYYY-MM-DD-HHMM-<slug>.md`). If one covers the same work lane (same plan/files/thread, not just topic words), add `- Previous: [[note]]`; add `- Supersedes: [[note]]` too if the old note can be skipped on normal resume. Never link more than one. Omit the fields entirely when unused — never write "none". Treat slugged filenames as legacy only; never infer scope from the slug.

The previous-note matching is best-effort: do not block the write on it.

---

## Step 5 — Ask one optional follow-up question

After the note is written, if `$ARGUMENTS` is non-empty, update the same note by adding one short top-level field near the header and skip the question:

```md
- Intended return: <argument text>
```

If `$ARGUMENTS` is empty, ask the user exactly one short question — via the harness's interactive question tool if one exists, otherwise as plain text at the end of the turn:

> "When you come back, what do you think you'll do first? Press enter to keep my inferred next step."

Rules:

- ask once only
- if the user does not answer, do not re-prompt
- if the user closes the app, the checkpoint is already complete
- keep the follow-up update tiny and append-safe
- if the session is non-interactive (headless, scripted), skip the question entirely — the checkpoint is already complete

If the user answers, update the same note by adding one short top-level field near the header:

```md
- Intended return: <user answer>
```

Do not rewrite the workstreams just because the user answered.

---

## Step 6 — Report back

Return a concise summary. Do not narrate the whole session back to the user.
