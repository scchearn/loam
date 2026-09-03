# Discovery protocol

Read when a skill searches or writes the wiki, or discovers code. Referenced from `loam::using`; the discovery order itself is in the router.

## qmd and code-graph discovery

When the integration reports `qmd_ready: true` and `collection: <name>`, prefer qmd over Glob/Grep for content discovery in the wiki.

- **Lookup**: `qmd search "<keywords>" --files -n 8 -c <collection>`
- **Comparison/synthesis**: `qmd query "<natural-language question>" --files -n 8 -c <collection>`
- Strip the `qmd://<collection>/` prefix from paths to get the relative wiki path (e.g. `code/validate-token.md`)
- Verify candidates by Reading the actual wiki files — qmd discovers paths, Read confirms content
- Ignore `.archive/` paths (historical, not active memory)
- After wiki writes, run `qmd update -c <collection>` then `qmd embed -c <collection>`; report both outcomes separately. If either fails, retain the wiki edits and report the failure.
- On qmd degradation (command fails or returns stale/noisy output): fall back to Grep/Glob for the rest of the session

## Code graph precedence (all code discovery)

When `wiki/code/` exists and qmd is ready, prefer code pages over raw source for **all** code discovery — orientation AND exact-pattern search. The code graph maps which modules exist and where symbols live before you scan raw bytes.

1. qmd first: `qmd search "<symbol or topic>" --files -n 8 -c <collection>`, Read the returned `code/<slug>.md` pages for the compressed map
2. Source pattern search (call sites, symbol usages): prefer **ast-grep** (`ast_grep_search` MCP tool, or `ast-grep`/`sg` CLI) — AST-aware, skips comments/strings, handles formatting. Scope to the files/modules the graph flagged
3. Fall back to `rg`/`grep` on raw source when `ast-grep` is unavailable (probe once; on failure use `rg`/`grep`)
4. Skip the code-graph-first step only if `code_ingest_pending` hint is set and flagged files overlap your target — then verify against raw source directly

`grep`/`Glob` remain correct for markdown/prose structural checks (inventory, orphans, wikilinks) and as the raw-source fallback.

**Wrong assumption to reject:** "qmd is only for memory; grep is correct for concrete code call sites." qmd indexes `wiki/code/` summaries, so qmd-first applies to code too. After qmd, `ast-grep` (not `grep`) is the source-pattern tool; `grep` only wins for non-code and the ast-grep-unavailable fallback.
