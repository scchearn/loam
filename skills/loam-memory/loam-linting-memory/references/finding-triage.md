# Finding Triage Guide

Use this guide to decide what to do with each lint finding.

## Fix now

Choose this when the issue is objective and can be corrected safely from memory (wiki substrate)'s current evidence.

Examples:

- `index.md` is missing a concise `## Overview` section near the top
- a durable page exists but is missing from `index.md`
- an index entry points at a deleted or renamed page
- a legacy root `overview.md` only contains structural orientation content that can be folded into `index.md` and removed
- a `[[wikilink]]` clearly points to an existing canonical note but uses the wrong target
- two pages obviously should cross-link based on current content
- a topic or entity note links to another note but the reciprocal backlink is obviously missing
- a page should contain a brief contradiction note because the conflict already exists elsewhere in memory (wiki substrate)
- code-graph pages (with `source_path:` front matter) are stranded in `entities/` instead of `code/` — move to `code/` and update `index.md` grouping

## Annotate now

Choose this when memory already shows a problem, but the correct resolution still requires judgment or more evidence.

Examples:

- a newer source appears to challenge an older synthesis, but not enough to fully replace it
- two pages use the same term differently and the ambiguity should be made visible
- `overview.md` and `index.md` disagree on scope, corpus boundary, or current state, and the conflict should be made visible during consolidation
- an entity page likely needs revision, but the current wiki does not yet settle the stronger claim
- two notes may represent the same durable identity, but the correct canonical filename is not fully obvious

Good outputs:

- a brief note on the affected page
- an open question in a relevant page
- a clear item in `log.md`

## Follow up later

Choose this when the issue cannot be resolved honestly from the current wiki state.

Examples:

- memory is missing raw source material needed to settle a contradiction
- a topic page is outdated because the relevant ingest never happened
- a legacy `overview.md` contains substantial unique synthesis that should be preserved in a more appropriate durable page after its root-level orientation has been compressed into `index.md`
- the right fix depends on user intent or domain-specific judgment not captured in memory (wiki substrate)
- two or more notes probably need consolidation, but doing so safely requires a more deliberate restructuring pass

Good outputs:

- a recommendation to run `/loam::adding-to-memory <local source path or topic>`
- a durable note in `log.md`
- a concise mention in the lint report back to the user
