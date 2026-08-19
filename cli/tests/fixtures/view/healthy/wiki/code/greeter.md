---
source_path: src/greeter.js
ingested_at: 1754812800
source_size: 60
content_hash: d93ba2d5e1ad3dc0e161e8aaa1869df3576d5fa9068f46a8e4ea465e8ad762d6
content_id: healthy-fixture/greeter
blob_oid: ""
source_commit: ""
source_state: provisional
generator_version: loam-code-page-v1
---

# greet

## Signature

```js
export function greet(name)
```

## Summary

Formats a friendly greeting string for the given name.

## What it does

- Interpolates `name` into a fixed greeting template.
- Returns the formatted string; performs no I/O.

## Dependencies

- none

## Callers

- [[greeting]] — documents the greeting concept this function implements.

## Failure modes

- None; the function cannot throw for any string input.
