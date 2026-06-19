# loam command installation reference (for agents)

This document is for agents, not humans. If you are an agent running in a loam-equipped workspace and the user wants slash-command shortcuts (e.g. `/checkpoint`, `/resume`), read this document to learn how to install them for your harness, then ask the user for permission before copying.

The command files are bundled as assets alongside this reference. Resolve them relative to the `loam-using` skill directory (the parent of `references/` and `assets/`).

## Available commands

| Command | Delegates to | Source file (markdown) | Source file (TOML) |
|---|---|---|---|
| `/checkpoint` | `loam::checkpointing` | `assets/commands/checkpoint.md` | `assets/commands/gemini/checkpoint.toml` |
| `/resume` | `loam::resuming` | `assets/commands/resume.md` | `assets/commands/gemini/resume.toml` |

## Compatibility matrix

Split by command format, not just harness. One format does not fit all.

| Harness | Format | Global location | Project location | Invoke as | Notes |
|---|---|---|---|---|---|
| **OpenCode** | Markdown | `~/.config/opencode/commands/` | `.opencode/commands/` | `/checkpoint` | Copy `assets/commands/*.md` |
| **Claude Code** | Markdown | `~/.claude/commands/` | `.claude/commands/` | `/checkpoint` | Copy `assets/commands/*.md` |
| **Gemini CLI** | TOML | `~/.gemini/commands/` | `.gemini/commands/` | `/checkpoint` | Copy `assets/commands/gemini/*.toml`. Run `/commands reload` after install. |
| **Codex** | (not supported) | — | — | `$loam::checkpointing` | Custom prompts were removed in codex-cli 0.117.0 (March 2026). Skills are invoked with `$` prefix, not `/`. Use `$loam::checkpointing` and `$loam::resuming` directly. Do not install to `~/.codex/prompts/`. |
| **Copilot CLI** | (not supported) | — | — | — | No prompt slash-command equivalent. Use `/loam::checkpointing` and `/loam::resuming` directly. Custom agents (`.agent.md`) are a different feature and overkill for thin wrappers. |

## Harness detection signals

Config dirs are hints only — multiple tools can be installed on the same machine. Active harness beats installed-dir detection.

| Signal | Indicates | Reliability |
|---|---|---|
| `$HCOM_TOOL` env var (e.g. `opencode`, `claude`, `codex`) | The hcom-managed harness | High — hcom sets this explicitly |
| `$OPENCODE_PERMISSION` env var | OpenCode | High — only opencode sets this |
| `HCOM_PANE_TITLE` contains `[opencode]` / `[claude]` / `[codex]` | The hcom-managed harness | Medium — parse the bracketed name |
| `~/.config/opencode/` dir exists | OpenCode installed | Low — installed ≠ active |
| `~/.claude/` dir exists | Claude Code installed | Low — installed ≠ active |
| `~/.gemini/` dir exists | Gemini CLI installed | Low — installed ≠ active |
| `~/.codex/` dir exists | Codex installed | Low — installed ≠ active |

**If detection is ambiguous** (multiple harnesses installed, no active-harness signal), ask the user which harness they are running.

## Protocol

1. **Detect active harness.** Check env vars first (`$HCOM_TOOL`, `$OPENCODE_PERMISSION`), then config dirs. If ambiguous, ask the user.
2. **Determine scope.** Default to **project-local** when in a workspace (global changes are surprising). If no workspace is active, use global. If unsure, ask the user.
3. **Check if commands already installed.** Look for `checkpoint.md` (OpenCode/Claude Code), `checkpoint.toml` (Gemini), or `checkpoint.md` in `~/.codex/prompts/` (Codex) in the target dir.
   - If already installed and identical: say "already installed", do nothing.
   - If already installed and differs: ask the user before overwriting.
   - If not installed: proceed to step 4.
4. **Ask the user.** "Would you like me to install the `/checkpoint` and `/resume` slash commands for `<harness>` at `<location>`?"
5. **If yes, copy the correct format files from the skill's assets:**
   - OpenCode / Claude Code: copy `assets/commands/checkpoint.md` and `assets/commands/resume.md` to `<target>/`
   - Gemini CLI: copy `assets/commands/gemini/checkpoint.toml` and `assets/commands/gemini/resume.toml` to `<target>/`
   - Codex: not supported — custom prompts were removed in codex-cli 0.117.0. Use `$loam::checkpointing` and `$loam::resuming` (the `$` prefix invokes skills) directly. Do not install to `~/.codex/prompts/`.
   - Copilot CLI: explain that slash commands aren't supported; use `/loam::checkpointing` and `/loam::resuming` directly
6. **Post-install actions:**
   - Gemini CLI: instruct the user to run `/commands reload` (or run it if the harness supports agent-initiated reload)
   - Codex: note that custom prompts are removed (codex-cli 0.117.0+); skills are invoked with `$` prefix
   - OpenCode / Claude Code: available next session start (or immediately, depending on harness)
7. **Confirm installation.** Tell the user which commands were installed, where, and how to invoke them.

## What not to do

- **Do not install without asking.** Always get user permission first.
- **Do not install globally by default.** Global changes are surprising; prefer project-local.
- **Do not overwrite silently.** If a command file exists and differs, ask first.
- **Do not copy markdown files to Gemini.** Gemini expects TOML. Use `assets/commands/gemini/*.toml`.
- **Do not install Codex prompts.** Custom prompts were removed in codex-cli 0.117.0. Use `$loam::checkpointing` and `$loam::resuming` directly.
- **Do not invent a Copilot CLI command path.** It doesn't have one for this purpose.
- **Do not add more commands (e.g. `/plan`, `/spec`, `/learn`, `/query`).** Higher collision risk with existing harness commands. Ship checkpoint and resume only.