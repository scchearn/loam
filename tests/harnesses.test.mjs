import assert from 'node:assert/strict';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { mkdtemp } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { test } from 'node:test';

import { createOpenCodeAdapter } from '../adapters/opencode.mjs';
import { main as runIngestWorker } from '../adapters/ingest-worker.mjs';
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
  const hookRuns = [];
  const workerRuns = [];
  const order = [];
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
      gate: async () => { order.push('gate'); gateCalls += 1; return { action: 'spawn_worker', workspace: '/workspace' }; },
      resolveGlobalRoot: () => root,
      resolveSkillsRoot: () => root,
      runWorker: async ({ openCodeSession, hookRun }) => {
        assert.deepEqual(hookRun, { id: 9 });
        const child = await openCodeSession.createChild({ parentId: 'parent-1', title: 'test' });
        await openCodeSession.promptAsync({ sessionId: child.id, parts: [] });
        observed = await openCodeSession.status(child.id);
        return { reason: 'ok' };
      },
    },
    hookRuns: {
      resolveGlobalRoot: () => root,
      beginHookRun: async (input) => {
        order.push('begin');
        hookRuns.push(['begin', input]);
        return { id: 9 };
      },
      finishHookRun: async (input) => {
        order.push('finish');
        hookRuns.push(['finish', input]);
      },
      startHookWorker: async (input) => workerRuns.push(['start', input]),
      finishHookWorker: async (input) => {
        workerRuns.push(['finish', input]);
        finished();
      },
    },
  })({ directory: '/workspace' });

  await plugin.event({ event: { type: 'session.idle', sessionID: 'parent-1', id: 'event-1' } });
  await done;
  await plugin.event({ event: { type: 'session.idle', sessionID: 'child-1', id: 'event-2' } });
  assert.deepEqual(observed, { type: 'idle' });
  assert.equal(gateCalls, 1);
  assert.deepEqual(order.slice(0, 3), ['begin', 'gate', 'finish']);
  assert.deepEqual(hookRuns, [
    ['begin', {
      globalRoot: root,
      harness: 'opencode',
      hook: 'session_idle',
      workspace: '/workspace',
      sessionId: 'parent-1',
    }],
    ['finish', { run: { id: 9 }, status: 'succeeded', action: 'spawn_worker' }],
  ]);
  assert.deepEqual(workerRuns, [
    ['start', { run: { id: 9 } }],
    ['finish', { run: { id: 9 }, reason: 'ok' }],
  ]);
});

test('detached workers report their result without changing worker failures', async () => {
  const calls = [];
  const options = {
    harness: 'codex',
    workspace: '/workspace',
    hookRunId: 12,
    globalRoot: '/global',
    skillsRoot: '/skills',
    env: {},
    startHookWorker: async (input) => { calls.push(['start', input]); throw new Error('locked'); },
    finishHookWorker: async (input) => calls.push(['finish', input]),
  };
  const result = await runIngestWorker({
    ...options,
    runWorker: async () => ({ reason: 'nothing_to_do', detail: 'wiki_missing' }),
  });
  assert.deepEqual(result, { reason: 'nothing_to_do', detail: 'wiki_missing' });
  assert.deepEqual(calls, [
    ['start', { run: { id: 12, globalRoot: '/global', workspace: '/workspace' } }],
    ['finish', {
      run: { id: 12, globalRoot: '/global', workspace: '/workspace' },
      reason: 'nothing_to_do',
      detail: 'wiki_missing',
    }],
  ]);

  calls.length = 0;
  await assert.rejects(() => runIngestWorker({
    ...options,
    startHookWorker: async () => undefined,
    runWorker: async () => { throw new Error('worker crashed'); },
  }), /worker crashed/);
  assert.equal(calls[0][0], 'finish');
  assert.equal(calls[0][1].reason, 'unavailable');
  assert.match(calls[0][1].detail, /worker crashed/);
});

test('OpenCode hook-run logging remains fail-open', async () => {
  let calls = 0;
  const plugin = await createOpenCodeAdapter({
    hookRuns: {
      resolveGlobalRoot: () => '/global',
      beginHookRun: async () => { calls += 1; throw new Error('locked'); },
      finishHookRun: async () => { throw new Error('must not finish'); },
    },
  })({ directory: '/workspace' });

  await assert.doesNotReject(() => plugin.event({ event: { type: 'session.idle', sessionID: 'parent' } }));
  assert.equal(calls, 1);
});

test('OpenCode finishes a catchable gate failure without rejecting the hook', async () => {
  const finished = [];
  const run = { id: 10 };
  const plugin = await createOpenCodeAdapter({
    ingestion: {
      gate: async () => { throw new Error('gate failed'); },
      resolveGlobalRoot: () => '/global',
      resolveSkillsRoot: () => '/skills',
      runWorker: async () => undefined,
    },
    hookRuns: {
      resolveGlobalRoot: () => '/global',
      beginHookRun: async () => run,
      finishHookRun: async (input) => finished.push(input),
    },
  })({ directory: '/workspace' });

  await assert.doesNotReject(() => plugin.event({
    event: { type: 'session.idle', sessionID: 'parent-session' },
  }));
  assert.equal(finished.length, 1);
  assert.equal(finished[0].run, run);
  assert.equal(finished[0].status, 'failed');
  assert.match(finished[0].detail, /gate failed/);
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
  const runtimePath = join(globalRoot, 'bin', '0.9.1', 'target', 'loam');
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
    runtimePath,
    detected: await detectHarnesses({ home }),
  });
  const claude = JSON.parse(await readFile(join(home, '.claude', 'settings.json'), 'utf8'));
  const cursor = JSON.parse(await readFile(join(home, '.cursor', 'hooks.json'), 'utf8'));
  const claudeHooks = claude.hooks.SessionStart.flatMap((entry) => entry.hooks || []);
  const cursorHooks = cursor.hooks.sessionStart;

  assert.deepEqual(claudeHooks[0], unrelatedClaude);
  assert.deepEqual(cursorHooks[0], unrelatedCursor);
  assert.equal(claudeHooks.filter((entry) => entry.command === runtimePath).length, 0, 'Claude is registered in its plugin, not in user settings');
  assert.deepEqual(
    cursorHooks.filter((entry) => entry.command === runtimePath).map((entry) => entry.args),
    [['hook', 'cursor', '--event', 'sessionStart']],
  );
  await result.rollback();
});

test('harness detection and installation use only user HOME paths and are idempotent', async () => {
  const home = await mkdtemp(join(tmpdir(), 'loam-home-'));
  const globalRoot = join(home, '.agents', 'loam');
  const runtimePath = join(globalRoot, 'bin', '0.9.1', 'target', 'loam');
  await mkdir(join(home, '.config', 'opencode'), { recursive: true });
  await mkdir(join(home, '.claude'), { recursive: true });
  await mkdir(join(home, '.cursor'), { recursive: true });
  const legacyOpenCodePath = join(home, '.config', 'opencode', 'plugins', 'loam.mjs');
  await mkdir(join(home, '.config', 'opencode', 'plugins'), { recursive: true });
  await writeFile(legacyOpenCodePath, 'legacy adapter');
  const detected = await detectHarnesses({ home });
  assert.equal(detected.opencode.state, 'detected');
  assert.equal(detected.claude.state, 'detected');
  assert.equal(detected.cursor.state, 'detected');

  const first = await installHarnesses({ home, globalRoot, pluginVersion: '0.8.3', runtimePath, detected });
  const second = await installHarnesses({ home, globalRoot, pluginVersion: '0.8.3', runtimePath, detected });
  assert.deepEqual(first.opencode.state, 'ready');
  assert.deepEqual(first.claude.state, 'skipped');
  assert.deepEqual(first.cursor.state, 'ready');
  assert.deepEqual(second.claude.state, 'skipped');
  assert.deepEqual(second.cursor.state, 'ready');
  assert.equal(first.opencode.path, join(home, '.config', 'opencode', 'plugins', 'loam.js'));
  await assert.rejects(() => readFile(legacyOpenCodePath), { code: 'ENOENT' });

  const cursorHooks = JSON.parse(await readFile(join(home, '.cursor', 'hooks.json'), 'utf8'));
  await assert.rejects(() => readFile(join(home, '.claude', 'settings.json')), { code: 'ENOENT' });
  assert.equal(cursorHooks.hooks.sessionStart.filter((hook) => JSON.stringify(hook).includes('loam')).length, 1);
});

test('marketplace plugins own all Claude and Codex lifecycle hooks', async () => {
  const home = await mkdtemp(join(tmpdir(), 'loam-marketplace-owned-'));
  const globalRoot = join(home, '.agents', 'loam');
  const runtimePath = join(globalRoot, 'bin', '0.9.1', 'target', 'loam');
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
  await mkdir(join(claudeCache, 'hooks'), { recursive: true });
  await writeFile(join(claudeCache, 'hooks', 'hooks.json'), JSON.stringify({ hooks: { Stop: [{}] } }));
  await writeFile(join(home, '.claude', 'plugins', 'installed_plugins.json'), JSON.stringify({
    version: 2,
    plugins: { 'loam@loam': [{ scope: 'user', installPath: claudeCache, version: '0.8.6' }] },
  }));
  const codexCache = join(home, '.codex', 'plugins', 'cache', 'loam', 'loam', '0.8.6');
  await mkdir(join(codexCache, 'hooks'), { recursive: true });
  await writeFile(join(codexCache, 'hooks', 'hooks.json'), JSON.stringify({ hooks: { Stop: [{}] } }));
  await writeFile(join(home, '.codex', 'hooks.json'), JSON.stringify({
    hooks: { SessionStart: [{ hooks: [unrelated, oldCodex] }] },
  }));

  const detected = await detectHarnesses({ home, pluginVersion: '0.8.6' });
  assert.equal(detected.claude.marketplaceOwned, true);
  assert.equal(detected.codex.marketplaceOwned, true);
  assert.equal(detected.claude.marketplaceReady, true);
  assert.equal(detected.codex.marketplaceReady, true);
  const stale = await detectHarnesses({ home, pluginVersion: '0.9.4' });
  assert.equal(stale.claude.marketplaceReady, false);
  assert.equal(stale.codex.marketplaceReady, false);
  const owned = await installHarnesses({ home, globalRoot, pluginVersion: '0.8.6', runtimePath, detected });
  const claude = JSON.parse(await readFile(join(home, '.claude', 'settings.json'), 'utf8'));
  const codex = JSON.parse(await readFile(join(home, '.codex', 'hooks.json'), 'utf8'));
  assert.equal(owned.claude.owner, 'marketplace');
  assert.equal(owned.codex.owner, 'marketplace');
  assert.deepEqual(claude.hooks.SessionStart[0].hooks, [unrelated]);
  assert.deepEqual(codex.hooks.SessionStart[0].hooks, [unrelated]);
  const claudeStop = claude.hooks.Stop?.flatMap((entry) => entry.hooks || []) || [];
  const codexStop = codex.hooks.Stop?.flatMap((entry) => entry.hooks || []) || [];
  assert.equal(claudeStop.filter((entry) => JSON.stringify(entry).includes('loam')).length, 0);
  assert.equal(codexStop.filter((entry) => JSON.stringify(entry).includes('loam')).length, 0);

  const fallbackHome = await mkdtemp(join(tmpdir(), 'loam-codex-fallback-'));
  await mkdir(join(fallbackHome, '.codex'), { recursive: true });
  const fallbackDetected = await detectHarnesses({ home: fallbackHome });
  const fallback = await installHarnesses({
    home: fallbackHome,
    globalRoot: join(fallbackHome, '.agents', 'loam'),
    pluginVersion: '0.8.6',
    runtimePath: join(fallbackHome, '.agents', 'loam', 'bin', '0.9.1', 'target', 'loam'),
    detected: fallbackDetected,
  });
  assert.equal(fallback.codex.state, 'skipped');
  await assert.rejects(() => readFile(join(fallbackHome, '.codex', 'hooks.json')), { code: 'ENOENT' });
});

test('enabled marketplace settings without installed plugin bytes do not claim marketplace ownership', async () => {
  const home = await mkdtemp(join(tmpdir(), 'loam-marketplace-missing-cache-'));
  await mkdir(join(home, '.claude'), { recursive: true });
  await mkdir(join(home, '.codex'), { recursive: true });
  await writeFile(join(home, '.claude', 'settings.json'), JSON.stringify({ enabledPlugins: { 'loam@loam': true } }));
  await writeFile(join(home, '.codex', 'config.toml'), '[plugins."loam@loam"]\nenabled = true\n');

  const detected = await detectHarnesses({ home });

  assert.equal(detected.claude.marketplaceConfigured, true);
  assert.equal(detected.codex.marketplaceConfigured, true);
  assert.equal(detected.claude.marketplaceOwned, false);
  assert.equal(detected.codex.marketplaceOwned, false);
});

test('disabled marketplace plugins remain discoverable for uninstall', async () => {
  const home = await mkdtemp(join(tmpdir(), 'loam-marketplace-disabled-'));
  const cache = join(home, '.claude', 'plugins', 'cache', 'loam', 'loam', '0.9.2');
  await mkdir(join(cache, 'hooks'), { recursive: true });
  await writeFile(join(cache, 'hooks', 'hooks.json'), JSON.stringify({ hooks: { Stop: [{}] } }));
  await writeFile(join(home, '.claude', 'settings.json'), JSON.stringify({ enabledPlugins: { 'loam@loam': false } }));
  await writeFile(join(home, '.claude', 'plugins', 'installed_plugins.json'), JSON.stringify({
    version: 2,
    plugins: { 'loam@loam': [{ scope: 'user', installPath: cache, version: '0.9.2' }] },
  }));

  const detected = await detectHarnesses({ home });
  assert.equal(detected.claude.marketplaceInstalled, true);
  assert.equal(detected.claude.marketplaceOwned, false);
});

test('project-scoped Claude plugins do not satisfy a user-scoped install', async () => {
  const home = await mkdtemp(join(tmpdir(), 'loam-marketplace-project-scope-'));
  const cache = join(home, '.claude', 'plugins', 'cache', 'loam', 'loam', '0.9.4');
  await mkdir(join(cache, 'hooks'), { recursive: true });
  await writeFile(join(cache, 'hooks', 'hooks.json'), JSON.stringify({ hooks: { Stop: [{}] } }));
  await writeFile(join(home, '.claude', 'settings.json'), JSON.stringify({ enabledPlugins: { 'loam@loam': true } }));
  await writeFile(join(home, '.claude', 'plugins', 'installed_plugins.json'), JSON.stringify({
    version: 2,
    plugins: { 'loam@loam': [{ scope: 'project', installPath: cache, version: '0.9.4' }] },
  }));

  const detected = await detectHarnesses({ home, pluginVersion: '0.9.4' });
  assert.equal(detected.claude.marketplaceConfigured, true);
  assert.equal(detected.claude.marketplaceInstalled, false);
  assert.equal(detected.claude.marketplaceReady, false);
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
    runtimePath: join(home, '.agents', 'loam', 'bin', '0.9.1', 'target', 'loam'),
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
    runtimePath: join(home, '.agents', 'loam', 'bin', '0.9.1', 'target', 'loam'),
    detected,
  });

  assert.equal(result.opencode.state, 'absent');
  assert.equal(result.claude.state, 'absent');
  assert.equal(result.cursor.state, 'absent');
});

test('Codex cleanup preserves unrelated hook groups without adding setup hooks', async () => {
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
    runtimePath: join(home, '.agents', 'loam', 'bin', '0.9.1', 'target', 'loam'),
    detected: { opencode: { id: 'opencode', state: 'absent' }, claude: { id: 'claude', state: 'absent' }, cursor: { id: 'cursor', state: 'absent' }, codex: { id: 'codex', state: 'detected', root: join(home, '.codex') } },
  });
  assert.equal(result.codex.state, 'skipped', JSON.stringify(result.codex));
  const config = JSON.parse(await readFile(join(home, '.codex', 'hooks.json'), 'utf8'));
  assert.equal(config.hooks.Stop.length, 1);
  assert.deepEqual(config.hooks.Stop[0].hooks, [unrelated]);
});
