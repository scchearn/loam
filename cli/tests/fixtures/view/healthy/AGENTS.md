# healthy-fixture

A minimal but complete loam workspace: wiki, code page, goal, spec, plan,
and checkpoint, all current and cross-linked.

## Commands

| Command | Purpose |
| --- | --- |
| `node -e "require('./src/greeter.js')"` | Load the fixture's one source file |

## Architecture

```
healthy/
  AGENTS.md
  src/
    greeter.js
  wiki/
    index.md
    topics/greeting.md
    code/_index.md
    code/greeter.md
    checkpoints/checkpoint-2026-08-10-0900.md
  goals/improve-greeting.md
  specs/greeting-spec.md
  plans/greeting-plan.md
```

## Gotchas

- `wiki/code/greeter.md`'s `content_hash` front matter must stay in sync with
  `src/greeter.js` for this fixture to read as "current" rather than "stale".
