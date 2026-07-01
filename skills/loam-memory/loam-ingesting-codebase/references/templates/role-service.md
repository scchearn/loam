# Role Template: service

Use this template for files that expose an API surface: Express routes, Django views, Spring controllers, FastAPI endpoints, GraphQL resolvers, RPC handlers. Replace `<...>` placeholders with extracted content.

```md
---
source_path: <relative-path-from-codebase-root>
ingested_at: <source-file-mtime-epoch>
source_size: <bytes>
content_hash: <sha256-hex>
---

# <PrimaryExportOrRouteName>

## Signature

<HTTP method and path if applicable, or function/route signature>
e.g. `POST /api/auth/login (req, res) => Promise<User>`
e.g. `loginRoute(app: Express): void`

## Summary

<one or two sentences: what this service/handler does>

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

- <what goes wrong and how it's handled: returns 4xx, throws, logs and continues, etc.>
- <failure mode>
```

## Extraction notes

- **Name**: use the route name, handler function name, or controller method name.
- **Signature**: for HTTP handlers, include the method and path. For function-based handlers, include the function signature.
- **What it does**: describe the intent (authenticate, validate, transform, persist), not the line-by-line implementation.
- **Dependencies**: list imported modules, called functions, and middleware. Resolve to `[[slug]]` when the dependency exists as an entity page; mark `(external)` otherwise.
- **Callers**: initially sparse. Backlinks from other entity pages populate this over time. Add known callers if discoverable from imports.
- **Failure modes**: HTTP status codes, thrown errors, null returns, logged-and-swallowed errors.
