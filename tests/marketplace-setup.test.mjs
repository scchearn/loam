import assert from 'node:assert/strict';
import { test } from 'node:test';

import { installMarketplacePlugins, removeMarketplacePlugins } from '../setup/marketplace.mjs';
import { selectMarketplaceHarnesses } from '../setup/wizard.mjs';

const harnesses = {
  opencode: { id: 'opencode', state: 'detected' },
  claude: { id: 'claude', state: 'detected', marketplaceOwned: false },
  codex: { id: 'codex', state: 'detected', marketplaceOwned: false },
  cursor: { id: 'cursor', state: 'detected' },
};

test('--yes selects every detected marketplace-capable harness', async () => {
  assert.deepEqual(await selectMarketplaceHarnesses({ yes: true, harnesses }), ['claude', 'codex']);
});

test('interactive selection offers only detected Claude and Codex with both preselected', async () => {
  let prompt;
  const selected = await selectMarketplaceHarnesses({
    harnesses: { ...harnesses, codex: { id: 'codex', state: 'absent' } },
    select: async (input) => {
      prompt = input;
      return ['claude'];
    },
  });

  assert.deepEqual(selected, ['claude']);
  assert.deepEqual(prompt.options.map(({ value }) => value), ['claude']);
  assert.deepEqual(prompt.initialValues, ['claude']);
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
