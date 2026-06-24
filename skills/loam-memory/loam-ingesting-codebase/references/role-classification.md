# Role Classification Rubric

Classify each code file into exactly one of five roles before summarizing it. One role per file. When ambiguous, pick the role matching the file's primary export or primary intent.

## Decision tree

```
Is the file a test or spec file?
  Path or filename contains test, spec, __tests__, .test., .spec., _test.?
  → YES → role: test
  → NO ↓

Does the file primarily define types, interfaces, models, schemas, or classes?
  Most exports are type definitions, interfaces, ORM models, Zod/JSON schemas,
  protobuf definitions, or class definitions with little logic?
  → YES → role: type
  → NO ↓

Does the file expose an API endpoint, HTTP route, GraphQL resolver, RPC handler,
controller, or view?
  Contains route definitions, HTTP method decorators, handler functions tied to
  a path/method, or a framework's controller/view pattern?
  → YES → role: service
  → NO ↓

Does the file primarily define constants, configuration values, or environment
variable mappings that other code depends on?
  Most exports are const values, config objects, or env var reads — not functions
  with logic?
  → YES → role: config
  → NO ↓

Otherwise: the file contains functions, helpers, or business logic that other
code calls.
  → role: utility
```

## Role descriptions

### service (Service / Handler)
Files exposing an API surface: Express routes, Django views, Spring controllers, FastAPI endpoints, GraphQL resolvers, RPC handlers. Captures: what the service does, HTTP method + path (if applicable), what it calls, what calls it.

### utility (Utility / Function)
Pure functions, helpers, business logic, computed values. Captures: signature, what it does, dependencies, edge cases.

### type (Type / Schema / Model)
Type definitions, interfaces, DB models, ORM classes, Zod/JSON schemas, protobuf definitions. Captures: the shape (fields and types), what it represents, relations, what uses it.

### config (Config / Constants)
Semantic config and constants worth ingesting: CORS allow-lists, feature flags, default values, env var mappings. Captures: what values are defined, what they're for, what uses them. Excludes framework config (tsconfig, webpack) — those are noise and already filtered by the exclusion list.

### test (Test)
Test and spec files. Captures: what is being tested, which cases are covered (coverage description, not test code), which module the tests target.

## Edge cases

- **Mixed file** (types + utilities in one file): classify by the primary export. If `export type User` and `export function getUser` coexist and `getUser` is the main export, classify as `utility` and note the type in the `## Depends on` section.
- **Barrel file** (re-exports only): classify as `utility`. The summary notes it is a barrel/index file listing what it re-exports.
- **Empty or trivial file** (license header, single re-export): still ingest. The summary notes the file is minimal.
- **Generated code that slipped past exclusions**: classify normally. The summary notes the file appears generated.