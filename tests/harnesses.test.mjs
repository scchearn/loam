import assert from 'node:assert/strict';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { mkdtemp } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { test } from 'node:test';

import { LoamPlugin } from '../adapters/opencode.mjs';

// adapters/opencode.mjs exports only LoamPlugin (OpenCode's loader calls every
// top-level export as a plugin factory); test helpers ride on it as properties.
const { createOpenCodeAdapter } = LoamPlugin;
import { main as runIngestWorker } from '../adapters/ingest-worker.mjs';
import { dedupe, mergeJsonConfig } from '../setup/config.mjs';
import {
  detectHarnesses,
  installHarnesses,
  reconcileOpenCodePluginEntry,
} from '../setup/harnesses.mjs';

test('opencode config plugin entries pointing at a repo-local loam.js are rewritten to the stable global path', async () => {
  const home = await mkdtemp(join(tmpdir(), 'loam-opencode-rewrite-'));
  const stable = join(home, '.config', 'opencode', 'plugins', 'loam.js');
  await mkdir(join(home, '.config', 'opencode', 'plugins'), { recursive: true });
  await writeFile(
    join(home, '.config', 'opencode', 'opencode.json'),
    JSON.stringify({
      plugin: [
        '/home/sam/Nextcloud/Clients/Dikokotech/loam/.opencode/plugins/loam.js',
        'superpowers@git+https://github.com/obra/superpowers.git',
        ['/repo/plugins/loam.js', { enabled: true }],
      ],
      mcp: { exa: { type: 'remote' } },
    }),
  );

  const result = await reconcileOpenCodePluginEntry(home, stable);
  assert.equal(result.action, 'rewritten');
  const config = JSON.parse(await readFile(join(home, '.config', 'opencode', 'opencode.json'), 'utf8'));
  assert.equal(config.plugin[0], stable, 'a repo-local loam.js entry points at the stable path');
  assert.equal(config.plugin[1], 'superpowers@git+https://github.com/obra/superpowers.git', 'unrelated entries survive');
  assert.deepEqual(config.plugin[2], [stable, { enabled: true }], 'tuple entries are rewritten in place');
  assert.deepEqual(config.mcp, { exa: { type: 'remote' } }, 'unrelated config sections survive');

  // Idempotent: a second pass rewrites nothing.
  const again = await reconcileOpenCodePluginEntry(home, stable);
  assert.equal(again.action, 'unchanged');

  // A config with no Loam-owned entries is left alone.
  await writeFile(
    join(home, '.config', 'opencode', 'opencode.json'),
    JSON.stringify({ plugin: ['other-plugin'] }),
  );
  const untouched = await reconcileOpenCodePluginEntry(home, stable);
  assert.equal(untouched.action, 'unchanged');
  assert.deepEqual(
    JSON.parse(await readFile(join(home, '.config', 'opencode', 'opencode.json'), 'utf8')),
    { plugin: ['other-plugin'] },
  );
});

test('OpenCode re-injects on every turn: full block first, federation refresh after', async () => {
  const calls = [];
  const wakeStarts = [];
  const plugin = await createOpenCodeAdapter({
    getContext: async ({ workspace, event }) => {
      calls.push({ workspace, event });
      return event === 'SessionStart'
        ? `<LOAM_IMPORTANT>\nYou have loam.\n${workspace}\n</LOAM_IMPORTANT>`
        : `<LOAM_IMPORTANT>\n## Federation\nrefresh\n</LOAM_IMPORTANT>`;
    },
    wakeServer: async ({ workspace, sessionId }) => {
      wakeStarts.push({ workspace, sessionId });
      return { wakeRef: 'notify-tcp://127.0.0.1:0', registered: false, close: async () => {} };
    },
  })({ directory: '/workspace' });
  const output = {
    messages: [{ info: { role: 'user', sessionID: 'sess-1' }, parts: [{ type: 'text', text: 'superpowers context' }] }],
  };

  await plugin['experimental.chat.messages.transform']({}, output);
  await plugin['experimental.chat.messages.transform']({}, output);
  await plugin['experimental.chat.messages.transform']({}, output);
  // T4: the first fire is the session start (full block); every later fire is a
  // per-turn refresh. The OWN_MARKER self-dedup is gone — the native hook
  // renders the right shape per event.
  assert.deepEqual(calls, [
    { workspace: '/workspace', event: 'SessionStart' },
    { workspace: '/workspace', event: 'UserPromptSubmit' },
    { workspace: '/workspace', event: 'UserPromptSubmit' },
  ]);
  // The notify listener opens once, on the first fire, against the session id
  // carried on the first user message (OpenCode emits no session.created for
  // the main session).
  assert.deepEqual(wakeStarts, [{ workspace: '/workspace', sessionId: 'sess-1' }]);
  const parts = output.messages[0].parts.filter((part) => part.type === 'text');
  assert.equal(parts.length, 4, 'one full block + two refreshes');
  // unshift puts the newest injection first: the two refreshes, then the full
  // block, then the original user text.
  assert.ok(parts[0].text.includes('## Federation'), 'newest part is a refresh');
  assert.ok(parts[1].text.includes('## Federation'), 'second part is a refresh');
  assert.ok(parts[2].text.includes('You have loam'), 'the full block is still present');
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
      runWorker: async ({ openCodeSession, hookRun, notify }) => {
        assert.deepEqual(hookRun, { id: 9 });
        assert.equal(notify, undefined, 'missing showToast must be detected before use');
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

test('OpenCode toast visibility uses the pinned SDK shape for launch and terminal outcomes', async () => {
  const root = await mkdtemp(join(tmpdir(), 'loam-opencode-toast-'));
  const calls = [];
  let finished;
  const done = new Promise((resolvePromise) => { finished = resolvePromise; });
  const plugin = await createOpenCodeAdapter({
    client: {
      session: {},
      tui: { showToast: async (input) => { calls.push(input); return true; } },
    },
    ingestion: {
      gate: async () => ({ action: 'spawn_worker', workspace: '/workspace' }),
      resolveGlobalRoot: () => root,
      resolveSkillsRoot: () => root,
      runWorker: async ({ notify }) => {
        const launch = new AbortController();
        const terminal = new AbortController();
        await notify({ phase: 'launch', visibility: 'toast', signal: launch.signal });
        await notify({ phase: 'terminal', visibility: 'toast', status: 'failed', signal: terminal.signal });
        await notify({ phase: 'terminal', visibility: 'native', status: 'ok', signal: terminal.signal });
        return { reason: 'ok' };
      },
    },
    hookRuns: {
      resolveGlobalRoot: () => root,
      beginHookRun: async () => ({ id: 1 }),
      finishHookRun: async () => undefined,
      startHookWorker: async () => undefined,
      finishHookWorker: async () => { finished(); },
    },
  })({ directory: '/workspace' });

  await plugin.event({ event: { type: 'session.idle', sessionID: 'parent-toast' } });
  await done;
  assert.equal(calls.length, 2);
  assert.deepEqual(calls.map(({ query, body }) => ({ query, body })), [
    {
      query: { directory: '/workspace' },
      body: { title: 'Loam', message: 'Background code ingestion started.', variant: 'info' },
    },
    {
      query: { directory: '/workspace' },
      body: { title: 'Loam', message: 'Background code ingestion failed.', variant: 'error' },
    },
  ]);
  assert.ok(calls.every(({ signal }) => signal instanceof AbortSignal));
});

test('OpenCode toast request rejects with AbortError when the seam aborts its signal', async () => {
  const root = await mkdtemp(join(tmpdir(), 'loam-opencode-toast-abort-'));
  let aborted = false;
  let finished;
  const done = new Promise((resolvePromise) => { finished = resolvePromise; });
  const plugin = await createOpenCodeAdapter({
    client: {
      session: {},
      tui: {
        showToast: ({ signal }) => new Promise((_resolvePromise, reject) => {
          signal.addEventListener('abort', () => {
            aborted = true;
            reject(new DOMException('toast aborted', 'AbortError'));
          }, { once: true });
        }),
      },
    },
    ingestion: {
      gate: async () => ({ action: 'spawn_worker', workspace: '/workspace' }),
      resolveGlobalRoot: () => root,
      resolveSkillsRoot: () => root,
      runWorker: async ({ notify }) => {
        const controller = new AbortController();
        const request = notify({ phase: 'launch', visibility: 'toast', signal: controller.signal });
        controller.abort();
        await assert.rejects(request, { name: 'AbortError' });
        return { reason: 'ok' };
      },
    },
    hookRuns: {
      resolveGlobalRoot: () => root,
      beginHookRun: async () => ({ id: 2 }),
      finishHookRun: async () => undefined,
      startHookWorker: async () => undefined,
      finishHookWorker: async () => { finished(); },
    },
  })({ directory: '/workspace' });

  await plugin.event({ event: { type: 'session.idle', sessionID: 'parent-abort' } });
  await done;
  assert.equal(aborted, true);
});

test('OpenCode toast maps a successful persisted terminal outcome to success', async () => {
  const root = await mkdtemp(join(tmpdir(), 'loam-opencode-toast-success-'));
  const calls = [];
  let finished;
  const done = new Promise((resolvePromise) => { finished = resolvePromise; });
  const plugin = await createOpenCodeAdapter({
    client: {
      session: {},
      tui: { showToast: async (input) => { calls.push(input); return true; } },
    },
    ingestion: {
      gate: async () => ({ action: 'spawn_worker', workspace: '/workspace' }),
      resolveGlobalRoot: () => root,
      resolveSkillsRoot: () => root,
      runWorker: async ({ notify }) => {
        const controller = new AbortController();
        await notify({ phase: 'terminal', visibility: 'toast', status: 'ok', signal: controller.signal });
        return { reason: 'ok' };
      },
    },
    hookRuns: {
      resolveGlobalRoot: () => root,
      beginHookRun: async () => ({ id: 3 }),
      finishHookRun: async () => undefined,
      startHookWorker: async () => undefined,
      finishHookWorker: async () => { finished(); },
    },
  })({ directory: '/workspace' });

  await plugin.event({ event: { type: 'session.idle', sessionID: 'parent-success' } });
  await done;
  assert.equal(calls.length, 1);
  assert.deepEqual(calls[0].body, {
    title: 'Loam', message: 'Background code ingestion completed.', variant: 'success',
  });
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

test('a detached fallback worker records codex_native/fallback/taken and finishes as fallback', async () => {
  const calls = [];
  await runIngestWorker({
    harness: 'codex', workspace: '/workspace', hookRunId: 12, globalRoot: '/global',
    skillsRoot: '/skills', env: {}, workerOrigin: 'fallback',
    startHookWorker: async (input) => calls.push(['start', input]),
    finishHookWorker: async (input) => calls.push(['finish', input]),
    runWorker: async () => ({ reason: 'ok' }),
  });
  assert.equal(calls[0][0], 'start');
  assert.equal(calls[0][1].origin, 'fallback');
  assert.deepEqual(calls[0][1].events, [
    { event: 'codex_native', phase: 'fallback', outcome: 'taken', visibility: 'native' },
  ]);
  assert.equal(calls[1][0], 'finish');
  assert.equal(calls[1][1].origin, 'fallback');
  assert.equal(calls[1][1].reason, 'ok');
});

test('OpenCode hook-run logging remains fail-open', async () => {
  let calls = 0;
  const plugin = await createOpenCodeAdapter({
    hookRuns: {
      resolveGlobalRoot: () => '/global',
      beginHookRun: async () => { calls += 1; throw new Error('locked'); },
      finishHookRun: async () => { throw new Error('must not finish'); },
    },
    // The idle event must not spawn the real gate's node subprocess after the
    // test ends; this test is about hook-run logging, not ingestion.
    ingestion: {
      gate: async () => ({ action: 'skip' }),
      resolveGlobalRoot: () => '/global',
      resolveSkillsRoot: () => '/skills',
      runWorker: async () => undefined,
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

test('Claude readiness follows the plugin cache when installed_plugins.json lags behind', async () => {
  // Reproduces the observed race: `claude plugin install` creates the 0.9.15 cache directory,
  // but installed_plugins.json still names 0.9.14 when setup writes install.json. Reading the
  // registry alone dropped Claude from configured_harnesses for the whole release.
  const home = await mkdtemp(join(tmpdir(), 'loam-marketplace-lag-'));
  await mkdir(join(home, '.claude'), { recursive: true });
  await writeFile(join(home, '.claude', 'settings.json'), JSON.stringify({
    enabledPlugins: { 'loam@loam': true },
  }));
  const stale = join(home, '.claude', 'plugins', 'cache', 'loam', 'loam', '0.9.14');
  const fresh = join(home, '.claude', 'plugins', 'cache', 'loam', 'loam', '0.9.15');
  for (const cache of [stale, fresh]) {
    await mkdir(join(cache, 'hooks'), { recursive: true });
    await writeFile(join(cache, 'hooks', 'hooks.json'), JSON.stringify({ hooks: { Stop: [{}] } }));
  }
  // The registry has not caught up: it still points at the previous version.
  await writeFile(join(home, '.claude', 'plugins', 'installed_plugins.json'), JSON.stringify({
    version: 2,
    plugins: { 'loam@loam': [{ scope: 'user', installPath: stale, version: '0.9.14' }] },
  }));

  const detected = await detectHarnesses({ home, pluginVersion: '0.9.15' });
  assert.equal(detected.claude.marketplaceInstalled, true);
  assert.equal(detected.claude.marketplaceOwned, true);
  assert.equal(detected.claude.marketplaceReady, true, 'the freshly installed cache version must count as ready');

  // installHarnesses is what writes the harness state that becomes configured_harnesses.
  const installed = await installHarnesses({
    home,
    globalRoot: join(home, '.agents', 'loam'),
    pluginVersion: '0.9.15',
    runtimePath: join(home, '.agents', 'loam', 'bin', '0.9.1', 'target', 'loam'),
    detected,
  });
  assert.equal(installed.claude.state, 'ready');
  assert.equal(installed.claude.owner, 'marketplace');

  // A version that was never installed still must not report ready.
  const missing = await detectHarnesses({ home, pluginVersion: '0.9.99' });
  assert.equal(missing.claude.marketplaceReady, false);
});

test('Claude readiness works from the plugin cache with no registry file at all', async () => {
  const home = await mkdtemp(join(tmpdir(), 'loam-marketplace-noregistry-'));
  await mkdir(join(home, '.claude'), { recursive: true });
  await writeFile(join(home, '.claude', 'settings.json'), JSON.stringify({
    enabledPlugins: { 'loam@loam': true },
  }));
  const cache = join(home, '.claude', 'plugins', 'cache', 'loam', 'loam', '0.9.15');
  await mkdir(join(cache, 'hooks'), { recursive: true });
  await writeFile(join(cache, 'hooks', 'hooks.json'), JSON.stringify({ hooks: { Stop: [{}] } }));

  const detected = await detectHarnesses({ home, pluginVersion: '0.9.15' });
  assert.equal(detected.claude.marketplaceInstalled, true);
  assert.equal(detected.claude.marketplaceReady, true);
});

test('Claude is not ready when the cached plugin is missing its required hooks', async () => {
  const home = await mkdtemp(join(tmpdir(), 'loam-marketplace-nohooks-'));
  await mkdir(join(home, '.claude'), { recursive: true });
  await writeFile(join(home, '.claude', 'settings.json'), JSON.stringify({
    enabledPlugins: { 'loam@loam': true },
  }));
  const cache = join(home, '.claude', 'plugins', 'cache', 'loam', 'loam', '0.9.15');
  await mkdir(join(cache, 'hooks'), { recursive: true });
  await writeFile(join(cache, 'hooks', 'hooks.json'), JSON.stringify({ hooks: {} }));

  const detected = await detectHarnesses({ home, pluginVersion: '0.9.15' });
  assert.equal(detected.claude.marketplaceInstalled, true);
  assert.equal(detected.claude.marketplaceReady, false, 'a cache without a Stop hook is not ready');
});
