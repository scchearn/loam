import { writeSync } from 'node:fs';
import { stat } from 'node:fs/promises';
import { spawn } from 'node:child_process';
import { homedir } from 'node:os';
import { fileURLToPath } from 'node:url';
import { resolve } from 'node:path';

import { resolveGlobalRoot } from './paths.mjs';
import { resolveRuntimePath } from './ledger.mjs';
import { invokeRuntime } from './runtime.mjs';

export async function resolveCurrentRuntime({
  globalRoot,
  home,
  platform,
  env = process.env,
  integrationPath = fileURLToPath(import.meta.url),
} = {}) {
  const runtimeHome = home || env.HOME || env.USERPROFILE || homedir();
  const runtimePlatform = platform || process.platform;
  const root = globalRoot || resolveGlobalRoot({ home: runtimeHome, env, integrationPath });
  const configuredRoot = env.LOAM_CONFIG_DIR?.trim();
  const runtimePath = await resolveRuntimePath({
    globalRoot: root,
    home: runtimeHome,
    platform: runtimePlatform,
    env,
    ...(configuredRoot ? { root: configuredRoot } : {}),
  });
  if (!runtimePath) {
    throw new Error('no current Loam runtime found; run `npx @scchearn/loam install`');
  }
  const resolvedRuntimePath = resolve(runtimePath);
  try {
    if (!(await stat(resolvedRuntimePath)).isFile()) throw new Error('runtime path is not a file');
  } catch (error) {
    throw new Error(`current Loam runtime is unavailable at ${resolvedRuntimePath}: ${error instanceof Error ? error.message : String(error)}`);
  }
  return { globalRoot: root, runtimePath: resolvedRuntimePath };
}

function spawnRuntime(runtimePath, args, { cwd, env, output, errorOutput }) {
  return new Promise((resolvePromise, reject) => {
    const passthrough = output === undefined && errorOutput === undefined;
    let child;
    const stdout = [];
    const stderr = [];
    try {
      child = spawn(runtimePath, args, {
        cwd,
        env,
        shell: false,
        stdio: ['inherit', 'pipe', 'pipe'],
        windowsHide: false,
      });
    } catch (error) {
      reject(error);
      return;
    }
    child.stdout?.on('data', (chunk) => {
      if (passthrough) stdout.push(chunk);
      else (output || process.stdout).write(chunk);
    });
    child.stderr?.on('data', (chunk) => {
      if (passthrough) stderr.push(chunk);
      else (errorOutput || process.stderr).write(chunk);
    });
    child.once('error', reject);
    child.once('close', (code) => {
      if (passthrough) {
        try { if (stdout.length) writeSync(1, Buffer.concat(stdout)); } catch {}
        try { if (stderr.length) writeSync(2, Buffer.concat(stderr)); } catch {}
      }
      resolvePromise(code ?? 1);
    });
  });
}

export async function runLauncher(argv = [], options = {}) {
  const env = options.env || process.env;
  const cwd = resolve(options.cwd || process.cwd());
  const { runtimePath } = await resolveCurrentRuntime({
    ...options,
    env,
    home: options.home || env.HOME || env.USERPROFILE || homedir(),
    platform: options.platform || process.platform,
  });
  if (options.runner) {
    const result = await invokeRuntime({
      runtimePath,
      args: argv,
      cwd,
      timeoutMs: options.timeoutMs,
      runner: options.runner,
    });
    if (result.stdout) (options.output || process.stdout).write(result.stdout);
    if (result.stderr) (options.errorOutput || process.stderr).write(result.stderr);
    return result.code ?? 1;
  }
  return spawnRuntime(runtimePath, argv, {
    cwd,
    env,
    output: options.output,
    errorOutput: options.errorOutput,
  });
}
