# Installing loam for Codex

Enable loam skills in Codex via native skill discovery. Just clone and symlink.

## Prerequisites

- Git

## Installation

1. **Clone the loam repository:**
   ```bash
   git clone https://github.com/scchearn/loam.git ~/.codex/loam
   ```

2. **Create the skills symlink:**
   ```bash
   mkdir -p ~/.agents/skills
   ln -s ~/.codex/loam/skills ~/.agents/skills/loam
   ```

   **Windows (PowerShell):**
   ```powershell
   New-Item -ItemType Directory -Force -Path "$env:USERPROFILE\.agents\skills"
   cmd /c mklink /J "$env:USERPROFILE\.agents\skills\loam" "$env:USERPROFILE\.codex\loam\skills"
   ```

3. **Restart Codex** (quit and relaunch the CLI) to discover the skills.

## Verify

```bash
ls -la ~/.agents/skills/loam
```

You should see a symlink (or junction on Windows) pointing to your loam skills directory.

## Updating

```bash
cd ~/.codex/loam && git pull
```

Skills update instantly through the symlink.

## Uninstalling

```bash
rm ~/.agents/skills/loam
```

Optionally delete the clone: `rm -rf ~/.codex/loam`.

## Note on auto-injection

Codex does not support session-start context injection. loam skills are discovered via `~/.agents/skills/loam-*` and invoked on demand through Codex's skill loader. The `loam::using` router skill is the entry point — invoke it at session start or whenever a loam task appears.