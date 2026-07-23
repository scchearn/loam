import assert from 'node:assert/strict';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { mkdtemp } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';

import { createClaudeAdapter, workspaceFromPayload as claudeWorkspace } from '../adapters/claude-session-start.mjs';
import { createCursorAdapter, workspaceFromPayload as cursorWorkspace } from '../adapters/cursor-session-start.mjs';
import { createOpenCodeAdapter } from '../adapters/opencode.mjs';
import { dedupe, mergeJsonConfig } from '../setup/config.mjs';
import { detectHarnesses, installHarnesses } from '../setup/harnesses.mjs';

test('OpenCode injects the shared context once and ignores unrelated dedup markers', async () => {
  const calls = [];
  const plugin = await createOpenCodeAdapter({
    getContext: async ({ workspace }) => {
      calls.push(workspace);
      return `<LOAM_IMPORTANT>\nYou have loam.\n${workspace}\n</LOAM_IMPORTANT>`;
    },
  })({ directory: '/workspace' });
  const output = {
    messages: [{ info: { role: 'user' }, parts: [{ type: 'text', text: 'superpowers context' }] }],
  };

  await plugin['experimental.chat.messages.transform']({}, output);
  await plugin['experimental.chat.messages.transform']({}, output);
  assert.equal(output.messages[0].parts.filter((part) => part.text?.includes('You have loam')).length, 1);
  assert.deepEqual(calls, ['/workspace']);
});

test('Claude and Cursor adapters use payload workspace roots and emit documented envelopes', async () => {
  const getContext = async ({ workspace }) => `context for ${workspace}`;
  const claude = createClaudeAdapter({ getContext });
  const cursor = createCursorAdapter({ getContext });
  const payload = { cwd: '/payload/workspace', workspace: { root: '/nested/root' } };

  assert.equal(claudeWorkspace(payload), '/payload/workspace');
  assert.equal(cursorWorkspace(payload), '/payload/workspace');
  const claudeOutput = await claude(payload);
  const cursorOutput = await cursor(payload);
  assert.deepEqual(claudeOutput, {
    hookSpecificOutput: {
      hookEventName: 'SessionStart',
      additionalContext: 'context for /payload/workspace',
    },
  });
  assert.deepEqual(cursorOutput, { additional_context: 'context for /payload/workspace' });
});

test('config merge preserves unrelated JSON, creates a backup, and deduplicates Loam entries', async () => {
  const home = await mkdtemp(join(tmpdir(), 'loam-config-'));
  const filePath = join(home, 'settings.json');
  await writeFile(filePath, JSON.stringify({ unrelated: { keep: true }, hooks: { SessionStart: [] } }));
  const result = await mergeJsonConfig({
    filePath,
    update: (config) => ({
      ...config,
      hooks: { ...config.hooks, SessionStart: [{ command: 'loam-hook' }, { command: 'loam-hook' }] },
    }),
  });

  assert.ok(result.backupPath);
  assert.deepEqual(JSON.parse(await readFile(filePath, 'utf8')).unrelated, { keep: true });
  assert.equal(JSON.parse(await readFile(filePath, 'utf8')).hooks.SessionStart.length, 1);
  assert.deepEqual(dedupe(['other', 'loam', 'loam']), ['other', 'loam']);
});

test('malformed and policy-owned config is rejected without mutation', async () => {
  const home = await mkdtemp(join(tmpdir(), 'loam-config-policy-'));
  const malformed = join(home, 'malformed.json');
  await writeFile(malformed, '{not-json');
  await assert.rejects(() => mergeJsonConfig({ filePath: malformed, update: () => ({}) }), /malformed JSON/);
  assert.equal(await readFile(malformed, 'utf8'), '{not-json');

  const managed = join(home, 'managed.json');
  await writeFile(managed, JSON.stringify({ managed: true, keep: 'yes' }));
  await assert.rejects(() => mergeJsonConfig({ filePath: managed, update: () => ({}) }), /policy-owned/);
  assert.deepEqual(JSON.parse(await readFile(managed, 'utf8')), { managed: true, keep: 'yes' });
});

test('harness installation preserves unrelated hook commands containing loam', async () => {
  const home = await mkdtemp(join(tmpdir(), 'loam-hook-ownership-'));
  const globalRoot = join(home, '.agents', 'loam');
  await mkdir(join(home, '.config', 'opencode'), { recursive: true });
  await mkdir(join(home, '.claude'), { recursive: true });
  await mkdir(join(home, '.cursor'), { recursive: true });
  const unrelatedClaude = { type: 'command', command: 'node "/usr/local/bin/loam-unrelated-hook.mjs"' };
  const unrelatedCursor = { type: 'command', command: 'node "/opt/loam-tools/cursor-hook.mjs"' };
  await writeFile(
    join(home, '.claude', 'settings.json'),
    JSON.stringify({ hooks: { SessionStart: [{ matcher: 'startup', hooks: [unrelatedClaude] }] } }),
  );
  await writeFile(join(home, '.cursor', 'hooks.json'), JSON.stringify({ hooks: { sessionStart: [unrelatedCursor] } }));

  const result = await installHarnesses({
    home,
    globalRoot,
    pluginVersion: '0.8.3',
    detected: await detectHarnesses({ home }),
  });
  const claude = JSON.parse(await readFile(join(home, '.claude', 'settings.json'), 'utf8'));
  const cursor = JSON.parse(await readFile(join(home, '.cursor', 'hooks.json'), 'utf8'));
  const claudeHooks = claude.hooks.SessionStart.flatMap((entry) => entry.hooks || []);
  const cursorHooks = cursor.hooks.sessionStart;

  assert.deepEqual(claudeHooks[0], unrelatedClaude);
  assert.deepEqual(cursorHooks[0], unrelatedCursor);
  assert.equal(claudeHooks.filter((entry) => entry.command === `node ${JSON.stringify(result.claude.path)}`).length, 1);
  assert.equal(cursorHooks.filter((entry) => entry.command === `node ${JSON.stringify(result.cursor.path)}`).length, 1);
  await result.rollback();
});

test('harness detection and installation use only user HOME paths and are idempotent', async () => {
  const home = await mkdtemp(join(tmpdir(), 'loam-home-'));
  const globalRoot = join(home, '.agents', 'loam');
  await mkdir(join(home, '.config', 'opencode'), { recursive: true });
  await mkdir(join(home, '.claude'), { recursive: true });
  await mkdir(join(home, '.cursor'), { recursive: true });
  const detected = await detectHarnesses({ home });
  assert.equal(detected.opencode.state, 'detected');
  assert.equal(detected.claude.state, 'detected');
  assert.equal(detected.cursor.state, 'detected');

  const first = await installHarnesses({ home, globalRoot, pluginVersion: '0.8.3', detected });
  const second = await installHarnesses({ home, globalRoot, pluginVersion: '0.8.3', detected });
  assert.deepEqual(first.opencode.state, 'ready');
  assert.deepEqual(first.claude.state, 'ready');
  assert.deepEqual(first.cursor.state, 'ready');
  assert.deepEqual(second.claude.state, 'ready');
  assert.deepEqual(second.cursor.state, 'ready');

  const claudeSettings = JSON.parse(await readFile(join(home, '.claude', 'settings.json'), 'utf8'));
  const cursorHooks = JSON.parse(await readFile(join(home, '.cursor', 'hooks.json'), 'utf8'));
  assert.equal(claudeSettings.unrelated, undefined);
  assert.equal(claudeSettings.hooks.SessionStart.filter((hook) => JSON.stringify(hook).includes('loam')).length, 1);
  assert.equal(cursorHooks.hooks.sessionStart.filter((hook) => JSON.stringify(hook).includes('loam')).length, 1);
});

test('managed harness policy becomes partial without changing its settings', async () => {
  const home = await mkdtemp(join(tmpdir(), 'loam-home-policy-'));
  await mkdir(join(home, '.claude'), { recursive: true });
  const settingsPath = join(home, '.claude', 'settings.json');
  await writeFile(settingsPath, JSON.stringify({ managed: true, unrelated: 'keep' }));
  const detected = await detectHarnesses({ home });
  const result = await installHarnesses({
    home,
    globalRoot: join(home, '.agents', 'loam'),
    pluginVersion: '0.8.3',
    detected,
  });

  assert.equal(result.claude.state, 'partial');
  assert.equal(result.claude.category, 'policy_owned');
  assert.deepEqual(JSON.parse(await readFile(settingsPath, 'utf8')), { managed: true, unrelated: 'keep' });
});

test('absent harnesses remain absent and do not receive project-local hook files', async () => {
  const home = await mkdtemp(join(tmpdir(), 'loam-home-absent-'));
  const detected = await detectHarnesses({ home });
  const result = await installHarnesses({
    home,
    globalRoot: join(home, '.agents', 'loam'),
    pluginVersion: '0.8.3',
    detected,
  });

  assert.equal(result.opencode.state, 'absent');
  assert.equal(result.claude.state, 'absent');
  assert.equal(result.cursor.state, 'absent');
});
