# Role Template: utility

Use this template for files containing pure functions, helpers, or business logic. Replace `<...>` placeholders with extracted content.

```md
---
source_path: <relative-path-from-codebase-root>
ingested_at: <source-file-mtime-epoch>
source_size: <bytes>
content_hash: <sha256-hex>
---

# <PrimaryExportName>

## Signature

<function signature with types>
e.g. `validateToken(token: string): User | null`
e.g. `formatCurrency(amount: number, currency: string): string`

## Summary

<one or two sentences: what this utility does>

## What it does

<intent, not full implementation. Bullet points for multi-step logic.>
- <step or responsibility>
- <step or responsibility>

## Dependencies

- [[<dependency-slug>]] — <what it's used for>
- <external-dependency-name> (external) — <what it's used for>

## Callers

<!-- Backlink-driven. List known callers here; Obsidian/qmd backlinks supplement this. -->

- [[<caller-slug>]] — <context>
- (backlinks will populate as other nodes link here)

## Failure modes

- <what goes wrong and how it's handled: returns null, throws TypeError, returns default value, etc.>
- <failure mode>
```

## Extraction notes

- **Name**: use the primary exported function name. If multiple utilities, use the most prominent or the filename as a fallback.
- **Signature**: include parameter types and return type.
- **What it does**: describe the algorithm or transformation at the intent level.
- **Dependencies**: list imported modules and called functions. Resolve to `[[slug]]` when the dependency exists as an entity page; mark `(external)` otherwise.
- **Callers**: initially sparse. Backlinks populate this over time.
- **Failure modes**: return values on error, thrown exceptions, edge cases.
