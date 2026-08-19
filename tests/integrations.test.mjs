import assert from 'node:assert/strict';
import { chmod, mkdir, mkdtemp, readFile, rm, stat, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';

import { catalogEntry } from '../setup/integrations/catalog.mjs';
import { managedBinPath, managedToolsPrefix } from '../setup/integrations/tools.mjs';
import { readLedger } from '../setup/integrations/ledger.mjs';

const ALL = ['claude', 'codex', 'opencode', 'cursor'];

function outputCapture() {
  const chunks = [];
  return { output: { write: (c) => chunks.push(String(c)) }, text: () => chunks.join('') };
}

async function installedHome({ harnesses = ALL } = {}) {
  const home = await mkdtemp(join(tmpdir(), 'loam-integ-home-'));
  const globalRoot = join(home, '.agents', 'loam');
  await mkdir(globalRoot, { recursive: true });
  const install = {
    schema_version: 1,
    plugin_version: '0.13.0',
    runtime_version: '0.9.1',
    runtime_path: join(globalRoot, 'bin', 'loam'),
    configured_harnesses: harnesses,
  };
  const discovery = { home, globalRoot, platform: 'linux', harnesses: Object.fromEntries(ALL.map((id) => [id, { state: 'ready' }])) };
  return { home, globalRoot, install, discovery };
}

const ctxFor = (fx, capture, extra = {}) => ({
  discovery: fx.discovery,
  install: fx.install,
  dryRun: false,
  purge: false,
  output: capture.output,
  // Isolate PATH so a real `qmd` on the test machine never leaks into resolution.
  env: { PATH: '' },
  ...extra,
});

// A toolRunner that models `npm install --prefix <prefix> @tobilu/qmd` by writing
// an executable bin shim, and answers `<bin> --version` with success.
function qmdToolRunner(globalRoot, { installCode = 0, versionCode = 0, installStderr = '' } = {}) {
  const calls = [];
  return {
    calls,
    runner: async ({ command, args = [] }) => {
      calls.push([command, ...args].join(' '));
      if (args.includes('install')) {
        if (installCode === 0) {
          const bin = managedBinPath(globalRoot, 'qmd', 'qmd', 'linux');
          await mkdir(join(bin, '..'), { recursive: true });
          await writeFile(bin, '#!/bin/sh\necho qmd\n');
          await chmod(bin, 0o755);
        }
        return { code: installCode, stdout: '', stderr: installStderr };
      }
      if (args.includes('--version')) return { code: versionCode, stdout: 'qmd 1.2.3\n', stderr: '' };
      return { code: 0, stdout: '', stderr: '' };
    },
  };
}

async function readJson(path) { return JSON.parse(await readFile(path, 'utf8')); }

test('grep enable registers a correctly-typed remote MCP in every configured harness and installs no tool', async () => {
  const fx = await installedHome();
  const capture = outputCapture();
  const result = await catalogEntry('grep').enable(ctxFor(fx, capture));
  assert.equal(result.ready, true, capture.text());
  assert.deepEqual(result.registered.sort(), [...ALL].sort());

  const claude = await readJson(join(fx.home, '.claude.json'));
  assert.deepEqual(claude.mcpServers.grep, { type: 'http', url: 'https://mcp.grep.app' });
  const cursor = await readJson(join(fx.home, '.cursor', 'mcp.json'));
  assert.deepEqual(cursor.mcpServers.grep, { type: 'http', url: 'https://mcp.grep.app' });
  const opencode = await readJson(join(fx.home, '.config', 'opencode', 'opencode.json'));
  assert.deepEqual(opencode.mcp.grep, { type: 'remote', url: 'https://mcp.grep.app', enabled: true });
  const codex = await readFile(join(fx.home, '.codex', 'config.toml'), 'utf8');
  assert.match(codex, /\[mcp_servers\.grep\]/);
  assert.match(codex, /url = 'https:\/\/mcp\.grep\.app'/);
  // No tool prefix created.
  await assert.rejects(() => stat(managedToolsPrefix(fx.globalRoot, 'qmd')));
});

test('qmd enable installs into the managed prefix, verifies, then registers a local MCP with the absolute path', async () => {
  const fx = await installedHome();
  const capture = outputCapture();
  const tool = qmdToolRunner(fx.globalRoot);
  const result = await catalogEntry('qmd').enable(ctxFor(fx, capture, { toolRunner: tool.runner }));
  assert.equal(result.ready, true, capture.text());
  // install ran before any registration and the health check ran.
  assert.ok(tool.calls.some((c) => c.includes('install') && c.includes('@tobilu/qmd')));
  assert.ok(tool.calls.some((c) => c.includes('--version')));

  const bin = managedBinPath(fx.globalRoot, 'qmd', 'qmd', 'linux');
  const opencode = await readJson(join(fx.home, '.config', 'opencode', 'opencode.json'));
  assert.deepEqual(opencode.mcp.qmd, { type: 'local', command: [bin, 'mcp'], enabled: true });
  const claude = await readJson(join(fx.home, '.claude.json'));
  assert.deepEqual(claude.mcpServers.qmd, { type: 'stdio', command: bin, args: ['mcp'] });
  const codex = await readFile(join(fx.home, '.codex', 'config.toml'), 'utf8');
  assert.match(codex, new RegExp(`command = '${bin.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}'`));
  const ledger = await readLedger(fx.globalRoot);
  assert.equal(ledger.integrations.qmd.tool.managed, true);
});

test('a failed tool install is classified, registers no MCP, rolls back, and leaves no managed prefix', async () => {
  const fx = await installedHome();
  const capture = outputCapture();
  const tool = qmdToolRunner(fx.globalRoot, { installCode: 1, installStderr: 'npm ERR! code E404\nnpm ERR! 404 Not Found' });
  const result = await catalogEntry('qmd').enable(ctxFor(fx, capture, { toolRunner: tool.runner }));
  assert.equal(result.ready, false);
  assert.equal(result.category, 'package-not-found');
  // No MCP written anywhere.
  await assert.rejects(() => readFile(join(fx.home, '.claude.json')));
  await assert.rejects(() => readFile(join(fx.home, '.codex', 'config.toml')));
  // Managed prefix rolled back.
  await assert.rejects(() => stat(managedToolsPrefix(fx.globalRoot, 'qmd')));
});

test('an existing user-owned MCP entry is neither duplicated nor overwritten', async () => {
  const fx = await installedHome({ harnesses: ['claude'] });
  await writeFile(join(fx.home, '.claude.json'), JSON.stringify({
    mcpServers: { grep: { type: 'http', url: 'https://user.example/grep', note: 'mine' } },
    other: true,
  }));
  const capture = outputCapture();
  const result = await catalogEntry('grep').enable(ctxFor(fx, capture));
  assert.equal(result.ready, true, capture.text());
  const claude = await readJson(join(fx.home, '.claude.json'));
  // User entry untouched; not duplicated; unrelated key preserved.
  assert.deepEqual(claude.mcpServers.grep, { type: 'http', url: 'https://user.example/grep', note: 'mine' });
  assert.equal(claude.other, true);
  // Not recorded as loam-owned, so disable will never touch it.
  const ledger = await readLedger(fx.globalRoot);
  assert.equal(ledger.integrations.grep?.mcp?.claude, undefined);
});

test('a pre-existing tool on PATH is used as-is: no install, MCP registered with the resolved path', async () => {
  const fx = await installedHome({ harnesses: ['opencode'] });
  // Put a fake qmd on PATH.
  const binDir = await mkdtemp(join(tmpdir(), 'loam-integ-path-'));
  const onPath = join(binDir, 'qmd');
  await writeFile(onPath, '#!/bin/sh\necho qmd\n');
  await chmod(onPath, 0o755);
  try {
    const capture = outputCapture();
    const tool = qmdToolRunner(fx.globalRoot);
    const result = await catalogEntry('qmd').enable(ctxFor(fx, capture, { toolRunner: tool.runner, env: { PATH: binDir } }));
    assert.equal(result.ready, true, capture.text());
    // No npm install ran.
    assert.ok(!tool.calls.some((c) => c.includes('install')), 'pre-existing tool must not be reinstalled');
    const opencode = await readJson(join(fx.home, '.config', 'opencode', 'opencode.json'));
    assert.deepEqual(opencode.mcp.qmd, { type: 'local', command: [onPath, 'mcp'], enabled: true });
    const ledger = await readLedger(fx.globalRoot);
    assert.equal(ledger.integrations.qmd.tool.managed, false, 'pre-existing tool is recorded as unmanaged');
  } finally {
    await rm(binDir, { recursive: true, force: true });
  }
});

test('codex TOML registration preserves unrelated tables and comments', async () => {
  const fx = await installedHome({ harnesses: ['codex'] });
  const configPath = join(fx.home, '.codex', 'config.toml');
  await mkdir(join(fx.home, '.codex'), { recursive: true });
  await writeFile(configPath, '# my codex config\n[settings]\nkeep = true\n\n[mcp_servers.other]\nurl = \'https://other\'\n');
  const capture = outputCapture();
  await catalogEntry('grep').enable(ctxFor(fx, capture));
  const toml = await readFile(configPath, 'utf8');
  assert.match(toml, /# my codex config/);
  assert.match(toml, /\[settings\]\nkeep = true/);
  assert.match(toml, /\[mcp_servers\.other\]/);
  assert.match(toml, /\[mcp_servers\.grep\]/);
});

test('disable is symmetric: deregisters everywhere, removes the managed tool, verifies absence', async () => {
  const fx = await installedHome();
  const capture = outputCapture();
  const tool = qmdToolRunner(fx.globalRoot);
  await catalogEntry('qmd').enable(ctxFor(fx, capture, { toolRunner: tool.runner }));
  await assert.doesNotReject(() => stat(managedBinPath(fx.globalRoot, 'qmd', 'qmd', 'linux')));

  const off = await catalogEntry('qmd').disable(ctxFor(fx, outputCapture().output ? outputCapture() : capture, { toolRunner: tool.runner }));
  assert.equal(off.ready, true);
  for (const harness of ALL) {
    const target = harness === 'codex'
      ? readFile(join(fx.home, '.codex', 'config.toml'), 'utf8').catch(() => '')
      : null;
    if (harness === 'codex') {
      assert.doesNotMatch(await target, /\[mcp_servers\.qmd\]/);
    } else {
      const path = harness === 'claude' ? join(fx.home, '.claude.json')
        : harness === 'cursor' ? join(fx.home, '.cursor', 'mcp.json')
        : join(fx.home, '.config', 'opencode', 'opencode.json');
      const cfg = await readJson(path);
      const bucket = harness === 'opencode' ? cfg.mcp : cfg.mcpServers;
      assert.equal(bucket?.qmd, undefined, `${harness} still has qmd`);
    }
  }
  await assert.rejects(() => stat(managedToolsPrefix(fx.globalRoot, 'qmd')), 'managed tool removed');
  const ledger = await readLedger(fx.globalRoot);
  assert.equal(ledger.integrations.qmd, undefined, 'ledger entry cleared');
});

test('round-trip enable -> disable -> enable converges with no residue', async () => {
  const fx = await installedHome();
  const run = async (mode) => {
    const capture = outputCapture();
    const tool = qmdToolRunner(fx.globalRoot);
    const entry = catalogEntry('qmd');
    const result = mode === 'enable'
      ? await entry.enable(ctxFor(fx, capture, { toolRunner: tool.runner }))
      : await entry.disable(ctxFor(fx, capture, { toolRunner: tool.runner }));
    return { result, text: capture.text() };
  };
  const on1 = await run('enable');
  assert.equal(on1.result.ready, true, on1.text);
  const off = await run('disable');
  assert.equal(off.result.ready, true, off.text);
  await assert.rejects(() => stat(managedToolsPrefix(fx.globalRoot, 'qmd')));
  const on2 = await run('enable');
  assert.equal(on2.result.ready, true, on2.text);
  const opencode = await readJson(join(fx.home, '.config', 'opencode', 'opencode.json'));
  assert.ok(opencode.mcp.qmd, 're-enable converges to a working registration');
  const ledger = await readLedger(fx.globalRoot);
  assert.deepEqual(Object.keys(ledger.integrations), ['qmd']);
});

test('the QMD model cache is kept by default and removed with --purge, size shown', async () => {
  const fx = await installedHome({ harnesses: ['opencode'] });
  const capture = outputCapture();
  const tool = qmdToolRunner(fx.globalRoot);
  await catalogEntry('qmd').enable(ctxFor(fx, capture, { toolRunner: tool.runner }));
  // Simulate a large model cache.
  const cacheDir = join(fx.home, '.cache', 'qmd', 'models');
  await mkdir(cacheDir, { recursive: true });
  await writeFile(join(cacheDir, 'model.gguf'), Buffer.alloc(2048));

  // Default keep (non-interactive, no purge).
  const keepCapture = outputCapture();
  const kept = await catalogEntry('qmd').disable(ctxFor(fx, keepCapture, { toolRunner: tool.runner }));
  assert.equal(kept.ready, true);
  assert.match(keepCapture.text(), /QMD model cache/);
  assert.match(keepCapture.text(), /KB|MB|GB/);
  await assert.doesNotReject(() => stat(cacheDir), 'cache kept by default');

  // Re-enable then disable with --purge removes it.
  await catalogEntry('qmd').enable(ctxFor(fx, outputCapture(), { toolRunner: tool.runner }));
  const purged = await catalogEntry('qmd').disable(ctxFor(fx, outputCapture(), { toolRunner: tool.runner, purge: true }));
  assert.equal(purged.ready, true);
  await assert.rejects(() => stat(cacheDir), 'cache removed with --purge');
});

test('dry-run enable writes no config and installs nothing', async () => {
  const fx = await installedHome();
  const capture = outputCapture();
  const tool = qmdToolRunner(fx.globalRoot);
  const result = await catalogEntry('qmd').enable(ctxFor(fx, capture, { dryRun: true, toolRunner: tool.runner }));
  assert.equal(result.ready, true);
  assert.equal(tool.calls.length, 0, 'dry-run installs nothing');
  await assert.rejects(() => readFile(join(fx.home, '.claude.json')));
  await assert.rejects(() => stat(managedToolsPrefix(fx.globalRoot, 'qmd')));
  const ledger = await readLedger(fx.globalRoot);
  assert.equal(ledger.integrations.qmd, undefined, 'dry-run records nothing');
});

// --- Review fixes ---------------------------------------------------------

test('disable with no ledger record is a no-op and never deletes a user-owned entry', async () => {
  const fx = await installedHome({ harnesses: ['claude'] });
  // The user has their OWN qmd MCP; loam never enabled the integration (no ledger).
  await writeFile(join(fx.home, '.claude.json'), JSON.stringify({
    mcpServers: { qmd: { type: 'stdio', command: '/usr/local/bin/qmd', args: ['mcp'] } },
  }));
  const capture = outputCapture();
  const result = await catalogEntry('qmd').disable(ctxFor(fx, capture, { env: { PATH: '' } }));
  assert.equal(result.ready, true, capture.text());
  assert.equal(result.noop, true, 'no ledger → success no-op');
  // The user's entry survives untouched.
  const claude = await readJson(join(fx.home, '.claude.json'));
  assert.deepEqual(claude.mcpServers.qmd, { type: 'stdio', command: '/usr/local/bin/qmd', args: ['mcp'] });
  assert.match(capture.text(), /user-owned|not enabled by loam/);
});

test('disabling one tool-backed integration leaves another integration\'s managed tool intact', async () => {
  const { enableIntegration, disableIntegration } = await import('../setup/integrations/registrar.mjs');
  const fx = await installedHome({ harnesses: ['opencode'] });

  // Two synthetic tool-backed entries with distinct ids/bins. A per-id runner
  // writes each id's own managed bin, proving prefixes are isolated.
  const makeRunner = (id, binName) => async ({ args = [] }) => {
    if (args.includes('install')) {
      const bin = managedBinPath(fx.globalRoot, id, binName, 'linux');
      await mkdir(join(bin, '..'), { recursive: true });
      await writeFile(bin, '#!/bin/sh\necho ok\n');
      await chmod(bin, 0o755);
      return { code: 0, stdout: '', stderr: '' };
    }
    if (args.includes('--version')) return { code: 0, stdout: `${binName} 1\n`, stderr: '' };
    return { code: 0, stdout: '', stderr: '' };
  };
  const entryFor = (id, binName, pkg) => ({
    id, label: id, capability: 'x', mcpName: id,
    tool: { pkg, binName, healthArgs: ['--version'] },
    descriptor: (toolPath) => ({ transport: 'local', command: toolPath, args: ['mcp'] }),
  });
  const alpha = entryFor('alpha', 'alpha', '@x/alpha');
  const beta = entryFor('beta', 'beta', '@x/beta');
  const baseCtx = (extra) => ctxFor(fx, outputCapture(), { env: { PATH: '' }, ...extra });

  assert.equal((await enableIntegration(alpha, baseCtx({ toolRunner: makeRunner('alpha', 'alpha') }))).ready, true);
  assert.equal((await enableIntegration(beta, baseCtx({ toolRunner: makeRunner('beta', 'beta') }))).ready, true);
  await assert.doesNotReject(() => stat(managedBinPath(fx.globalRoot, 'alpha', 'alpha', 'linux')));
  await assert.doesNotReject(() => stat(managedBinPath(fx.globalRoot, 'beta', 'beta', 'linux')));

  // Disable alpha — beta's tool must survive (blast radius scoped to alpha).
  assert.equal((await disableIntegration(alpha, baseCtx({}))).ready, true);
  await assert.rejects(() => stat(managedBinPath(fx.globalRoot, 'alpha', 'alpha', 'linux')), 'alpha tool removed');
  await assert.doesNotReject(() => stat(managedBinPath(fx.globalRoot, 'beta', 'beta', 'linux')), 'beta tool survives');
});

// --- hcom: the detection-only shape ---------------------------------------

// A stand-in hcom binary at a chosen site. Detection is filesystem-real (the
// resolver stats for an executable), so the fake has to be a real executable
// file; only the health check goes through the injected runner.
async function fakeHcom(directory) {
  await mkdir(directory, { recursive: true });
  const path = join(directory, 'hcom');
  await writeFile(path, '#!/bin/sh\necho "hcom 0.7.25"\n');
  await chmod(path, 0o755);
  return path;
}

function hcomRunner({ versionCode = 0 } = {}) {
  const calls = [];
  return {
    calls,
    runner: async ({ command, args = [] }) => {
      calls.push([command, ...args].join(' '));
      return { code: versionCode, stdout: 'hcom 0.7.25\n', stderr: versionCode === 0 ? '' : 'boom' };
    },
  };
}

async function noMcpAnywhere(home) {
  for (const path of [
    join(home, '.claude.json'),
    join(home, '.cursor', 'mcp.json'),
    join(home, '.config', 'opencode', 'opencode.json'),
    join(home, '.codex', 'config.toml'),
  ]) {
    await assert.rejects(() => stat(path), `${path} was written for an entry with no MCP lane`);
  }
}

test('enabling hcom when it is absent refuses with the per-OS install recipes and records nothing', async () => {
  const fx = await installedHome();
  const capture = outputCapture();
  const tool = hcomRunner();
  const result = await catalogEntry('hcom').enable(ctxFor(fx, capture, { toolRunner: tool.runner }));

  assert.equal(result.ready, false);
  assert.equal(result.category, 'tool-not-installed');
  assert.match(result.detail, /never installs it/);
  // Recipes, not a label: every route the user could actually run.
  const text = capture.text();
  assert.match(text, /brew install aannoo\/hcom\/hcom/);
  assert.match(text, /hcom-installer\.sh/);
  assert.match(text, /hcom-installer\.ps1/);
  assert.match(text, /uv tool install hcom/);
  // The reason comes BEFORE the commands: a wall of install lines with no
  // framing, explained only afterwards, is the shape #163 is sweeping out.
  const why = text.indexOf('is not installed');
  const how = text.indexOf('brew install');
  assert.ok(why >= 0 && why < how, `the refusal must say why before it says how:\n${text}`);
  // A refusal never half-enables: no health spawn, no ledger, no config.
  assert.equal(tool.calls.length, 0, 'nothing to health-check when nothing resolved');
  const ledger = await readLedger(fx.globalRoot);
  assert.equal(ledger.integrations.hcom, undefined);
  await noMcpAnywhere(fx.home);
});

test('enabling hcom found on PATH records it as unmanaged and registers no MCP', async () => {
  const fx = await installedHome();
  const binDir = join(fx.home, 'bin');
  const binPath = await fakeHcom(binDir);
  const capture = outputCapture();
  const tool = hcomRunner();
  const result = await catalogEntry('hcom').enable(ctxFor(fx, capture, { toolRunner: tool.runner, env: { PATH: binDir } }));

  assert.equal(result.ready, true, capture.text());
  assert.deepEqual(result.registered, [], 'no MCP lane, so nothing is registered');
  assert.deepEqual(result.tool, { managed: false, pkg: null, path: binPath });
  assert.deepEqual(tool.calls, [`${binPath} --version`], 'detection health-checks with --version');
  assert.match(capture.text(), /registers no MCP server/);
  await noMcpAnywhere(fx.home);

  // The ledger records ownership of exactly nothing but the record itself.
  const ledger = await readLedger(fx.globalRoot);
  assert.deepEqual(ledger.integrations.hcom, { mcp: {}, tool: { managed: false, pkg: null, path: binPath } });
});

test('a chatty version answer is reduced to one short line', async () => {
  // doctor prints one line per integration and the ledger records the string,
  // so a tool that answers with a build banner must not be able to reshape
  // either. The version is the tool's own words — untrusted for shape.
  const fx = await installedHome();
  const binDir = join(fx.home, 'bin');
  await fakeHcom(binDir);
  const runner = async () => ({
    code: 0,
    stdout: `hcom 0.7.25\nbuild: abc\n${'x'.repeat(200)}\n`,
    stderr: '',
  });
  const result = await catalogEntry('hcom').enable(ctxFor(fx, outputCapture(), { toolRunner: runner, env: { PATH: binDir } }));
  assert.equal(result.ready, true);

  const state = await catalogEntry('hcom').verify(ctxFor(fx, outputCapture(), { toolRunner: runner, env: { PATH: binDir } }));
  assert.equal(state.tool.version, 'hcom 0.7.25');
  assert.ok(!state.tool.version.includes('\n'), 'a version is one line');
});

test('hcom is detected in the installer default directory and under HCOM_INSTALL_DIR, off PATH', async () => {
  const fx = await installedHome();
  const capture = outputCapture();
  const tool = hcomRunner();
  // The installers target ~/.local/bin on every OS; a non-login shell may not
  // carry it on PATH, so the entry's declared sites are searched too.
  const localBin = await fakeHcom(join(fx.home, '.local', 'bin'));
  const viaSite = await catalogEntry('hcom').enable(ctxFor(fx, capture, { toolRunner: tool.runner, env: { PATH: '' } }));
  assert.equal(viaSite.ready, true, capture.text());
  assert.equal(viaSite.tool.path, localBin);
  assert.match(capture.text(), /install site/);

  const overrideRoot = join(fx.home, 'opt', 'hcom');
  const overrideBin = await fakeHcom(join(overrideRoot, 'bin'));
  const viaOverride = await catalogEntry('hcom').enable(ctxFor(fx, outputCapture(), {
    toolRunner: tool.runner,
    env: { PATH: '', HCOM_INSTALL_DIR: overrideRoot },
  }));
  assert.equal(viaOverride.ready, true);
  assert.equal(viaOverride.tool.path, overrideBin, 'the override wins over the default site');
});

test('an hcom binary that fails its health check is not enabled', async () => {
  const fx = await installedHome();
  const binDir = join(fx.home, 'bin');
  await fakeHcom(binDir);
  const capture = outputCapture();
  const tool = hcomRunner({ versionCode: 1 });
  const result = await catalogEntry('hcom').enable(ctxFor(fx, capture, { toolRunner: tool.runner, env: { PATH: binDir } }));

  assert.equal(result.ready, false, capture.text());
  assert.equal(result.category, 'tool-not-installed');
  const ledger = await readLedger(fx.globalRoot);
  assert.equal(ledger.integrations.hcom, undefined);
});

test('disabling hcom clears the ledger record and touches nothing else, even with --purge', async () => {
  const fx = await installedHome();
  const binDir = join(fx.home, 'bin');
  const binPath = await fakeHcom(binDir);
  const tool = hcomRunner();
  const enableCtx = { toolRunner: tool.runner, env: { PATH: binDir } };
  await catalogEntry('hcom').enable(ctxFor(fx, outputCapture(), enableCtx));

  // Live hcom state: message history and agent data, which loam never created.
  const hcomDir = join(fx.home, '.hcom');
  await mkdir(hcomDir, { recursive: true });
  await writeFile(join(hcomDir, 'hcom.db'), Buffer.alloc(4096));

  const capture = outputCapture();
  const off = await catalogEntry('hcom').disable(ctxFor(fx, capture, { ...enableCtx, purge: true }));
  assert.equal(off.ready, true, capture.text());
  assert.deepEqual(off.caches, [], '~/.hcom is user data — never offered as a purgeable cache');

  const ledger = await readLedger(fx.globalRoot);
  assert.equal(ledger.integrations.hcom, undefined, 'ledger record cleared');
  await assert.doesNotReject(() => stat(binPath), 'a detected binary is never removed');
  await assert.doesNotReject(() => stat(join(hcomDir, 'hcom.db')), '~/.hcom survives --purge');
  assert.match(capture.text(), /never installed by loam/);
});

test('hcom verify reports the detected tool and no MCP state for doctor', async () => {
  const fx = await installedHome();
  const binDir = join(fx.home, 'bin');
  const binPath = await fakeHcom(binDir);
  const tool = hcomRunner();
  const state = await catalogEntry('hcom').verify(ctxFor(fx, outputCapture(), { toolRunner: tool.runner, env: { PATH: binDir } }));

  assert.equal(state.ready, true);
  assert.equal(state.id, 'hcom');
  assert.equal(state.tool.present, true);
  assert.equal(state.tool.managed, false);
  assert.equal(state.tool.source, 'PATH');
  assert.equal(state.tool.path, binPath);
  assert.equal(state.tool.version, 'hcom 0.7.25', 'the health-check answer doctor prints');
  assert.deepEqual(state.registered, {}, 'no MCP lane means no per-harness state');
});
