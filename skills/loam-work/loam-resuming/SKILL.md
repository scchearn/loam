---
name: loam::resuming
description: "Use when resuming work after a pause, reboot, or context switch and the workspace uses `wiki/checkpoints/` resumable notes. Read the latest relevant checkpoint chain, orient to the most likely in-flight scope, verify current files and tools before acting, and report the safest next step. Treats a goal path as concrete context; checks live goal status over stale checkpoints."
allowed-tools: Read Glob Grep Bash
metadata:
  version: "1.3.0"
  author: scchearn
  argument-hint: "[optional hint or focus]"
---

You are a senior engineer resuming work from checkpoint notes in the current workspace. Your job is to find the most relevant recent checkpoint chain, extract only restart-relevant state, verify the live workspace before taking action, and tell the user the safest next step.

This is a **read-only resume** skill. Never edit checkpoint notes, plans, or source files in this skill. Empty checkpoint-lane directory creation is allowed when no lane exists, so a subsequent loam::checkpointing call has somewhere to land.

## Input

Optional hint: $ARGUMENTS

If no hint is provided, derive the likely resume target from the current session context and workspace state.

---

## Step 1 — Locate checkpoints and derive the current resume context

1. **Resolve the wiki root via `loamstate`** (git-agnostic; Glob respects `.gitignore` and `.git/info/exclude`, so `wiki/`, `specs/`, `plans/` are invisible to Glob when projects locally ignore them):

   ```bash
   bash "${LOAM_SKILL_DIR:-${CLAUDE_SKILL_DIR}}/../loam-using/scripts/loamstate.sh" "$(pwd)" 2>/dev/null \
     || powershell "${LOAM_SKILL_DIR:-${CLAUDE_SKILL_DIR}}/../loam-using/scripts/loamstate.ps1" "$(pwd)" 2>/dev/null
   ```

   Parse the JSON `wiki_root`. Treat empty `wiki_root` as "no wiki," not as an error. Runtime guard: if `loamstate` fails or returns invalid JSON, fall back to testing `wiki/SCHEMA.md`, `wiki/index.md`, `wiki/log.md` with `Read` (filesystem open, git-agnostic) — **do not use Glob to discover the wiki root**.

   A `resume_available` or `resume_stale` hint in the `loamstate` output is the advisory signal for this skill (see the hint contract in `loam::using`); `resume_stale` means the latest checkpoint is over 24h old, so verify live state extra carefully.

2. **List checkpoints with `ls`, not Glob** (same gitignore caveat applies to checkpoint files). From the resolved `<wiki_root>`:

   ```bash
   ls -1 "$WIKI_ROOT/checkpoints/checkpoint-"*.md 2>/dev/null | sort -r | head -5
   ```

   `ls` is a direct filesystem call and ignores git ignore state. Sort is by filename-timestamp prefix, not mtime (sync clients rewrite mtimes). If the glob expands to nothing, also try `ls -1 "$WIKI_ROOT/checkpoints/" | grep '^checkpoint-' | sort -r | head -5` to catch slugged legacy filenames like `checkpoint-YYYY-MM-DD-HHMM-<slug>.md`.

3. When no `<wiki_root>/checkpoints/` directory exists, create it (or fall back to `notes/checkpoints/` if wiki is undesired) so a future loam::checkpointing call has a place to land. If both `ls` attempts return no checkpoint files, stop and say there is nothing to resume.
4. Read only the newest **3-5** checkpoint notes from the `ls` output (sorted by the filename timestamp prefix: `checkpoint-YYYY-MM-DD-HHMM.md`, suffixed collision files like `checkpoint-YYYY-MM-DD-HHMM-2.md`, and legacy slugged files), not by mtime.
5. Derive the current resume context from:
   - the current conversation/session
   - the optional hint, if present
   - the concrete files, plans, specs, notes, or tools already in play

Do not scan the whole wiki. Resume is a recent-state workflow, not an archive search. Do not use Glob for checkpoint or wiki-root discovery — git-ignored wikis silently return zero matches.

---

## Step 2 — Choose the best checkpoint candidate

Evaluate the recent checkpoints newest-first.

Use **semantic-first matching**, but only accept a strong match when there is also at least one concrete overlap with the current resume context, such as:

- the same plan/spec/task file
- the same file cluster or repo area
- the same tool/system/service
- the same thread or named artifact family

### Confidence rules

- **High** — semantic scope match is clear and there is concrete overlap.
- **Medium** — semantic match is plausible and there is at least one weaker concrete overlap.
- **Low** — no strong overlap; the newest checkpoint is only the best available guess.

If multiple recent checkpoints look similarly plausible, keep confidence low and say so.

---

## Step 3 — Read the checkpoint chain

1. Start with the best candidate checkpoint.
2. Read its `Previous` link when present.
3. Default to the latest checkpoint plus **one** `Previous` note.
4. Hard maximum: **3** notes total in the chain.
5. If a note is clearly superseded by a later note already in the chain, prefer the later note.
6. Read the checkpoint's `Reason` field. If `Reason: shutdown`, treat pointers under `/tmp`, env-dependent paths, or session-local paths as volatile — do not assume they survive into the resumed session. Promote them in the report so the user can decide whether to re-anchor them.

Do not turn the chain into history reconstruction. Read only enough to restart safely.

---

## Step 4 — Synthesize the restart brief

From the chosen checkpoint chain, extract only:

- the inferred scope
- the user's intended return, or exactly `none recorded` when absent
- workstreams with Status: active, blocked, or ready-to-resume. Also surface any Status: waiting blockers prominently in the report.
- the immediate `Next` action
- the concrete pointers worth opening first
- blockers or uncertainty that still matter

Treat checkpoint notes as orientation, not authority. They say where work likely stopped, not whether the current world still matches. Do not infer return intent from workstream `Next`; if `Intended return` is absent, say so and use `Next` only as the operational restart action.

---

## Step 5 — Verify live state before acting

Before recommending action, verify the checkpoint against current evidence.

Verify only what the checkpoint would cause you to act on, such as:

- does `Intended return` still fit the live workspace state, when one was recorded
- do referenced files still exist
- does the cited plan/spec/note still match the checkpoint summary
- do named tools, services, or threads still appear current. Treat empty `hcom events --thread <name>` results as "thread idle/expired — re-open or re-delegate," not as an error. The thread may have been pruned or never persisted; do not fail the resume.
- is the claimed blocker or next step still plausible from live workspace state
- if the checkpoint or plan references a goal path and the file is missing or unreadable, report the path and stop. Otherwise read live status; do not resume a `paused`, `achieved`, or `abandoned` goal unless explicitly authorized

If the live state conflicts with the checkpoint, say so directly and prefer the live state.

---

## Step 6 — Report back

Output a compact restart brief in this shape:

```md
### Resume

Return intent: <intent | none recorded>.

Pick up `<scope>` by <single next action>.

Open first: <1-3 paths/tools/threads>.

Note: <only blocker/mismatch/uncertainty, or omit if none>
```

If confidence is low, or the checkpoint chain is stale or ambiguous, say that before suggesting action.

If `Intended return` exists, surface it as `Return intent:` and treat it as the preferred statement of what the user expected to do first, unless live verification clearly contradicts it. If it is absent, write `Return intent: none recorded.` and use the checkpoint `Next:` value for `Pick up ... by ...`; mention that basis in `Note:` when it prevents confusion.

---

## Rules

- Read only the newest 3-5 checkpoint notes when choosing a candidate.
- Use semantic scope matching, but require at least one concrete overlap for a strong match.
- Default to the latest checkpoint plus one `Previous` note; never read more than 3 notes total.
- Always surface `Return intent:`. If no checkpoint records `Intended return`, say `none recorded` rather than inferring it from `Next:`.
- Verify live files and tools before recommending action.
- Trust live workspace state over checkpoint notes when they conflict.
- Never edit checkpoint notes, plans, or source files. Directory creation for empty checkpoint lanes is allowed when no lane exists.
- Keep the output focused on the safest concrete restart step, not a long narrative.
