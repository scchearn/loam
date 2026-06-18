# Guidance File Triage

Use this guide when deciding what belongs in `AGENTS.md` / `CLAUDE.md` versus the wiki.

## Keep

Keep content in guidance files when it is operational, local, and safety-relevant.

Typical examples:

- commands and runbooks
- hard rules and safety constraints
- scope boundaries for the current subtree
- canonical source-of-truth locations
- short entrypoint guidance for how to work in the current area

## Shorten and link

Shorten content and replace it with concise wiki pointers when the current guidance file is carrying deep reference material that belongs in the durable wiki layer.

Typical examples:

- long architecture narratives
- domain summaries
- workflow deep dives
- large gotcha catalogs
- repeated implementation explanations that also appear elsewhere

Good pattern:

- keep the short operational rule in the guidance file
- add a precise pointer to `index.md` or the most relevant scoped wiki note

## Stale-check

Treat a guidance section as stale-check material when it makes specific claims that may have drifted.

Typical examples:

- old route names or package names
- outdated workflow descriptions
- stale environment-variable expectations
- behavior claims that conflict with the current repo structure or code

Verify against current repo state before editing or removing it.

## Mirror-only

Some files are mirrors rather than canonical guidance sources.

Typical example:

- `CLAUDE.md` containing only `@AGENTS.md`

Keep these thin unless the workspace clearly expects something else.

## Out of scope

Leave a guidance file out of the current pass when:

- the alignment scope does not meaningfully touch that subtree
- the wiki scope does not yet cover that area
- updating it would require a broad repo-wide rewrite that was not requested

## Non-negotiable rules

- Do not delete unique operational guidance unless the durable replacement clearly exists and is referenced.
- Do not turn guidance files into shadow wiki pages.
- Prefer concise pointers into the wiki over duplicated deep-reference prose.
- Preserve commands, hard rules, and safety constraints.
