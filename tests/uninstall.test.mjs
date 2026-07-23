import assert from 'node:assert/strict';
import { mkdir, mkdtemp, readFile, rm, stat, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';

import { installHarnesses, detectHarnesses } from '../setup/harnesses.mjs';
import { uninstall } from '../setup/uninstall.mjs';

async function readyFixture() {
  const home = await mkdtemp(join(tmpdir(), 'loam-uninstall-'));
  const globalRoot = join(home, '.agents', 'loam');
  await mkdir(join(home, '.config', 'opencode'), { recursive: true });
  await mkdir(join(home, '.claude'), { recursive: true });
  await mkdir(join(home, '.cursor'), { recursive: true });
  await mkdir(join(home, '.agents', 'skills', 'loam-using'), { recursive: true });
  await writeFile(join(home, '.agents', 'skills', 'loam-using', 'SKILL.md'), '# using\n');
  const detected = await detectHarnesses({ home });
  const installed = await installHarnesses({ home, globalRoot, pluginVersion: '0.8.3', detected });
  const install = {
    schema_version: 1,
    plugin_version: '0.8.3',
    runtime_version: '0.9.1',
    target: 'x86_64-unknown-linux-musl',
    runtime_path: join(globalRoot, 'bin', '0.9.1', 'x86_64-unknown-linux-musl', 'loam'),
    runtime_sha256: 'a'.repeat(64),
    adapter_root: installed.versionRoot,
    integration_path: join(globalRoot, 'integration', 'loam.mjs'),
    skills_scope: 'global',
    skills_source: 'scchearn/loam',
    configured_harnesses: ['opencode', 'claude', 'cursor'],
  };
  await mkdir(join(globalRoot, 'integration'), { recursive: true });
  await writeFile(install.integration_path, 'export async function runIntegration() { return 0; }\n');
  await mkdir(join(globalRoot, 'bin', '0.9.1', 'x86_64-unknown-linux-musl'), { recursive: true });
  await writeFile(install.runtime_path, 'fake runtime\n');
  await writeFile(join(globalRoot, 'install.json'), `${JSON.stringify(install, null, 2)}\n`);
  return { home, globalRoot, install, installed };
}

async function exists(path) {
  try {
    await stat(path);
    return true;
  } catch {
    return false;
  }
}

test('uninstall removes the global root, adapter, and Loam-owned hooks; preserves unrelated config and skills', async () => {
  const { home, globalRoot, install, installed } = await readyFixture();
  const unrelatedClaude = { type: 'command', command: 'node "/usr/local/bin/other-hook.mjs"' };
  const unrelatedCursor = { type: 'command', command: 'node "/opt/other-tools/cursor-hook.mjs"' };
  const claudePath = join(home, '.claude', 'settings.json');
  const cursorPath = join(home, '.cursor', 'hooks.json');
  const claude = JSON.parse(await readFile(claudePath, 'utf8'));
  const cursor = JSON.parse(await readFile(cursorPath, 'utf8'));
  claude.hooks.SessionStart[0].hooks.unshift(unrelatedClaude);
  cursor.hooks.sessionStart.unshift(unrelatedCursor);
  await writeFile(claudePath, JSON.stringify(claude));
  await writeFile(cursorPath, JSON.stringify(cursor));

  const code = await uninstall({ home, globalRoot, yes: true, output: { write: () => {} } });

  assert.equal(code, 0);
  assert.equal(await exists(globalRoot), false, 'global root removed');
  assert.equal(await exists(join(home, '.config', 'opencode', 'plugins', 'loam.mjs')), false, 'opencode adapter removed');
  assert.equal(await exists(join(home, '.agents', 'skills', 'loam-using', 'SKILL.md')), true, 'global skills preserved');

  const claudeAfter = JSON.parse(await readFile(claudePath, 'utf8'));
  const cursorAfter = JSON.parse(await readFile(cursorPath, 'utf8'));
  const claudeHooks = claudeAfter.hooks.SessionStart.flatMap((e) => e.hooks || []);
  const cursorHooks = cursorAfter.hooks.sessionStart;
  assert.deepEqual(claudeHooks[0], unrelatedClaude, 'unrelated claude hook preserved');
  assert.deepEqual(cursorHooks[0], unrelatedCursor, 'unrelated cursor hook preserved');
  assert.equal(claudeHooks.filter((h) => h.command === `node ${JSON.stringify(installed.claude.path)}`).length, 0, 'loam claude hook removed');
  assert.equal(cursorHooks.filter((h) => h.command === `node ${JSON.stringify(installed.cursor.path)}`).length, 0, 'loam cursor hook removed');
});

test('uninstall removes backup files created by setup', async () => {
  const { home, globalRoot } = await readyFixture();
  const claudeBackup = join(home, '.claude', 'settings.json.backup-deadbeef');
  const cursorBackup = join(home, '.cursor', 'hooks.json.backup-cafebabe');
  await writeFile(claudeBackup, '{"old":true}');
  await writeFile(cursorBackup, '{"old":true}');

  const code = await uninstall({ home, globalRoot, yes: true, output: { write: () => {} } });

  assert.equal(code, 0);
  assert.equal(await exists(claudeBackup), false, 'claude backup removed');
  assert.equal(await exists(cursorBackup), false, 'cursor backup removed');
});

test('uninstall without install.json reports nothing to remove', async () => {
  const home = await mkdtemp(join(tmpdir(), 'loam-uninstall-empty-'));
  const globalRoot = join(home, '.agents', 'loam');
  let message = '';
  const code = await uninstall({ home, globalRoot, yes: true, output: { write: (s) => { message += s; } } });

  assert.equal(code, 0);
  assert.match(message, /Nothing to uninstall/);
});

test('uninstall cancelled without --yes returns 130', async () => {
  const { home, globalRoot } = await readyFixture();
  const code = await uninstall({
    home,
    globalRoot,
    yes: false,
    confirm: async () => false,
    output: { write: () => {} },
  });

  assert.equal(code, 130);
  assert.equal(await exists(join(globalRoot, 'install.json')), true, 'global root preserved on cancel');
});

test('uninstall does not touch global skills', async () => {
  const { home, globalRoot } = await readyFixture();
  const skillsPath = join(home, '.agents', 'skills', 'loam-using', 'SKILL.md');
  await uninstall({ home, globalRoot, yes: true, output: { write: () => {} } });
  assert.equal(await exists(skillsPath), true, 'skills untouched');
  assert.equal(await exists(join(home, '.agents', 'skills')), true, 'skills root untouched');
});

test('uninstall deletes a fresh harness config created by setup, not leaving an empty husk', async () => {
  const home = await mkdtemp(join(tmpdir(), 'loam-uninstall-fresh-'));
  const globalRoot = join(home, '.agents', 'loam');
  const adapterRoot = join(globalRoot, 'plugins', '0.8.3-abc');
  await mkdir(join(home, '.cursor'), { recursive: true });
  // No pre-existing hooks.json — setup created it fresh, no backup
  await writeFile(join(home, '.cursor', 'hooks.json'), JSON.stringify({
    hooks: { sessionStart: [{ type: 'command', command: `node ${JSON.stringify(join(adapterRoot, 'cursor-session-start.mjs'))}` }] },
  }));
  await mkdir(join(globalRoot, 'integration'), { recursive: true });
  await mkdir(join(globalRoot, 'bin', '0.9.1', 'x86_64-unknown-linux-musl'), { recursive: true });
  await mkdir(adapterRoot, { recursive: true });
  await writeFile(join(globalRoot, 'install.json'), `${JSON.stringify({
    schema_version: 1, plugin_version: '0.8.3', runtime_version: '0.9.1',
    target: 'x86_64-unknown-linux-musl', runtime_path: join(globalRoot, 'bin/0.9.1/x86_64-unknown-linux-musl/loam'),
    runtime_sha256: 'a'.repeat(64), adapter_root: adapterRoot,
    integration_path: join(globalRoot, 'integration/loam.mjs'), skills_scope: 'global',
    skills_source: 'scchearn/loam', configured_harnesses: ['cursor'],
  }, null, 2)}\n`);

  const code = await uninstall({ home, globalRoot, yes: true, output: { write: () => {} } });

  assert.equal(code, 0);
  assert.equal(await exists(join(home, '.cursor', 'hooks.json')), false, 'fresh config deleted, not left as empty husk');
});

test('uninstall preserves a pre-existing harness config that setup modified', async () => {
  const home = await mkdtemp(join(tmpdir(), 'loam-uninstall-modified-'));
  const globalRoot = join(home, '.agents', 'loam');
  const adapterRoot = join(globalRoot, 'plugins', '0.8.3-abc');
  await mkdir(join(home, '.cursor'), { recursive: true });
  // Pre-existing config with unrelated content + a backup from setup
  await writeFile(join(home, '.cursor', 'hooks.json'), JSON.stringify({
    hooks: { sessionStart: [{ type: 'command', command: 'node "/opt/other/hook.mjs"' }] },
  }));
  await writeFile(join(home, '.cursor', 'hooks.json.backup-deadbeef'), JSON.stringify({
    hooks: { sessionStart: [{ type: 'command', command: 'node "/opt/other/hook.mjs"' }] },
  }));
  await mkdir(join(globalRoot, 'integration'), { recursive: true });
  await mkdir(join(globalRoot, 'bin', '0.9.1', 'x86_64-unknown-linux-musl'), { recursive: true });
  await mkdir(adapterRoot, { recursive: true });
  await writeFile(join(globalRoot, 'install.json'), `${JSON.stringify({
    schema_version: 1, plugin_version: '0.8.3', runtime_version: '0.9.1',
    target: 'x86_64-unknown-linux-musl', runtime_path: join(globalRoot, 'bin/0.9.1/x86_64-unknown-linux-musl/loam'),
    runtime_sha256: 'a'.repeat(64), adapter_root: adapterRoot,
    integration_path: join(globalRoot, 'integration/loam.mjs'), skills_scope: 'global',
    skills_source: 'scchearn/loam', configured_harnesses: ['cursor'],
  }, null, 2)}\n`);

  const code = await uninstall({ home, globalRoot, yes: true, output: { write: () => {} } });
  const cursor = JSON.parse(await readFile(join(home, '.cursor', 'hooks.json'), 'utf8'));

  assert.equal(code, 0);
  assert.equal(await exists(join(home, '.cursor', 'hooks.json')), true, 'pre-existing config preserved');
  assert.deepEqual(cursor.hooks.sessionStart, [{ type: 'command', command: 'node "/opt/other/hook.mjs"' }], 'unrelated hook preserved');
  assert.equal(await exists(join(home, '.cursor', 'hooks.json.backup-deadbeef')), false, 'backup removed');
});