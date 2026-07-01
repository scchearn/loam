# Role Template: type

Use this template for files defining types, interfaces, DB models, ORM classes, Zod/JSON schemas, or protobuf definitions. Replace `<...>` placeholders with extracted content.

```md
---
source_path: <relative-path-from-codebase-root>
ingested_at: <source-file-mtime-epoch>
source_size: <bytes>
content_hash: <sha256-hex>
---

# <TypeName>

## Shape

<the fields and their types, as a list or code block>
e.g.
```ts
interface User {
  id: string
  email: string
  role: 'admin' | 'member'
  createdAt: Date
}
```

## Summary

<one or two sentences: what this type/model represents>

## Relations

- [[<related-type-slug>]] — <relation: has-many, belongs-to, extends, implements, etc.>
- <external-relation> (external) — <relation>

## Used by

<!-- Backlink-driven. List known consumers here. -->

- [[<consumer-slug>]] — <context>
- (backlinks will populate as other nodes link here)

## Edge cases

- <validation constraints, nullable fields, default values, serialization quirks>
- <edge case>
```

## Extraction notes

- **Name**: use the type/interface/class/schema name.
- **Shape**: reproduce the fields and types faithfully. For large types, include the most important fields and note the count of omitted fields.
- **Relations**: foreign keys, extends/implements, composition. Resolve to `[[slug]]` when the relation exists as an entity page.
- **Used by**: initially sparse. Backlinks populate as service/utility pages link to the type.
- **Edge cases**: validation rules, optional vs required, defaults, serialization behavior, ORM constraints.
