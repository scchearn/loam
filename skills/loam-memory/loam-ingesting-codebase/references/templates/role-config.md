# Role Template: config

Use this template for files defining semantic constants, configuration objects, or environment variable mappings that other code depends on. Excludes framework config (tsconfig, webpack) — those are filtered by the exclusion list. Replace `<...>` placeholders with extracted content.

```md
---
source_path: <relative-path-from-codebase-root>
ingested_at: <source-file-mtime-epoch>
source_size: <bytes>
content_hash: <sha256-hex>
---

# <ConfigName>

## Defined values

<list the key constants or config values with their types and purposes>
e.g.
- `CORS_ORIGINS: string[]` — allowed CORS origins for the API
- `RATE_LIMIT_WINDOW_MS: number` — sliding window for rate limiting (default: 60000)
- `MAX_REQUESTS_PER_WINDOW: number` — max requests per window (default: 100)

## Summary

<one or two sentences: what this config controls or defines>

## Used by

<!-- Backlink-driven. List known consumers here. -->

- [[<consumer-slug>]] — <context>
- (backlinks will populate as other nodes link here)

## Notes

- <defaults, environment overrides, or gotchas>
- <e.g. "CORS_ORIGINS reads from process.env.CORS_ORIGINS, comma-separated">
```

## Extraction notes

- **Name**: use the primary config object name, or the filename if the config is a flat set of constants.
- **Defined values**: list the keys with types and one-line purposes. Omit trivially obvious values.
- **Used by**: initially sparse. Backlinks populate as service/utility pages link to the config.
- **Notes**: environment variable mappings, defaults, conditional logic, gotchas.
