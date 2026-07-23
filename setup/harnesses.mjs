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

export async function detectHarnesses({ home = homedir() } = {}) {
  const roots = {
    opencode: join(home, '.config', 'opencode'),
    claude: join(home, '.claude'),
    cursor: join(home, '.cursor'),
  };
  const result = {};
  for (const [id, root] of Object.entries(roots)) {
    result[id] = { id, root, state: await exists(root) ? 'detected' : 'absent' };
  }
  return result;
}

async function publishAssets(globalRoot, pluginVersion) {
  const versionRoot = join(resolve(globalRoot), 'plugins', `${pluginVersion}-${randomUUID()}`);
  try {
    await mkdir(versionRoot, { recursive: true, mode: 0o700 });
    const names = ['opencode.mjs', 'claude-session-start.mjs', 'cursor-session-start.mjs'];
    const assets = {};
    for (const name of names) {
      const source = await readFile(join(adapterRoot, name), 'utf8');
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

function hookEntry(command) {
  return { type: 'command', command: `node ${JSON.stringify(command)}` };
}

function isOwnedCommand(item, globalRoot, assetName) {
  if (item?.type !== 'command' || typeof item.command !== 'string' || !item.command.startsWith('node ')) return false;
  let commandPath;
  try {
    commandPath = JSON.parse(item.command.slice(5));
  } catch {
    return false;
  }
  if (typeof commandPath !== 'string') return false;
  const pluginRoot = resolve(globalRoot, 'plugins');
  const candidate = resolve(commandPath);
  const relativePath = relative(pluginRoot, candidate);
  return relativePath && !relativePath.startsWith('..') && !isAbsolute(relativePath) && basename(candidate) === assetName;
}

function mergeClaudeHooks(existing, entry, globalRoot) {
  const current = Array.isArray(existing) ? existing : [];
  const cleaned = [];
  for (const item of current) {
    if (!item || typeof item !== 'object' || !Array.isArray(item.hooks)) {
      cleaned.push(item);
      continue;
    }
    const hooks = item.hooks.filter((hook) => !isOwnedCommand(hook, globalRoot, 'claude-session-start.mjs'));
    if (hooks.length || item.hooks.length === 0) cleaned.push(hooks.length === item.hooks.length ? item : { ...item, hooks });
  }
  return [...cleaned, entry];
}

function mergeCursorHooks(existing, entry, globalRoot) {
  const current = Array.isArray(existing) ? existing : [];
  return [...current.filter((item) => !isOwnedCommand(item, globalRoot, 'cursor-session-start.mjs')), entry];
}

async function installClaude({ home, globalRoot, assetPath }) {
  const filePath = join(home, '.claude', 'settings.json');
  return mergeJsonConfig({
    filePath,
    update: (config) => ({
      ...config,
      hooks: {
        ...(config.hooks || {}),
        SessionStart: mergeClaudeHooks(config.hooks?.SessionStart, {
          matcher: 'startup|clear|compact',
          hooks: [hookEntry(assetPath)],
        }, globalRoot),
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
  detected ||= await detectHarnesses({ home });
  const affectedFiles = Object.entries(detected)
    .filter(([, harness]) => harness.state !== 'absent')
    .map(([id]) => id === 'opencode'
      ? join(home, '.config', 'opencode', 'plugins', 'loam.mjs')
      : join(home, id === 'claude' ? '.claude' : '.cursor', id === 'claude' ? 'settings.json' : 'hooks.json'));
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
  for (const id of ['opencode', 'claude', 'cursor']) {
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
        const config = await installClaude({ home, globalRoot, assetPath: assets.assets['claude-session-start'] });
        if (config.backupPath) backupPaths.push(config.backupPath);
        result[id] = { ...harness, state: 'ready', path: assets.assets['claude-session-start'], backupPath: config.backupPath };
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
