import { homedir } from 'node:os';
import { mkdir, readFile, readdir, rm, stat } from 'node:fs/promises';
import { randomUUID } from 'node:crypto';
import { fileURLToPath } from 'node:url';
import { basename, dirname, isAbsolute, join, relative, resolve } from 'node:path';

import { writeAtomicFile } from './atomic.mjs';
import { mergeJsonConfig } from './config.mjs';

const adapterRoot = fileURLToPath(new URL('../adapters', import.meta.url));
const marketplaceAdapterPath = fileURLToPath(new URL('../plugins/loam-adapter/adapter.mjs', import.meta.url));

async function exists(path) {
  try {
    return (await stat(path)).isDirectory();
  } catch {
    return false;
  }
}

async function hasRequiredHooks(pluginRoot) {
  try {
    const config = JSON.parse(await readFile(join(pluginRoot, 'hooks', 'hooks.json'), 'utf8'));
    return Array.isArray(config.hooks?.SessionStart) && config.hooks.SessionStart.length > 0
      && Array.isArray(config.hooks?.Stop) && config.hooks.Stop.length > 0;
  } catch {
    return false;
  }
}

async function claudeMarketplaceInstall(root, name, pluginVersion) {
  try {
    const registry = JSON.parse(await readFile(join(root, 'plugins', 'installed_plugins.json'), 'utf8'));
    const installs = registry.plugins?.[name];
    const candidates = Array.isArray(installs)
      ? installs.filter(({ scope, installPath }) => scope === 'user' && typeof installPath === 'string' && installPath)
      : [];
    const present = await Promise.all(candidates.map(async (entry) => ({ ...entry, exists: await exists(entry.installPath) })));
    const installed = present.some((entry) => entry.exists);
    const ready = (await Promise.all(present
      .filter((entry) => entry.exists && (!pluginVersion || entry.version === pluginVersion || basename(entry.installPath) === pluginVersion))
      .map((entry) => hasRequiredHooks(entry.installPath)))).some(Boolean);
    return { installed, ready };
  } catch {
    return { installed: false, ready: false };
  }
}

async function codexMarketplaceInstall(root, name, pluginVersion) {
  const [plugin, marketplace, extra] = name.split('@');
  if (plugin !== 'loam' || !marketplace || extra) return { installed: false, ready: false };
  const pluginRoot = join(root, 'plugins', 'cache', marketplace, plugin);
  const installed = await exists(pluginRoot);
  if (!installed) return { installed: false, ready: false };
  const versions = await readdir(pluginRoot, { withFileTypes: true }).catch(() => []);
  const ready = (await Promise.all(versions
    .filter((entry) => entry.isDirectory() && (!pluginVersion || entry.name === pluginVersion))
    .map((entry) => hasRequiredHooks(join(pluginRoot, entry.name))))).some(Boolean);
  return { installed: true, ready };
}

export async function detectHarnesses({ home = homedir(), pluginVersion } = {}) {
  const roots = {
    opencode: join(home, '.config', 'opencode'),
    claude: join(home, '.claude'),
    codex: join(home, '.codex'),
    cursor: join(home, '.cursor'),
  };
  const result = {};
  for (const [id, root] of Object.entries(roots)) {
    const state = await exists(root) ? 'detected' : 'absent';
    let marketplaceOwned = false;
    let marketplaceConfigured = false;
    let marketplaceInstalled = false;
    let marketplaceReady = false;
    if (state === 'detected' && id === 'claude') {
      const install = await claudeMarketplaceInstall(root, 'loam@loam', pluginVersion);
      marketplaceInstalled = install.installed;
      try {
        const settings = JSON.parse(await readFile(join(root, 'settings.json'), 'utf8'));
        marketplaceConfigured = Object.hasOwn(settings.enabledPlugins || {}, 'loam@loam');
        marketplaceOwned = settings.enabledPlugins?.['loam@loam'] === true && install.installed;
        marketplaceReady = marketplaceOwned && install.ready;
      } catch {
        marketplaceOwned = false;
      }
    } else if (state === 'detected' && id === 'codex') {
      const install = await codexMarketplaceInstall(root, 'loam@loam', pluginVersion);
      marketplaceInstalled = install.installed;
      try {
        const config = await readFile(join(root, 'config.toml'), 'utf8');
        let loamPlugin = '';
        // ponytail: parse the table form Codex writes; unsupported TOML forms fail closed to setup ownership.
        for (const line of config.split(/\r?\n/)) {
          const section = line.match(/^\s*\[plugins\."([^"]+)"\]\s*$/);
          if (section) {
            loamPlugin = section[1].startsWith('loam@') ? section[1] : '';
            if (loamPlugin === 'loam@loam') marketplaceConfigured = true;
            continue;
          }
          if (/^\s*\[/.test(line)) loamPlugin = '';
          if (loamPlugin && /^\s*enabled\s*=\s*true\s*(?:#.*)?$/.test(line)) {
            marketplaceOwned = install.installed;
            marketplaceReady = marketplaceOwned && install.ready;
            break;
          }
        }
      } catch {
        marketplaceOwned = false;
      }
    }
    result[id] = { id, root, state, marketplaceOwned, marketplaceConfigured, marketplaceInstalled, marketplaceReady };
  }
  return result;
}

async function publishAssets(globalRoot, pluginVersion) {
  const versionRoot = join(resolve(globalRoot), 'plugins', `${pluginVersion}-${randomUUID()}`);
  try {
    await mkdir(versionRoot, { recursive: true, mode: 0o700 });
    const names = ['opencode.mjs', 'claude-session-start.mjs', 'claude-stop.mjs', 'codex-session-start.mjs', 'codex-stop.mjs', 'ingest-worker.mjs', 'ingest-modules.mjs', 'cursor-session-start.mjs'];
    const assets = {};
    for (const name of names) {
      const source = await readFile(
        name === 'claude-session-start.mjs' || name === 'codex-session-start.mjs'
          ? marketplaceAdapterPath
          : join(adapterRoot, name),
        'utf8',
      );
      const destination = join(versionRoot, name);
      await writeAtomicFile(destination, source);
      assets[name.replace('.mjs', '')] = destination;
    }
    return { versionRoot, assets };
  } catch (error) {
    await rm(versionRoot, { recursive: true, force: true });
    throw error;
  }
}

async function snapshotFile(path) {
  try {
    return { path, exists: true, contents: await readFile(path, 'utf8') };
  } catch (error) {
    if (error?.code === 'ENOENT') return { path, exists: false };
    throw error;
  }
}

async function snapshotDirectory(path) {
  try {
    return { path, entries: new Set(await readdir(path)) };
  } catch (error) {
    if (error?.code === 'ENOENT') return { path, entries: new Set() };
    throw error;
  }
}

async function restoreFile(snapshot) {
  if (snapshot.exists) await writeAtomicFile(snapshot.path, snapshot.contents);
  else await rm(snapshot.path, { force: true });
}

function hookEntry(command, { async = false, timeout } = {}) {
  return { type: 'command', command: 'node', args: [command], async, ...(timeout ? { timeout } : {}) };
}

export function isOwnedCommand(item, globalRoot, assetName) {
  if (Array.isArray(item?.hooks)) return item.hooks.some((hook) => isOwnedCommand(hook, globalRoot, assetName));
  if (item?.type !== 'command') return false;
  let commandPath;
  if (item.command === 'node' && Array.isArray(item.args) && item.args.length === 1) commandPath = item.args[0];
  else if (typeof item.command === 'string' && item.command.startsWith('node ')) {
    try { commandPath = JSON.parse(item.command.slice(5)); } catch { commandPath = item.command.slice(5); }
  } else return false;
  if (typeof commandPath !== 'string') return false;
  const pluginRoot = resolve(globalRoot, 'plugins');
  const candidate = resolve(commandPath);
  const relativePath = relative(pluginRoot, candidate);
  return relativePath && !relativePath.startsWith('..') && !isAbsolute(relativePath) && basename(candidate) === assetName;
}

function mergeClaudeHooks(existing, entry, globalRoot, assetName = 'claude-session-start.mjs') {
  const current = Array.isArray(existing) ? existing : [];
  const cleaned = [];
  for (const item of current) {
    if (!item || typeof item !== 'object' || !Array.isArray(item.hooks)) {
      cleaned.push(item);
      continue;
    }
    const hooks = item.hooks.filter((hook) => !isOwnedCommand(hook, globalRoot, assetName));
    if (hooks.length || item.hooks.length === 0) cleaned.push(hooks.length === item.hooks.length ? item : { ...item, hooks });
  }
  return entry ? [...cleaned, entry] : cleaned;
}

function mergeCursorHooks(existing, entry, globalRoot) {
  const current = Array.isArray(existing) ? existing : [];
  return [...current.filter((item) => !isOwnedCommand(item, globalRoot, 'cursor-session-start.mjs')), entry];
}

async function installClaude({ home, globalRoot, assetPath, marketplaceOwned }) {
  const filePath = join(home, '.claude', 'settings.json');
  try {
    await readFile(filePath, 'utf8');
  } catch (error) {
    if (error?.code === 'ENOENT') return { path: filePath, backupPath: null };
    throw error;
  }
  return mergeJsonConfig({
    filePath,
    update: (config) => ({
      ...config,
      hooks: {
        ...(config.hooks || {}),
        SessionStart: mergeClaudeHooks(
          config.hooks?.SessionStart,
          null,
          globalRoot,
        ),
        Stop: mergeClaudeHooks(config.hooks?.Stop, null, globalRoot, 'claude-stop.mjs'),
      },
    }),
  });
}

function mergeCodexHooks(existing, handler, globalRoot, assetName = 'codex-stop.mjs') {
  if (existing !== undefined && !Array.isArray(existing)) throw new Error('Codex hooks policy-owned; install manually');
  const groups = Array.isArray(existing) ? existing : [];
  const cleaned = groups.map((group) => {
    if (!group || typeof group !== 'object' || !Array.isArray(group.hooks)) return isOwnedCommand(group, globalRoot, assetName) ? null : group;
    const hooks = group.hooks.filter((item) => !isOwnedCommand(item, globalRoot, assetName));
    return hooks.length === group.hooks.length ? group : { ...group, hooks };
  }).filter(Boolean);
  return handler ? [...cleaned, { hooks: [handler] }] : cleaned;
}

async function installCodex({ home, globalRoot, sessionAssetPath, stopAssetPath, marketplaceOwned }) {
  const filePath = join(home, '.codex', 'hooks.json');
  try {
    await readFile(filePath, 'utf8');
  } catch (error) {
    if (error?.code === 'ENOENT') return { path: filePath, backupPath: null };
    throw error;
  }
  return mergeJsonConfig({
    filePath,
    update: (config) => ({
      ...config,
      hooks: {
        ...(config.hooks || {}),
        SessionStart: mergeCodexHooks(
          config.hooks?.SessionStart,
          null,
          globalRoot,
          'codex-session-start.mjs',
        ),
        Stop: mergeCodexHooks(config.hooks?.Stop, null, globalRoot),
      },
    }),
  });
}

async function installCursor({ home, globalRoot, assetPath }) {
  const filePath = join(home, '.cursor', 'hooks.json');
  return mergeJsonConfig({
    filePath,
    update: (config) => ({
      ...config,
      hooks: {
        ...(config.hooks || {}),
        sessionStart: mergeCursorHooks(config.hooks?.sessionStart, hookEntry(assetPath), globalRoot),
      },
    }),
  });
}

export async function installHarnesses({
  home = homedir(),
  globalRoot,
  pluginVersion,
  integrationPath: _integrationPath,
  detected,
} = {}) {
  detected ||= await detectHarnesses({ home, pluginVersion });
  const affectedFiles = Object.entries(detected)
    .filter(([, harness]) => harness.state !== 'absent')
    .map(([id]) => id === 'opencode'
      ? join(home, '.config', 'opencode', 'plugins', 'loam.mjs')
      : join(home, id === 'claude' ? '.claude' : id === 'codex' ? '.codex' : '.cursor', id === 'claude' ? 'settings.json' : 'hooks.json'));
  const snapshots = await Promise.all(affectedFiles.map(snapshotFile));
  const directories = await Promise.all([...new Set(affectedFiles.map(dirname))].map(snapshotDirectory));
  let assets;
  const backupPaths = [];
  let rolledBack = false;
  const rollback = async () => {
    if (rolledBack) return;
    rolledBack = true;
    for (const snapshot of snapshots) await restoreFile(snapshot);
    for (const backupPath of backupPaths) await rm(backupPath, { force: true });
    for (const directory of directories) {
      for (const entry of await readdir(directory.path).catch(() => [])) {
        if (!directory.entries.has(entry) && entry.includes('.backup-')) await rm(join(directory.path, entry), { force: true });
      }
    }
    if (assets) await rm(assets.versionRoot, { recursive: true, force: true });
  };

  assets = await publishAssets(globalRoot, pluginVersion);
  const result = {};
  for (const id of ['opencode', 'claude', 'codex', 'cursor']) {
    const harness = detected[id] || { id, state: 'absent' };
    if (harness.state === 'absent') {
      result[id] = { ...harness, state: 'absent' };
      continue;
    }
    try {
      if (id === 'opencode') {
        const stablePath = join(home, '.config', 'opencode', 'plugins', 'loam.mjs');
        const source = await readFile(join(adapterRoot, 'opencode.mjs'), 'utf8');
        await writeAtomicFile(stablePath, source);
        result[id] = { ...harness, state: 'ready', path: stablePath, versionRoot: assets.versionRoot };
      } else if (id === 'claude') {
        const config = await installClaude({
          home,
          globalRoot,
          assetPath: assets.assets['claude-session-start'],
          marketplaceOwned: harness.marketplaceOwned,
        });
        if (config.backupPath) backupPaths.push(config.backupPath);
        result[id] = {
          ...harness,
          state: harness.marketplaceReady ? 'ready' : 'skipped',
          owner: harness.marketplaceReady ? 'marketplace' : null,
          path: assets.assets['claude-session-start'],
          sessionPath: assets.assets['claude-session-start'],
          stopPath: assets.assets['claude-stop'],
          backupPath: config.backupPath,
        };
      } else if (id === 'codex') {
        const config = await installCodex({
          home,
          globalRoot,
          sessionAssetPath: assets.assets['codex-session-start'],
          stopAssetPath: assets.assets['codex-stop'],
          marketplaceOwned: harness.marketplaceOwned,
        });
        if (config.backupPath) backupPaths.push(config.backupPath);
        result[id] = {
          ...harness,
          state: harness.marketplaceReady ? 'ready' : 'skipped',
          owner: harness.marketplaceReady ? 'marketplace' : null,
          path: assets.assets['codex-stop'],
          sessionPath: assets.assets['codex-session-start'],
          stopPath: assets.assets['codex-stop'],
          backupPath: config.backupPath,
        };
      } else {
        const config = await installCursor({ home, globalRoot, assetPath: assets.assets['cursor-session-start'] });
        if (config.backupPath) backupPaths.push(config.backupPath);
        result[id] = { ...harness, state: 'ready', path: assets.assets['cursor-session-start'], backupPath: config.backupPath };
      }
    } catch (error) {
      result[id] = { ...harness, state: 'partial', category: error?.message?.includes('policy-owned') ? 'policy_owned' : 'install_failed', detail: error?.message || String(error) };
    }
  }
  if (Object.values(result).some((harness) => harness.state === 'partial')) await rollback();
  Object.defineProperties(result, {
    versionRoot: { value: assets.versionRoot, enumerable: false },
    assets: { value: assets.assets, enumerable: false },
    rollback: { value: rollback, enumerable: false },
  });
  return result;
}
