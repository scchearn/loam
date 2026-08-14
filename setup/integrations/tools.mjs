import { spawn } from 'node:child_process';
import { access, mkdir, rm, stat } from 'node:fs/promises';
import { constants } from 'node:fs';
import { delimiter, join } from 'node:path';

// Production-grade companion-tool install (spec: loam-optional-integrations).
// Node/npx is guaranteed but the package manager, global-install permissions,
// and PATH are NOT. So loam installs companion Node tools into a LOAM-MANAGED
// prefix and registers the MCP with the RESOLVED ABSOLUTE binary path — the same
// "never trust PATH" discipline as the private native runtime. Every install is
// non-interactive, time-bounded, verified by a health check before the MCP is
// registered, and classifies failure into an actionable category.

const DEFAULT_TIMEOUT_MS = 180_000;

// A loam-owned npm prefix under the global root. Tools installed here are
// loam-owned; uninstall/disable removes this tree, a pre-existing PATH tool is
// never touched.
export function managedToolsPrefix(globalRoot) {
  return join(globalRoot, 'integrations', 'tools');
}

// The absolute path to a managed bin. npm writes shims to node_modules/.bin;
// Windows npm shims are `.cmd`.
export function managedBinPath(globalRoot, binName, platform = process.platform) {
  const base = join(managedToolsPrefix(globalRoot), 'node_modules', '.bin', binName);
  return platform === 'win32' ? `${base}.cmd` : base;
}

// Default process runner: spawn, capture, time-bound. Injectable for tests.
// win32 needs shell:true to launch `.cmd` shims (Node refuses otherwise).
function defaultRunner({ command, args = [], cwd, timeoutMs = DEFAULT_TIMEOUT_MS, platform = process.platform } = {}) {
  return new Promise((resolve) => {
    let child;
    try {
      child = spawn(command, args, { cwd, stdio: ['ignore', 'pipe', 'pipe'], shell: platform === 'win32' });
    } catch (error) {
      resolve({ code: null, category: 'process_error', stdout: '', stderr: String(error?.message || error) });
      return;
    }
    let stdout = '';
    let stderr = '';
    let settled = false;
    const done = (result) => { if (!settled) { settled = true; clearTimeout(timer); resolve(result); } };
    const timer = setTimeout(() => { try { child.kill(); } catch {} done({ code: null, category: 'timeout', stdout, stderr }); }, timeoutMs);
    child.stdout?.on('data', (c) => { stdout += c; });
    child.stderr?.on('data', (c) => { stderr += c; });
    child.once('error', (error) => done({ code: null, category: 'process_error', stdout, stderr: String(error?.message || error) }));
    child.once('close', (code) => done({ code, stdout, stderr }));
  });
}

// npm entrypoint. npx sets npm_execpath to the invoking npm-cli.js — prefer it
// (works even when `npm` is not on PATH); otherwise fall back to the platform
// npm shim.
function npmInvocation(platform = process.platform) {
  const execpath = process.env.npm_execpath;
  if (execpath && /npm-cli\.js$/.test(execpath)) return { command: process.execPath, prefixArgs: [execpath] };
  return { command: platform === 'win32' ? 'npm.cmd' : 'npm', prefixArgs: [] };
}

// Classify an npm install failure into an actionable category.
export function classifyInstallFailure(result) {
  if (!result) return 'process-error';
  if (result.category === 'timeout') return 'timeout';
  if (result.category === 'process_error') {
    return /ENOENT/.test(result.stderr || '') ? 'package-manager-missing' : 'process-error';
  }
  const text = `${result.stdout || ''}\n${result.stderr || ''}`;
  if (/E404|404 Not Found|not found in the npm registry|is not in this registry/i.test(text)) return 'package-not-found';
  if (/EACCES|permission denied|EPERM/i.test(text)) return 'permission';
  if (/ENOTFOUND|ETIMEDOUT|getaddrinfo|network|ECONNREFUSED|ECONNRESET|registry error/i.test(text)) return 'network';
  if (/EBADENGINE|Unsupported engine|requires a node version/i.test(text)) return 'version-incompatible';
  return 'install-failed';
}

async function isExecutable(path, platform) {
  try {
    if (platform === 'win32') return (await stat(path)).isFile();
    await access(path, constants.X_OK);
    return true;
  } catch {
    return false;
  }
}

// Resolve a tool already on PATH to an ABSOLUTE path, without spawning `which`.
// Returns null when not found — the caller then installs into the managed prefix.
export async function resolvePathTool(binName, { platform = process.platform, env = process.env } = {}) {
  const dirs = (env.PATH || '').split(delimiter).filter(Boolean);
  const names = platform === 'win32'
    ? [`${binName}.cmd`, `${binName}.exe`, `${binName}.bat`, binName]
    : [binName];
  for (const dir of dirs) {
    for (const name of names) {
      const candidate = join(dir, name);
      if (await isExecutable(candidate, platform)) return candidate;
    }
  }
  return null;
}

// Run the entry's health check against a resolved absolute path. Returns
// { ok, version, detail }.
async function healthCheck({ binPath, healthArgs, runner, platform, timeoutMs }) {
  const result = await runner({ command: binPath, args: healthArgs, timeoutMs, platform });
  if (!result || result.code !== 0) {
    return { ok: false, detail: (result?.stderr || result?.category || 'health check failed').trim() };
  }
  return { ok: true, version: `${result.stdout || ''}${result.stderr || ''}`.trim() };
}

// Resolve a tool for an integration: managed copy first, then PATH. Returns
// { present, managed, path, version } or { present:false }.
export async function resolveTool({ globalRoot, binName, healthArgs = ['--version'], runner = defaultRunner, platform = process.platform, timeoutMs, env = process.env } = {}) {
  const managed = managedBinPath(globalRoot, binName, platform);
  if (await isExecutable(managed, platform)) {
    const health = await healthCheck({ binPath: managed, healthArgs, runner, platform, timeoutMs });
    if (health.ok) return { present: true, managed: true, path: managed, version: health.version };
  }
  const onPath = await resolvePathTool(binName, { platform, env });
  if (onPath) {
    const health = await healthCheck({ binPath: onPath, healthArgs, runner, platform, timeoutMs });
    if (health.ok) return { present: true, managed: false, path: onPath, version: health.version };
  }
  return { present: false };
}

// Install a Node package into the managed prefix, then verify it. Never
// registers anything — the caller registers the MCP only on { ready:true }.
// Returns { ready, managed, path, version } or { ready:false, category, detail }.
export async function installNodeTool({
  pkg,
  binName,
  healthArgs = ['--version'],
  globalRoot,
  runner = defaultRunner,
  platform = process.platform,
  timeoutMs = DEFAULT_TIMEOUT_MS,
} = {}) {
  const prefix = managedToolsPrefix(globalRoot);
  await mkdir(prefix, { recursive: true });
  const { command, prefixArgs } = npmInvocation(platform);
  const install = await runner({
    command,
    args: [...prefixArgs, 'install', '--prefix', prefix, pkg, '--no-audit', '--no-fund', '--no-save', '--loglevel=error'],
    cwd: prefix,
    timeoutMs,
    platform,
  });
  if (!install || install.code !== 0) {
    return { ready: false, category: classifyInstallFailure(install), detail: (install?.stderr || install?.category || 'npm install failed').trim() };
  }
  const binPath = managedBinPath(globalRoot, binName, platform);
  if (!(await isExecutable(binPath, platform))) {
    return { ready: false, category: 'health-check-failed', detail: `installed but ${binName} binary is missing at ${binPath}` };
  }
  const health = await healthCheck({ binPath, healthArgs, runner, platform, timeoutMs });
  if (!health.ok) {
    return { ready: false, category: 'health-check-failed', detail: health.detail };
  }
  return { ready: true, managed: true, path: binPath, version: health.version };
}

// Remove the managed tool tree (uninstall/disable). Only ever removes loam's own
// prefix; a pre-existing PATH tool is never here. Returns { removed }.
export async function removeManagedTool({ globalRoot } = {}) {
  const prefix = managedToolsPrefix(globalRoot);
  try {
    await stat(prefix);
  } catch {
    return { removed: false, path: prefix };
  }
  await rm(prefix, { recursive: true, force: true });
  return { removed: true, path: prefix };
}

export { DEFAULT_TIMEOUT_MS };
