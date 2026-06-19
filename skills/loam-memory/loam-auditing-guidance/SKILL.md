---
name: loam::auditing-guidance
description: "Audit, prune, and improve agent guidance markdown files in repositories. Use when the user asks to check, audit, update, improve, or fix AGENTS.md, CLAUDE.md, or related guidance files. Adds missing commands and gotchas, removes stale entries, deduplicates, and keeps the file small and relevant. Scan for guidance files, evaluate quality against templates, output a quality report, then make targeted updates after approval."
allowed-tools: Read Glob Grep Bash Edit
metadata:
  version: "0.1.1"
  author: scchearn
---

# Agent Markdown Improver

Audit, evaluate, and improve agent guidance markdown files across a codebase so future agent sessions have better project context.

**This skill can write to guidance files.** After presenting a quality report and getting user approval, it updates `AGENTS.md`, `CLAUDE.md`, or `.claude.local.md` with targeted improvements.

## Workflow

### Phase 1: Discovery

Find all guidance files in the repository:

```bash
find . \( -name "AGENTS.md" -o -name "CLAUDE.md" -o -name ".claude.local.md" \) 2>/dev/null | head -50
```

**File Types & Locations:**

| Type | Location | Purpose |
|------|----------|---------|
| Project root | `./AGENTS.md` | Primary shared project guidance across agent harnesses |
| Claude-specific | `./CLAUDE.md` | Shared Claude-specific guidance when the repo uses it |
| Local overrides | `./.claude.local.md` | Personal/local settings (gitignored, not shared) |
| Package-specific | `./packages/*/AGENTS.md` or `./packages/*/CLAUDE.md` | Module-level context in monorepos |
| Subdirectory | Any nested location | Feature/domain-specific context |

**Default update rule:** `AGENTS.md` is the canonical guidance file — all shared guidance goes there. `CLAUDE.md` must exist but contains only `@AGENTS.md` (Claude Code's import syntax). Never write content to `CLAUDE.md` — it is an import shim, not a content file. Use `.claude.local.md` for personal preferences only. If you find a `CLAUDE.md` with content beyond `@AGENTS.md`, flag it as drift (see Phase 1b below).

### Phase 1b: CLAUDE.md Shim Check

If a `CLAUDE.md` was found in Phase 1, read it and verify it contains only `@AGENTS.md` (Claude Code's import syntax). This is the canonical pattern:

- `AGENTS.md` = canonical shared guidance (the single source of truth)
- `CLAUDE.md` = thin import shim containing exactly one line: `@AGENTS.md`
- `.claude.local.md` = personal/local overrides (gitignored)
- `.claude/rules/` = Claude-specific path-scoped rules (if needed)

**If CLAUDE.md has content beyond `@AGENTS.md`:**
1. Check if the extra content is unique and valuable
2. If yes: propose moving it to `AGENTS.md` (if shared) or `.claude/rules/` (if Claude-specific and path-scoped) or `.claude.local.md` (if personal)
3. Then propose collapsing `CLAUDE.md` back to `@AGENTS.md` only
4. Flag this as drift in the quality report

**If CLAUDE.md is already just `@AGENTS.md`:** no action needed, it's compliant.

### Phase 2: Quality Assessment

For each guidance file, evaluate against quality criteria. See [references/quality-criteria.md](references/quality-criteria.md) for detailed rubrics.

**Quick Assessment Checklist:**

| Criterion | Weight | Check |
|-----------|--------|-------|
| Commands/workflows documented | High | Are build/test/deploy commands present? |
| Architecture clarity | High | Can an agent understand the codebase structure? |
| Non-obvious patterns | Medium | Are gotchas and quirks documented? |
| Conciseness | Medium | No verbose explanations or obvious info? |
| Currency | High | Does it reflect current codebase state? |
| Actionability | High | Are instructions executable, not vague? |

**Quality Scores:**
- **A (90-100)**: Comprehensive, current, actionable
- **B (70-89)**: Good coverage, minor gaps
- **C (50-69)**: Basic info, missing key sections
- **D (30-49)**: Sparse or outdated
- **F (0-29)**: Missing or severely outdated

### Phase 3: Quality Report Output

**ALWAYS output the quality report BEFORE making any updates.**

Format:

```
## Guidance File Quality Report

### Summary
- Files found: X
- Average score: X/100
- Files needing update: X

### File-by-File Assessment

#### 1. ./AGENTS.md (Project Root)
**Score: XX/100 (Grade: X)**

| Criterion | Score | Notes |
|-----------|-------|-------|
| Commands/workflows | X/20 | ... |
| Architecture clarity | X/20 | ... |
| Non-obvious patterns | X/15 | ... |
| Conciseness | X/15 | ... |
| Currency | X/15 | ... |
| Actionability | X/15 | ... |

**Issues:**
- [List specific problems]

**Recommended additions:**
- [List what should be added]

#### 2. ./packages/api/CLAUDE.md (Package-specific)
...
```

### Phase 4: Targeted Updates (Additions)

After outputting the quality report, ask user for confirmation before updating.

**All additions go to `AGENTS.md`.** Never write content to `CLAUDE.md` — it is an import shim (`@AGENTS.md` only). If Claude-specific content is needed, use `.claude/rules/` (team-shared, path-scoped) or `.claude.local.md` (personal).

**Update Guidelines (Critical):**

1. **Propose targeted additions only** - Focus on genuinely useful info:
   - Commands or workflows discovered during analysis
   - Gotchas or non-obvious patterns found in code
   - Package relationships that weren't clear
   - Testing approaches that work
   - Configuration quirks

2. **Keep it minimal** - Avoid:
   - Restating what's obvious from the code
   - Generic best practices already covered
   - One-off fixes unlikely to recur
   - Verbose explanations when a one-liner suffices

3. **Show diffs** - For each change, show:
   - Which guidance file to update
   - The specific addition (as a diff or quoted block)
   - Brief explanation of why this helps future agent sessions

**Diff Format:**

```markdown
### Update: ./AGENTS.md

**Why:** Build command was missing, causing confusion about how to run the project.

```diff
+ ## Quick Start
+
+ ```bash
+ npm install
+ npm run dev  # Start development server on port 3000
+ ```
```
```

### Phase 4b: Prune (Removals)

The audit is two-directional: add what's missing, remove what's stale. After proposing additions, scan the guidance file for content that should be removed or consolidated. See [references/update-guidelines.md](references/update-guidelines.md) "What to Remove" for the full criteria.

**Prune checks:**

1. **Validate commands.** For each documented command, check it still works:
   - `which <tool>` for CLI tools
   - `grep` in `package.json` scripts, `Makefile`, or equivalent
   - Check referenced file paths exist (`ls <path>`)
   - Flag any command that would fail

2. **Flag stale gotchas.** One-off fixes unlikely to recur, workarounds for issues long since fixed in code, env vars no longer referenced in config.

3. **Deduplicate.** Overlapping entries saying the same thing in different sections. Keep the better one, propose removing the other.

4. **Propose consolidation.** When sections grew organically and overlap, propose a merged version. Show the before/after.

5. **Size guard.** If a root `AGENTS.md` is over 150 lines (or a package-level one over 50), flag it. Suggest what to trim or move to the wiki / `references/` docs.

6. **Collapse drifted CLAUDE.md.** If Phase 1b found CLAUDE.md with content beyond `@AGENTS.md`, and the extra content has been moved (to AGENTS.md, `.claude/rules/`, or `.claude.local.md`), propose collapsing CLAUDE.md back to `@AGENTS.md` only.

**Prune diff format:**

```markdown
### Prune: ./AGENTS.md

**Why:** `grunt serve` command no longer exists — project migrated to Vite.

```diff
- `grunt serve` - Start development server
```

### Prune: ./AGENTS.md

**Why:** Gotcha for SSL issue was fixed in v2.1.0; no longer relevant.

```diff
- - SSL certificate errors on dev: set `NODE_TLS_REJECT_UNAUTHORIZED=0` (fixed in v2.1.0)
```
```

**Do not remove without showing the prune diff and getting user approval.** Same proposal-first rule as additions.

### Phase 5: Apply Updates

After user approval, apply changes using the Edit tool. Preserve existing content structure.

## Templates

See [references/templates.md](references/templates.md) for guidance file templates by project type.

## Common Issues to Flag

1. **Stale commands**: Build commands that no longer work → Phase 4b prune
2. **Missing dependencies**: Required tools not mentioned → Phase 4 addition
3. **Outdated architecture**: File structure that's changed → Phase 4b prune + Phase 4 addition
4. **Missing environment setup**: Required env vars or config → Phase 4 addition
5. **Broken test commands**: Test scripts that have changed → Phase 4b prune
6. **Undocumented gotchas**: Non-obvious patterns not captured → Phase 4 addition
7. **Duplicated info**: Same content in two places → Phase 4b deduplicate
8. **Overgrown file**: AGENTS.md over 150 lines → Phase 4b size guard

## User Tips to Share

When presenting recommendations, remind users:

- **Keep it concise**: Guidance files should be human-readable; dense is better than verbose
- **Actionable commands**: All documented commands should be copy-paste ready
- **Use `.claude.local.md`**: For personal preferences not shared with team (add to `.gitignore`)
- **Prefer `AGENTS.md`**: Use it for shared, harness-agnostic guidance when the repo does not already center on `CLAUDE.md`

## What Makes a Great Guidance File

**Key principles:**
- Concise and human-readable
- Actionable commands that can be copy-pasted
- Project-specific patterns, not generic advice
- Non-obvious gotchas and warnings

**Recommended sections** (use only what's relevant):
- Commands (build, test, dev, lint)
- Architecture (directory structure)
- Key Files (entry points, config)
- Code Style (project conventions)
- Environment (required vars, setup)
- Testing (commands, patterns)
- Gotchas (quirks, common mistakes)
- Workflow (when to do what)
