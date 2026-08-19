# Changelog

All notable changes to loam are documented here. This file follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/).

## [Unreleased] — planned v1.0.0

**v1.0.0 is not released yet.** This entry describes the feature set on the
`federation` branch while [PR #123] remains open and has not been merged into
`main`. Prerelease versions continue until that promotion is complete.

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

The next release entry must be added or moved from this section as part of the
release work; see [RELEASING.md](./docs/RELEASING.md).

[Unreleased]: https://github.com/scchearn/loam/compare/main...federation
[PR #123]: https://github.com/scchearn/loam/pull/123
[#114]: https://github.com/scchearn/loam/pull/114
[#146]: https://github.com/scchearn/loam/issues/146
[#163]: https://github.com/scchearn/loam/issues/163
