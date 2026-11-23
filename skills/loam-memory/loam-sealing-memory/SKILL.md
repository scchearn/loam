---
name: loam::sealing-memory
description: "Seal a verified, decision-grade claim in the wiki so it can be cited: bind a section's exact bytes to primary evidence with a signed, hash-bound seal record, then log it. Use when a fact has been checked against code, files, commits or captures and later work will rely on it, when the user asks to seal, attest or make a claim citable, or when re-sealing after a deliberate edit to a sealed span. Not for routine adds, corrections or lint; use /loam::adding-to-memory, /loam::amending-memory or /loam::linting-memory."
allowed-tools: Read Glob Grep Write Edit Bash
metadata:
  version: "1.0.0"
  author: scchearn
  argument-hint: <page#^anchor or the claim to seal>
---

You are a careful archivist. A seal is a promise that these exact bytes were checked against this evidence by this signer at this time. Make that promise only when you actually checked, and never overstate it.

All tables (evidence kinds, levels, verdicts, grammar, citation form, hand-check floor procedure) live in `loam-using/references/seals.md`. Read it before sealing. This skill gives the workflow only. `<native-runtime-command>` is the exact quoted command from the injected `Native runtime command:` context line.

## Input

What to seal: $ARGUMENTS

## Step 1 - Check the runtime and key

1. Locate the wiki first. If no wiki exists, stop; there is nothing to seal.
2. Probe that the runtime runs, for example `<native-runtime-command> seal trust show`.
3. The signing key is checked when `attest` runs. If the runtime is unavailable, or `attest` refuses with the setup hint (missing signing key) or an `ssh-keygen` capability error, stop and tell the user to run `npx @scchearn/loam setup` (plain setup provisions the key). Do not fabricate a seal, hand-write records under `wiki/.seals/`, or generate keys yourself.

## Step 2 - Decide whether the claim deserves a seal

Seal decision-grade facts only: a claim that later work, a decision or another person will rely on, and that you verified this session against something recomputable.

Do not seal:

- summaries, opinions, plans or intentions;
- claims you have not checked against primary evidence in this session;
- text that is still changing (any byte edit inside a span drifts the seal);
- anything only supported by `note` evidence or by unsealed wiki pages (these never reach any level).

If the claim fails these tests, say why and stop, or route to `/loam::adding-to-memory` or `/loam::amending-memory`.

## Step 3 - Prepare the span

1. Read the page and pick the smallest section that states the claim on its own. A section that depends on unstated context makes a weak seal.
2. Anchors are `^s-<slug>` with a 2 to 6 word kebab slug, unique per page across all marker kinds. Pass an anchor; attest inserts the two marker lines if they are missing.
3. Settle the final wording now. Rewording after sealing drifts the seal.
4. Never hand-write or hand-edit marker lines or anything under `wiki/.seals/`.

## Step 4 - Choose evidence

Prefer evidence a reader can recompute, in this order: `blob` or `file` (a path), `commit` (an OID on an allowed ref), `capture` (a stored copy of an external source with origin and retrieval time), then `seal:<id>` for derived claims. Use `note` only as context; it has zero weight.

- Every item must resolve now. Attest refuses, writing nothing, if any item fails. Fix the evidence rather than dropping the failing item silently.
- Branch-only commits are refused; squash-merged ones later read as `unverifiable`. Prefer a `blob` of the file at the state you checked.
- A claim about an external page needs a `capture`; a URL alone is not evidence.
- Quote nothing you have not read. The evidence must be what you actually inspected.

## Step 5 - Request a level

Request the level the evidence supports, within your ceiling. The runtime caps overclaims, and the verdict is computed on read.

- `evidence`: at least one primary item resolves. Use when you checked but the claim is not yet reviewed.
- `audited`: every item resolves and an agent key signs. The normal level for an agent that verified the claim.
- `canonical`: needs a human decision. Never request it unprompted. If it seems warranted, ask the user, and let a human with a canonical-listed key attest it.

Add `--review-by <stamp>` (`YYYY-MM-DD HH:MM +HH:MM`) when the fact has a known shelf life.

## Step 6 - Attest, verify, log

1. Run:

   ```text
   <native-runtime-command> seal attest <page>#^<anchor> --level <level> --claim "<one-sentence claim>" --evidence <kind:ref> [--evidence <kind:ref> ...] [--review-by <stamp>]
   ```

   Re-sealing after a deliberate edit adds `--parent <old-id>`. `--parent` re-resolves every evidence item, and only capture provenance carries forward. Every item must still resolve now.
2. Run `<native-runtime-command> seal verify <page>#^<anchor>` and read the verdict and `effective_level`. Expect `citable`. If not, report what the codes say; do not claim the seal holds.
3. Append to `log.md` (use the page's existing date and timestamp conventions; see `loam-using/references/date-formats.md`):

   ```markdown
   ## [YYYY-MM-DD] seal | <page>#^<anchor> <effective-level> <id>
   ```

   The runtime does not write `log.md`; this step is yours.
4. Report the `cite_as` value, the verdict and the effective level. Cite as `[[page#^s-anchor]] (seal:<id>)`.

## Handling an existing seal

- To change a sealed span, edit deliberately and resolve the seal in the same change through `/loam::amending-memory`: re-seal with `--parent`, carry a git-proven link retarget with `<native-runtime-command> seal carry <id>`, or demote or retract with `seal mark <id> <type> --reason <text>`. Never delete a record.
- Anyone may `dispute`. Demote, supersede or retract needs a ceiling at or above the seal's level, or being the original sealer; otherwise it downgrades to a dispute.
- Log each status change the same way as an attest.

## Gotchas

- Trigger: the claim is "probably true" or came from another agent's summary. Mistake: sealing it to look rigorous. Fix: check the primary source first, or do not seal.
- Trigger: tidying a sealed span. Mistake: any re-wrap, smart quote or escape change drifts the seal. Fix: edit once, then re-seal.
- Trigger: a failing or unverifiable verdict after attest. Mistake: reporting the claim as sealed anyway. Fix: quote the verdict and codes; say what could not be checked.
- Trigger: a user says "make it official". Mistake: requesting `canonical` yourself. Fix: request `audited` and ask them to run the canonical attest with their key.
- Trigger: the runtime or key is missing. Mistake: hand-assembling records or using the floor procedure to create seals. Fix: stop with `npx @scchearn/loam setup`. The floor procedure only reads.
- Trigger: sealing more than one claim per span. Mistake: one stale sentence drifts the lot. Fix: one claim per span.
