import { homedir } from 'node:os';
import { readFile, readdir, rm, stat } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';

import { writeAtomicFile } from './atomic.mjs';
import { isOwnedCommand } from './harnesses.mjs';

// ponytail: uninstall reverses setup. No Skills CLI touch — global skills
// remain under ~/.agents/skills/ for `npx skills remove` or a future setup rerun.
// Harness configs are cleaned in-place (remove only Loam-owned hook entries,
// preserve unrelated config) rather than blind-restoring backups, because a
// later setup rerun may have superseded the backup. If no Loam entries remain
// and the config file is empty of other content, leave it — deleting user files
// is setup's job, not uninstall's.

async function exists(path) {
  try {
    await stat(path);
    return true;
  } catch {
    return false;
  }
}

function stripLoamHooks(hooks, globalRoot, assetName) {
  if (!Array.isArray(hooks)) return hooks;
  return hooks.filter((hook) => !isOwnedCommand(hook, globalRoot, assetName));
}

function cleanClaudeConfig(config, globalRoot) {
  if (!config?.hooks?.SessionStart) return config;
  const cleaned = Array.isArray(config.hooks.SessionStart)
    ? config.hooks.SessionStart
        .map((entry) => {
          if (!entry || typeof entry !== 'object' || !Array.isArray(entry.hooks)) return entry;
          const hooks = stripLoamHooks(entry.hooks, globalRoot, 'claude-session-start.mjs');
          return hooks.length === entry.hooks.length ? entry : { ...entry, hooks };
        })
        .filter((entry) => entry?.hooks?.length !== 0 || !Array.isArray(entry?.hooks))
    : config.hooks.SessionStart;
  return { ...config, hooks: { ...config.hooks, SessionStart: cleaned } };
}

function cleanCursorConfig(config, globalRoot) {
  if (!config?.hooks?.sessionStart) return config;
  return {
    ...config,
    hooks: {
      ...config.hooks,
      sessionStart: stripLoamHooks(config.hooks.sessionStart, globalRoot, 'cursor-session-start.mjs'),
    },
  };
}

async function hasBackup(dir) {
  for (const entry of await readdir(dir, { withFileTypes: true }).catch(() => [])) {
    if (entry.isFile() && entry.name.includes('.backup-')) return true;
  }
  return false;
}

async function cleanHarnessConfig(path, globalRoot, cleanFn) {
  let config;
  try {
    config = JSON.parse(await readFile(path, 'utf8'));
  } catch (error) {
    if (error?.code === 'ENOENT') return { path, action: 'absent' };
    return { path, action: 'skipped', reason: 'malformed JSON' };
  }
  const cleaned = cleanFn(config, globalRoot);
  // ponytail: if no backup exists, setup created this file fresh — delete it
  // after cleaning rather than leaving an empty husk. If a backup exists, setup
  // modified a pre-existing file — clean in place and let removeBackups handle
  // the backup separately.
  const hadBackup = await hasBackup(dirname(path));
  const hooks = cleaned?.hooks || {};
  const hasHooks = Object.values(hooks).some((value) =>
    Array.isArray(value) ? value.length > 0 : value != null);
  if (!hadBackup && !hasHooks) {
    await rm(path, { force: true });
    return { path, action: 'deleted' };
  }
  await writeAtomicFile(path, `${JSON.stringify(cleaned, null, 2)}\n`);
  return { path, action: 'cleaned' };
}

async function removeBackups(dir) {
  const removed = [];
  for (const entry of await readdir(dir, { withFileTypes: true }).catch(() => [])) {
    if (entry.isFile() && entry.name.includes('.backup-')) {
      const path = join(dir, entry.name);
      await rm(path, { force: true });
      removed.push(path);
    }
  }
  return removed;
}

export async function uninstall({
  home = homedir(),
  globalRoot,
  yes = false,
  confirm = async () => false,
  output = process.stdout,
} = {}) {
  const root = resolve(globalRoot || join(home, '.agents', 'loam'));
  const metadataPath = join(root, 'install.json');

  let install = null;
  try {
    install = JSON.parse(await readFile(metadataPath, 'utf8'));
  } catch {
    output.write('No Loam installation found at %s. Nothing to uninstall.\n'.replace('%s', root));
    return 0;
  }

  output.write('Loam uninstall will:\n');
  output.write('  - Remove Loam-owned hook entries from Claude and Cursor configs\n');
  output.write('  - Remove the OpenCode Loam adapter\n');
  output.write('  - Remove the global Loam root (install.json, runtime, integration, plugins)\n');
  output.write('  - Leave global skills intact (use `npx skills remove` separately)\n');
  output.write(`  - Global root: ${root}\n`);

  if (!yes && !(await confirm())) {
    output.write('Uninstall cancelled.\n');
    return 130;
  }

  const results = { configs: [], opencode: null, globalRoot: null, backups: [] };

  // Clean harness configs in-place
  if (install.configured_harnesses?.includes('claude')) {
    results.configs.push(await cleanHarnessConfig(join(home, '.claude', 'settings.json'), root, cleanClaudeConfig));
    results.backups.push(...(await removeBackups(join(home, '.claude'))));
  }
  if (install.configured_harnesses?.includes('cursor')) {
    results.configs.push(await cleanHarnessConfig(join(home, '.cursor', 'hooks.json'), root, cleanCursorConfig));
    results.backups.push(...(await removeBackups(join(home, '.cursor'))));
  }

  // Remove OpenCode adapter
  const opencodePath = join(home, '.config', 'opencode', 'plugins', 'loam.mjs');
  if (await exists(opencodePath)) {
    await rm(opencodePath, { force: true });
    results.opencode = { path: opencodePath, action: 'removed' };
  }

  // Remove global root
  await rm(root, { recursive: true, force: true });
  results.globalRoot = { path: root, action: 'removed' };

  output.write('Loam uninstalled.\n');
  return 0;
}