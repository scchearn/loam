import { homedir } from 'node:os';
import { readFile, readdir, rm, stat } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';

import { writeAtomicFile } from './atomic.mjs';
import { PACKAGE_ROOT } from './constants.mjs';
import { detectHarnesses, isOwnedCommand } from './harnesses.mjs';
import { classifyChild } from '../integration/ingest-process.mjs';
import { inspectIntent } from '../integration/ingest.mjs';
import { loadSkillInventory } from './inventory.mjs';
import { listSkills, skillEntryAliases, skillEntrySource } from './skills.mjs';
import { runSkills } from './process.mjs';
import { announce, confirmUninstall, finish } from './wizard.mjs';
import { removeMarketplacePlugins } from './marketplace.mjs';

const codexAgentMarker = '# Managed by @scchearn/loam setup.';

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

async function inspectCodexAgentProfile(home) {
  const path = join(home, '.codex', 'agents', 'loam_ingestor.toml');
  const backupPath = `${path}.loam-backup`;
  let contents;
  let backup;
  try { contents = await readFile(path, 'utf8'); }
  catch (error) { if (error?.code !== 'ENOENT') throw error; }
  try { backup = await readFile(backupPath, 'utf8'); }
  catch (error) { if (error?.code !== 'ENOENT') throw error; }
  return {
    path,
    backupPath,
    contents,
    backup,
    owned: contents?.startsWith(codexAgentMarker) === true,
  };
}

async function removeCodexAgentProfile(profile) {
  if (profile.owned) {
    if (profile.backup !== undefined) {
      await writeAtomicFile(profile.path, profile.backup);
      await rm(profile.backupPath, { force: true });
      return { path: profile.path, action: 'restored' };
    }
    await rm(profile.path, { force: true });
    return { path: profile.path, action: 'removed' };
  }
  if (profile.contents === undefined && profile.backup !== undefined) {
    await writeAtomicFile(profile.path, profile.backup);
    await rm(profile.backupPath, { force: true });
    return { path: profile.path, action: 'restored' };
  }
  return { path: profile.path, action: profile.contents === undefined ? 'absent' : 'preserved' };
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

function installedSkillName(entry) {
  return [entry.name, entry.skill, entry.id, entry.slug, entry.directory]
    .find((value) => typeof value === 'string' && value);
}

async function findGlobalLoamSkills({ packageRoot = PACKAGE_ROOT, expectedSource = '', runner } = {}) {
  const inventory = await loadSkillInventory({ packageRoot });
  const knownAliases = new Set(inventory.skills.flatMap((skill) => skill.aliases));
  const listed = await listSkills({ global: true, runner });
  if (!listed.ok) {
    return {
      ready: false,
      category: listed.category || 'skills_list_failed',
      detail: listed.stderr || 'Skills CLI list failed',
      names: [],
    };
  }

  const names = [...new Set(listed.entries.flatMap((entry) => {
    if (!skillEntryAliases(entry).some((alias) => knownAliases.has(alias))) return [];
    const source = skillEntrySource(entry, listed.source) || expectedSource;
    if (!source.includes('scchearn/loam')) return [];
    const name = installedSkillName(entry);
    return name ? [name] : [];
  }))];
  return { ready: true, names };
}

async function removeGlobalSkills({ packageRoot, expectedSource, runner, initial } = {}) {
  const found = initial || await findGlobalLoamSkills({ packageRoot, expectedSource, runner });
  if (!found.ready || !found.names.length) return found;

  const removed = await runSkills(['remove', ...found.names, '--global', '--yes'], { runner });
  if (!removed.ok) {
    return {
      ready: false,
      category: removed.category || 'skills_remove_failed',
      detail: removed.stderr || 'Skills CLI remove failed',
      names: found.names,
    };
  }

  const remaining = await findGlobalLoamSkills({ packageRoot, expectedSource, runner });
  if (!remaining.ready) return remaining;
  return remaining.names.length
    ? { ready: false, category: 'skills_remove_incomplete', detail: remaining.names.join(', '), names: remaining.names }
    : { ready: true, names: found.names };
}

// Tear down a specific set of harnesses (used when setup deselects a
// previously-configured one). Reuses the same in-place cleaners as a full
// uninstall, scoped to the given ids. Never touches the global root or skills.
export async function removeHarnesses({
  ids = [],
  home = homedir(),
  globalRoot,
  runner,
  cwd = process.cwd(),
} = {}) {
  const root = resolve(globalRoot || join(home, '.agents', 'loam'));
  const idSet = new Set(ids);
  const result = {};

  const marketIds = ['claude', 'codex'].filter((id) => idSet.has(id));
  if (marketIds.length) {
    const detected = await detectHarnesses({ home });
    const scoped = Object.fromEntries(marketIds.map((id) => [id, detected[id]]));
    result.marketplace = await removeMarketplacePlugins({ harnesses: scoped, cwd, runner });
  }

  for (const id of ids) {
    if (id === 'claude') {
      result.claude = await cleanHarnessConfig(join(home, '.claude', 'settings.json'), root, cleanClaudeConfig);
      await removeBackups(join(home, '.claude'));
    } else if (id === 'codex') {
      result.codex = await cleanHarnessConfig(join(home, '.codex', 'hooks.json'), root, cleanCodexConfig);
      await removeBackups(join(home, '.codex'));
      await removeCodexAgentProfile(await inspectCodexAgentProfile(home));
    } else if (id === 'cursor') {
      result.cursor = await cleanHarnessConfig(join(home, '.cursor', 'hooks.json'), root, cleanCursorConfig);
      await removeBackups(join(home, '.cursor'));
    } else if (id === 'opencode') {
      for (const name of ['loam.js', 'loam.mjs']) {
        const path = join(home, '.config', 'opencode', 'plugins', name);
        if (await exists(path)) await rm(path, { force: true });
      }
      result.opencode = { action: 'removed' };
    }
  }
  return result;
}

export async function uninstall({
  home = homedir(),
  globalRoot,
  packageRoot = PACKAGE_ROOT,
  runner,
  yes = false,
  confirm,
  input = process.stdin,
  output = process.stdout,
} = {}) {
  const root = resolve(globalRoot || join(home, '.agents', 'loam'));
  const metadataPath = join(root, 'install.json');

  let install = null;
  try {
    install = JSON.parse(await readFile(metadataPath, 'utf8'));
  } catch {
    install = null;
  }

  const listedSkills = await findGlobalLoamSkills({
    packageRoot,
    expectedSource: install?.skills_source,
    runner,
  });
  if (!listedSkills.ready) {
    output.write(`Unable to inspect global Loam skills: ${listedSkills.detail || listedSkills.category}\n`);
    return 1;
  }
  const detectedHarnesses = await detectHarnesses({ home });
  const codexAgentProfile = await inspectCodexAgentProfile(home);
  const hasMarketplacePlugin = ['claude', 'codex'].some((id) =>
    detectedHarnesses[id]?.marketplaceInstalled || detectedHarnesses[id]?.marketplaceConfigured);
  if (!install && !listedSkills.names.length && !hasMarketplacePlugin
    && !codexAgentProfile.owned && codexAgentProfile.backup === undefined) {
    output.write('No Loam installation found at %s. Nothing to uninstall.\n'.replace('%s', root));
    return 0;
  }

  await announce(output, 'Loam uninstall will:', [
    `- Remove ${listedSkills.names.length || 'any remaining'} globally installed Loam skills via the Skills CLI`,
    '- Remove Loam-owned hook entries from the Claude, Codex, and Cursor configs',
    '- Remove the Loam-owned Codex ingestion profile, restoring any pre-existing profile preserved by setup',
    '- Remove the Loam plugin file from OpenCode, which integrates by plugin rather than hooks',
    '- Remove installed Claude and Codex marketplace plugins through their native CLIs',
    '- Remove the global Loam root (install.json, runtime, integration, plugins, local operational history)',
    `- Global root: ${root}`,
  ], { level: 'warn' });

  if (!(await confirmUninstall({ yes, confirm, input, output }))) {
    finish(output, 'Uninstall cancelled.');
    return 130;
  }

  const workers = await blockingWorkers(root);
  if (workers.length) {
    output.write(`Uninstall blocked by ${workers.length} active or uncertain background worker lease(s).\n`);
    return 1;
  }

  const results = { configs: [], codexAgentProfile: null, opencode: null, globalRoot: null, backups: [], skills: null, marketplace: null };

  results.marketplace = await removeMarketplacePlugins({
    harnesses: detectedHarnesses,
    cwd: process.cwd(),
    runner,
  });
  if (Object.values(results.marketplace).some((entry) => entry.state === 'partial')) {
    output.write('Marketplace plugin removal failed; Loam core was preserved.\n');
    return 1;
  }

  results.skills = await removeGlobalSkills({
    packageRoot,
    expectedSource: install?.skills_source,
    runner,
    initial: listedSkills,
  });
  if (!results.skills.ready) {
    output.write(`Skills removal failed: ${results.skills.detail || results.skills.category}\n`);
    return 1;
  }

  results.codexAgentProfile = await removeCodexAgentProfile(codexAgentProfile);

  // Clean harness configs in-place
  if (install?.configured_harnesses?.includes('claude')) {
    results.configs.push(await cleanHarnessConfig(join(home, '.claude', 'settings.json'), root, cleanClaudeConfig));
    results.backups.push(...(await removeBackups(join(home, '.claude'))));
  }
  if (install?.configured_harnesses?.includes('codex')) {
    results.configs.push(await cleanHarnessConfig(join(home, '.codex', 'hooks.json'), root, cleanCodexConfig));
    results.backups.push(...(await removeBackups(join(home, '.codex'))));
  }
  if (install?.configured_harnesses?.includes('cursor')) {
    results.configs.push(await cleanHarnessConfig(join(home, '.cursor', 'hooks.json'), root, cleanCursorConfig));
    results.backups.push(...(await removeBackups(join(home, '.cursor'))));
  }

  // Remove the current OpenCode adapter and the legacy undiscoverable path.
  for (const name of ['loam.js', 'loam.mjs']) {
    const opencodePath = join(home, '.config', 'opencode', 'plugins', name);
    if (await exists(opencodePath)) {
      await rm(opencodePath, { force: true });
      results.opencode = { path: opencodePath, action: 'removed' };
    }
  }

  // Remove global root
  await rm(root, { recursive: true, force: true });
  results.globalRoot = { path: root, action: 'removed' };

  finish(output, 'Loam uninstalled.');
  return 0;
}
