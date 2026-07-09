# Installing loam for Codex

Enable loam skills in Codex via native skill discovery.

## Prerequisites

- Git

## Installation

**Option A — npx skills (recommended):**

```bash
npx skills add scchearn/loam
```

Skills install to `~/.agents/skills/loam-*` and are discovered automatically by Codex on the next session.

**Option B — clone and symlink (manual):**

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
npx skills list -g | grep loam
```

You should see all 20 loam skills listed.

## Updating

```bash
npx skills update
```

Or if you used the clone+symlink path: `cd ~/.codex/loam && git pull`

## Uninstalling

```bash
npx skills remove loam
```

Or if you used the clone+symlink path: `rm ~/.agents/skills/loam` and `rm -rf ~/.codex/loam`.

## Note on auto-injection

Codex does not support session-start context injection. loam skills are discovered via `~/.agents/skills/loam-*` and invoked on demand through Codex's skill loader. The `loam::using` router skill is the entry point — invoke it at session start or whenever a loam task appears.