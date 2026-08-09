import assert from 'node:assert/strict';
import { execFile, spawn } from 'node:child_process';
import { cp, mkdir, mkdtemp, readFile, rm } from 'node:fs/promises';
import { promisify } from 'node:util';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { test } from 'node:test';

import { assertPackageAssets } from '../setup/package-check.mjs';

const execFileAsync = promisify(execFile);
const packageRoot = fileURLToPath(new URL('..', import.meta.url));
const loaderPath = join(packageRoot, '.opencode', 'plugins', 'loam.js');
const marketplaceRoot = join(packageRoot, 'plugins', 'loam-adapter');
const marketplaceStopPath = join(marketplaceRoot, 'hooks', 'stop.mjs');
const npmCommand = process.platform === 'win32' ? 'npm.cmd' : 'npm';

async function runHook(env, payload = {}, path = marketplaceStopPath) {
  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath, [path], {
      cwd: packageRoot,
      env: { ...process.env, ...env },
    });
    let stdout = '';
    let stderr = '';
    child.stdout.on('data', (chunk) => { stdout += chunk; });
    child.stderr.on('data', (chunk) => { stderr += chunk; });
    child.once('error', reject);
    child.once('close', (code) => resolve({ code, stdout, stderr }));
    child.stdin.end(JSON.stringify(payload));
  });
}

test('legacy OpenCode entry delegates to the shared adapter without startup polling', async () => {
  const source = await readFile(loaderPath, 'utf8');
  assert.match(source, /adapters[\\/]opencode\.mjs/);
  assert.doesNotMatch(source, /git ls-remote|loamstate\.(sh|ps1)|findSkillPath/);

  const plugin = await import(pathToFileURL(loaderPath).href);
  assert.equal(typeof plugin.LoamPlugin, 'function');
  assert.equal(typeof plugin.default, 'function');
});

test('missing adapter in an existing clone returns setup recovery instead of a loader error', async () => {
  const clone = await mkdtemp(join(tmpdir(), 'loam-legacy-clone-'));
  const cloneLoader = join(clone, '.opencode', 'plugins', 'loam.js');
  await mkdir(dirname(cloneLoader), { recursive: true });
  await cp(loaderPath, cloneLoader);

  try {
    const loaded = await import(`${pathToFileURL(cloneLoader).href}?fixture=${Date.now()}`);
    const plugin = await loaded.LoamPlugin({ directory: clone });
    const output = { messages: [{ info: { role: 'user' }, parts: [{ type: 'text', text: 'hello' }] }] };
    await plugin['experimental.chat.messages.transform']({}, output);
    assert.match(output.messages[0].parts[0].text, /npx @scchearn\/loam setup/);
  } finally {
    await rm(clone, { recursive: true, force: true });
  }
});

test('packed tarball contains a loadable adapter through the preserved main entry', async () => {
  const destination = await mkdtemp(join(tmpdir(), 'loam-pack-'));
  const extracted = join(destination, 'extracted');
  await mkdir(extracted);
  try {
    const { stdout } = await execFileAsync(npmCommand, [
      'pack',
      '--ignore-scripts',
      '--silent',
      '--pack-destination',
      destination,
    ], { cwd: packageRoot, shell: process.platform === 'win32' });
    const tarball = join(destination, stdout.trim().split(/\r?\n/).at(-1));
    await execFileAsync('tar', ['-xzf', tarball], { cwd: extracted });
    const packedRoot = join(extracted, 'package');
    const packedLoader = join(packedRoot, '.opencode', 'plugins', 'loam.js');
    const loaded = await import(`${pathToFileURL(packedLoader).href}?fixture=${Date.now()}`);
    const plugin = await loaded.LoamPlugin({ directory: packedRoot });
    assert.equal(typeof plugin['experimental.chat.messages.transform'], 'function');
  } finally {
    await rm(destination, { recursive: true, force: true });
  }
});

test('publication guard rejects a package fixture missing the shared integration', async () => {
  const fixture = await mkdtemp(join(tmpdir(), 'loam-package-fixture-'));
  try {
    const excluded = ['/node_modules/', '/.git/', '/cli/', '/plans/', '/specs/', '/target/', '/tests/'];
    await cp(packageRoot, fixture, {
      recursive: true,
      filter: (source) => !excluded.some((part) => source.includes(part)),
    });
    await rm(join(fixture, 'integration'), { recursive: true, force: true });
    await assert.rejects(
      () => assertPackageAssets({ packageRoot: fixture }),
      /package asset is missing: integration/,
    );
  } finally {
    await rm(fixture, { recursive: true, force: true });
  }
});

test('thin marketplace adapter contains no skills and no session-context surface', async () => {
  const claude = JSON.parse(await readFile(join(marketplaceRoot, '.claude-plugin', 'plugin.json'), 'utf8'));
  const codex = JSON.parse(await readFile(join(marketplaceRoot, '.codex-plugin', 'plugin.json'), 'utf8'));
  const adapter = await import(pathToFileURL(join(marketplaceRoot, 'adapter.mjs')).href);

  assert.equal('skills' in claude, false);
  assert.equal('skills' in codex, false);
  // Claude finds `hooks/hooks.json` by convention; Codex names it explicitly.
  // Either way the file setup rewrites is the same one.
  assert.equal('hooks' in claude, false);
  assert.equal(codex.hooks, './hooks/hooks.json');

  // The adapter keeps the Stop/ingestion half only. Session context is served
  // by the native `loam hook <harness>` command setup registers directly.
  assert.equal(typeof adapter.handleMarketplaceStop, 'function');
  for (const retired of ['createMarketplaceAdapter', 'createClaudeAdapter', 'handleMarketplaceHook', 'handleClaudeHook']) {
    assert.equal(retired in adapter, false, `${retired} must be retired`);
  }
});

test('plugin manifests carry skills only and register no Node hook entry', async () => {
  const claude = JSON.parse(await readFile(join(packageRoot, '.claude-plugin', 'plugin.json'), 'utf8'));
  const cursor = JSON.parse(await readFile(join(packageRoot, '.cursor-plugin', 'plugin.json'), 'utf8'));

  // Hook registration moved to setup, the only party that knows the version-
  // and target-qualified private runtime path.
  assert.equal('hooks' in claude, false);
  assert.equal('hooks' in cursor, false);
  assert.ok(Array.isArray(claude.skills) && claude.skills.length > 0);
  await assert.rejects(() => readFile(join(packageRoot, 'hooks', 'hooks.json'), 'utf8'));
});

test('marketplace plugin ships Stop only and leaves the session boundary to setup', async () => {
  const hooks = JSON.parse(await readFile(join(marketplaceRoot, 'hooks', 'hooks.json'), 'utf8'));
  assert.equal('SessionStart' in hooks.hooks, false);
  assert.equal('UserPromptSubmit' in hooks.hooks, false);
  assert.match(hooks.hooks.Stop[0].hooks[0].command, /stop\.mjs/);

  const stop = await import(pathToFileURL(marketplaceStopPath).href);
  const calls = [];
  const run = { id: 8 };
  const workspace = resolve('/workspace');
  const loadHooks = async () => ({
    resolveGlobalRoot: () => '/global',
    beginHookRun: async (input) => { calls.push(['begin', input]); return run; },
    finishHookRun: async (input) => calls.push(['finish', input]),
  });
  const loadIngest = async () => ({
    resolveGlobalRoot: () => '/global',
    resolveSkillsRoot: () => '/skills',
    dispatchBoundary: async (input) => {
      calls.push(['ingest', input]);
      return { action: 'skip', reason: 'nothing_to_do', detail: 'no changes' };
    },
  });

  assert.deepEqual(await stop.handleStop(
    { cwd: workspace, session_id: 'session' },
    { PLUGIN_ROOT: marketplaceRoot },
    { loadHooks, loadIngest },
  ), {});
  assert.deepEqual(calls.map(([kind]) => kind), ['begin', 'ingest', 'finish']);
  assert.deepEqual(calls[0][1], {
    globalRoot: '/global', harness: 'codex', hook: 'stop', workspace, sessionId: 'session',
  });
  assert.equal(calls[1][1].harness, 'codex');
  assert.equal(calls[1][1].globalRoot, '/global');
  assert.equal(calls[1][1].skillsRoot, '/skills');
  assert.equal(calls[1][1].hookRunId, 8);
  assert.deepEqual(calls[2][1], {
    run,
    status: 'succeeded',
    action: 'skip',
    reason: 'nothing_to_do',
    detail: 'no changes',
  });

  calls.length = 0;
  assert.deepEqual(await stop.handleStop(
    { cwd: workspace, session_id: 'failed-session' },
    { PLUGIN_ROOT: marketplaceRoot },
    {
      loadHooks,
      loadIngest: async () => ({
        resolveGlobalRoot: () => '/global',
        resolveSkillsRoot: () => '/skills',
        dispatchBoundary: async () => { throw new Error('gate failed'); },
      }),
    },
  ), {});
  assert.deepEqual(calls.map(([kind]) => kind), ['begin', 'finish']);
  assert.equal(calls[1][1].status, 'failed');
  assert.match(calls[1][1].detail, /gate failed/);

  calls.length = 0;
  assert.deepEqual(await stop.handleStop(
    { cwd: workspace },
    { PLUGIN_ROOT: marketplaceRoot },
    { loadHooks: async () => { throw new Error('old integration'); }, loadIngest },
  ), {});
  assert.deepEqual(calls.map(([kind]) => kind), ['ingest']);
});

test('marketplace Stop writes exact hook JSON when logging is unavailable', async () => {
  const home = await mkdtemp(join(tmpdir(), 'loam-stop-output-'));
  const result = await runHook({
    HOME: home,
    USERPROFILE: home,
    LOAM_HOME: join(home, '.agents', 'loam'),
    PLUGIN_ROOT: marketplaceRoot,
  }, { cwd: home }, marketplaceStopPath);

  assert.equal(result.code, 0, result.stderr);
  assert.equal(result.stdout, '{}\n');
  assert.equal(result.stderr, '');
});
