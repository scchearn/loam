# Generated Code Hub

Read this reference only when active ordinary `code/*.md` pages exist or lint will move stranded code pages there.

## Contract

- Root `index.md` contains exactly one `[[code/_index|Code graph]]` entry under `## Code` and no direct ordinary code-page entries.
- `code/_index.md` lists every active `code/*.md` page except itself, sorted by slug.
- Each entry is `- [[<slug>]] — <one-line summary>`. Use the page's `## Summary`, falling back to `source_path:`.
- The hub carries no `source_path:`, is not a code node, and is exempt from ordinary filename and self-membership checks.
- Do not add individual code pages to root `index.md`.

If no ordinary code pages exist, do not require or create a hub.

## Safe repair

When the hub is missing, incomplete, unsorted, linked zero or multiple times from root, or bypassed by direct root code entries:

1. Rebuild it from active ordinary code pages using the contract above.
2. Keep one root hub link and remove direct ordinary code-page entries.
3. Let the normal unresolved-link pass handle links elsewhere; do not duplicate those findings here.

Rebuild after moving stranded code pages from `entities/` to `code/`. This is derived navigation, not source ingestion.
