# malformed-fixture

Exercises the `artifact-parse` signal: bad front-matter YAML and malformed
timestamps that must not drop their artifacts from inventory.

## Commands

None; this is a static fixture, not a runnable project.

## Architecture

```
malformed/
  AGENTS.md
  wiki/
    index.md
    topics/bad-frontmatter.md   # unterminated YAML string/list
  goals/
    bad-timestamps.md           # unparseable created_at, invalid updated_at
```

## Gotchas

- Keep both malformed fields malformed -- fixing the YAML or the timestamps
  defeats the fixture's purpose.
