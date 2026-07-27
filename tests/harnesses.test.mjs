import assert from 'node:assert/strict';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { mkdtemp } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
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

test('OpenCode background events queue work and normalize the all-session status map', async () => {
  const root = await mkdtemp(join(tmpdir(), 'loam-opencode-worker-'));
  let observed;
  let finished;
  let gateCalls = 0;
  const done = new Promise((resolve) => { finished = resolve; });
  const plugin = await createOpenCodeAdapter({
    client: {
      session: {
        create: async () => ({ id: 'child-1' }),
        promptAsync: async () => undefined,
        status: async (input) => {
          assert.deepEqual(input, { query: { directory: '/workspace' } });
          return { data: { 'child-1': { type: 'idle' } } };
        },
      },
    },
    ingestion: {
      gate: async () => { gateCalls += 1; return { action: 'spawn_worker', workspace: '/workspace' }; },
      resolveGlobalRoot: () => root,
      resolveSkillsRoot: () => root,
      runWorker: async ({ openCodeSession }) => {
        const child = await openCodeSession.createChild({ parentId: 'parent-1', title: 'test' });
        await openCodeSession.promptAsync({ sessionId: child.id, parts: [] });
        observed = await openCodeSession.status(child.id);
        finished();
      },
    },
  })({ directory: '/workspace' });

  await plugin.event({ event: { type: 'session.idle', sessionID: 'parent-1', id: 'event-1' } });
  await done;
  await plugin.event({ event: { type: 'session.idle', sessionID: 'child-1', id: 'event-2' } });
  assert.deepEqual(observed, { type: 'idle' });
  assert.equal(gateCalls, 1);
});

test('Claude and Cursor adapters use payload workspace roots and emit documented envelopes', async () => {
  const getContext = async ({ workspace }) => `context for ${workspace}`;
  const claude = createClaudeAdapter({ getContext });
  const cursor = createCursorAdapter({ getContext });
  const workspace = resolve('/payload/workspace');
  const payload = { cwd: workspace, workspace: { root: resolve('/nested/root') } };

  assert.equal(claudeWorkspace(payload), workspace);
  assert.equal(cursorWorkspace(payload), workspace);
  const claudeOutput = await claude(payload);
  const cursorOutput = await cursor(payload);
  assert.deepEqual(claudeOutput, {
    hookSpecificOutput: {
      hookEventName: 'SessionStart',
      additionalContext: `context for ${workspace}`,
    },
  });
  assert.deepEqual(cursorOutput, { additional_context: `context for ${workspace}` });
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
  assert.equal(claudeHooks.filter((entry) => entry.command === 'node' && entry.args?.[0] === result.claude.path).length, 1);
  assert.equal(cursorHooks.filter((entry) => entry.command === 'node' && entry.args?.[0] === result.cursor.path).length, 1);
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

test('marketplace ownership removes setup SessionStart hooks but keeps background Stop hooks', async () => {
  const home = await mkdtemp(join(tmpdir(), 'loam-marketplace-owned-'));
  const globalRoot = join(home, '.agents', 'loam');
  const oldRoot = join(globalRoot, 'plugins', 'old');
  const oldClaude = { type: 'command', command: `node ${JSON.stringify(join(oldRoot, 'claude-session-start.mjs'))}` };
  const oldCodex = { type: 'command', command: `node ${JSON.stringify(join(oldRoot, 'codex-session-start.mjs'))}` };
  const unrelated = { type: 'command', command: 'node "/usr/local/bin/unrelated.mjs"' };
  await mkdir(join(home, '.claude'), { recursive: true });
  await mkdir(join(home, '.codex'), { recursive: true });
  await writeFile(join(home, '.claude', 'settings.json'), JSON.stringify({
    enabledPlugins: { 'loam@loam': true },
    hooks: { SessionStart: [{ hooks: [unrelated, oldClaude] }] },
  }));
  await writeFile(join(home, '.codex', 'config.toml'), '[plugins."loam@loam"]\nenabled = true\n');
  const claudeCache = join(home, '.claude', 'plugins', 'cache', 'loam', 'loam', '0.8.6');
  await mkdir(claudeCache, { recursive: true });
  await writeFile(join(home, '.claude', 'plugins', 'installed_plugins.json'), JSON.stringify({
    version: 2,
    plugins: { 'loam@loam': [{ scope: 'user', installPath: claudeCache, version: '0.8.6' }] },
  }));
  await mkdir(join(home, '.codex', 'plugins', 'cache', 'loam', 'loam', '0.8.6'), { recursive: true });
  await writeFile(join(home, '.codex', 'hooks.json'), JSON.stringify({
    hooks: { SessionStart: [{ hooks: [unrelated, oldCodex] }] },
  }));

  const detected = await detectHarnesses({ home });
  assert.equal(detected.claude.marketplaceOwned, true);
  assert.equal(detected.codex.marketplaceOwned, true);
  const owned = await installHarnesses({ home, globalRoot, pluginVersion: '0.8.6', detected });
  const claude = JSON.parse(await readFile(join(home, '.claude', 'settings.json'), 'utf8'));
  const codex = JSON.parse(await readFile(join(home, '.codex', 'hooks.json'), 'utf8'));
  assert.equal(owned.claude.owner, 'marketplace');
  assert.equal(owned.codex.owner, 'marketplace');
  assert.deepEqual(claude.hooks.SessionStart[0].hooks, [unrelated]);
  assert.deepEqual(codex.hooks.SessionStart[0].hooks, [unrelated]);
  const claudeStop = claude.hooks.Stop.flatMap((entry) => entry.hooks || []);
  const codexStop = codex.hooks.Stop.flatMap((entry) => entry.hooks || []);
  assert.equal(claudeStop.filter((entry) => entry.command === 'node' && entry.args?.[0] === owned.claude.stopPath).length, 1);
  assert.equal(codexStop.filter((entry) => entry.command === `node ${JSON.stringify(owned.codex.stopPath)}`).length, 1);

  const fallbackHome = await mkdtemp(join(tmpdir(), 'loam-codex-fallback-'));
  await mkdir(join(fallbackHome, '.codex'), { recursive: true });
  const fallbackDetected = await detectHarnesses({ home: fallbackHome });
  const fallback = await installHarnesses({
    home: fallbackHome,
    globalRoot: join(fallbackHome, '.agents', 'loam'),
    pluginVersion: '0.8.6',
    detected: fallbackDetected,
  });
  const hooks = JSON.parse(await readFile(join(fallbackHome, '.codex', 'hooks.json'), 'utf8'));
  const commands = hooks.hooks.SessionStart.flatMap((entry) => entry.hooks || []);
  assert.equal(fallback.codex.owner, 'setup');
  assert.equal(commands.filter((entry) => entry.command === `node ${JSON.stringify(fallback.codex.sessionPath)}`).length, 1);
  assert.equal(hooks.hooks.Stop.flatMap((entry) => entry.hooks || [])
    .filter((entry) => entry.command === `node ${JSON.stringify(fallback.codex.stopPath)}`).length, 1);
});

test('enabled marketplace settings without installed plugin bytes keep setup ownership', async () => {
  const home = await mkdtemp(join(tmpdir(), 'loam-marketplace-missing-cache-'));
  await mkdir(join(home, '.claude'), { recursive: true });
  await mkdir(join(home, '.codex'), { recursive: true });
  await writeFile(join(home, '.claude', 'settings.json'), JSON.stringify({ enabledPlugins: { 'loam@loam': true } }));
  await writeFile(join(home, '.codex', 'config.toml'), '[plugins."loam@loam"]\nenabled = true\n');

  const detected = await detectHarnesses({ home });

  assert.equal(detected.claude.marketplaceOwned, false);
  assert.equal(detected.codex.marketplaceOwned, false);
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

test('Codex preserves hook groups, accepts Windows 8.3 paths, and rejects expansion paths', async () => {
  const root = await mkdtemp(join(tmpdir(), 'loam-codex-hooks-'));
  const home = join(root, 'RUNNER~1');
  await mkdir(join(home, '.codex'), { recursive: true });
  await writeFile(join(home, '.codex', 'config.toml'), 'model = "keep"\n');
  const unrelated = { type: 'command', command: 'node "/opt/other-stop.mjs"' };
  await writeFile(join(home, '.codex', 'hooks.json'), JSON.stringify({ hooks: { Stop: [{ matcher: "", hooks: [unrelated] }] } }));
  const result = await installHarnesses({
    home,
    globalRoot: join(home, '.agents', 'loam'),
    pluginVersion: '0.8.3',
    detected: { opencode: { id: 'opencode', state: 'absent' }, claude: { id: 'claude', state: 'absent' }, cursor: { id: 'cursor', state: 'absent' }, codex: { id: 'codex', state: 'detected', root: join(home, '.codex') } },
  });
  assert.equal(result.codex.state, 'ready', JSON.stringify(result.codex));
  const config = JSON.parse(await readFile(join(home, '.codex', 'hooks.json'), 'utf8'));
  assert.equal(config.hooks.Stop.length, 2);
  assert.deepEqual(config.hooks.Stop[0].hooks, [unrelated]);
  assert.equal(config.hooks.Stop[1].hooks.length, 1);
  assert.equal(config.hooks.Stop[1].hooks[0].command, 'node ' + JSON.stringify(result.codex.path));

  for (const character of ['%', '!']) {
    const blockedHome = join(root, `RUNNER${character}1`);
    await mkdir(join(blockedHome, '.codex'), { recursive: true });
    const blocked = await installHarnesses({
      home: blockedHome,
      globalRoot: join(blockedHome, '.agents', 'loam'),
      pluginVersion: '0.8.3',
      detected: { opencode: { id: 'opencode', state: 'absent' }, claude: { id: 'claude', state: 'absent' }, cursor: { id: 'cursor', state: 'absent' }, codex: { id: 'codex', state: 'detected', root: join(blockedHome, '.codex') } },
    });
    assert.equal(blocked.codex.state, 'partial');
    assert.equal(blocked.codex.category, 'install_failed');
    assert.match(blocked.codex.detail, /Codex adapter path is unsafe/u);
  }
});
