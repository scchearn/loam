# Guidance File Quality Criteria

## Scoring Rubric

### 1. Commands/Workflows (20 points)

**20 points**: All essential commands documented with context
- Build, test, lint, deploy commands present
- Development workflow clear
- Common operations documented

**15 points**: Most commands present, some missing context

**10 points**: Basic commands only, no workflow

**5 points**: Few commands, many missing

**0 points**: No commands documented

### 2. Architecture Clarity (20 points)

**20 points**: Clear codebase map
- Key directories explained
- Module relationships documented
- Entry points identified
- Data flow described where relevant

**15 points**: Good structure overview, minor gaps

**10 points**: Basic directory listing only

**5 points**: Vague or incomplete

**0 points**: No architecture info

### 3. Non-Obvious Patterns (15 points)

**15 points**: Gotchas and quirks captured
- Known issues documented
- Workarounds explained
- Edge cases noted
- "Why we do it this way" for unusual patterns
- Cross-references to repo-level canonical files (e.g., a root `DESIGN.md`) when one exists but isn't mentioned in AGENTS.md

**10 points**: Some patterns documented

**5 points**: Minimal pattern documentation

**0 points**: No patterns or gotchas

### 4. Conciseness (15 points)

**15 points**: Dense, valuable content
- No filler or obvious info
- Each line adds value
- No redundancy with code comments
- CLAUDE.md (if present) contains only `@AGENTS.md` — no duplicated content

**10 points**: Mostly concise, some padding

**5 points**: Verbose in places

**0 points**: Mostly filler or restates obvious code

### 5. Currency (15 points)

**15 points**: Reflects current codebase
- Commands work as documented
- File references accurate
- Tech stack current

**10 points**: Mostly current, minor staleness

**5 points**: Several outdated references

**0 points**: Severely outdated

### 6. Actionability (15 points)

**15 points**: Instructions are executable
- Commands can be copy-pasted
- Steps are concrete
- Paths are real

**10 points**: Mostly actionable

**5 points**: Some vague instructions

**0 points**: Vague or theoretical

### 7. Memory map (pass/fail, wiki-bearing workspaces only)

Not scored out of a weight — it is a gate that applies only when the workspace
has a `wiki/`. Workspaces without one skip this criterion entirely.

| Check | Pass |
|-------|------|
| Present | `AGENTS.md` carries both `loam:memory-map` markers under a `## Memory` heading |
| Current | Every slug between the markers matches the durable pages under `wiki/`, with nothing added or missing |
| Prose intact | The human-authored prose above the opening marker still reads correctly and was not clobbered by regeneration |
| Bounded | No category exceeds the 30-slug inline threshold without the ` … (+M more, see index.md)` truncation |
| Shim intact | `CLAUDE.md`, if present, is exactly `@AGENTS.md` — no memory-map content was written into it |

A failing Present or Current check is the same signal the runtime reports as
`guidance-map-missing` / `guidance-map-stale`; fix it via the ensure/regenerate
step rather than by hand-editing inside the markers.

## Assessment Process

1. Read the guidance file completely
2. Cross-reference with actual codebase:
   - Run documented commands (mentally or actually)
   - Check if referenced files exist
   - Verify architecture descriptions
3. Score each criterion
4. Calculate total and assign grade
5. List specific issues found
6. Propose concrete improvements

## Pruning Criteria

The currency score (section 5) feeds directly into prune proposals. When currency score is low, the file has stale content that should be removed.

### What counts as stale

- Commands for tools no longer installed (`which <tool>` fails)
- References to deleted files/dirs (`ls <path>` fails)
- Gotchas for issues fixed in code (the workaround is no longer needed)
- Env vars no longer referenced in any config file
- Tech stack references that no longer match `package.json` / `go.mod` / equivalent

### What counts as redundant

- Same info in two sections of the same file
- Same info in both `AGENTS.md` and `CLAUDE.md`
- Generic advice restated from standard tooling docs
- Info that's obvious from `package.json` or the repo README

### What counts as overgrown

- Root `AGENTS.md` over 150 lines — flag for trimming
- Package-level guidance over 50 lines — flag for trimming
- Sections that grew beyond their purpose (a one-liner list became paragraphs)
- Multiple sections covering the same domain

### Prune rules

1. Always show the removal as a diff (`-` lines)
2. Give a one-line "why" for each removal
3. Apply the removal directly — guidance is an agent-owned memory substrate and lives in git, so the diff in your report is the audit trail, not an approval request
4. Never remove without showing what's being removed

## Red Flags

- Commands that would fail (wrong paths, missing deps)
- References to deleted files/folders
- Outdated tech versions
- Copy-paste from templates without customization
- Generic advice not specific to the project
- "TODO" items never completed
- Duplicate info across multiple guidance files
- A repo-root `DESIGN.md` exists but AGENTS.md does not mention it (agents miss the canonical design system)
- A `wiki/` exists but AGENTS.md carries no `loam:memory-map` region (the memory is invisible at session start)
- Slugs between the markers no longer match `wiki/` (the map points at pages that moved or vanished)
- Hand-written content sits between the markers (it will be destroyed on the next regeneration — move it above the opening marker)
