# Installing loam for OpenCode

## Prerequisites

- [OpenCode.ai](https://opencode.ai) installed

## Installation

Add loam to the `plugin` array in your `opencode.json` (global or project-level):

```json
{
  "plugin": ["loam@git+https://github.com/scchearn/loam.git"]
}
```

For local development, point at your clone path:

```json
{
  "plugin": ["loam@file:///path/to/your/loam"]
}
```

Restart OpenCode. That's it — the plugin auto-registers all loam skills and injects `loam::using` into the first user message of each session.

Verify by asking: "Tell me about loam" — the session should already have the `loam::using` router context (look for `<LOAM_IMPORTANT>`).

## Usage

Use OpenCode's native `skill` tool:

```
use skill tool to list skills
use skill tool to load loam::planning
```

## Updating

If you pointed at a local path, `git pull` the repo and restart OpenCode.
If you pointed at the remote, OpenCode updates automatically on restart.

To pin a specific version:

```json
{
  "plugin": ["loam@git+https://github.com/scchearn/loam.git#v0.1.0"]
}
```

## Troubleshooting

### Plugin not loading

1. Check logs: `opencode run --print-logs "hello" 2>&1 | grep -i loam`
2. Verify the plugin line in your `opencode.json`
3. Make sure you're running a recent version of OpenCode

### Skills not found

1. Use the `skill` tool to list what's discovered
2. Check that the plugin is loading (see above)
3. Skills are also available via `npx skills add scchearn/loam` as a fallback discovery path

### Tool mapping

When skills reference Claude Code tools:
- `TodoWrite` → `todowrite`
- `Task` with subagents → `@mention` syntax
- `Skill` tool → OpenCode's native `skill` tool
- File operations → your native tools