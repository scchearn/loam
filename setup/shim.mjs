import { chmod, readFile, rm, stat } from 'node:fs/promises';
import { dirname, isAbsolute, join, resolve, win32 } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

import { writeAtomicFile } from './atomic.mjs';
import { runCommand } from './process.mjs';

const SHIM_ASSET = fileURLToPath(new URL('./assets/loam-shim.mjs', import.meta.url));
const WINDOWS_CMD_ASSET = fileURLToPath(new URL('./assets/loam.cmd', import.meta.url));

const POWERSHELL_PATH_COMMAND = `
$entry = $env:LOAM_PATH_ENTRY
$current = [Environment]::GetEnvironmentVariable('Path', 'User')
$parts = @($current -split ';' | Where-Object { $_ -and $_ -ne $entry })
if ($env:LOAM_PATH_ACTION -eq 'add') { $parts += $entry }
[Environment]::SetEnvironmentVariable('Path', ($parts -join ';'), 'User')
`;

export function windowsPowerShellPath(env = process.env) {
  const systemRoot = env.SystemRoot || env.SYSTEMROOT || env.windir || env.WINDIR || 'C:\\Windows';
  return win32.join(systemRoot, 'System32', 'WindowsPowerShell', 'v1.0', 'powershell.exe');
}

function pathParts(value, platform) {
  return String(value || '').split(platform === 'win32' ? ';' : ':').map((part) => part.trim()).filter(Boolean);
}

function comparablePath(value, platform) {
  const text = String(value || '').trim();
  if (!text) return '';
  if (platform === 'win32') {
    return win32.resolve(text.replaceAll('\\', '/')).replaceAll('\\', '/').toLowerCase();
  }
  return resolve(text);
}

export function pathHasEntry(pathValue, entry, platform = process.platform) {
  const expected = comparablePath(entry, platform);
  return pathParts(pathValue, platform).some((part) => comparablePath(part, platform) === expected);
}

export function shimLocations({
  home = process.env.HOME || process.env.USERPROFILE,
  env = process.env,
  platform = process.platform,
} = {}) {
  const resolvedHome = resolve(home || process.cwd());
  if (platform === 'win32') {
    const localAppData = env.LOCALAPPDATA || join(resolvedHome, 'AppData', 'Local');
    const binDir = join(localAppData, 'loam', 'bin');
    return {
      binDir,
      pathEntry: binDir,
      shimPath: join(binDir, 'loam.cmd'),
      scriptPath: join(binDir, 'loam.mjs'),
    };
  }
  const binDir = join(resolvedHome, '.local', 'bin');
  return { binDir, pathEntry: binDir, shimPath: join(binDir, 'loam'), scriptPath: join(binDir, 'loam') };
}

async function readUserPath({ entry, env, pathRunner, runner }) {
  if (pathRunner) {
    const result = await pathRunner({ action: 'read', entry, env });
    return typeof result === 'string' ? result : String(result?.stdout || '');
  }
  const result = await runCommand({
    command: windowsPowerShellPath(env),
    args: ['-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-Command', "[Environment]::GetEnvironmentVariable('Path', 'User')"],
    env,
    runner,
  });
  if (!result.ok) throw new Error(`cannot read the per-user Windows PATH: ${result.stderr || 'PowerShell failed'}`);
  return result.stdout;
}

async function mutateUserPath({ action, entry, env, pathRunner, userPath, runner }) {
  if (pathRunner) {
    const result = await pathRunner({ action, entry, env, userPath });
    if (result && typeof result === 'object' && result.code !== undefined && result.code !== 0) {
      throw new Error(result.stderr || `cannot ${action} the per-user Windows PATH entry`);
    }
    return;
  }
  const commandEnv = { ...env, LOAM_PATH_ACTION: action, LOAM_PATH_ENTRY: entry };
  const result = await runCommand({
    command: windowsPowerShellPath(commandEnv),
    args: ['-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-Command', POWERSHELL_PATH_COMMAND],
    env: commandEnv,
    runner,
  });
  if (!result.ok) throw new Error(`cannot ${action} the per-user Windows PATH entry: ${result.stderr || 'PowerShell failed'}`);
}

async function fileExists(path) {
  try { return (await stat(path)).isFile(); } catch { return false; }
}

async function writeIfChanged(destination, source, mode) {
  const expected = await readFile(source);
  try {
    if (await readFile(destination).then((actual) => actual.equals(expected))) return false;
  } catch (error) {
    if (error?.code !== 'ENOENT') throw error;
  }
  await writeAtomicFile(destination, expected.toString('utf8'), { mode });
  await chmod(destination, mode).catch((error) => {
    if (process.platform !== 'win32') throw error;
  });
  return true;
}

function recordFor(locations, pathAdded) {
  return {
    path: locations.shimPath,
    script_path: locations.scriptPath,
    bin_dir: locations.binDir,
    path_entry: locations.pathEntry,
    path_added: pathAdded,
  };
}

export async function installShim({
  home,
  globalRoot: _globalRoot,
  env = process.env,
  platform = process.platform,
  runner,
  pathRunner,
  update = false,
  existing,
} = {}) {
  const locations = shimLocations({ home, env, platform });
  const previous = {};
  for (const path of [locations.shimPath, ...(platform === 'win32' ? [locations.scriptPath] : [])]) {
    try { previous[path] = await readFile(path); } catch (error) { if (error?.code !== 'ENOENT') throw error; }
  }

  let pathAdded = existing?.path_added === true || existing?.pathAdded === true;
  let pathChanged = false;
  if (platform === 'win32') {
    const userPath = await readUserPath({ entry: locations.pathEntry, env, pathRunner, runner });
    if (!pathHasEntry(userPath, locations.pathEntry, platform)) {
      await mutateUserPath({ action: 'add', entry: locations.pathEntry, env, pathRunner, userPath, runner });
      pathAdded = true;
      pathChanged = true;
    }
  }

  try {
    let filesChanged = false;
    if (platform === 'win32') {
      filesChanged = await writeIfChanged(locations.scriptPath, SHIM_ASSET, 0o700) || filesChanged;
      filesChanged = await writeIfChanged(locations.shimPath, WINDOWS_CMD_ASSET, 0o600) || filesChanged;
    } else {
      filesChanged = await writeIfChanged(locations.shimPath, SHIM_ASSET, 0o700);
    }
    const action = filesChanged || pathChanged ? 'installed' : 'unchanged';
    const record = recordFor(locations, pathAdded);
    let rolledBack = false;
    return {
      ...record,
      action,
      pathAdded,
      record,
      rollback: async () => {
        if (rolledBack) return;
        rolledBack = true;
        for (const path of [locations.shimPath, ...(platform === 'win32' ? [locations.scriptPath] : [])]) {
          if (previous[path]) await writeAtomicFile(path, previous[path].toString('utf8'), { mode: platform === 'win32' && path.endsWith('.cmd') ? 0o600 : 0o700 });
          else await rm(path, { force: true });
        }
        if (pathChanged) await mutateUserPath({ action: 'remove', entry: locations.pathEntry, env, pathRunner, runner });
      },
    };
  } catch (error) {
    if (pathChanged) await mutateUserPath({ action: 'remove', entry: locations.pathEntry, env, pathRunner, runner });
    throw error;
  }
}

async function resolveLauncherRuntime({ integrationPath, globalRoot, home, platform, env }) {
  if (typeof integrationPath !== 'string' || !isAbsolute(integrationPath)) {
    throw new Error('install metadata has no safe integration path');
  }
  const launcherPath = join(dirname(integrationPath), 'launcher.mjs');
  const launcher = await import(pathToFileURL(launcherPath).href);
  if (typeof launcher.resolveCurrentRuntime !== 'function') throw new Error('integration launcher cannot resolve a runtime');
  return launcher.resolveCurrentRuntime({ integrationPath, globalRoot, home, platform, env });
}

export async function verifyShim({
  home,
  globalRoot,
  env = process.env,
  platform = process.platform,
  requireOnPath = false,
  pathRunner,
  runner,
  integrationPath,
  expectedRuntimePath,
} = {}) {
  const locations = shimLocations({ home, env, platform });
  const requiredFiles = platform === 'win32' ? [locations.shimPath, locations.scriptPath] : [locations.shimPath];
  if (!(await Promise.all(requiredFiles.map(fileExists))).every(Boolean)) {
    return {
      ready: false,
      category: 'shim_missing',
      path: locations.shimPath,
      detail: `Loam launcher is missing at ${locations.shimPath}; run \`npx @scchearn/loam install\``,
    };
  }

  let onPath = true;
  if (requireOnPath) {
    const pathValue = platform === 'win32'
      ? await readUserPath({ entry: locations.pathEntry, env, pathRunner, runner })
      : (env.PATH || '');
    onPath = pathHasEntry(pathValue, locations.pathEntry, platform);
    if (!onPath) {
      const hint = platform === 'win32'
        ? `add ${locations.pathEntry} to your per-user PATH, then open a new shell`
        : `run \`export PATH="${locations.pathEntry}:$PATH"\` or add ${locations.pathEntry} to your shell environment`;
      return { ready: false, category: 'shim_off_path', path: locations.shimPath, detail: `Loam launcher is not on PATH; ${hint}` };
    }
  }

  try {
    let runtime;
    if (integrationPath) {
      runtime = await resolveLauncherRuntime({ integrationPath, globalRoot, home, platform, env });
    } else {
      const { resolveShimRuntime } = await import('./assets/loam-shim.mjs');
      runtime = await resolveShimRuntime({ home, env, globalRoot });
    }
    if (expectedRuntimePath && comparablePath(runtime.runtimePath, platform) !== comparablePath(expectedRuntimePath, platform)) {
      return {
        ready: false,
        category: 'shim_runtime_stale',
        path: locations.shimPath,
        runtimePath: runtime.runtimePath,
        detail: `Loam launcher resolves ${runtime.runtimePath}, expected current runtime ${expectedRuntimePath}; run update`,
      };
    }
    if (!(await fileExists(runtime.runtimePath))) {
      return {
        ready: false,
        category: 'shim_runtime_missing',
        path: locations.shimPath,
        runtimePath: runtime.runtimePath,
        detail: `Loam launcher resolves a missing runtime at ${runtime.runtimePath}; run install or update`,
      };
    }
    return { ready: true, path: locations.shimPath, runtimePath: runtime.runtimePath, onPath };
  } catch (error) {
    return {
      ready: false,
      category: 'shim_runtime_unavailable',
      path: locations.shimPath,
      detail: `${error instanceof Error ? error.message : String(error)}; run install or update`,
    };
  }
}

export async function removeShim({
  home,
  env = process.env,
  platform = process.platform,
  pathRunner,
  runner,
  record,
} = {}) {
  const locations = shimLocations({ home, env, platform });
  const paths = platform === 'win32' ? [locations.shimPath, locations.scriptPath] : [locations.shimPath];
  for (const path of paths) await rm(path, { force: true });
  const pathAdded = record?.path_added === true || record?.pathAdded === true;
  if (platform === 'win32' && pathAdded) {
    await mutateUserPath({ action: 'remove', entry: record?.path_entry || locations.pathEntry, env, pathRunner, runner });
  }
  return { ...locations, pathAdded, removed: true };
}
