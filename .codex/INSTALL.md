# Installing loam for Codex

## Installation

Run the global setup wizard:

```bash
npx @scchearn/loam setup
```

Setup installs global skills through Skills CLI and verifies the private native
runtime. Codex can discover the global skills under `~/.agents/skills/`. When
Codex is detected, setup offers to add the Loam marketplace and install its thin
plugin through the native Codex CLI.

Setup then writes the plugin's hook registration itself, pointing `SessionStart`
and `UserPromptSubmit` at the **absolute private native runtime** (`loam hook
codex --event <boundary>`) and leaving `Stop` on the Node ingestion entry. The
runtime path is version- and target-qualified, so no shipped file can name it;
setup rewrites the registration on every update, the same way it re-enables an
active service on a new runtime. There is no Node session shim and no shared
Node integration in the session path. Setup also removes legacy Loam user hooks
and never creates new ones.

The plugin contains no skills. Canonical skill content remains under
`~/.agents/skills/`. If the plugin is installed before setup, it stays
network-free and reports `npx @scchearn/loam setup` until the shared core exists.

Use `--yes` for automation or `--dry-run` to preview without mutation or
download. The runtime remains outside `PATH`.

## Verify and update

```bash
npx skills list --global
npx skills update --global
npx @scchearn/loam setup
```

The existing clone plus symlink path is a repository-development or migration
compatibility option, not the normal installation path. It must not create a
project-local Loam runtime or skill copy.

## Session use

The registered native hook injects `loam::using`, the absolute native runtime
command, and current workspace state at SessionStart, and refreshes at the next
user prompt. It is a read path: it cannot publish, it reads no transcript, and
it never starts a background service. If the runtime is unavailable, the harness
is left with its own context rather than a partial claim.

**Codex collaboration compatibility is advertised — observed on Codex CLI
0.142.4.** Two Codex behaviors are worth knowing, because both look like a
broken install if you do not expect them:

- **The context arrives on your first turn, not before it.** Codex runs
  registered `SessionStart` hooks as *pending* hooks inside the first turn, so
  nothing is injected until you send a message. An empty session shows nothing;
  that is Codex's boundary, not a missing registration.
- **Codex gates every hook behind a one-time trust review.** A newly registered
  hook does not run — silently — until it is approved and enabled in Codex.
  Approve the Loam hook once when Codex offers the review; after that it fires
  on every session. For unattended automation Codex offers
  `--dangerously-bypass-hook-trust`, which runs enabled hooks without a
  persisted approval.

Codex also truncates long hook output in model context. The `<LOAM_IMPORTANT>`
framing, the runtime command, the workspace state block, and the federation
section all survive; part of the middle of the skill body may be elided, with
the full text written to a file Codex links in the same message.

## Background ingestion

Background code ingestion is enabled by default. To opt out, set
`LOAM_INGEST_BACKGROUND=0` as the unconditional kill switch, or set
`background_ingest.enabled` to `false` in `~/.agents/loam/config.json`.
`LOAM_INGEST_BACKGROUND=1` explicitly overrides a disabled config. The adapter
reads only the documented stop payload (`cwd`, `session_id`, and
`stop_hook_active`), closes handoff stdin, and writes `{}` to stdout. It never
reads a transcript.

Before launching, the worker takes a per-workspace lease, runs full native
state, requires `code_ingest_pending`, resolves installed exclusions, and
computes a complete actionable fingerprint. Live or uncertain workers,
missing exclusions, unreadable/racing files, and unknown process identity fail
closed. Check `ingest-status --workspace <path> --json` through the installed
integration. It never modifies source files, commits, or pushes.

## Background session harvest

Background session-learning harvest is enabled by default. On every turn end
the hook measures what has been said since the last harvest; when enough new
conversation exists, a detached agent reviews the window and routes durable
learnings through `loam::learning-from-session`. To opt out, set
`LOAM_HARVEST_BACKGROUND=0` as the unconditional kill switch, or set
`background_harvest.enabled` to `false` in `~/.agents/loam/config.json`.

Each session keeps its own cursor and harvests into its workspace's memory.
Harvest shares the per-workspace memory-writer lease with code ingestion and
never runs while another worker holds it. The harvest agent's own turn ends
are never re-harvested. Check `harvest-status --workspace <path> --json`
through the installed integration to inspect per-session cursors, the wiki
cache, last run, and the shared lease.

## Optional integrations

Loam skills are better with companion tools, but never require them (soft
dependency — a skill degrades gracefully when a tool is absent). Enable them
per install with the configurator, off by default:

```bash
npx @scchearn/loam setup --integration grep    # grep.app code search (remote MCP; queries egress to a public-repo index)
npx @scchearn/loam setup --integration qmd     # QMD markdown search (local Node tool + local MCP; no egress)
```

`setup` installs any needed tool into a loam-managed prefix, verifies it, then
registers the MCP into each configured harness using the tool's absolute path.
Disable is symmetric and complete:

```bash
npx @scchearn/loam setup --disable-integration qmd            # deregister everywhere + remove the loam-managed tool
npx @scchearn/loam setup --disable-integration qmd --purge    # also remove large derived caches (e.g. QMD's ~2–3GB model cache)
```

Loam never installs a tool or registers an MCP you did not select, and never
removes a user-owned MCP entry or a tool it did not install. `doctor` reports
per-integration state without failing.
