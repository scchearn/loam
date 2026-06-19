# Guidance File Update Guidelines

## Core Principle

Only add information that will genuinely help future agent sessions. The context window is precious - every line must earn its place.

## What TO Add

### 1. Commands/Workflows Discovered

```markdown
## Build

`npm run build:prod` - Full production build with optimization
`npm run build:dev` - Fast dev build (no minification)
```

Why: Saves future sessions from discovering these again.

### 2. Gotchas and Non-Obvious Patterns

```markdown
## Gotchas

- Tests must run sequentially (`--runInBand`) due to shared DB state
- `yarn.lock` is authoritative; delete `node_modules` if deps mismatch
```

Why: Prevents repeating debugging sessions.

### 3. Package Relationships

```markdown
## Dependencies

The `auth` module depends on `crypto` being initialized first.
Import order matters in `src/bootstrap.ts`.
```

Why: Architecture knowledge that isn't obvious from code.

### 4. Testing Approaches That Worked

```markdown
## Testing

For API endpoints: Use `supertest` with the test helper in `tests/setup.ts`
Mocking: Factory functions in `tests/factories/` (not inline mocks)
```

Why: Establishes patterns that work.

### 5. Configuration Quirks

```markdown
## Config

- `NEXT_PUBLIC_*` vars must be set at build time, not runtime
- Redis connection requires `?family=0` suffix for IPv6
```

Why: Environment-specific knowledge.

## What NOT to Add

### 1. Obvious Code Info

Bad:
```markdown
The `UserService` class handles user operations.
```

The class name already tells us this.

### 2. Generic Best Practices

Bad:
```markdown
Always write tests for new features.
Use meaningful variable names.
```

This is universal advice, not project-specific.

### 3. One-Off Fixes

Bad:
```markdown
We fixed a bug in commit abc123 where the login button didn't work.
```

Won't recur; clutters the file.

### 4. Verbose Explanations

Bad:
```markdown
The authentication system uses JWT tokens. JWT (JSON Web Tokens) are
an open standard (RFC 7519) that defines a compact and self-contained
way for securely transmitting information between parties as a JSON
object. In our implementation, we use the HS256 algorithm which...
```

Good:
```markdown
Auth: JWT with HS256, tokens in `Authorization: Bearer <token>` header.
```

## What to Remove

The audit is two-directional. Removing stale content is as important as adding missing content. A guidance file that only grows becomes noise.

### 1. Commands That No Longer Exist

Test each documented command. If it fails, propose removal.

```bash
which <tool>           # CLI tool no longer installed?
grep <script> package.json  # npm script removed?
ls <path>              # referenced path deleted?
```

### 2. Gotchas for Fixed Issues

Workarounds for bugs that have been fixed in code. If the issue is resolved, the gotcha is dead weight.

### 3. Env Vars No Longer Used

Check if env vars referenced in the guidance file are still used in config files (`grep -r ENV_VAR .`). If not referenced anywhere, propose removal.

### 4. References to Deleted Files/Dirs

Any path mention that no longer exists. Run `ls` on each referenced path.

### 5. Duplicated Info

Same content in two sections, or same info in both `AGENTS.md` and `CLAUDE.md`. Keep the better version, remove the other.

### 6. Generic Advice

Anything an agent would know without the file. "Always write tests" or "Use meaningful variable names" — these don't earn their place in a project-specific guidance file.

### 7. Verbose Explanations

Paragraphs where a one-liner would do. Condense.

### Size Guard

If a root `AGENTS.md` exceeds 150 lines, or a package-level one exceeds 50, flag it. Suggest:
- Trim sections that are no longer earning their place
- Move deep reference material to the wiki / `references/` docs
- Consolidate overlapping sections

## Diff Format for Updates

For each suggested change:

### 1. Identify the File

```
File: ./AGENTS.md
Section: Commands (new section after ## Architecture)
```

### 2. Show the Change

```diff
 ## Architecture
 ...

+## Commands
+
+| Command | Purpose |
+|---------|---------|
+| `npm run dev` | Dev server with HMR |
+| `npm run build` | Production build |
+| `npm test` | Run test suite |
```

### 3. Explain Why

> **Why this helps:** The build commands weren't documented, causing
> confusion about how to run the project. This saves future agent sessions
> from needing to inspect `package.json`.

## Validation Checklist

Before finalizing an update, verify:

- [ ] Each addition is project-specific
- [ ] No generic advice or obvious info
- [ ] Commands are tested and work
- [ ] File paths are accurate
- [ ] Would a new agent session find this helpful?
- [ ] Is this the most concise way to express the info?
