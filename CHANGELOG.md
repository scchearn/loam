# Changelog

All notable changes to loam are documented here. This file follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Fixed

- **The injected router now fits the harness hook cap.** `loam::using` is
  injected whole at session start; it had grown to ~5,000 tokens, and with
  the runtime line and state blocks the injection reached 21 KB. Claude Code
  caps hook context near 10 KB, so the model saw a 2 KB preview and the
  workspace state at the tail (wiki root, qmd, `hcom:`) never arrived. The
  router now carries only what a session needs before any skill is invoked
  (1,609 tokens); every other sentence moved verbatim into
  `references/memory-lifecycle.md`, `references/discovery.md`, and
  `references/runtime.md`, each named at its trigger point. The native
  session-start assembly emits the runtime command and state blocks before
  the skill body, and rewrites the body's `references/` pointers to absolute
  paths so progressive disclosure resolves from a system prompt. Skills that
  cited protocol sections in the router now name the reference file. ([#213])

### Added

- **hcom transcripts as a secondary memory source.** `querying-memory` may
  search agent transcripts through the hcom MCP tool or CLI when the injected
  `hcom:` line says `ready`, ranked below the wiki for curation but not for
  recency; on disagreement it reports both and routes to `amending-memory`.
  The router gains a discovery order (wiki via qmd, then transcripts, then raw
  source) and a memory-first red flag. Skips silently when hcom is not
  installed. ([#213])

## [1.0.3]

Plugin 1.0.3 and runtime 1.0.2 ship together.

> **Existing workspaces: run the guidance lint once** if you skipped it on
> 1.0.2. Session start now tells you when the map is missing (see below).

### Fixed

- **OpenCode agents keep their loam context past the first turn.** The
  OpenCode adapter injected everything through a transform hook that OpenCode
  applies per model call and never stores, so the router block and workspace
  state existed for exactly one call and every later turn saw only the
  federation line; items drained from the federation mailbox were shown once
  and then lost; and a second session in the same OpenCode server never got a
  session start at all. Each kind of context now rides the hook whose lifetime
  matches it: the session-start block lives in the system prompt (cached per
  session, so provider prompt caching pays for it once), the per-turn
  federation delta is written into the stored user message as a hidden part,
  and the block is re-supplied after compaction. Claude Code, Codex, and
  Cursor were never affected. ([#209], [#210])
- **Quiet turns inject nothing.** The per-prompt federation refresh used to
  re-inject the status line on every prompt even when nothing had changed,
  including the permanent `unenrolled` line in workspaces that never joined a
  project. A turn with no new items now injects an empty envelope on every
  harness; the status line renders at session start only. A session whose
  mailbox registration was lost to a connector restart re-registers and
  retries once instead of re-rendering the whole backlog each turn. ([#204],
  [#210])

### Added

- **Session start says when the memory map is missing.** A workspace whose
  `AGENTS.md` has no `loam:memory-map` region, or whose map has drifted from
  the wiki, now gets a `guidance_map_missing` / `guidance_map_stale` signal in
  the injected workspace state pointing at `/loam::linting-memory`, instead of
  only surfacing when someone runs the guidance lint by hand. ([#211])

## [1.0.2]

> **Existing workspaces: run the guidance lint once.** The `AGENTS.md` memory
> map below is seeded by scaffolding for new workspaces only. A workspace that
> already has a wiki gets it by running
> `loam lint --only guidance --fix <workspace>` (or `/loam::linting-memory`)
> once after updating. Until then `loam lint` reports the map as missing.

### Added

- **Specs for judged work now name their oracle.** When a request's quality bar
  is judged rather than test-proven (visual/UI/design, generated content, prose,
  media), `loam::writing-spec` elicits four oracle questions — the reference
  that defines "good", the evidence contract an agent must produce before
  claiming done, the stop condition (including blind comparison where
  available), and the retry budget — and records them in two new spec sections,
  `## Quality anchor` and `## Verification oracle`. The completeness gate
  enforces both, and the evidence contract must appear in Scope so planning
  schedules the verification harness as early work instead of an afterthought.
  Hard-oracle work (done provable by tests or commands) is unaffected.
  (`loam-writing-spec` 3.3.0) ([#207])
- **The memory announces itself in `AGENTS.md`.** A workspace with a wiki now
  carries a small Loam-owned memory map inside its guidance file, listing the
  durable page slugs by type so an agent learns the memory exists at session
  start instead of having to go looking. It is plain markdown that every
  harness already reads — no import syntax, no session-start injection — and it
  stays bounded as the wiki grows. `loam lint` gains a `guidance` domain that
  reports the map missing or out of date and a drifted `CLAUDE.md` shim, and
  `loam lint --fix` regenerates the map in place without touching anything
  outside its markers. Scaffolding seeds the block, the guidance audit can
  refresh it with or without the native runtime, and normalization leaves it
  alone. ([#206])

### Fixed

- **Federation connector stays online.** The production broker's access rules
  now grant the live-session subscriptions and retained member-card publication
  the connector needs, so an enrolled machine no longer connects and then loops
  offline. A real-broker contract test guards the rules, the deployment gate
  runs it, and the broker acceptance checklist reflects the settled trust model:
  the organization is the trust boundary, project membership is routing, and
  member cards make project sharing visible across the organization. ([#173])

## [1.0.1]

### Added

- **Optional integrations are offered during install.** The setup wizard now
  surfaces the opt-in integrations (research and coordination tools) while
  installing, so a new install can enable them interactively instead of having
  to edit configuration afterward. ([#197])

## [1.0.0]

### Added

- **Shared work state between computers.** Federation connects workspaces to a
  self-hosted MQTT broker with mutual TLS, so collaborators can see current
  updates in their own coding tools without copying state by hand.
- **One-command project enrollment.** `loam federation connect` checks the
  workspace, Git identity, project, broker, and credentials before saving an
  enrollment; a new machine can obtain its client certificate with an
  enrollment token. Failures identify the part that needs attention instead of
  leaving a half-configured project. ([#163])
- **Enrollment inventory and local status.** `loam federation list` shows every
  project joined by the computer, and `loam federation status` shows local
  enrollment and service-manager state. Both commands are read-only and support
  JSON output, making it easier to inspect or script setup. ([#146])
- **Automatic session delivery.** Claude Code, Codex CLI, and OpenCode can show
  federation updates at session start and during a session. Cursor keeps the
  skills and CLI access, but does not claim automatic collaboration support
  until it has been evaluated. ([#114])
- **Optional research and coordination tools.** grep.app code search, local
  qmd Markdown search, and detection-only hcom support are opt-in, so the core
  skills remain useful without extra tools or network access.
- **Background memory maintenance.** Session learning and code indexing can run
  in the background, remember what each session has already harvested, and
  leave source files unchanged.

### Changed

- **Clear installation commands.** `install` handles first-time installation and
  same-version repair, `update` moves an existing installation to a new package
  version, and `setup` changes federation, integrations, or selected coding
  tools without moving versions. The commands now explain which one to use.
- **Reliable connector lifecycle.** The connector retries failed broker sessions,
  can be restarted by the platform service manager after a sustained failure,
  and reports degraded collaboration instead of calling stale state live. ([PR #123])
- **Runtime channel ledger.** The runtime target is recorded in a config-dir
  ledger and checked against the native binary's own version. The retired skills
  tree `CLI_VERSION` no longer controls runtime selection, so a stale skills
  copy cannot route installation to the wrong binary.
- **Independent release tracks.** Plugin packages use `v<version>` tags and
  npm channels; native runtimes use `cli-v<version>` tags and per-platform
  GitHub release artifacts. Either can change without forcing a release of the
  other.
- **Safe removal and reinstallation.** A normal uninstall removes the global
  installation but keeps the durable federation identity and enrollment. The
  explicit `--purge` option is required to destroy that config directory, so a
  reinstall can normally resume with the same machine identity.

### Fixed

- **Actionable enrollment refusals.** Invalid organization scope, bad tokens,
  missing Git identity, unreachable or slow signers, trust problems, and local
  certificate-store failures now have separate messages and next steps. ([#163])
- **Service updates on macOS.** Updating an installation now refreshes the
  LaunchAgent definition and applies the new runtime instead of leaving the
  connector on an older binary. ([PR #123])
- **Cross-platform setup reliability.** Full-matrix checks and platform-specific
  fixes cover Linux, macOS, and Windows setup, service lifecycle, hooks, and
  runtime downloads before a release is promoted.

Release entries are maintained as part of release work; see
[RELEASING.md](./docs/RELEASING.md).

[Unreleased]: https://github.com/scchearn/loam/compare/v1.0.3...HEAD
[1.0.3]: https://github.com/scchearn/loam/releases/tag/v1.0.3
[1.0.2]: https://github.com/scchearn/loam/releases/tag/v1.0.2
[1.0.1]: https://github.com/scchearn/loam/releases/tag/v1.0.1
[1.0.0]: https://github.com/scchearn/loam/releases/tag/v1.0.0
[#114]: https://github.com/scchearn/loam/pull/114
[#197]: https://github.com/scchearn/loam/pull/197
[#146]: https://github.com/scchearn/loam/issues/146
[#163]: https://github.com/scchearn/loam/issues/163
[#173]: https://github.com/scchearn/loam/issues/173
[#206]: https://github.com/scchearn/loam/pull/206
[#207]: https://github.com/scchearn/loam/pull/207
[#204]: https://github.com/scchearn/loam/issues/204
[#209]: https://github.com/scchearn/loam/issues/209
[#210]: https://github.com/scchearn/loam/pull/210
[#211]: https://github.com/scchearn/loam/pull/211
[#213]: https://github.com/scchearn/loam/pull/213
