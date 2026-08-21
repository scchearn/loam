# degraded-fixture

Exercises the "one optional probe fails, workspace stays readable" scenario.

## Commands

None; this is a static fixture, not a runnable project.

## Architecture

```
degraded/
  AGENTS.md
  wiki/
    index.md
    topics/status.md
    code/corrupt.md   # invalid UTF-8 bytes -- the probe-failure trigger
```

## Gotchas

- `wiki/code/corrupt.md` is intentionally not valid UTF-8/Markdown. Do not
  "fix" it -- a probe choking on it (while the rest of the workspace stays
  readable) is the fixture's whole purpose.
