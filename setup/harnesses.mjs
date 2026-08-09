import { homedir } from 'node:os';
import { mkdir, readFile, readdir, rm, stat } from 'node:fs/promises';
import { randomUUID } from 'node:crypto';
import { fileURLToPath } from 'node:url';
import { basename, dirname, isAbsolute, join, relative, resolve } from 'node:path';

import { writeAtomicFile } from './atomic.mjs';
import { mergeJsonConfig } from './config.mjs';

const adapterRoot = fileURLToPath(new URL('../adapters', import.meta.url));

async function exists(path) {
  try {
    return (await stat(path)).isDirectory();
  } catch {
    return false;
  }
}

// The shipped plugin owns only Stop (the Node ingestion boundary). SessionStart
// and UserPromptSubmit are written into the installed plugin at stage time,
// because they name the version- and target-qualified private runtime, which no
// shipped file can know.
async function hasRequiredHooks(pluginRoot) {
  try {
    const config = JSON.parse(await readFile(join(pluginRoot, 'hooks', 'hooks.json'), 'utf8'));
    return Array.isArray(config.hooks?.Stop) && config.hooks.Stop.length > 0;
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
    const versioned = present
      .filter((entry) => entry.exists && (!pluginVersion || entry.version === pluginVersion || basename(entry.installPath) === pluginVersion));
    const hooks = await Promise.all(versioned.map((entry) => hasRequiredHooks(entry.installPath)));
    const index = hooks.indexOf(true);
    return { installed, ready: index >= 0, pluginRoot: index >= 0 ? versioned[index].installPath : null };
  } catch {
    return { installed: false, ready: false, pluginRoot: null };
  }
}

async function codexMarketplaceInstall(root, name, pluginVersion) {
  const [plugin, marketplace, extra] = name.split('@');
  if (plugin !== 'loam' || !marketplace || extra) return { installed: false, ready: false };
  const pluginRoot = join(root, 'plugins', 'cache', marketplace, plugin);
  const installed = await exists(pluginRoot);
  if (!installed) return { installed: false, ready: false, pluginRoot: null };
  const versions = (await readdir(pluginRoot, { withFileTypes: true }).catch(() => []))
    .filter((entry) => entry.isDirectory() && (!pluginVersion || entry.name === pluginVersion));
  const hooks = await Promise.all(versions.map((entry) => hasRequiredHooks(join(pluginRoot, entry.name))));
  const index = hooks.indexOf(true);
  return { installed: true, ready: index >= 0, pluginRoot: index >= 0 ? join(pluginRoot, versions[index].name) : null };
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
    let marketplaceRoot = null;
    if (state === 'detected' && id === 'claude') {
      const install = await claudeMarketplaceInstall(root, 'loam@loam', pluginVersion);
      marketplaceInstalled = install.installed;
      marketplaceRoot = install.pluginRoot;
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
      marketplaceRoot = install.pluginRoot;
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
    result[id] = { id, root, state, marketplaceOwned, marketplaceConfigured, marketplaceInstalled, marketplaceReady, marketplaceRoot };
  }
  return result;
}

// The one slot in the OpenCode plugin that setup fills at stage time. OpenCode
// loads its plugin in-process, so the only way to reach the private runtime
// without resolving install.json at session time is to bake the resolved path
// in when the plugin is staged — and rewrite it on update.
const RUNTIME_PATH_SLOT = '"__LOAM_RUNTIME_PATH__"';

export function renderOpenCodePlugin(source, runtimePath) {
  if (!source.includes(RUNTIME_PATH_SLOT)) throw new Error('OpenCode adapter has no runtime path slot');
  return source.replaceAll(RUNTIME_PATH_SLOT, JSON.stringify(resolve(runtimePath)));
}

export function nativeHookEntry(runtimePath, harness, event, { async: isAsync = false, timeout } = {}) {
  return {
    type: 'command',
    command: resolve(runtimePath),
    args: ['hook', harness, '--event', event],
    async: isAsync,
    ...(timeout ? { timeout } : {}),
  };
}

// The plugin hooks file setup writes into the installed marketplace plugin.
// SessionStart and UserPromptSubmit are native; Stop stays Node because it is
// the ingestion boundary, not the collaboration read path.
export function renderPluginHooks(runtimePath, harness) {
  return {
    hooks: {
      SessionStart: [{
        matcher: 'startup|resume|clear|compact',
        hooks: [nativeHookEntry(runtimePath, harness, 'SessionStart')],
      }],
      UserPromptSubmit: [{ hooks: [nativeHookEntry(runtimePath, harness, 'UserPromptSubmit')] }],
      Stop: [{ hooks: [{ type: 'command', command: 'node "${CLAUDE_PLUGIN_ROOT}/hooks/stop.mjs"', timeout: 5 }] }],
    },
  };
}

async function publishAssets(globalRoot, pluginVersion, runtimePath) {
  const versionRoot = join(resolve(globalRoot), 'plugins', `${pluginVersion}-${randomUUID()}`);
  try {
    await mkdir(versionRoot, { recursive: true, mode: 0o700 });
    const names = ['opencode.mjs', 'claude-stop.mjs', 'codex-stop.mjs', 'ingest-worker.mjs', 'ingest-modules.mjs'];
    const assets = {};
    for (const name of names) {
      const raw = await readFile(join(adapterRoot, name), 'utf8');
      const source = name === 'opencode.mjs' ? renderOpenCodePlugin(raw, runtimePath) : raw;
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

// A native entry is ours when it runs a command inside our global root as a
// `hook` read. Legacy Node entries are still recognized by asset name so an
// update removes the shim it replaces instead of stacking beside it.
export function isOwnedNativeHook(item, globalRoot) {
  if (Array.isArray(item?.hooks)) return item.hooks.some((hook) => isOwnedNativeHook(hook, globalRoot));
  if (item?.type !== 'command' || typeof item.command !== 'string') return false;
  if (!Array.isArray(item.args) || item.args[0] !== 'hook') return false;
  const relativePath = relative(resolve(globalRoot), resolve(item.command));
  return Boolean(relativePath) && !relativePath.startsWith('..') && !isAbsolute(relativePath);
}

export function isOwnedCommand(item, globalRoot, assetName) {
  if (Array.isArray(item?.hooks)) return item.hooks.some((hook) => isOwnedCommand(hook, globalRoot, assetName));
  if (isOwnedNativeHook(item, globalRoot)) return true;
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

async function installClaude({ home, globalRoot }) {
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

async function installCodex({ home, globalRoot }) {
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

async function installCursor({ home, globalRoot, runtimePath }) {
  const filePath = join(home, '.cursor', 'hooks.json');
  return mergeJsonConfig({
    filePath,
    update: (config) => ({
      ...config,
      hooks: {
        ...(config.hooks || {}),
        sessionStart: mergeCursorHooks(
          config.hooks?.sessionStart,
          nativeHookEntry(runtimePath, 'cursor', 'sessionStart'),
          globalRoot,
        ),
      },
    }),
  });
}

// Setup owns the installed plugin's hook registration because it is the only
// party that knows the version- and target-qualified runtime path. `plugin
// update` replaces the shipped file; setup rewrites it afterwards, exactly as
// an active service is re-enabled on a new runtime.
async function stagePluginHooks({ pluginRoot, runtimePath, harness }) {
  if (!pluginRoot) throw new Error(`${harness} marketplace plugin root is unresolved`);
  const filePath = join(pluginRoot, 'hooks', 'hooks.json');
  await mkdir(dirname(filePath), { recursive: true });
  await writeAtomicFile(filePath, `${JSON.stringify(renderPluginHooks(runtimePath, harness), null, 2)}\n`);
  return filePath;
}

export async function installHarnesses({
  home = homedir(),
  globalRoot,
  pluginVersion,
  runtimePath,
  detected,
} = {}) {
  // No fallback: without a staged runtime there is nothing to point a harness
  // at, and the only alternative — a Node shim resolving the path at session
  // time — is precisely what this slice retired.
  if (typeof runtimePath !== 'string' || !runtimePath) throw new Error('harness installation requires a staged runtime path');
  detected ||= await detectHarnesses({ home, pluginVersion });
  const affectedFiles = Object.entries(detected)
    .filter(([, harness]) => harness.state !== 'absent')
    .flatMap(([id, harness]) => id === 'opencode'
      ? ['loam.js', 'loam.mjs'].map((name) => join(home, '.config', 'opencode', 'plugins', name))
      : [
          join(home, id === 'claude' ? '.claude' : id === 'codex' ? '.codex' : '.cursor', id === 'claude' ? 'settings.json' : 'hooks.json'),
          // The staged plugin hooks file setup rewrites belongs to the same transaction.
          ...(harness.marketplaceRoot ? [join(harness.marketplaceRoot, 'hooks', 'hooks.json')] : []),
        ]);
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

  assets = await publishAssets(globalRoot, pluginVersion, runtimePath);
  const result = {};
  for (const id of ['opencode', 'claude', 'codex', 'cursor']) {
    const harness = detected[id] || { id, state: 'absent' };
    if (harness.state === 'absent') {
      result[id] = { ...harness, state: 'absent' };
      continue;
    }
    try {
      if (id === 'opencode') {
        const stablePath = join(home, '.config', 'opencode', 'plugins', 'loam.js');
        const source = await readFile(join(adapterRoot, 'opencode.mjs'), 'utf8');
        await writeAtomicFile(stablePath, renderOpenCodePlugin(source, runtimePath));
        await rm(join(dirname(stablePath), 'loam.mjs'), { force: true });
        result[id] = { ...harness, state: 'ready', path: stablePath, versionRoot: assets.versionRoot };
      } else if (id === 'claude' || id === 'codex') {
        const config = id === 'claude'
          ? await installClaude({ home, globalRoot })
          : await installCodex({ home, globalRoot });
        if (config.backupPath) backupPaths.push(config.backupPath);
        const hooksPath = harness.marketplaceReady
          ? await stagePluginHooks({ pluginRoot: harness.marketplaceRoot, runtimePath, harness: id })
          : null;
        result[id] = {
          ...harness,
          state: harness.marketplaceReady ? 'ready' : 'skipped',
          owner: harness.marketplaceReady ? 'marketplace' : null,
          path: assets.assets[`${id}-stop`],
          sessionPath: hooksPath,
          stopPath: assets.assets[`${id}-stop`],
          backupPath: config.backupPath,
        };
      } else {
        const config = await installCursor({ home, globalRoot, runtimePath });
        if (config.backupPath) backupPaths.push(config.backupPath);
        result[id] = { ...harness, state: 'ready', path: join(home, '.cursor', 'hooks.json'), backupPath: config.backupPath };
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
