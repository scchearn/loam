import assert from 'node:assert/strict';
import { mkdir, mkdtemp, stat, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';

import { installMarketplacePlugins, removeMarketplacePlugins } from '../setup/marketplace.mjs';
import { selectHarnesses } from '../setup/wizard.mjs';

const harnesses = {
  opencode: { id: 'opencode', state: 'detected' },
  claude: { id: 'claude', state: 'detected', marketplaceOwned: false },
  codex: { id: 'codex', state: 'detected', marketplaceOwned: false },
  cursor: { id: 'cursor', state: 'detected' },
};

async function exists(path) {
  try {
    await stat(path);
    return true;
  } catch {
    return false;
  }
}

async function claudeRoot() {
  return mkdtemp(join(tmpdir(), 'loam-marketplace-remove-'));
}

test('--yes selects every detected harness', async () => {
  assert.deepEqual(await selectHarnesses({ yes: true, harnesses }), {
    selected: ['claude', 'codex', 'opencode', 'cursor'],
    toRemove: [],
  });
});

test('update --yes maintains only the previously-configured set', async () => {
  assert.deepEqual(await selectHarnesses({
    yes: true,
    refresh: true,
    harnesses,
    previouslyConfigured: ['claude', 'opencode'],
  }), { selected: ['claude', 'opencode'], toRemove: [] });
});

test('interactive selection offers every detected harness, all preselected', async () => {
  let prompt;
  const result = await selectHarnesses({
    harnesses: { ...harnesses, codex: { id: 'codex', state: 'absent' } },
    previouslyConfigured: ['opencode'],
    select: async (input) => {
      prompt = input;
      return ['claude'];
    },
  });

  assert.deepEqual(result.selected, ['claude']);
  assert.deepEqual(result.toRemove, ['opencode']);
  assert.deepEqual(prompt.options.map(({ value }) => value), ['claude', 'opencode', 'cursor']);
  assert.deepEqual(prompt.initialValues, ['claude', 'opencode', 'cursor']);
});

test('marketplace installation uses exact native argv and skips existing plugins', async () => {
  const calls = [];
  const result = await installMarketplacePlugins({
    selected: ['claude', 'codex'],
    harnesses: { ...harnesses, claude: { ...harnesses.claude, marketplaceOwned: true, marketplaceReady: true } },
    runner: async (request) => {
      calls.push({ command: request.command, args: request.args });
      return { code: 0, stdout: '', stderr: '' };
    },
  });

  assert.deepEqual(calls, [
    { command: 'codex', args: ['plugin', 'marketplace', 'add', 'scchearn/loam'] },
    { command: 'codex', args: ['plugin', 'add', 'loam@loam'] },
  ]);
  assert.equal(result.claude.state, 'ready');
  assert.equal(result.claude.action, 'existing');
  assert.equal(result.codex.state, 'ready');
  assert.equal(result.codex.action, 'installed');
});

test('installed plugins missing required hooks are updated instead of skipped', async () => {
  const calls = [];
  const result = await installMarketplacePlugins({
    selected: ['claude', 'codex'],
    harnesses: {
      claude: { ...harnesses.claude, marketplaceInstalled: true, marketplaceOwned: true, marketplaceReady: false },
      codex: { ...harnesses.codex, marketplaceInstalled: true, marketplaceOwned: true, marketplaceReady: false },
    },
    runner: async (request) => {
      calls.push({ command: request.command, args: request.args });
      return { code: 0, stdout: '', stderr: '' };
    },
  });

  assert.deepEqual(calls, [
    { command: 'claude', args: ['plugin', 'update', 'loam@loam', '--scope', 'user'] },
    { command: 'claude', args: ['plugin', 'enable', 'loam@loam', '--scope', 'user'] },
    { command: 'codex', args: ['plugin', 'marketplace', 'upgrade', 'loam'] },
    { command: 'codex', args: ['plugin', 'add', 'loam@loam'] },
  ]);
  assert.equal(result.claude.action, 'updated');
  assert.equal(result.codex.action, 'updated');
});

test('refresh updates ready marketplace plugins instead of skipping them', async () => {
  const calls = [];
  const ready = Object.fromEntries(['claude', 'codex'].map((id) => [id, {
    ...harnesses[id],
    marketplaceInstalled: true,
    marketplaceOwned: true,
    marketplaceReady: true,
  }]));
  const result = await installMarketplacePlugins({
    selected: ['claude', 'codex'],
    harnesses: ready,
    refresh: true,
    runner: async (request) => {
      calls.push({ command: request.command, args: request.args });
      return { code: 0, stdout: '', stderr: '' };
    },
  });

  assert.deepEqual(calls, [
    { command: 'claude', args: ['plugin', 'update', 'loam@loam', '--scope', 'user'] },
    { command: 'claude', args: ['plugin', 'enable', 'loam@loam', '--scope', 'user'] },
    { command: 'codex', args: ['plugin', 'marketplace', 'upgrade', 'loam'] },
    { command: 'codex', args: ['plugin', 'add', 'loam@loam'] },
  ]);
  assert.equal(result.claude.action, 'updated');
  assert.equal(result.codex.action, 'updated');
});

test('one marketplace failure does not stop the other selected harness', async () => {
  const calls = [];
  const result = await installMarketplacePlugins({
    selected: ['claude', 'codex'],
    harnesses,
    runner: async (request) => {
      calls.push(request.command);
      return request.command === 'claude' && request.args.includes('install')
        ? { code: 1, stdout: '', stderr: 'failed' }
        : { code: 0, stdout: '', stderr: '' };
    },
  });

  assert.deepEqual(calls, ['claude', 'claude', 'codex', 'codex']);
  assert.equal(result.claude.state, 'partial');
  assert.equal(result.codex.state, 'ready');
});

test('marketplace removal delegates to each owning harness', async () => {
  const calls = [];
  const result = await removeMarketplacePlugins({
    harnesses: {
      claude: { ...harnesses.claude, marketplaceInstalled: true },
      codex: { ...harnesses.codex, marketplaceInstalled: true },
    },
    runner: async (request) => {
      calls.push({ command: request.command, args: request.args });
      return { code: 0, stdout: '', stderr: '' };
    },
  });

  assert.deepEqual(calls, [
    { command: 'claude', args: ['plugin', 'uninstall', 'loam@loam', '--scope', 'user', '--yes'] },
    { command: 'codex', args: ['plugin', 'remove', 'loam@loam'] },
  ]);
  assert.equal(result.claude.state, 'removed');
  assert.equal(result.codex.state, 'removed');
});

test('marketplace removal delegates configured plugins even when cache bytes are missing', async () => {
  const calls = [];
  await removeMarketplacePlugins({
    harnesses: {
      claude: { ...harnesses.claude, marketplaceConfigured: true, marketplaceInstalled: false },
      codex: { ...harnesses.codex, marketplaceConfigured: true, marketplaceInstalled: false },
    },
    runner: async (request) => {
      calls.push(request.command);
      return { code: 0, stdout: '', stderr: '' };
    },
  });

  assert.deepEqual(calls, ['claude', 'codex']);
});

test('marketplace removal invokes Claude CLI for a user-scoped registry entry', async () => {
  const root = await claudeRoot();
  await mkdir(join(root, 'plugins'), { recursive: true });
  await writeFile(join(root, 'plugins', 'installed_plugins.json'), JSON.stringify({
    version: 2,
    plugins: { 'loam@loam': [{ scope: 'user', installPath: '/cache/loam' }] },
  }));
  const calls = [];

  const result = await removeMarketplacePlugins({
    harnesses: {
      claude: { ...harnesses.claude, root, marketplaceInstalled: true },
    },
    runner: async (request) => {
      calls.push({ command: request.command, args: request.args });
      return { code: 0, stdout: '', stderr: '' };
    },
  });

  assert.deepEqual(calls, [
    { command: 'claude', args: ['plugin', 'uninstall', 'loam@loam', '--scope', 'user', '--yes'] },
  ]);
  assert.equal(result.claude.state, 'removed');
});

test('marketplace removal skips Claude CLI and cleans an orphan cache without a registry', async () => {
  const root = await claudeRoot();
  const cache = join(root, 'plugins', 'cache', 'loam');
  await mkdir(cache, { recursive: true });
  await writeFile(join(cache, 'orphan.txt'), 'orphaned plugin cache');
  let called = false;

  const result = await removeMarketplacePlugins({
    harnesses: {
      claude: { ...harnesses.claude, root, marketplaceInstalled: true },
    },
    runner: async () => {
      called = true;
      return { code: 1, stdout: '', stderr: 'Claude CLI must not run for an orphan cache' };
    },
  });

  assert.equal(called, false);
  assert.equal(result.claude.state, 'removed');
  assert.equal(await exists(cache), false);
});

test('marketplace removal skips Claude CLI when the registry and cache are absent', async () => {
  const root = await claudeRoot();
  let called = false;

  const result = await removeMarketplacePlugins({
    harnesses: {
      claude: { ...harnesses.claude, root, marketplaceInstalled: true },
    },
    runner: async () => {
      called = true;
      return { code: 1, stdout: '', stderr: 'Claude CLI must not run without a registry entry' };
    },
  });

  assert.equal(called, false);
  assert.equal(result.claude.state, 'removed');
});

test('marketplace removal preserves a project-scoped Claude cache', async () => {
  const root = await claudeRoot();
  const cache = join(root, 'plugins', 'cache', 'loam', 'loam', '0.9.2');
  await mkdir(cache, { recursive: true });
  await writeFile(join(cache, 'project-plugin.txt'), 'project-owned plugin cache');
  await mkdir(join(root, 'plugins'), { recursive: true });
  await writeFile(join(root, 'plugins', 'installed_plugins.json'), JSON.stringify({
    version: 2,
    plugins: { 'loam@loam': [{ scope: 'project', installPath: '/cache/loam' }] },
  }));
  let called = false;

  const result = await removeMarketplacePlugins({
    harnesses: {
      claude: { ...harnesses.claude, root, marketplaceInstalled: true },
    },
    runner: async () => {
      called = true;
      return { code: 1, stdout: '', stderr: 'Claude CLI must not run for project-only state' };
    },
  });

  assert.equal(called, false);
  assert.equal(await exists(cache), true);
  assert.equal(result.claude.state, 'removed');
});
