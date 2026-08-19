# Broken links demo

This page exercises all three wikilink diagnostics in one place.

- A broken link to a page that does not exist anywhere in this workspace: [[does-not-exist]].
- An ambiguous link: [[overview]] matches both `wiki/topics/overview.md` and `wiki/entities/overview.md`.
- A case-drift link: [[setup]] should resolve to `wiki/topics/Setup.md` but is written in the wrong case.

A fenced code block must not be scanned for links, so this one is inert:

```
[[also-does-not-exist]]
```

An inline code span is inert too: `[[inline-code-not-a-link]]`.
