import { spawn } from 'node:child_process';
import { access, mkdir, rm, stat } from 'node:fs/promises';
import { constants } from 'node:fs';
import { delimiter, isAbsolute, join } from 'node:path';

// Production-grade companion-tool install (spec: loam-optional-integrations).
// Node/npx is guaranteed but the package manager, global-install permissions,
// and PATH are NOT. So loam installs companion Node tools into a LOAM-MANAGED
// prefix and registers the MCP with the RESOLVED ABSOLUTE binary path — the same
// "never trust PATH" discipline as the private native runtime. Every install is
// non-interactive, time-bounded, verified by a health check before the MCP is
// registered, and classifies failure into an actionable category.

const DEFAULT_TIMEOUT_MS = 180_000;

// A loam-owned npm prefix, PER INTEGRATION, under the global root. Scoping the
// prefix to the integration id keeps blast radius exact: removing (or failing to
// install) one tool-backed integration never touches another's binary. Tools
// installed here are loam-owned; a pre-existing PATH tool is never installed here
// and never removed.
export function managedToolsPrefix(globalRoot, id) {
  return join(globalRoot, 'integrations', 'tools', id);
}

// The absolute path to a managed bin for an integration. npm writes shims to
// node_modules/.bin; Windows npm shims are `.cmd`.
export function managedBinPath(globalRoot, id, binName, platform = process.platform) {
  const base = join(managedToolsPrefix(globalRoot, id), 'node_modules', '.bin', binName);
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
// `extraDirs` are non-PATH install sites an entry declares (a detection-only
// tool's installer target, e.g. hcom's ~/.local/bin, which a non-login shell may
// not have on PATH); they are searched AFTER PATH, so a PATH copy always wins.
// Returns { path, source } or null — the caller then installs, or refuses.
//
// Only ABSOLUTE directories are searched. A relative one — the empty element in
// `PATH=/usr/bin:`, or a relative HCOM_INSTALL_DIR — makes `join` produce a
// relative candidate, which resolves against the process CWD; that is how a
// binary checked into a cloned repository gets found and then run.
export async function resolvePathTool(binName, { platform = process.platform, env = process.env, extraDirs = [] } = {}) {
  const dirs = [
    ...(env.PATH || '').split(delimiter).map((path) => ({ path, source: 'PATH' })),
    ...extraDirs.map((path) => ({ path, source: 'install site' })),
  ].filter((dir) => dir.path && isAbsolute(dir.path));
  const names = platform === 'win32'
    ? [`${binName}.cmd`, `${binName}.exe`, `${binName}.bat`, binName]
    : [binName];
  for (const dir of dirs) {
    for (const name of names) {
      const candidate = join(dir.path, name);
      if (await isExecutable(candidate, platform)) return { path: candidate, source: dir.source };
    }
  }
  return null;
}

// How much of a version answer is kept. Doctor prints one line per integration
// and the ledger records the string, so a tool that answers with a build banner,
// a git sha and a deprecation notice must not be able to reshape either. Long
// enough for any real "<name> <semver> (<sha>)"; short enough to stay a line.
const MAX_VERSION = 80;

// Run the entry's health check against a resolved absolute path. Returns
// { ok, version, detail }. `version` is the tool's own words, so it is trimmed
// to its first line and capped — see MAX_VERSION.
async function healthCheck({ binPath, healthArgs, runner, platform, timeoutMs }) {
  const result = await runner({ command: binPath, args: healthArgs, timeoutMs, platform });
  if (!result || result.code !== 0) {
    return { ok: false, detail: (result?.stderr || result?.category || 'health check failed').trim() };
  }
  const answer = `${result.stdout || ''}${result.stderr || ''}`.trim();
  const line = answer.split(/\r?\n/, 1)[0].trim();
  return { ok: true, version: line.length > MAX_VERSION ? `${line.slice(0, MAX_VERSION)}…` : line };
}

// Resolve a tool for an integration: managed copy first, then PATH, then the
// entry's declared install sites. A resolved binary is only reported present
// once its health check passes — present-but-broken is not usable, and reporting
// it as present would register an MCP against a binary that cannot answer.
// Returns { present, managed, source, path, version } or { present:false }.
export async function resolveTool({ globalRoot, id, binName, healthArgs = ['--version'], runner = defaultRunner, platform = process.platform, timeoutMs, env = process.env, extraDirs = [] } = {}) {
  const managed = managedBinPath(globalRoot, id, binName, platform);
  if (await isExecutable(managed, platform)) {
    const health = await healthCheck({ binPath: managed, healthArgs, runner, platform, timeoutMs });
    if (health.ok) return { present: true, managed: true, source: 'loam-managed', path: managed, version: health.version };
  }
  const found = await resolvePathTool(binName, { platform, env, extraDirs });
  if (found) {
    const health = await healthCheck({ binPath: found.path, healthArgs, runner, platform, timeoutMs });
    if (health.ok) return { present: true, managed: false, source: found.source, path: found.path, version: health.version };
  }
  return { present: false };
}

// Install a Node package into the managed prefix, then verify it. Never
// registers anything — the caller registers the MCP only on { ready:true }.
// Returns { ready, managed, path, version } or { ready:false, category, detail }.
export async function installNodeTool({
  pkg,
  id,
  binName,
  healthArgs = ['--version'],
  globalRoot,
  runner = defaultRunner,
  platform = process.platform,
  timeoutMs = DEFAULT_TIMEOUT_MS,
} = {}) {
  const prefix = managedToolsPrefix(globalRoot, id);
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
  const binPath = managedBinPath(globalRoot, id, binName, platform);
  if (!(await isExecutable(binPath, platform))) {
    return { ready: false, category: 'health-check-failed', detail: `installed but ${binName} binary is missing at ${binPath}` };
  }
  const health = await healthCheck({ binPath, healthArgs, runner, platform, timeoutMs });
  if (!health.ok) {
    return { ready: false, category: 'health-check-failed', detail: health.detail };
  }
  return { ready: true, managed: true, path: binPath, version: health.version };
}

// Remove ONE integration's managed tool tree (uninstall/disable). Scoped to the
// integration id, so removing one never touches another's binary; a pre-existing
// PATH tool is never here. Returns { removed }.
export async function removeManagedTool({ globalRoot, id } = {}) {
  const prefix = managedToolsPrefix(globalRoot, id);
  try {
    await stat(prefix);
  } catch {
    return { removed: false, path: prefix };
  }
  await rm(prefix, { recursive: true, force: true });
  return { removed: true, path: prefix };
}

export { DEFAULT_TIMEOUT_MS };
