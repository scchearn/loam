#!/usr/bin/env node

import { realpathSync } from 'node:fs';
import { readFile } from 'node:fs/promises';
import { homedir } from 'node:os';
import { dirname, isAbsolute, relative, resolve, join } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

function defaultGlobalRoot({ home = homedir(), env = process.env } = {}) {
  const configured = env.LOAM_HOME?.trim();
  return configured ? resolve(configured) : resolve(home, '.agents', 'loam');
}

function comparablePath(value) {
  const resolved = resolve(value);
  return process.platform === 'win32' ? resolved.replaceAll('\\', '/').toLowerCase() : resolved;
}

function inside(root, candidate) {
  const relativePath = relative(comparablePath(root), comparablePath(candidate));
  return relativePath === '' || (!relativePath.startsWith('..') && !isAbsolute(relativePath));
}

export async function loadCurrentLauncher({
  home = process.env.HOME || process.env.USERPROFILE || homedir(),
  env = process.env,
  globalRoot,
} = {}) {
  const root = resolve(globalRoot || defaultGlobalRoot({ home, env }));
  let metadata;
  try {
    metadata = JSON.parse(await readFile(join(root, 'install.json'), 'utf8'));
  } catch (error) {
    throw new Error(`cannot read Loam install metadata at ${join(root, 'install.json')}: ${error instanceof Error ? error.message : String(error)}`);
  }
  const integrationPath = metadata?.integration_path;
  if (typeof integrationPath !== 'string' || !isAbsolute(integrationPath) || !inside(root, integrationPath)) {
    throw new Error('Loam install metadata has no safe integration path; run `npx @scchearn/loam install`');
  }
  const launcherPath = join(dirname(integrationPath), 'launcher.mjs');
  try {
    const module = await import(pathToFileURL(launcherPath).href);
    if (typeof module.runLauncher !== 'function' || typeof module.resolveCurrentRuntime !== 'function') {
      throw new Error('current integration launcher is incomplete');
    }
    return { module, globalRoot: root, integrationPath, launcherPath };
  } catch (error) {
    throw new Error(`current Loam integration is unavailable at ${launcherPath}: ${error instanceof Error ? error.message : String(error)}`);
  }
}

export async function resolveShimRuntime(options = {}) {
  const loaded = await loadCurrentLauncher(options);
  return loaded.module.resolveCurrentRuntime({
    ...options,
    globalRoot: loaded.globalRoot,
    integrationPath: loaded.integrationPath,
  });
}

export async function main(argv = process.argv.slice(2), options = {}) {
  const loaded = await loadCurrentLauncher(options);
  return loaded.module.runLauncher(argv, {
    ...options,
    globalRoot: loaded.globalRoot,
    integrationPath: loaded.integrationPath,
  });
}

// Is this module the process entrypoint? Compare canonical paths: `resolve`
// only normalizes, so on macOS (where temp dirs live under /var -> /private/var
// symlinks) argv[1] and the realpath'd import.meta.url differ and the shim would
// silently do nothing. realpathSync canonicalizes both; fall back to resolve if
// a path can't be realpath'd (e.g. it no longer exists).
function isEntrypoint() {
  const invoked = process.argv[1];
  if (!invoked) return false;
  const self = fileURLToPath(import.meta.url);
  try {
    return realpathSync(invoked) === realpathSync(self);
  } catch {
    return resolve(invoked) === resolve(self);
  }
}

if (isEntrypoint()) {
  try {
    process.exitCode = await main();
  } catch (error) {
    process.stderr.write(`loam: ${error instanceof Error ? error.message : String(error)}\n`);
    process.exitCode = 1;
  }
}
