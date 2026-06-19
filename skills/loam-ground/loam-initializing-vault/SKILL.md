---
name: loam::initializing-vault
description: "Scaffold a new Obsidian vault with pre-configured settings, keymaps, and core plugins. Use when the user wants to \"set up a new Obsidian vault\", \"create an Obsidian vault\", or \"replicate my Obsidian setup\"."
allowed-tools: Bash
metadata:
  version: "1.0.0"
  author: scchearn
  argument-hint: <target-vault-path>
---

The skill file is at `$SKILL_PATH`. Derive the script path from it:

```
SKILL_DIR="$(dirname "$SKILL_PATH")"
bash "$SKILL_DIR/scripts/setup.sh" <vault-path>
```

Run that single command with the vault path from `$ARGUMENTS`. Do nothing else — no file reads, no workspace exploration.
