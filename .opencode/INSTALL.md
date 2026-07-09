# Installing loam for OpenCode

## Prerequisites

- [OpenCode.ai](https://opencode.ai) installed
- loam skills installed via `npx skills add scchearn/loam` (the plugin reads skill content from here — there is no bundled copy)
- Git

## Installation

1. **Clone the loam repository:**

   ```bash
   git clone https://github.com/scchearn/loam.git ~/.config/opencode/loam
   ```

2. **Add the plugin entry point to your `opencode.json`:**

   ```json
   {
     "plugin": ["/home/YOU/.config/opencode/loam/.opencode/plugins/loam.js"]
   }
   ```

   Replace `/home/YOU` with your actual home path. The path must point to the
   `loam.js` file inside the clone, not a `git+https://` spec — OpenCode's Bun
   cache SHA-pins git deps and never re-resolves them
   ([#6774](https://github.com/anomalyco/opencode/issues/6774),
   [#10546](https://github.com/anomalyco/opencode/issues/10546)).

3. **Restart OpenCode.**

   The plugin injects `loam::using` into the first user message of each session.
   Skill discovery is handled by OpenCode natively scanning `~/.agents/skills/`
   (where `npx skills add` installed them).

## Verify

Ask the agent: "Do you have loam?" — it should confirm and show the plugin
version (e.g. `You have loam (v0.2.0).`). The `<LOAM_IMPORTANT>` block should
be present from the first message without invoking the skill.

## Updating

**Skill content:**

```bash
npx skills update
```

**Plugin code:**

```bash
cd ~/.config/opencode/loam && git pull
```

Restart OpenCode after updating.

The plugin also auto-checks for updates at session start — if the local clone
is behind `origin/main`, the injected context includes an update notice that
the agent can surface to you.

## Troubleshooting

### Plugin not loading

1. Check logs: `opencode run --print-logs "hello" 2>&1 | grep -i loam`
2. Verify the path in your `opencode.json` points to the `.opencode/plugins/loam.js` file
3. Make sure the clone exists at the path you specified

### Skills not found

1. Run `npx skills add scchearn/loam` to install skill content
2. Use the `skill` tool to list what's discovered
3. Check that the plugin is loading (see above)

### Injection not appearing

1. Check stderr for `[loam]` warnings — the plugin logs there when skill content is missing
2. Verify `~/.agents/skills/loam-using/SKILL.md` exists (`npx skills list -g | grep loam`)

### Tool mapping

When skills reference Claude Code tools:
- `TodoWrite` → `todowrite`
- `Task` with subagents → `@mention` syntax
- `Skill` tool → OpenCode's native `skill` tool
- File operations → your native tools