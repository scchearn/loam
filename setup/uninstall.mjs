import { homedir } from 'node:os';
import { readFile, readdir, rm, stat } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';

import { writeAtomicFile } from './atomic.mjs';
import { isOwnedCommand } from './harnesses.mjs';
import { classifyChild } from '../integration/ingest-process.mjs';
import { inspectIntent } from '../integration/ingest.mjs';

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
  if (!config?.hooks) return config;
  const cleanEntries = (entries, assetName) => Array.isArray(entries)
    ? entries
        .map((entry) => {
          if (!entry || typeof entry !== 'object' || !Array.isArray(entry.hooks)) return entry;
          const hooks = stripLoamHooks(entry.hooks, globalRoot, assetName);
          return hooks.length === entry.hooks.length ? entry : { ...entry, hooks };
        })
        .filter((entry) => entry?.hooks?.length !== 0 || !Array.isArray(entry?.hooks))
    : entries;
  return {
    ...config,
    hooks: {
      ...config.hooks,
      SessionStart: cleanEntries(config.hooks.SessionStart, 'claude-session-start.mjs'),
      Stop: cleanEntries(config.hooks.Stop, 'claude-stop.mjs'),
    },
  };
}

function cleanCodexGroups(groups, globalRoot, assetName) {
  if (!Array.isArray(groups)) return groups;
  return groups
    .map((entry) => {
      if (!Array.isArray(entry?.hooks)) return isOwnedCommand(entry, globalRoot, assetName) ? null : entry;
      const hooks = stripLoamHooks(entry.hooks, globalRoot, assetName);
      return hooks.length === entry.hooks.length ? entry : { ...entry, hooks };
    })
    .filter((entry) => entry && (!Array.isArray(entry?.hooks) || entry.hooks.length > 0));
}

function cleanCodexConfig(config, globalRoot) {
  if (!config?.hooks) return config;
  return {
    ...config,
    hooks: {
      ...config.hooks,
      SessionStart: cleanCodexGroups(config.hooks.SessionStart, globalRoot, 'codex-session-start.mjs'),
      Stop: cleanCodexGroups(config.hooks.Stop, globalRoot, 'codex-stop.mjs'),
    },
  };
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

async function blockingWorkers(root) {
  const blocked = [];
  const runRoot = join(root, 'run');
  for (const entry of await readdir(runRoot, { withFileTypes: true }).catch(() => [])) {
    if (!entry.isDirectory()) continue;
    const leasePath = join(runRoot, entry.name, 'lease.json');
    const readRecord = async (path) => {
      try { return { present: true, value: JSON.parse(await readFile(path, 'utf8')) }; }
      catch (error) { return error?.code === 'ENOENT' ? { present: false } : { present: true, malformed: true }; }
    };
    const leaseRecord = await readRecord(leasePath);
    if (leaseRecord.malformed || (leaseRecord.present && (leaseRecord.value?.schema !== 1
      || !Number.isInteger(leaseRecord.value.owner_pid) || !leaseRecord.value.boot_id))) {
      blocked.push({ path: leasePath, state: 'unknown' });
      continue;
    }
    const lease = leaseRecord.value;
    if (leaseRecord.present) {
      const state = await classifyChild({ pid: lease.owner_pid, boot_id: lease.boot_id, process_start: lease.process_start });
      if (state === 'live' || state === 'unknown') {
        blocked.push({ path: leasePath, state });
        continue;
      }
      const child = await inspectIntent(leasePath, lease.workspace || '', undefined);
      if (child.state === 'live' || child.state === 'unknown') blocked.push({ path: leasePath, state: child.state });
    }
  }
  return blocked;
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
  output.write('  - Remove Loam-owned hook entries from Claude, Codex, and Cursor configs\n');
  output.write('  - Remove the OpenCode Loam adapter\n');
  output.write('  - Remove the global Loam root (install.json, runtime, integration, plugins)\n');
  output.write('  - Leave global skills intact (use `npx skills remove` separately)\n');
  output.write(`  - Global root: ${root}\n`);

  if (!yes && !(await confirm())) {
    output.write('Uninstall cancelled.\n');
    return 130;
  }

  const workers = await blockingWorkers(root);
  if (workers.length) {
    output.write(`Uninstall blocked by ${workers.length} active or uncertain background worker lease(s).\n`);
    return 1;
  }

  const results = { configs: [], opencode: null, globalRoot: null, backups: [] };

  // Clean harness configs in-place
  if (install.configured_harnesses?.includes('claude')) {
    results.configs.push(await cleanHarnessConfig(join(home, '.claude', 'settings.json'), root, cleanClaudeConfig));
    results.backups.push(...(await removeBackups(join(home, '.claude'))));
  }
  if (install.configured_harnesses?.includes('codex')) {
    results.configs.push(await cleanHarnessConfig(join(home, '.codex', 'hooks.json'), root, cleanCodexConfig));
    results.backups.push(...(await removeBackups(join(home, '.codex'))));
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
