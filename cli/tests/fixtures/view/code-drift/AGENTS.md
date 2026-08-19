# code-drift-fixture

Exercises codegraph `stale` / `new` / `orphan` drift detection.

## Commands

None; this is a static fixture, not a runnable project.

## Architecture

```
code-drift/
  AGENTS.md
  src/
    stale-module.js   # edited since wiki/code/stale-module.md was ingested
    new-module.js      # no wiki page yet
  wiki/
    index.md
    code/_index.md
    code/stale-module.md   # content_hash is the pre-edit hash (stale)
    code/orphan-module.md  # source_path points at a deleted file (orphan)
```

## Gotchas

- `wiki/code/stale-module.md`'s `content_hash` intentionally does NOT match
  `src/stale-module.js`'s current sha256 — that mismatch is the fixture.
- `wiki/code/orphan-module.md`'s `source_path` intentionally does not exist
  on disk.
