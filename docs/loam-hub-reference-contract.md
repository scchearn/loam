# Design note: loam hub reference contract

Status: proposal for v1.1.0 view integration  
Source prototype: `_hub`'s offline `bin/loam.ps1`, `LoamOverlay.ps1`, and `DriftGit.ps1`  
Scope: a local, read-only portfolio view over independent loam workspaces

## Summary

This note proposes the behavioral contract for a future `loam hub` (or equivalent
view subcommand). It is a one-shot local projection, not a server and not a new
memory store. It lets a user see where their loam workspaces are and which local
git work is waiting, while every workspace remains authoritative for its own
memory, goals, plans, and source files.

The contract deliberately separates two meanings of federation:

- Loam's live federation mesh coordinates peers, transport, wake, mailboxes, and
  session context.
- This hub view aggregates pointers to local workspaces without moving their
  contents or requiring the mesh to be running.

The hub MUST continue to work with plain files and local git state. It MUST NOT
require an MQTT broker, contact a network service, or persist portfolio memory
outside the participating repositories.

## 1. Design constraints

The implementation MUST satisfy these constraints:

1. **Path is identity.** A canonical path, not a display name, identifies a row.
   Duplicate names, multiple worktrees, and paths outside a scan root remain
   distinct.
2. **Link, do not embed.** The view may expose a path and links into a workspace,
   but never copies memory, goals, plans, or file bodies into the hub.
3. **Read-only by default.** Indexing reads local metadata and git state. It does
   not edit a workspace, push commits, mutate a stash, or delete a worktree.
4. **Offline first.** A missing network, broker, optional indexer, or native
   helper cannot prevent the local view from rendering.
5. **Repository ownership.** A workspace owns its memory and workflow artifacts.
   The hub owns only its derived snapshot, if the caller chooses to save one.
6. **Deterministic output.** Given the same filesystem/git state and an injected
   evaluation time, ordering and values are stable. The clock MUST be supplied
   for tests and reproducible renders.
7. **Bounded disclosure.** The index reads only aggregate counts, selected
   frontmatter, and actionable status. It MUST NOT read or transmit file bodies.

## 2. Federation index contract

### 2.1 Inputs

The command accepts a scan root and, optionally, an explicit enrollment file.
Discovery finds local loam workspaces under the root. Enrollment adds named
locations that are outside the root or otherwise not discoverable. An enrolled
location that no longer exists remains visible as `missing` so disappearance is
not silently mistaken for completion.

The implementation may recognize a workspace through its loam layout, but layout
recognition MUST be a local filesystem check. It MUST NOT call a remote API to
decide whether a workspace exists.

### 2.2 Row identity and states

Each row is keyed by `path_key`:

```yaml
path_key: /canonical/path/to/workspace
display_name: example-repo
path: /canonical/path/to/workspace
state: enrolled # enrolled | discovered | missing
source: tracked # tracked | scan
```

`path_key` is the normalized canonical path used for joins, deduplication, and
stable sorting. `path` is the display/link target. On a case-insensitive
filesystem, normalization MUST use the filesystem's canonical case behavior; on
case-sensitive systems, distinct paths MUST remain distinct. Separators MUST be
rendered consistently for the selected output format.

State has this meaning:

| State | Meaning | Required behavior |
| --- | --- | --- |
| `enrolled` | Explicitly listed by the user and present | Include local summary and links |
| `discovered` | Found by the scan and not explicitly enrolled | Include local summary and links |
| `missing` | Explicitly enrolled or previously known, but absent now | Include identity and next action; read no workspace files |

If a path is both discovered and enrolled, it has one row with state
`enrolled`. The output MUST contain one row per unique `path_key`, sorted by
`path_key`, never one row per name.

### 2.3 Allowed workspace reads

For a present workspace, the index MAY read:

- counts of recognized goals, specs, plans, and checkpoints;
- `status`, `title`, and `updated_at` from the frontmatter of recognized
  records, where needed to calculate status counts or a compact active-item
  summary;
- a workspace's explicitly declared next action, when present;
- directory existence needed to produce links such as `goals/`, `wiki/`,
  `specs/`, and `plans/`.

The index MUST NOT read record bodies, transcript bodies, source files, commit
patches, or arbitrary prose in order to construct the view. A title or next
action shown in the output MUST come from the permitted frontmatter/metadata,
not from body extraction. Missing or malformed frontmatter yields an explicit
`unknown` value or omitted optional field; it MUST NOT cause a whole scan to
fail.

### 2.4 Reference row schema

The canonical exchange shape is structured data. Markdown is a rendering, not
the source contract:

```yaml
schema: loam-hub-index/v1
generated_at: 2026-01-01T00:00:00Z
root: /canonical/scan/root
rows:
  - path_key: /canonical/path/to/workspace
    display_name: example-repo
    path: /canonical/path/to/workspace
    state: enrolled
    source: tracked
    counts:
      goals: 3
      active_goals: 1
      specs: 2
      plans: 4
      checkpoints: 1
    status_counts:
      active: 1
      draft: 2
    items:
      - kind: goal
        title: Example goal
        status: active
        updated_at: 2026-01-01T00:00:00Z
        next_action: continue the active plan
    links:
      - label: goals
        path: goals/
    next_action: continue the active plan
```

Normative rules for this shape:

- `schema`, `generated_at`, and `rows` are required.
- `path_key`, `path`, `state`, and `counts` are required for every row;
  `display_name` is presentation only and MUST NOT be used as identity.
- `items` is optional and bounded. It contains frontmatter summaries only, not
  bodies. Implementations MAY omit it and retain only aggregate counts.
- `links` are references back into the workspace. They are not embedded
  documents. A renderer MUST preserve the distinction between a link and a
  copied excerpt.
- `next_action` is a short, literal operator action. It MUST be safe to display
  without implying that the hub has performed it.
- `missing` rows MUST retain `path_key`, `path`, `state`, and a next action, but
  MUST NOT fabricate counts or inspect a replacement path.
- Unknown future fields MUST be ignored by readers. Producers MUST keep the
  required fields stable for the lifetime of schema version 1.

### 2.5 Markdown rendering

The reference Markdown view SHOULD contain one deterministic table row per
`path_key`, with columns for workspace, path, state, counts, compact status, and
local links. It SHOULD end with a summary of enrolled, missing, and total unique
locations. It MUST state that linked workspaces remain authoritative.

The Markdown renderer MUST escape table cells and normalize line endings. It
MUST sort rows by `path_key` and use the injected `generated_at` value. It MUST
not add body excerpts merely because Markdown makes them convenient.

## 3. Waiting-state git drift contract

The second part of the hub view is a local attention projection. These signals
are deliberately **de-gated**: they are computed from git state and prior local
scan state, not from loam enrollment. A newly adopted workspace can therefore
surface useful waiting work on its first scan.

Every signal has the same envelope:

```yaml
type: unpushed-age
path_key: /canonical/path/to/workspace
value: 12
detail: oldest unpushed commit is 12d old (threshold 7d)
next_action: git push
observed_at: 2026-01-01T00:00:00Z
```

`type`, `path_key`, `detail`, and `next_action` are required. `value` is a
numeric age/count when meaningful and null for binary conditions. `observed_at`
comes from the injected evaluation time. The hub reports signals; it does not
execute their actions.

### 3.1 Required signals

| Type | Condition | Literal next action |
| --- | --- | --- |
| `needs-upstream` | The repository has commits and a configured remote, but the current branch has no upstream tracking branch. | `git push -u origin <branch>` |
| `unpushed-age` | The oldest local-only commit ahead of its upstream is at least the configured age threshold. | `git push` |
| `stale-branches` | A local branch is unmerged into the resolved default branch and its latest commit is at least the configured stale age. | `merge, rebase, or delete the branch` |
| `stash-age` | The oldest stash entry is at least the configured stash age. | `apply or drop the stash` |
| `thrash` | The same non-allowlisted file is re-committed above the configured count and concentration thresholds in the recent commit window. | `record dead-end in lessons.md` |
| `dirty-Nd` | Tracked files are dirty, the newest dirty-file mtime is older than the configured age, and the repository's recent-commit heat gate says the work is still active. | `commit, stash, or gitignore` |
| `path-stale` | A previously known/enrolled workspace path is no longer present. | `restore path, or drop from tracked.md` |

The exact threshold values are configuration, not part of the stable signal
names. Age calculations MUST normalize timestamps to UTC and use the supplied
evaluation time. `thrash` MUST ignore allowlisted generated/lock artifacts and
MUST not use commit-message words as a trigger. `stale-branches` MUST exclude
branches whose commits are already reachable from the default branch.

`dirty-Nd` and `stale-branches` may be less reproducible than the strictly
git-derived signals because they depend on filesystem mtimes or bounded branch
scan budgets. Implementations MUST expose timeout/degraded status rather than
inventing a confident flag from incomplete data.

### 3.2 Signal ordering and safety

Signal ordering SHOULD be stable and severity-aware, with `path-stale` first,
followed by `thrash`, `dirty-Nd`, `unpushed-age`, `stale-branches`,
`needs-upstream`, and `stash-age`. Ties sort by `path_key` and then signal type.

The hub MUST NOT run `push`, `merge`, `rebase`, `delete`, `stash`, or any other
next action. It MUST NOT require network access to calculate local signals.
Remote configuration may be inspected locally, but no remote command is implied
by the presence of a `needs-upstream` or `unpushed-age` signal.

## 4. Ownership and integration boundary

The proposed Rust command should own local discovery, canonical-path identity,
bounded metadata reads, git drift calculation, and structured output. A Markdown
skill may explain how to invoke it and how to interpret `next_action`; it should
not become a second scanner.

The live federation mesh remains independent. A future integration MAY add a
read-only peer/workspace source, but the base hub command MUST continue to work
when MQTT, wake services, mailbox drains, TLS configuration, and session
injection are absent. Mesh state MUST never be required to decide whether local
memory exists or to make a local workspace authoritative.

No portfolio memory is copied into loam's code hub or into a central database.
Any saved index is a derived, discardable projection with source paths and
timestamps; the durable records remain in their repositories.

## 5. Conformance examples

A conforming implementation must satisfy these cases:

1. Two workspaces named `agent-skills` at different canonical paths produce two
   rows.
2. An enrolled path outside the scan root is included as `enrolled`.
3. An enrolled path that disappears remains as `missing` with no fabricated
   counts and a restore/drop action.
4. A workspace with a large memory file produces counts/frontmatter summaries,
   but never embeds that file's body in JSON or Markdown.
5. A newly discovered workspace with an unpushed local commit raises
   `unpushed-age` without any loam enrollment marker.
6. A repository with no configured upstream but a configured remote raises
   `needs-upstream` with the literal `git push -u origin <branch>` action.
7. A stash, stale unmerged branch, dirty old tracked file, or churned file emits
   its corresponding literal action without changing the repository.
8. Removing MQTT/network access does not prevent the command from producing the
   local index and local drift signals.
9. Re-running with the same filesystem/git state and `generated_at` produces
   byte-identical structured and Markdown output.

## 6. Suggested implementation sequence

1. Land this contract and a small fixture corpus on the view integration branch.
2. Implement the structured `loam hub` output first; keep Markdown rendering
   thin and deterministic.
3. Add the path/state cases and metadata disclosure tests before adding more
   views.
4. Add the seven de-gated drift signals with injected time, bounded git calls,
   and literal action assertions.
5. Keep `_hub` as a compatibility renderer until its output matches the loam
   contract, then retire duplicate scanning rather than maintaining two hubs.

This proposal contributes the semantics proven by `_hub`, not its PowerShell
implementation. It gives loam a small local view that complements the live mesh
while preserving loam's central promise: memory stays distributed, portable,
plain, and owned by the repository where it lives.
