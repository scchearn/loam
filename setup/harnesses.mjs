import { homedir } from 'node:os';
import { existsSync } from 'node:fs';
import { mkdir, readFile, readdir, rm, stat } from 'node:fs/promises';
import { randomUUID } from 'node:crypto';
import { fileURLToPath } from 'node:url';
import { basename, dirname, isAbsolute, join, relative, resolve } from 'node:path';

import { writeAtomicFile } from './atomic.mjs';
import { mergeJsonConfig } from './config.mjs';

const adapterRoot = fileURLToPath(new URL('../adapters', import.meta.url));
const marketplaceAdapterPath = fileURLToPath(new URL('../plugins/loam-adapter/adapter.mjs', import.meta.url));
const codexAgentSourcePath = join(adapterRoot, 'loam_ingestor.toml');
const codexAgentMarker = '# Managed by @scchearn/loam setup.';

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

/// Claude Code writes the plugin cache directory during `plugin install`, but refreshes
/// `installed_plugins.json` on its own schedule — observed 32 minutes behind the install.
/// Reading the registry alone therefore misses a version setup just installed, and the
/// harness is recorded as skipped for the rest of that release. Fall back to the cache
/// directory, which is what `codexMarketplaceInstall` already reads.
async function claudeMarketplaceCache(root, name, pluginVersion) {
  const [plugin, marketplace, extra] = name.split('@');
  if (plugin !== 'loam' || !marketplace || extra) return { installed: false, ready: false, pluginRoot: null };
  const pluginRoot = join(root, 'plugins', 'cache', marketplace, plugin);
  if (!(await exists(pluginRoot))) return { installed: false, ready: false, pluginRoot: null };
  const versions = await readdir(pluginRoot, { withFileTypes: true }).catch(() => []);
  const candidates = versions.filter((entry) => entry.isDirectory()
    && (!pluginVersion || entry.name === pluginVersion));
  const ready = (await Promise.all(candidates.map((entry) => hasRequiredHooks(join(pluginRoot, entry.name))))).some(Boolean);
  const readyIndex = (await Promise.all(candidates.map((entry) => hasRequiredHooks(join(pluginRoot, entry.name))))).indexOf(true);
  return {
    installed: versions.some((entry) => entry.isDirectory()),
    ready,
    pluginRoot: readyIndex >= 0 ? join(pluginRoot, candidates[readyIndex].name) : null,
  };
}

async function claudeMarketplaceInstall(root, name, pluginVersion) {
  const cache = await claudeMarketplaceCache(root, name, pluginVersion);
  let registryEntries;
  try {
    const registry = JSON.parse(await readFile(join(root, 'plugins', 'installed_plugins.json'), 'utf8'));
    registryEntries = registry.plugins?.[name];
  } catch {
    // No readable registry yet. Claude Code writes it after the install, so the cache is the
    // only evidence available and there is no recorded scope to contradict it.
    return cache;
  }
  const installs = Array.isArray(registryEntries) ? registryEntries : [];
  const candidates = installs.filter(({ scope, installPath }) => scope === 'user' && typeof installPath === 'string' && installPath);
  const present = await Promise.all(candidates.map(async (entry) => ({ ...entry, exists: await exists(entry.installPath) })));
  const installed = present.some((entry) => entry.exists);
  const ready = (await Promise.all(present
    .filter((entry) => entry.exists && (!pluginVersion || entry.version === pluginVersion || basename(entry.installPath) === pluginVersion))
    .map((entry) => hasRequiredHooks(entry.installPath)))).some(Boolean);
  // The cache directory records no scope, so it may only stand in where the registry does not
  // contradict a user-scoped install: either it names one already, or it names none at all. A
  // registry listing only project-scoped installs still fails, which is the point of scoping.
  const scopeAllowsCache = installs.length === 0 || candidates.length > 0;
  const merged = scopeAllowsCache
    ? { installed: installed || cache.installed, ready: ready || cache.ready, pluginRoot: cache.pluginRoot }
    : { installed, ready, pluginRoot: null };
  if (!merged.pluginRoot) {
    const readyEntry = present.find((entry) => entry.exists
      && (!pluginVersion || entry.version === pluginVersion || basename(entry.installPath) === pluginVersion)
      && entry.installPath);
    merged.pluginRoot = readyEntry?.installPath || null;
  }
  return merged;
}

async function codexMarketplaceInstall(root, name, pluginVersion) {
  const [plugin, marketplace, extra] = name.split('@');
  if (plugin !== 'loam' || !marketplace || extra) return { installed: false, ready: false, pluginRoot: null };
  const pluginRoot = join(root, 'plugins', 'cache', marketplace, plugin);
  const installed = await exists(pluginRoot);
  if (!installed) return { installed: false, ready: false, pluginRoot: null };
  const versions = await readdir(pluginRoot, { withFileTypes: true }).catch(() => []);
  const candidates = versions.filter((entry) => entry.isDirectory()
    && (!pluginVersion || entry.name === pluginVersion));
  const ready = (await Promise.all(candidates.map((entry) => hasRequiredHooks(join(pluginRoot, entry.name))))).some(Boolean);
  const readyIndex = (await Promise.all(candidates.map((entry) => hasRequiredHooks(join(pluginRoot, entry.name))))).indexOf(true);
  return { installed: true, ready, pluginRoot: readyIndex >= 0 ? join(pluginRoot, candidates[readyIndex].name) : null };
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

// The OpenCode adapter's one runtime slot, filled when the plugin is staged
// rather than resolved at session time. The slot is a quoted JSON string in the
// source; setup replaces it with the absolute private runtime path.
const RUNTIME_PATH_SLOT = '"__LOAM_RUNTIME_PATH__"';

export function renderOpenCodePlugin(source, runtimePath) {
  if (!source.includes(RUNTIME_PATH_SLOT)) throw new Error('OpenCode adapter has no runtime path slot');
  return source.replaceAll(RUNTIME_PATH_SLOT, JSON.stringify(resolve(runtimePath)));
}

// Cursor's hooks.json takes the command as ONE shell string: its per-script
// schema is `command`/`type`/`timeout`/`loop_limit`/`failClosed`/`matcher` —
// there is no `args` and no `async` (cursor.com/docs/hooks; every published
// ~/.cursor/hooks.json in the wild uses the single-string form). The
// args-array form we emitted was accepted as JSON and then ignored, so the
// registration ran the bare runtime with no subcommand: the same silent
// failure #133 was for Claude/Codex, which #134 fixed for those two and left
// here (#135).
//
// The runtime path is wrapped in double quotes so a path with a space survives
// the shell. Quoted literally, NOT JSON-escaped: a JSON-escaped Windows path
// doubles every separator (`C:\\Users\\...`), which is a different string for
// anything that reads the command back and leans on how the executor unescapes
// it. A `"` cannot occur in a Windows path and is pathological in a POSIX one.
export function nativeHookCommand(runtimePath, harness, event) {
  return `"${resolve(runtimePath)}" hook ${harness} --event ${event}`;
}

export function nativeHookEntry(runtimePath, harness, event, { timeout } = {}) {
  return {
    type: 'command',
    command: nativeHookCommand(runtimePath, harness, event),
    ...(timeout ? { timeout } : {}),
  };
}

const RUNTIME_EXECUTABLES = new Set(['loam', 'loam.exe']);

function comparablePath(value) {
  const resolved = resolve(value);
  return process.platform === 'win32' ? resolved.replaceAll('\\', '/').toLowerCase() : resolved;
}

function pathInside(root, candidate) {
  const relativePath = relative(comparablePath(root), comparablePath(candidate));
  return Boolean(relativePath) && !relativePath.startsWith('..') && !isAbsolute(relativePath);
}

// The runtime path inside one of our native hook commands, in either form:
// the current `"<runtime>" hook <harness> --event <event>` string, or the
// pre-#135 args-array entry, which is still recognized so an update replaces
// it instead of stacking a second registration beside it.
function nativeHookRuntimePath(item) {
  if (typeof item?.command !== 'string') return null;
  const quoted = /^"([^"]+)"\s+hook\s/.exec(item.command);
  if (quoted) return quoted[1];
  if (Array.isArray(item.args) && item.args[0] === 'hook') return item.command;
  return null;
}

// #137: the marketplace plugin CARRIES its native-event hooks (self-resolving
// node shims in plugins/loam-adapter/hooks/, landed with #114) because Claude
// Code and Codex load hooks from the marketplace SOURCE directory, never the
// installed cache copy setup could write — a staged hooks.json is a file the
// harness ignores. Setup no longer renders or stages a hooks.json for claude or
// codex; it only installs and enables the plugin. The prior renderPluginHooks/
// stagePluginHooks path is retired.

async function publishAssets(globalRoot, pluginVersion, runtimePath) {
  const versionRoot = join(resolve(globalRoot), 'plugins', `${pluginVersion}-${randomUUID()}`);
  try {
    await mkdir(versionRoot, { recursive: true, mode: 0o700 });
    const names = ['opencode.mjs', 'claude-stop.mjs', 'codex-stop.mjs', 'ingest-worker.mjs', 'ingest-modules.mjs', 'harvest-worker.mjs', 'harvest-modules.mjs'];
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

function hookEntry(command, { async = false, timeout } = {}) {
  return { type: 'command', command: 'node', args: [command], async, ...(timeout ? { timeout } : {}) };
}

// A native entry is ours when it runs a command inside our global root as a
// `hook` read. Legacy Node entries are still recognized by asset name so an
// update removes the shim it replaces instead of stacking beside it.
export function isOwnedNativeHook(item, globalRoot) {
  if (Array.isArray(item?.hooks)) return item.hooks.some((hook) => isOwnedNativeHook(hook, globalRoot));
  if (item?.type !== 'command') return false;
  const runtimePath = nativeHookRuntimePath(item);
  if (!runtimePath) return false;
  const resolved = resolve(runtimePath);
  if (pathInside(globalRoot, resolved)) return true;
  // The staged runtime lives in the config-dir runtime store, not under the
  // global root, and that store is versioned:
  // `<config>/runtime/<version>/<target>/loam`. Recognizing ownership by the
  // global root alone therefore matched none of our own registrations, so
  // every setup run appended one more and left the previous entries — by then
  // pointing at runtime versions that had already been deleted — behind.
  // Observed on a machine running the cursor lane: four sessionStart entries,
  // three of them naming a runtime that no longer exists. A `hook` read of an
  // executable named `loam` is our registration wherever it was staged from.
  return RUNTIME_EXECUTABLES.has(basename(resolved));
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
  return pathInside(pluginRoot, candidate) && basename(candidate) === assetName;
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

async function installClaude({ home, globalRoot, marketplaceOwned }) {
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

async function installCodex({ home, globalRoot, marketplaceOwned }) {
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

function codexAgentPaths(home) {
  const profilePath = join(home, '.codex', 'agents', 'loam_ingestor.toml');
  return { profilePath, backupPath: `${profilePath}.loam-backup` };
}

async function installCodexAgent({ home }) {
  const { profilePath, backupPath } = codexAgentPaths(home);
  const source = await readFile(codexAgentSourcePath, 'utf8');
  let existing;
  let backup;
  try { existing = await readFile(profilePath, 'utf8'); }
  catch (error) { if (error?.code !== 'ENOENT') throw error; }
  try { backup = await readFile(backupPath, 'utf8'); }
  catch (error) { if (error?.code !== 'ENOENT') throw error; }

  if (existing !== undefined && existing !== source && !existing.startsWith(codexAgentMarker)) {
    if (backup !== undefined) throw new Error(`Codex agent profile collision with existing backup: ${profilePath}`);
    await writeAtomicFile(backupPath, existing);
  }
  await writeAtomicFile(profilePath, source);
  return { profilePath, profileBackupPath: backupPath };
}

async function installCursor({ home, globalRoot, runtimePath }) {
  const filePath = join(home, '.cursor', 'hooks.json');
  return mergeJsonConfig({
    filePath,
    update: (config) => ({
      ...config,
      // hooks.json is a versioned schema and 1 is the only version Cursor
      // currently reads; a file without it is a file Cursor may not load. An
      // explicit version the user already set is left alone.
      version: config.version ?? 1,
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

// A stale plugin entry in the user's opencode config pointing at a repo-local
// `.opencode/plugins/loam.js` (the path most checkouts never shipped) would
// leave OpenCode with a nonexistent plugin file and no adapter. OpenCode
// auto-discovers `~/.config/opencode/plugins/*.js`, so the entry is rewritten
// to the global stable path when it names a Loam-owned repo-local path. The
// rewrite goes through the same atomic merge as every other harness config,
// preserving unrelated entries. #88.
export async function reconcileOpenCodePluginEntry(home, stablePath) {
  const candidates = ['opencode.jsonc', 'opencode.json'].map((name) => join(home, '.config', 'opencode', name));
  const filePath = candidates.find((candidate) => existsSync(candidate));
  if (!filePath) return { path: null, action: 'absent' };
  let config;
  try {
    config = JSON.parse(await readFile(filePath, 'utf8'));
  } catch (error) {
    if (error?.code === 'ENOENT') return { path: filePath, action: 'absent' };
    return { path: filePath, action: 'skipped', reason: 'malformed JSON' };
  }
  const plugin = Array.isArray(config.plugin) ? config.plugin : [];
  const rewritten = plugin.map((spec) => {
    const value = Array.isArray(spec) ? spec[0] : spec;
    if (typeof value !== 'string') return spec;
    const owned = /(^|[/\\])\.?opencode[/\\]plugins[/\\]loam\.js$/u.test(value)
      || /(^|[/\\])plugins[/\\]loam\.js$/u.test(value);
    if (!owned) return spec;
    return Array.isArray(spec) ? [stablePath, spec[1]] : stablePath;
  });
  if (JSON.stringify(rewritten) === JSON.stringify(plugin)) return { path: filePath, action: 'unchanged' };
  const merged = await mergeJsonConfig({
    filePath,
    update: (current) => ({ ...current, plugin: rewritten }),
  });
  return { ...merged, path: filePath, action: 'rewritten' };
}

export async function installHarnesses({
  home = homedir(),
  globalRoot,
  pluginVersion,
  runtimePath,
  integrationPath: _integrationPath,
  detected,
} = {}) {
  // No fallback: without a staged runtime there is nothing to point a harness
  // at, and the only alternative — a Node shim resolving the path at session
  // time — is precisely what this slice retired.
  if (typeof runtimePath !== 'string' || !runtimePath) throw new Error('harness installation requires a staged runtime path');
  detected ||= await detectHarnesses({ home, pluginVersion });
  const affectedFiles = Object.entries(detected)
    .filter(([, harness]) => harness.state !== 'absent')
    .flatMap(([id]) => {
      if (id === 'opencode') return ['loam.js', 'loam.mjs'].map((name) => join(home, '.config', 'opencode', 'plugins', name));
      const configPath = join(home, id === 'claude' ? '.claude' : id === 'codex' ? '.codex' : '.cursor', id === 'claude' ? 'settings.json' : 'hooks.json');
      if (id !== 'codex') return [configPath];
      const { profilePath, backupPath } = codexAgentPaths(home);
      return [configPath, profilePath, backupPath];
    });
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
        // #88: a stale repo-local plugin entry in opencode.json would point
        // OpenCode at a nonexistent file; rewrite it to the stable global path.
        await reconcileOpenCodePluginEntry(home, stablePath);
        result[id] = { ...harness, state: 'ready', path: stablePath, versionRoot: assets.versionRoot };
      } else if (id === 'claude') {
        const config = await installClaude({
          home,
          globalRoot,
          marketplaceOwned: harness.marketplaceOwned,
        });
        if (config.backupPath) backupPaths.push(config.backupPath);
        // #137: the plugin carries its own hooks (self-resolving shims); setup
        // no longer stages a hooks.json. Readiness is the marketplace install.
        result[id] = {
          ...harness,
          state: harness.marketplaceReady ? 'ready' : 'skipped',
          owner: harness.marketplaceReady ? 'marketplace' : null,
          path: assets.assets['claude-stop'],
          stopPath: assets.assets['claude-stop'],
          backupPath: config.backupPath,
        };
      } else if (id === 'codex') {
        const config = await installCodex({
          home,
          globalRoot,
          marketplaceOwned: harness.marketplaceOwned,
        });
        const profile = await installCodexAgent({ home });
        if (config.backupPath) backupPaths.push(config.backupPath);
        // #137: plugin-carried hooks — no staging; readiness is the install.
        result[id] = {
          ...harness,
          state: harness.marketplaceReady ? 'ready' : 'skipped',
          owner: harness.marketplaceReady ? 'marketplace' : null,
          path: assets.assets['codex-stop'],
          stopPath: assets.assets['codex-stop'],
          backupPath: config.backupPath,
          ...profile,
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
