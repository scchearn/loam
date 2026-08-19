# broken-links-fixture

Exercises the wikilink scanner's broken / ambiguous / case-drift diagnostics.

## Commands

None; this is a static fixture, not a runnable project.

## Architecture

```
broken-links/
  AGENTS.md
  wiki/
    index.md
    topics/broken-links-demo.md   # contains all three link diagnostics
    topics/overview.md            # ambiguous target 1
    entities/overview.md          # ambiguous target 2 (same basename)
    topics/Setup.md               # case-drift target (capitalized stem)
```

## Gotchas

- `[[does-not-exist]]` in `broken-links-demo.md` must never be created — it
  is the fixture's `broken-wikilink` case.
- Do not rename either `overview.md` file to be unique — the duplicate
  basename is the fixture's `ambiguous-wikilink` case.
- Do not rename `Setup.md` to lowercase — the case mismatch against the
  `[[setup]]` link is the fixture's `noncanonical-link-case` case.
