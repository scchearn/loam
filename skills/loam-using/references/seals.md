# Seals — Single Definition

A seal binds a claim to the exact bytes of a wiki section and to the evidence
that supports it, so a later reader can check it instead of trusting prose.
This file is the one definition shared by reader and writer skills. Other
skills point here and never copy its tables. `<native-runtime-command>` is the
injected native runtime command; `loam` in any quoted example means that
command.

## What a seal does not mean

A seal does not mean "true". It means: these exact bytes were checked against
this evidence by this signer at this time, and nothing relevant has changed
since. Evidence can be wrong, a signer can be mistaken, and a page can be
re-sealed with new claims. Only a `citable` verdict may be relied on without
re-verifying; every other verdict is provisional.

## Grammar

A sealed span is two marker lines around the text, each on its own line:

```markdown
## Trust rung order
<!-- loam:seal s-tls-rung-order -->
The connector builds its trust store in this order: ...
<!-- /loam:seal -->
```

- Opener `<!-- loam:seal <id> -->` carries only the id. Any other token is SEL001.
- Anchors are `^<prefix>-<slug>` with a 2 to 6 word kebab slug: `^s-` seal,
  `^c-` class, `^q-` question. An anchor is unique per page across all kinds.
- Seal spans never nest and never contain other markers. A `class` span may
  contain seal spans.
- The hashed bytes are strictly between the opener and closer lines.

## Binding: `loam.canon/v1`

SHA-256 over the span bytes between the marker lines, after: strict UTF-8;
CRLF and lone CR to LF; trailing spaces and tabs stripped per line; leading and
trailing blank lines dropped; exactly one final LF. There is no Unicode
normalization, no reflow and no markdown re-escaping, so rewording, re-wrapping
or re-escaping inside a span changes the digest. An empty or blank-only span
canonicalizes to the empty byte string.

## Record kinds

Records live in `wiki/.seals/records/<id>.json` (exact in-toto statement
bytes; `<id>` is the first 20 hex characters of their SHA-256) with a detached
`<id>.json.sig` (SSHSIG, namespace `loam-seal`). Records are immutable and
append-only.

| Kind | Purpose |
|---|---|
| `seal` | Binds a span digest to a claim, requested level, evidence and signer. A re-seal is a new record whose `parent` is the old id. |
| `update` | Carries a seal across a link retarget proven by git history (tracked memory only). |
| `status` | `dispute`, `demote`, `supersede` or `retract` an earlier record, with a reason. |

## Evidence

| Kind | Form | Check | Weight |
|---|---|---|---|
| `blob` | path with git blob and sha256 | blob exists; drifts when `git hash-object <path>` differs | primary |
| `file` | path with sha256 | recompute (works for untracked sources) | primary |
| `commit` | OID on an allowed ref | must be an ancestor of the ref; squash-orphaned is `unverifiable`, not forged | primary |
| `capture` | `wiki/.seals/evidence/sha256/<hex>` with origin and retrieval time | recompute sha256; unreachable origin is informational | primary (observational) |
| `seal` | `seal:<id>` | must itself verify; caps the level at its lowest | derived |
| `note` | free text | none | zero |

Evidence that is only `note` items or unsealed wiki pages never reaches any
level. Choose primary evidence a reader can recompute.

## Levels and verdicts

Level is requested in the record and capped by the runtime; it is cumulative.

| Level | Attainable when |
|---|---|
| `evidence` | Binding matches and at least one primary evidence item resolves. No signature needed. |
| `audited` | `evidence`, plus every evidence item resolves, plus a valid `loam-seal` signature from a principal in the reader's `allowed_signers` that policy grants `audited`. Agent keys qualify. |
| `canonical` | `audited`, plus the signer is listed for `canonical` in the reader's policy (optionally two or more signers). Needs a human decision; never request it unprompted. |

Verdict is computed on every read, never typed:

| Verdict | Meaning | Permitted action |
|---|---|---|
| `citable` | Binding, signature, trust and freshness all hold | Rely on and cite, stating level |
| `drifted` | Span text or evidence changed since sealing | Do not cite as sealed; re-check, then re-seal or amend |
| `stale` | `review_by` or a recheck interval has passed | Do not cite as sealed; re-verify evidence, then re-seal |
| `unverifiable` | Evidence or tooling cannot be checked here | Treat as provisional; say what could not be checked |
| `untrusted` (tampered: malformed record or id mismatch, SEL003) | The record is not well formed, or its id is not the first 20 hex characters of its SHA-256 | Never rely on it; report it, do not repair it |
| `untrusted` (key) | Signing key is revoked (SEL003), or a valid signature from a key not in the reader's `allowed_signers` | Provisional; trust is the reader's decision |
| `forged` (SEL003) | Bad signature, wrong namespace, or `verifier.principal` differs from the signer | Never rely on it; report it, do not repair it |
| `retracted` | A status record withdrew it | Never cite as sealed |

Lifecycle is separate: `active`, `disputed` (flag only), `superseded`,
`retracted`.

## Who may change a seal

- Anyone may seal, up to their ceiling, and anyone may `dispute`.
- Demote, supersede or retract needs a ceiling at or above the seal's level, or
  to be the original sealer. Otherwise the status downgrades to a dispute.
- Never delete or edit a record. Retract it with a status record.

## Citation form

```text
[[page#^s-anchor]] (seal:<id>)
```

Cite with the `cite_as` value from `seal verify`, plus the verdict and
effective level. Never write "sealed" or "verified" without a verify result
from this session.

## Floor procedure (no runtime)

When the native runtime is unavailable, a reader can still check a seal by
hand. It approximates `<native-runtime-command> seal verify`; only a pass of
every step below, including steps 0 and 4, is a `citable`-quality check.

0. Identity. Hash `<id>.json` (same tools as step 3) and require the first 20
   hex characters to equal `<id>`. Read `subject[0].name` (`<page>#^<anchor>`)
   from the record and require it to match the page and anchor you were
   given; use the record's values in step 1. A mismatch is tampering: do not
   rely on the seal and report it.
1. Span digest. The scripts are deliberately conservative: exit 2 means they
   cannot model the page (a lone CR, a NUL byte, ambiguous structure), so use
   the runtime; it never means the seal is bad.
   - Linux or macOS: `sh <Skill root>/scripts/seal-canon.sh --digest <page> <anchor>`
   - Windows, on one line: `powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File <Skill root>/scripts/seal-canon.ps1 -Digest <page> <anchor>`
2. Signature (skip this step when the record has no `.sig`: a clean steps 0,
   1, 3 and 4 is then citable-quality at `evidence` only). `<principal>` is the
   record's `verifier.principal`. One command form on every OS, run in a POSIX shell (on Windows,
   Git Bash, which ships `bash` and a capable `ssh-keygen`; never cmd or
   PowerShell, whose quoting and `<` redirection break it):
   ```sh
   "${LOAM_SSH_KEYGEN:-ssh-keygen}" -Y verify -f <configRoot>/seal/allowed_signers \
     -I <principal> -n loam-seal -O verify-time=<time_verified> \
     [-r <configRoot>/seal/revoked] -s <id>.json.sig < <id>.json
   ```
   `<configRoot>` is the loam config root: `LOAM_CONFIG_DIR` when set,
   otherwise the per-user config directory that holds the runtime ledger (see
   `runtime.md`); Git Bash users translate the Windows path to its POSIX form.
   This needs OpenSSH 9.1 or newer for `verify-time` (check `ssh -V`; Ubuntu
   22.04 ships 8.9, in-box Windows OpenSSH is 8.x). `LOAM_SSH_KEYGEN`, when
   set, names the binary to use on any OS. With no capable binary the result
   is `unverifiable`, not a failed signature.
3. Evidence, per kind. `blob`: `git hash-object <path>`. `file`: sha256 of
   `<path>` via `sha256sum` (Linux), `shasum -a 256` (macOS) or
   `certutil -hashfile <path> SHA256` (Windows). Compare digests
   case-insensitively with spaces removed, since older `certutil` prints
   uppercase, space-separated hex. `commit`: `git merge-base --is-ancestor
   <oid> <ref>`; an orphaned commit is `unverifiable`. `capture`: hash
   `wiki/.seals/evidence/sha256/<hex>` and require it to equal both the file
   name and the recorded value. `seal`: apply this whole procedure to the
   referenced seal; the result caps the level. `note`: no check, and it never
   counts toward a level. If you cannot run a check for a kind, the result is
   `unverifiable`.

`<time_verified>` is the record's `time_verified` converted to UTC and written
`YYYYMMDDHHMMZ`, with the trailing `Z` (without it ssh-keygen reads local
time), the same as the runtime. Example: `2026-11-20 14:05 +02:00` becomes
`202611201205Z`. It matters because rotated keys keep a closed validity window
in `allowed_signers`. Pass `-r` only when the `revoked` file exists; a signer
it lists is `untrusted`.

4. Standing. Confirm that no `status` record in `wiki/.seals/records/` names
   this id in `updates` with retract, supersede or demote (the floor is
   deliberately stricter than the runtime, which keeps a superseded or
   demoted seal citable, at the capped level for demote, and ignores
   unauthorised status records; treat any such record as a reason to run
   `seal verify` when the runtime returns); that freshness holds
   (the earlier of `review_by` and `time_verified` plus the policy max_review
   has not passed; no evidence recheck is due); and that step 2
   succeeded for that principal.

Compare the step 1 digest with the record's `subject[0].digest.sha256`, and
each evidence digest with its recorded value. A match on steps 1 to 3 with
the signer in your `allowed_signers` and a clean step 4 is `citable`-quality.
If step 4 fails or you skipped it, treat the seal as provisional. Level grants
from policy are not checked by hand.

To trust another person's seals, run
`<native-runtime-command> seal trust add <principal> <pubkey>`; it shows the
key fingerprint to confirm first.

## Commands

All run through `<native-runtime-command>`: `seal hash <file> <anchor>`,
`seal verify [<page>[#^anchor]] [--json] [--now <stamp>]`, `seal status`,
`seal attest <page>#^<anchor> --level <l> --claim <text> --evidence <kind:ref>...
[--review-by <stamp>] [--parent <id>]`, `seal carry <id>`,
`seal mark <id> dispute|demote|supersede|retract --reason <text> [--to <evidence|audited>]`,
`seal trust init|add|show`. Verify exits 0 when citable, 1 when any seal is not,
2 on usage errors. Lint: `lint --only seals`.

## Gotchas

- Trigger: a marker or a file under `wiki/.seals/` needs fixing by hand. Mistake: editing it; a hand-edited record verifies as `untrusted` (malformed or id mismatch, SEL003) and a hand-edited `.sig` as `forged`; neither may be relied on, and a hand-edited marker orphans the seal (SEL002) or drifts it. Fix: never hand-edit; re-seal with `seal attest` or retract with `seal mark`.
- Trigger: tidying text inside a sealed span (re-wrapping, smart quotes, escapes). Mistake: any byte change drifts the seal. Fix: edit deliberately, then resolve the seal in the same change through `/loam::amending-memory`.
- Trigger: reporting a section as sealed or verified. Mistake: claiming it from memory or from the marker alone. Fix: run `seal verify` and quote the verdict.
- Trigger: `datecheck fix` or `lint --fix` over a sealed page. Mistake: expecting spans to be rewritten. Fix: they skip sealed spans and report SEL008; change span text only by amending.
- Trigger: a seal on a requested level above the ceiling. Mistake: assuming it holds. Fix: read `effective_level`; the runtime caps overclaims.
