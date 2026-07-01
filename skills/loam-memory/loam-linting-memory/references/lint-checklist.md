# Wiki Lint Checklist

Use this checklist to health-check a markdown wiki without turning the pass into a speculative rewrite.

## Structure

- Does `index.md` contain a concise `## Overview` section near the top?
- Are all durable pages represented in `index.md`?
- Does `index.md` reference any missing pages?
- Is a legacy root `overview.md` still present?
- If `overview.md` exists, does it duplicate or conflict with the root-hub content already in `index.md`?
- Are any durable notes violating the canonical kebab-case naming convention?
- Are there obvious duplicate note identities under different filenames?
- Are there duplicate or overlapping pages that should likely be consolidated later?
- Are there obvious placeholder pages with no useful content?
- Are there code-graph pages (with `source_path:` front matter) stranded in `entities/` instead of `code/`?

## Links

- Which `[[wikilinks]]` do not resolve?
- Which notes have no useful inbound links?
- Which pages have no useful outbound links?
- Which pages are difficult to discover from `index.md` or neighboring pages?
- Which notes are only discoverable from `index.md` but not from the graph itself?
- Which topic/entity/concept relationships are missing reciprocal backlinks?
- Which repeated entities or concepts probably deserve their own page?

## Integrity

- Do two pages make materially conflicting claims?
- Does a newer synthesis appear to supersede an older one?
- Are there synthesis pages that no longer reflect the known source set?
- Are important claims missing source linkage or caveats?

## Maintenance

- Do recent log entries imply updates that never reached related pages?
- Are open questions recorded anywhere durable?
- Are follow-up leads visible to the next session?

## Safe fixes during lint

- add or refresh a concise `## Overview` section in `index.md`
- fold safe structural content from a legacy `overview.md` into `index.md`
- delete a legacy `overview.md` after consolidation
- repair index drift
- resolve obvious broken wikilinks
- add missing cross-links
- add missing reciprocal backlinks
- add short contradiction notes
- add short stale-claim notes
- create a minimal reusable page for an entity or concept already well established in memory (wiki substrate)
- normalize obvious internal links to canonical `[[kebab-case-note-name]]` form
- move stranded code pages (with `source_path:` front matter) from `entities/` to `code/`, update `index.md` grouping, and append a migration log entry

## Avoid during lint

- new source ingestion
- speculative synthesis
- broad refactors with weak evidence
- copying a long legacy `overview.md` into `index.md` without compressing it
- silent merges or renames of ambiguous duplicate notes
- leaving a redundant `overview.md` behind after consolidation
- deletion of meaningful disagreement
