# Guidance File Templates

## Key Principles

- **Concise**: Dense, human-readable content; one line per concept when possible
- **Actionable**: Commands should be copy-paste ready
- **Project-specific**: Document patterns unique to this project, not generic advice
- **Current**: All info should reflect actual codebase state

---

## Recommended Sections

Use only the sections relevant to the project. Not all sections are needed.

### Commands

Document the essential commands for working with the project.

```markdown
## Commands

| Command | Description |
|---------|-------------|
| `<install command>` | Install dependencies |
| `<dev command>` | Start development server |
| `<build command>` | Production build |
| `<test command>` | Run tests |
| `<lint command>` | Lint/format code |
```

### Architecture

Describe the project structure so an agent understands where things live.

```markdown
## Architecture

```
<root>/
  <dir>/    # <purpose>
  <dir>/    # <purpose>
  <dir>/    # <purpose>
```
```

### Key Files

List important files that an agent should know about.

```markdown
## Key Files

- `<path>` - <purpose>
- `<path>` - <purpose>
```

### Code Style

Document project-specific coding conventions.

```markdown
## Code Style

- <convention>
- <convention>
- <preference over alternative>
```

### Environment

Document required environment variables and setup.

```markdown
## Environment

Required:
- `<VAR_NAME>` - <purpose>
- `<VAR_NAME>` - <purpose>

Setup:
- <setup step>
```

### Testing

Document testing approach and commands.

```markdown
## Testing

- `<test command>` - <what it tests>
- <testing convention or pattern>
```

### Gotchas

Document non-obvious patterns, quirks, and warnings.

```markdown
## Gotchas

- <non-obvious thing that causes issues>
- <ordering dependency or prerequisite>
- <common mistake to avoid>
```

### Workflow

Document development workflow patterns.

```markdown
## Workflow

- <when to do X>
- <preferred approach for Y>
```

---

## Template: Project Root (Minimal)

```markdown
# <Project Name>

<One-line description>

## Commands

| Command | Description |
|---------|-------------|
| `<command>` | <description> |

## Architecture

```
<structure>
```

## Gotchas

- <gotcha>
```

---

## Template: Project Root (Comprehensive)

```markdown
# <Project Name>

<One-line description>

## Commands

| Command | Description |
|---------|-------------|
| `<command>` | <description> |

## Architecture

```
<structure with descriptions>
```

## Key Files

- `<path>` - <purpose>

## Code Style

- <convention>

## Environment

- `<VAR>` - <purpose>

## Testing

- `<command>` - <scope>

## Gotchas

- <gotcha>
```

---

## Template: Package/Module

For packages within a monorepo or distinct modules.

```markdown
# <Package Name>

<Purpose of this package>

## Usage

```
<import/usage example>
```

## Key Exports

- `<export>` - <purpose>

## Dependencies

- `<dependency>` - <why needed>

## Notes

- <important note>
```

---

## Template: Monorepo Root

```markdown
# <Monorepo Name>

<Description>

## Packages

| Package | Description | Path |
|---------|-------------|------|
| `<name>` | <purpose> | `<path>` |

## Commands

| Command | Description |
|---------|-------------|
| `<command>` | <description> |

## Cross-Package Patterns

- <shared pattern>
- <generation/sync pattern>
```

---

## Template: Memory (Loam memory map)

**Loam-owned generated region.** Loam owns the bytes between the two markers and
overwrites them on every regeneration; hand-edits inside the markers are lost.
The `## Memory` heading and the prose above the opening marker are seeded once
and stay human-editable. Only add this section when the workspace has a `wiki/`.

The two marker strings are a fixed contract shared with `loam lint --only
guidance`, `loam::scaffolding-wiki`, and `loam::normalizing-memory`. Reproduce
them byte-for-byte — a changed marker makes the region invisible to the runtime.

````markdown
## Memory

This project keeps a **Loam memory** — agent-owned markdown in `wiki/`. Consult it
before non-trivial work and keep it current. Start at `wiki/index.md`.

<!-- loam:memory-map · generated from wiki/index.md · do not edit by hand -->
Topics (24): authentication · connector-hardening · pricing-strategy … (+21 more, see index.md)
Entities (3): ovhcloud · second-spectrum · skillcorner
Concepts (1): kitman-labs-oauth-scopes
Analyses (2): chelsea-vendor-integration-learnings · gtm-assessment
Code graph: 429 pages → wiki/code/_index.md
<!-- /loam:memory-map -->
````

### Generation rules

The region is a pure function of the wiki page tree, so two agents generating it
independently produce the same bytes:

| Rule | Detail |
|------|--------|
| Groups and order | `Topics` (`wiki/topics/`), `Entities` (`wiki/entities/`), `Concepts` (`wiki/concepts/`), `Analyses` (`wiki/analyses/`) — always this order |
| Slug | The file stem, e.g. `wiki/topics/authentication.md` → `authentication` |
| Excluded | `_index.md` (reserved hub name), `index.md`, `SCHEMA.md`, `log.md`, `overview.md`, and anything under `.archive/`, `.obsidian/`, `checkpoints/` |
| Sort | Kebab-lexical ascending within each group |
| Empty groups | Omitted entirely — never rendered as `Topics (0):` |
| Line shape | `Topics (N): slug · slug · slug` (middle dot `·`, spaced) |
| Truncation | Over 30 slugs: list the first 30, then ` … (+M more, see index.md)` where M is the remainder |
| Code pointer | `Code graph: N pages → wiki/code/_index.md`, where N counts `wiki/code/*.md` excluding `_index.md`; omitted when there are no code pages |
| Empty wiki | The two markers with nothing between them — still valid, still current |

Descriptions never appear here. The three disclosure altitudes are: this block
says *what exists*, `wiki/index.md` says *what each page is about*, the page
itself carries the synthesis.

---

## Template: CLAUDE.md (Import Shim)

`CLAUDE.md` is not a content file. It contains exactly one line — the import that makes Claude Code read `AGENTS.md`:

```markdown
@AGENTS.md
```

That's it. No title, no sections, no commands, no gotchas. All shared guidance lives in `AGENTS.md`. Claude-specific additions (if any) go in `.claude/rules/*.md` (team-shared, path-scoped) or `.claude.local.md` (personal, gitignored).

If you find a `CLAUDE.md` with content beyond `@AGENTS.md`, it has drifted. Propose collapsing it back to `@AGENTS.md` only (after moving any unique content to the right place).

---

## Update Principles

When updating any guidance file:

1. **Be specific**: Use actual file paths, real commands from this project
2. **Be current**: Verify info against the actual codebase
3. **Be brief**: One line per concept when possible
4. **Be useful**: Would this help a new agent session understand the project?
