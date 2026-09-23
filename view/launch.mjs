#!/usr/bin/env node

import { spawn } from 'node:child_process';
import { access, constants } from 'node:fs/promises';
import { isAbsolute, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { checkReadiness, invokeRuntime, safeDetail, MAX_VIEW_STDOUT_BYTES } from '../integration/runtime.mjs';
import { resolveGlobalRoot, resolveSkillsRoot } from '../integration/paths.mjs';
import { createServer } from './server/server.mjs';
import { validateSnapshot } from './server/validate-snapshot.mjs';

const NODE_MAJOR_MIN = 22;
const PRODUCER_TIMEOUT_MS = 60_000;

export class LaunchError extends Error {
  constructor(message, { exitCode = 1 } = {}) {
    super(message);
    this.name = 'LaunchError';
    this.exitCode = exitCode;
  }
}

export function assertSupportedNode(version = process.version) {
  const major = Number(String(version).replace(/^v/, '').split('.')[0]);
  if (!Number.isInteger(major) || major < NODE_MAJOR_MIN) {
    throw new LaunchError(
      `Loam View requires Node.js >= ${NODE_MAJOR_MIN} (running ${version}). Install a supported Node.js version and re-run.`,
    );
  }
}

// Resolve and verify the installed native loam runtime the same way the
// shared Node integration does (integration/runtime.mjs + integration/paths.mjs):
// never a PATH lookup, never a project-local copy.
// LOAM_NATIVE_BIN is the dev/test seam (same env name bin/check-release-resolution.sh
// and the bump-release contract test use): an absolute path to a loam binary that
// bypasses installed-runtime resolution entirely. It exists so this repo's own
// checkout can drive View before the runtime it builds is installed globally.
async function resolveRuntime({ home, env = process.env, platform = process.platform, arch = process.arch } = {}) {
  const override = env.LOAM_NATIVE_BIN;
  if (override) {
    if (!isAbsolute(override)) {
      throw new LaunchError(`LOAM_NATIVE_BIN must be an absolute path to a loam binary (got ${override}).`);
    }
    try {
      await access(override, constants.X_OK);
    } catch {
      throw new LaunchError(`LOAM_NATIVE_BIN is not an executable file: ${override}`);
    }
    return { ready: true, runtimePath: override };
  }
  const globalRoot = resolveGlobalRoot({ home, env });
  const skillsRoot = resolveSkillsRoot({ home, env });
  const readiness = await checkReadiness({ globalRoot, skillsRoot, platform, arch, env, home });
  if (!readiness.ready) {
    throw new LaunchError(
      `Loam is unavailable: ${readiness.message || readiness.category}\nRecovery: npx @scchearn/loam setup`,
    );
  }
  return readiness;
}

async function captureSnapshot({ runtimePath, workspaceRoot, timeoutMs = PRODUCER_TIMEOUT_MS, runner } = {}) {
  const result = await invokeRuntime({
    runtimePath,
    args: ['state', '--view', workspaceRoot],
    cwd: workspaceRoot,
    timeoutMs,
    runner,
    // The view snapshot legitimately runs to many MB on large workspaces; give
    // it a generous ceiling so it is never silently truncated into invalid JSON.
    maxStdoutBytes: MAX_VIEW_STDOUT_BYTES,
  });
  if (result.category === 'timeout') {
    throw Object.assign(new Error(`loam state --view timed out after ${timeoutMs}ms`), { status: 504 });
  }
  if (result.code !== 0) {
    throw Object.assign(
      new Error(safeDetail(result.stderr || result.stdout || `loam state --view exited with code ${result.code}`)),
      { status: 500 },
    );
  }
  if (result.stdoutTruncated) {
    throw Object.assign(
      new Error(
        `loam state --view output exceeded ${MAX_VIEW_STDOUT_BYTES} bytes and was truncated; the workspace snapshot is too large to render.`,
      ),
      { status: 500 },
    );
  }
  try {
    return JSON.parse(result.stdout);
  } catch (error) {
    throw Object.assign(new Error(`loam state --view produced invalid JSON: ${error.message}`), { status: 500 });
  }
}

function openBrowser(url, { platform = process.platform, spawnFn = spawn, output = process.stdout } = {}) {
  const [command, args] = platform === 'darwin' ? ['open', [url]]
    : platform === 'win32' ? ['cmd', ['/c', 'start', '""', url]]
    : ['xdg-open', [url]];
  try {
    const child = spawnFn(command, args, { stdio: 'ignore', detached: true });
    child.once('error', () => output.write(`Open this URL in your browser: ${url}\n`));
    child.unref?.();
  } catch {
    output.write(`Open this URL in your browser: ${url}\n`);
  }
}

/**
 * Launch Loam View: resolve the workspace, capture one Loam State snapshot,
 * serve it on loopback, open the browser, and run in the foreground until
 * interrupted. No daemonization.
 */
export async function launch({
  workspace = process.cwd(),
  output = process.stdout,
  errorOutput = process.stderr,
  home,
  env = process.env,
  platform = process.platform,
  arch = process.arch,
  runner,
  nodeVersion = process.version,
  openBrowserFn = openBrowser,
  publicRoot,
  listen = true,
  open = true,
} = {}) {
  assertSupportedNode(nodeVersion);
  const workspaceRoot = resolve(workspace);
  const readiness = await resolveRuntime({ home, env, platform, arch });

  const rawSnapshot = await captureSnapshot({ runtimePath: readiness.runtimePath, workspaceRoot, runner });
  const check = validateSnapshot(rawSnapshot);
  if (!check.valid) {
    throw new LaunchError(
      `loam state --view produced a snapshot that failed schema validation: ${check.errors.join('; ')}`,
    );
  }

  const server = createServer({
    workspaceRoot,
    publicRoot,
    initialSnapshot: rawSnapshot,
    refreshProducer: () => captureSnapshot({ runtimePath: readiness.runtimePath, workspaceRoot, runner }),
    stderr: errorOutput,
  });

  if (!listen) return { server, workspaceRoot };

  await new Promise((resolveListen, rejectListen) => {
    server.once('error', rejectListen);
    server.listen(0, '127.0.0.1', resolveListen);
  });
  const { port } = server.address();
  const url = `http://127.0.0.1:${port}/`;
  // Parseable, first: a harness that background-spawns the launcher reads this
  // line to learn the URL, and must not have to wait on the browser opener.
  output.write(`Loam View: ${url}\n`);
  if (open) openBrowserFn(url, { platform, output });

  await new Promise((resolveClose) => {
    const shutdown = () => server.close(() => resolveClose());
    process.once('SIGINT', shutdown);
    process.once('SIGTERM', shutdown);
  });

  return { server, workspaceRoot };
}

export function parseArgs(argv = []) {
  const positional = [];
  let open = true;
  for (const arg of argv) {
    if (arg === '--no-open') open = false;
    else if (arg === '--') continue;
    else if (arg.startsWith('-')) throw new LaunchError(`Unknown option ${arg}. Usage: loam view [workspace-root] [--no-open]`);
    else positional.push(arg);
  }
  if (positional.length > 1) {
    throw new LaunchError('Too many arguments. Usage: loam view [workspace-root] [--no-open]');
  }
  return { workspace: positional[0], open };
}

const invokedPath = process.argv[1] ? resolve(process.argv[1]) : '';
if (invokedPath === resolve(fileURLToPath(import.meta.url))) {
  try {
    const { workspace, open } = parseArgs(process.argv.slice(2));
    await launch({ workspace: workspace ?? process.cwd(), open });
  } catch (error) {
    process.stderr.write(`loam view: ${error instanceof Error ? error.message : String(error)}\n`);
    process.exitCode = Number.isInteger(error?.exitCode) ? error.exitCode : 1;
  }
}
