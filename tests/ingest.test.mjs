import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { chmod, mkdir, mkdtemp, readdir, readFile, realpath, rm, stat, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { delimiter, join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { test } from 'node:test';

import { fingerprintActionable } from '../integration/ingest-fingerprint.mjs';
import {
  bindNativeAgent, claudeSessionName, dispatchBoundary, finalizeNativeAgentRun,
  finalizeWorkerRun, gate, ingestStatus, inspectIntent, prepareNativeAgentRun, prepareWorkerRun,
  readIngestConfig, runRoot, runWorker, startWorker,
} from '../integration/ingest.mjs';
import { bootIdentity, childIdentity, processDescriptor, resolveExecutable, startTracked } from '../integration/ingest-process.mjs';

async function fixture() {
  const root = await realpath(await mkdtemp(join(tmpdir(), 'loam-ingest-')));
  const workspace = join(root, 'workspace');
  const wiki = join(workspace, 'wiki');
  const skills = join(root, 'skills');
  await mkdir(join(workspace, 'src'), { recursive: true });
  await mkdir(join(wiki, 'code'), { recursive: true });
  await mkdir(join(skills, 'loam-ingesting-codebase', 'references'), { recursive: true });
  await writeFile(join(workspace, 'src', 'a.js'), 'export const a = 1;\n');
  const exclusions = join(skills, 'loam-ingesting-codebase', 'references', 'ingestion-exclusions.md');
  await writeFile(exclusions, '# exclusions\n');
  return { root, workspace, wiki, skills, exclusions };
}

async function nativeInstall(root) {
  const adapterRoot = join(root, 'plugins', '0.9.10');
  const integrationPath = join(root, 'integration', 'loam.mjs');
  await mkdir(adapterRoot, { recursive: true });
  await mkdir(join(root, 'integration'), { recursive: true });
  await writeFile(join(adapterRoot, 'ingest-worker.mjs'), '// installed worker\n');
  await writeFile(integrationPath, '// installed integration\n');
  await writeFile(join(root, 'install.json'), JSON.stringify({
    schema_version: 1,
    plugin_version: '0.9.10',
    runtime_version: '0.9.5',
    target: 'x86_64-unknown-linux-musl',
    runtime_sha256: 'a'.repeat(64),
    runtime_path: join(root, 'bin', 'loam'),
    adapter_root: adapterRoot,
    integration_path: integrationPath,
    skills_scope: 'global',
    skills_source: 'scchearn/loam',
    configured_harnesses: ['codex'],
  }));
  return { adapterRoot, integrationPath };
}

function actionableRuntime(wiki, entries = [{ path: 'src/a.js', mtime: '1', reason: 'new' }]) {
  return {
    readiness: { ready: true, runtimePath: '/private/loam' },
    runtimeRunner: async ({ args }) => args[0] === 'state'
      ? { code: 0, stdout: JSON.stringify({ wiki_root: wiki, hints: [{ kind: 'code_ingest_pending', evidence: { pending_count: 1 } }] }), stderr: '' }
      : { code: 0, stdout: JSON.stringify(entries), stderr: '' },
  };
}

test('fingerprints the complete UTF-8 actionable set and includes exclusions identity', async () => {
  const { workspace, exclusions } = await fixture();
  const entries = [
    { path: 'src/a.js', mtime: '10', reason: 'stale' },
    { path: 'src/ä.js', mtime: '11', reason: 'new' },
  ];
  await writeFile(join(workspace, 'src', 'ä.js'), 'unicode\n');
  const first = await fingerprintActionable({ workspace, entries, exclusionsPath: exclusions });
  const reversed = await fingerprintActionable({ workspace, entries: [...entries].reverse(), exclusionsPath: exclusions });
  assert.equal(first.complete, true);
  assert.equal(first.count, 2);
  assert.equal(first.fingerprint, reversed.fingerprint);
  assert.equal(first.fingerprint.length, 64);

  await writeFile(exclusions, '# changed exclusions\n');
  const changedExclusions = await fingerprintActionable({ workspace, entries, exclusionsPath: exclusions });
  assert.notEqual(first.fingerprint, changedExclusions.fingerprint);
  await assert.rejects(
    () => fingerprintActionable({ workspace, entries: [{ path: '../outside.js', mtime: '1', reason: 'new' }], exclusionsPath: exclusions }),
    (error) => error.reason === 'fingerprint_unavailable',
  );
  const empty = await fingerprintActionable({ workspace, entries: [], exclusionsPath: exclusions });
  assert.equal(empty.complete, true);
  assert.equal(empty.count, 0);
});

test('fingerprint validation rejects controls/traversal and normalizes Windows separators deterministically', async () => {
  const { workspace, exclusions } = await fixture();
  await writeFile(join(workspace, 'src', 'utils.js'), 'export const ok = true;\n');
  const slash = await fingerprintActionable({ workspace, exclusionsPath: exclusions, entries: [{ path: 'src/utils.js', mtime: '1', reason: 'new' }] });
  const backslash = await fingerprintActionable({ workspace, exclusionsPath: exclusions, entries: [{ path: 'src\\utils.js', mtime: '1', reason: 'new' }] });
  assert.equal(slash.fingerprint, backslash.fingerprint);
  for (const path of ['src/\n.js', 'src/\u0000.js', 'src/../outside.js', 'C:/outside.js']) {
    await assert.rejects(
      () => fingerprintActionable({ workspace, exclusionsPath: exclusions, entries: [{ path, mtime: '1', reason: 'new' }] }),
      (error) => error.reason === 'fingerprint_unavailable',
    );
  }
});

test('boundary gate defaults on without an environment override or config file', async () => {
  const workspace = await mkdtemp(join(tmpdir(), 'loam-gate-'));
  const globalRoot = join(await mkdtemp(join(tmpdir(), 'loam-gate-parent-')), 'missing-global');
  const options = { harness: 'claude', payload: { cwd: workspace, event_id: 'same-event' }, globalRoot };

  const result = await gate({ ...options, env: {} });
  assert.equal(result.action, 'spawn_worker');
  assert.equal(result.config.enabled, true);
  assert.equal((await stat(globalRoot)).isDirectory(), true);
  await assert.rejects(() => realpath(join(globalRoot, 'config.json')), (error) => error.code === 'ENOENT');
});

test('explicitly disabled boundary gate rejects before creating state or probing native runtime', async () => {
  const workspace = await mkdtemp(join(tmpdir(), 'loam-gate-disabled-'));
  const globalRoot = join(await mkdtemp(join(tmpdir(), 'loam-gate-disabled-parent-')), 'missing-global');

  assert.deepEqual(await gate({ harness: 'claude', payload: { cwd: workspace }, globalRoot, env: { LOAM_INGEST_BACKGROUND: '0' } }), { action: 'skip', reason: 'disabled' });
  await assert.rejects(() => realpath(globalRoot), (error) => error.code === 'ENOENT');
});

test('boundary gate honors config and environment precedence and blocks worker recursion', async () => {
  const workspace = await mkdtemp(join(tmpdir(), 'loam-gate-precedence-'));
  const globalRoot = await mkdtemp(join(tmpdir(), 'loam-gate-precedence-global-'));
  const options = { harness: 'claude', payload: { cwd: workspace, event_id: 'same-event' }, globalRoot };

  await writeFile(join(globalRoot, 'config.json'), JSON.stringify({ background_ingest: { enabled: false } }));
  assert.deepEqual(await gate({ ...options, env: {} }), { action: 'skip', reason: 'disabled' });
  assert.equal((await gate({ ...options, env: { LOAM_INGEST_BACKGROUND: '1' } })).action, 'spawn_worker');

  await writeFile(join(globalRoot, 'config.json'), JSON.stringify({ background_ingest: { enabled: true } }));
  assert.deepEqual(await gate({ ...options, env: { LOAM_INGEST_BACKGROUND: '0' } }), { action: 'skip', reason: 'disabled' });
  assert.deepEqual(await gate({ ...options, env: { LOAM_INGEST_WORKER: '1' } }), { action: 'skip', reason: 'disabled', recursion: true });
  assert.deepEqual(await gate({ ...options, payload: { cwd: workspace, agent_type: 'loam:ingestor' }, env: {} }), { action: 'skip', reason: 'disabled', recursion: true });
  assert.equal((await gate({ ...options, payload: { cwd: workspace, agent_type: 'other-agent' }, env: {} })).action, 'spawn_worker');
});

test('boundary debounce trusts only coherent successful or nothing-to-do outcomes', async () => {
  const workspace = await realpath(await mkdtemp(join(tmpdir(), 'loam-gate-outcomes-')));
  const globalRoot = await mkdtemp(join(tmpdir(), 'loam-gate-outcomes-global-'));
  const options = { harness: 'codex', payload: { cwd: workspace }, globalRoot, env: {}, now: 1_000 };
  await writeFile(join(globalRoot, 'config.json'), JSON.stringify({ background_ingest: { min_interval_seconds: 300 } }));
  await mkdir(runRoot(globalRoot, workspace), { recursive: true });

  for (const previous of [
    { status: 'ok' },
    { status: 'skipped', reason: 'nothing_to_do' },
  ]) {
    await writeFile(join(runRoot(globalRoot, workspace), 'last-run.json'), JSON.stringify({ schema: 1, completed_at: 900, ...previous }));
    assert.deepEqual(await gate(options), { action: 'skip', reason: 'too_soon', workspace });
  }
  for (const previous of [
    { status: 'failed', reason: 'nothing_to_do' },
    { status: 'skipped', reason: 'unavailable' },
  ]) {
    await writeFile(join(runRoot(globalRoot, workspace), 'last-run.json'), JSON.stringify({ schema: 1, completed_at: 900, ...previous }));
    assert.equal((await gate(options)).action, 'spawn_worker');
  }
});

test('visibility config accepts supported values and silently normalizes everything else', async () => {
  const globalRoot = await mkdtemp(join(tmpdir(), 'loam-visibility-config-'));
  assert.equal((await readIngestConfig(globalRoot, {})).visibility, 'native');
  assert.equal((await readIngestConfig(globalRoot, {})).require_visible_worker, false);
  for (const visibility of ['silent', 'toast', 'native']) {
    await writeFile(join(globalRoot, 'config.json'), JSON.stringify({ background_ingest: { visibility } }));
    assert.equal((await readIngestConfig(globalRoot, {})).visibility, visibility);
  }
  await writeFile(join(globalRoot, 'config.json'), JSON.stringify({ background_ingest: { require_visible_worker: true } }));
  assert.equal((await readIngestConfig(globalRoot, {})).require_visible_worker, true);
  await writeFile(join(globalRoot, 'config.json'), JSON.stringify({ background_ingest: { require_visible_worker: 'true' } }));
  assert.equal((await readIngestConfig(globalRoot, {})).require_visible_worker, false);
  await writeFile(join(globalRoot, 'config.json'), JSON.stringify({ background_ingest: { visibility: 'loud' } }));
  assert.equal((await readIngestConfig(globalRoot, {})).visibility, 'native');
});

test('Claude session name is deterministic, Loam-attributable, and workspace scoped', () => {
  const first = claudeSessionName('/workspace/one');
  assert.equal(first, claudeSessionName('/workspace/one'));
  assert.notEqual(first, claudeSessionName('/workspace/two'));
  assert.match(first, /^loam-ingest-[a-f0-9]{10}$/u);
  assert.doesNotMatch(first, /workspace/u);
});

test('notification launch is non-blocking and terminal status follows persisted outcome', async (context) => {
  context.mock.timers.enable({ apis: ['setTimeout'] });
  const { root, workspace, wiki, skills } = await fixture();
  await writeFile(join(root, 'config.json'), JSON.stringify({ background_ingest: { visibility: 'toast' } }));
  const calls = [];
  let modelCompleted = false;
  let launchNotificationSettled = false;
  let markLaunchCalled;
  const launchCalled = new Promise((resolvePromise) => { markLaunchCalled = resolvePromise; });
  const resultPromise = runWorker({
    harness: 'opencode', workspace, globalRoot: root, skillsRoot: skills,
    readiness: { ready: true, runtimePath: '/private/loam' },
    runtimeRunner: async ({ args }) => args[0] === 'state'
      ? { code: 0, stdout: JSON.stringify({ wiki_root: wiki, hints: [{ kind: 'code_ingest_pending', evidence: { pending_count: 1 } }] }), stderr: '' }
      : { code: 0, stdout: JSON.stringify([{ path: 'src/a.js', mtime: '1', reason: 'new' }]), stderr: '' },
    modelRunner: async () => ({
      completion: Promise.resolve().then(() => { modelCompleted = true; return { code: 0 }; }),
    }),
    notify: async (event) => {
      calls.push(event);
      if (event.phase === 'launch') {
        markLaunchCalled();
        await new Promise((resolvePromise) => event.signal.addEventListener('abort', resolvePromise, { once: true }));
        launchNotificationSettled = true;
      }
    },
  });

  await launchCalled;
  assert.equal(modelCompleted, true, 'launch notification must not block worker completion');
  context.mock.timers.tick(249);
  assert.equal(calls[0].signal.aborted, false, 'notification deadline must not fire early');
  assert.equal(launchNotificationSettled, false);
  context.mock.timers.tick(1);
  assert.equal(calls[0].signal.aborted, true, 'notification deadline must fire at 250 ms');
  const result = await resultPromise;
  const stored = JSON.parse(await readFile(join(runRoot(root, workspace), 'last-run.json'), 'utf8'));
  assert.equal(result.reason, 'ok');
  assert.equal(stored.status, 'failed');
  assert.deepEqual(calls.map(({ phase }) => phase), ['launch', 'terminal']);
  assert.equal(launchNotificationSettled, true, 'deadline must settle an abort-aware notification resource');
  assert.equal(calls[1].signal.aborted, false);
  assert.equal(calls[1].status, stored.status);
});

test('silent and failing notifications cannot change ingestion state or exceed two calls', async () => {
  for (const visibility of ['silent', 'toast']) {
    const { root, workspace, wiki, skills } = await fixture();
    await writeFile(join(root, 'config.json'), JSON.stringify({ background_ingest: { visibility } }));
    const calls = [];
    const result = await runWorker({
      harness: 'opencode', workspace, globalRoot: root, skillsRoot: skills,
      readiness: { ready: true, runtimePath: '/private/loam' },
      runtimeRunner: async ({ args }) => args[0] === 'state'
        ? { code: 0, stdout: JSON.stringify({ wiki_root: wiki, hints: [{ kind: 'code_ingest_pending', evidence: { pending_count: 1 } }] }), stderr: '' }
        : { code: 0, stdout: JSON.stringify([{ path: 'src/a.js', mtime: '1', reason: 'new' }]), stderr: '' },
      modelRunner: async () => ({ completion: Promise.resolve({ code: 0 }) }),
      notify: async (event) => { calls.push(event); throw new Error('toast failed'); },
    });
    const stored = JSON.parse(await readFile(join(runRoot(root, workspace), 'last-run.json'), 'utf8'));
    assert.equal(result.reason, 'ok');
    assert.equal(stored.status, 'failed');
    assert.equal(await readFile(join(runRoot(root, workspace), 'lease.json')).then(() => true).catch(() => false), false);
    assert.equal(calls.length, visibility === 'silent' ? 0 : 2);
  }
});

test('detached worker launch forwards the hook-run correlation id', () => {
  let request;
  const workspace = resolve('/workspace');
  const result = startWorker({
    harness: 'codex',
    workspace,
    globalRoot: '/global',
    skillsRoot: '/skills',
    workerPath: '/worker.mjs',
    hookRunId: 17,
    env: {},
    spawn: (input) => { request = input; return 'started'; },
  });
  assert.equal(result, 'started');
  assert.deepEqual(request.args, [
    '/worker.mjs', '--harness', 'codex', '--workspace', workspace, '--hook-run-id', '17',
  ]);
});

test('Codex native boundary records one intent and falls back exactly once on the continuation Stop', async () => {
  const { root, workspace, wiki, skills } = await fixture();
  const adapterRoot = join(root, 'plugins', 'version');
  await mkdir(adapterRoot, { recursive: true });
  await writeFile(join(root, 'install.json'), JSON.stringify({ adapter_root: adapterRoot }));
  await writeFile(join(root, 'config.json'), JSON.stringify({ background_ingest: { visibility: 'native', min_interval_seconds: 0 } }));
  const spawns = [];
  const options = {
    harness: 'codex', globalRoot: root, skillsRoot: skills, env: {},
    payload: { cwd: workspace, session_id: 'session-1', stop_hook_active: false },
    hookRunId: 41,
    spawn: (request) => { spawns.push(request); return { pid: 123 }; },
    ...actionableRuntime(wiki),
  };

  const first = await dispatchBoundary(options);
  assert.equal(first.action, 'spawn_worker');
  assert.equal(first.native_continuation.decision, 'block');
  assert.match(first.native_continuation.reason, /fork_turns set to "none"/u);
  assert.match(first.native_continuation.reason, /task_name set to "loam_ingest_stop_<N>"/u);
  assert.equal(spawns.length, 0, 'first native Stop must not start a detached worker');
  const intentPath = join(runRoot(root, workspace), 'native-intent.json');
  const intent = JSON.parse(await readFile(intentPath, 'utf8'));
  assert.equal(intent.workspace, workspace);
  assert.equal(intent.session_id, 'session-1');
  assert.equal(intent.claim, 'pending');

  assert.deepEqual(await dispatchBoundary(options), { action: 'skip', reason: 'busy', workspace });
  assert.equal(spawns.length, 0, 'duplicate first Stop must not issue another continuation or spawn');

  const fallback = await dispatchBoundary({
    ...options,
    hookRunId: 42,
    payload: { ...options.payload, stop_hook_active: true },
  });
  assert.equal(fallback.action, 'spawn_worker');
  assert.equal(fallback.native_fallback, true);
  assert.equal(spawns.length, 1);
  assert.ok(spawns[0].args.includes('42'));
  const claim = JSON.parse(await readFile(join(runRoot(root, workspace), 'native-claim.json'), 'utf8'));
  assert.equal(claim.claim, 'fallback');
  assert.equal(claim.intent_id, intent.intent_id);

  assert.deepEqual(await dispatchBoundary({
    ...options,
    payload: { ...options.payload, stop_hook_active: true },
  }), { action: 'skip', reason: 'busy', workspace });
  assert.equal(spawns.length, 1, 'duplicate continuation must not start a second fallback');
});

test('Codex native cheap skips, bound intents, stale intents, and corrupt state are deterministic', async () => {
  const { root, workspace, wiki, skills } = await fixture();
  const adapterRoot = join(root, 'plugins', 'version');
  await mkdir(adapterRoot, { recursive: true });
  await writeFile(join(root, 'install.json'), JSON.stringify({ adapter_root: adapterRoot }));
  await writeFile(join(root, 'config.json'), JSON.stringify({ background_ingest: { visibility: 'native', timeout_seconds: 1 } }));
  const intentRoot = runRoot(root, workspace);
  let spawns = 0;
  const base = {
    harness: 'codex', globalRoot: root, skillsRoot: skills, now: 100,
    payload: { cwd: workspace, session_id: 'session-2' }, env: {},
    spawn: () => { spawns += 1; return { pid: 1 }; },
    ...actionableRuntime(wiki),
  };

  assert.deepEqual(await dispatchBoundary({ ...base, env: { LOAM_INGEST_BACKGROUND: '0' } }), { action: 'skip', reason: 'disabled' });
  await assert.rejects(() => readFile(join(intentRoot, 'native-intent.json')), { code: 'ENOENT' });

  const first = await dispatchBoundary(base);
  assert.equal(first.native_continuation.decision, 'block');
  const intent = JSON.parse(await readFile(join(intentRoot, 'native-intent.json'), 'utf8'));
  await writeFile(join(intentRoot, 'native-claim.json'), JSON.stringify({ ...intent, claim: 'agent', agent_id: 'agent-1' }));
  await rm(join(intentRoot, 'native-intent.json'));
  assert.deepEqual(await dispatchBoundary({
    ...base,
    payload: { ...base.payload, stop_hook_active: true },
  }), { action: 'skip', reason: 'busy', workspace });
  assert.equal(spawns, 0, 'a bound native intent must never fall back');

  await rm(join(intentRoot, 'native-claim.json'));
  const replacement = await dispatchBoundary({ ...base, now: 2_000 });
  assert.equal(replacement.native_continuation.decision, 'block', 'an expired intent must not suppress a later continuation');
  const replacedIntent = JSON.parse(await readFile(join(intentRoot, 'native-intent.json'), 'utf8'));
  assert.notEqual(replacedIntent.intent_id, intent.intent_id);

  await writeFile(join(intentRoot, 'native-intent.json'), '{malformed');
  const degraded = await dispatchBoundary({
    ...base,
    now: 2_001,
    payload: { ...base.payload, stop_hook_active: true },
  });
  assert.equal(degraded.action, 'spawn_worker');
  assert.equal(degraded.native_fallback, true);
  assert.equal(spawns, 1, 'corrupt intent state must degrade to one detached worker');
});

test('Codex native fallback launch failure clears its claim for a later attempt', async () => {
  const { root, workspace, wiki, skills } = await fixture();
  await writeFile(join(root, 'config.json'), JSON.stringify({ background_ingest: { visibility: 'native' } }));
  const options = {
    harness: 'codex', globalRoot: root, skillsRoot: skills, env: {},
    payload: { cwd: workspace, session_id: 'retry-session' },
    ...actionableRuntime(wiki),
  };

  assert.equal((await dispatchBoundary(options)).native_continuation.decision, 'block');
  assert.deepEqual(await dispatchBoundary({
    ...options,
    payload: { ...options.payload, stop_hook_active: true },
  }), { action: 'skip', reason: 'unavailable', detail: 'installed ingestion worker is unavailable' });
  await assert.rejects(() => readFile(join(runRoot(root, workspace), 'native-claim.json')), { code: 'ENOENT' });
  assert.equal((await dispatchBoundary(options)).native_continuation.decision, 'block');
});

test('Codex native boundary skips every no-work state before recording an intent', async () => {
  const cases = [
    { detail: 'wiki_missing', state: ({ wiki }) => ({ wiki_root: join(wiki, 'missing'), hints: [{ kind: 'code_ingest_pending', evidence: { pending_count: 1 } }] }) },
    { detail: 'codegraph_missing', removeCodegraph: true, state: ({ wiki }) => ({ wiki_root: wiki, hints: [{ kind: 'code_ingest_pending', evidence: { pending_count: 1 } }] }) },
    { detail: 'no_pending', state: ({ wiki }) => ({ wiki_root: wiki, hints: [] }) },
    { detail: 'no_actionable_work', entries: [], state: ({ wiki }) => ({ wiki_root: wiki, hints: [{ kind: 'code_ingest_pending', evidence: { pending_count: 1 } }] }) },
  ];
  for (const item of cases) {
    const { root, workspace, wiki, skills } = await fixture();
    if (item.removeCodegraph) await rm(join(wiki, 'code'), { recursive: true });
    await writeFile(join(root, 'config.json'), JSON.stringify({ background_ingest: { visibility: 'native', min_interval_seconds: 0 } }));
    const state = item.state({ wiki });
    const result = await dispatchBoundary({
      harness: 'codex', payload: { cwd: workspace }, globalRoot: root, skillsRoot: skills, env: {},
      readiness: { ready: true, runtimePath: '/private/loam' },
      runtimeRunner: async ({ args }) => args[0] === 'state'
        ? { code: 0, stdout: JSON.stringify(state), stderr: '' }
        : { code: 0, stdout: JSON.stringify(item.entries), stderr: '' },
    });
    assert.deepEqual(result, { action: 'skip', reason: 'nothing_to_do', workspace }, item.detail);
    await assert.rejects(() => readFile(join(runRoot(root, workspace), 'native-intent.json')), { code: 'ENOENT' });
    const stored = JSON.parse(await readFile(join(runRoot(root, workspace), 'last-run.json'), 'utf8'));
    assert.equal(stored.status, 'skipped', item.detail);
    assert.equal(stored.reason, 'nothing_to_do', item.detail);
    assert.equal(stored.detail, item.detail, item.detail);
  }
});

test('Codex native boundary fails closed when its zero-token preflight is unavailable', async () => {
  for (const item of [
    { detail: 'runtime_missing', readiness: { ready: false, category: 'runtime_missing' } },
    { detail: 'probe_timeout', response: { code: null, category: 'timeout', stdout: '', stderr: '' } },
    { detail: 'probe_failed', response: { code: 1, stdout: '', stderr: 'failed' } },
  ]) {
    const { root, workspace, skills } = await fixture();
    await writeFile(join(root, 'config.json'), JSON.stringify({ background_ingest: { visibility: 'native', min_interval_seconds: 0 } }));
    const result = await dispatchBoundary({
      harness: 'codex', payload: { cwd: workspace }, globalRoot: root, skillsRoot: skills, env: {},
      readiness: item.readiness || { ready: true, runtimePath: '/private/loam' },
      runtimeRunner: async () => item.response,
    });
    assert.deepEqual(result, { action: 'skip', reason: 'unavailable', workspace }, item.detail);
    await assert.rejects(() => readFile(join(runRoot(root, workspace), 'native-intent.json')), { code: 'ENOENT' });
    const stored = JSON.parse(await readFile(join(runRoot(root, workspace), 'last-run.json'), 'utf8'));
    assert.equal(stored.detail, item.detail, item.detail);
  }
});

test('Codex native agent revalidates if work disappears after the boundary preflight', async () => {
  const { root, workspace, wiki, skills } = await fixture();
  await nativeInstall(root);
  await writeFile(join(root, 'config.json'), JSON.stringify({ background_ingest: { visibility: 'native', min_interval_seconds: 0 } }));
  let pending = true;
  const readiness = { ready: true, runtimePath: '/private/loam' };
  const runtimeRunner = async ({ args }) => args[0] === 'state'
    ? { code: 0, stdout: JSON.stringify({ wiki_root: wiki, hints: [{ kind: 'code_ingest_pending', evidence: { pending_count: pending ? 1 : 0 } }] }), stderr: '' }
    : { code: 0, stdout: JSON.stringify(pending ? [{ path: 'src/a.js', mtime: '1', reason: 'new' }] : []), stderr: '' };
  const boundary = await dispatchBoundary({
    harness: 'codex', payload: { cwd: workspace }, globalRoot: root, skillsRoot: skills,
    readiness, runtimeRunner, env: {},
  });
  assert.equal(boundary.native_continuation.decision, 'block');
  assert.equal((await bindNativeAgent({ globalRoot: root, workspace, agentId: 'agent-race' })).status, 'bound');

  pending = false;
  assert.deepEqual(await prepareNativeAgentRun({
    globalRoot: root, workspace, agentId: 'agent-race', skillsRoot: skills,
    readiness, runtimeRunner, env: {},
  }), { action: 'skip', reason: 'nothing_to_do' });
  await assert.rejects(() => readFile(join(runRoot(root, workspace), 'lease.json')), { code: 'ENOENT' });
  const finalized = await finalizeNativeAgentRun({ globalRoot: root, workspace, agentId: 'agent-race', env: {} });
  assert.equal(finalized.reason, 'nothing_to_do');
  assert.equal(finalized.owns_claim, true);
});

test('Codex native race admits only the first lease holder in either ordering', async () => {
  for (const order of ['fallback-first', 'native-first']) {
    const { root, workspace, wiki, skills } = await fixture();
    let diffCalls = 0;
    const runtimeRunner = async ({ args }) => args[0] === 'state'
      ? { code: 0, stdout: JSON.stringify({ wiki_root: wiki, hints: [{ kind: 'code_ingest_pending', evidence: { pending_count: 1 } }] }), stderr: '' }
      : { code: 0, stdout: JSON.stringify(diffCalls++ === 0 ? [{ path: 'src/a.js', mtime: '1', reason: 'new' }] : []), stderr: '' };
    const options = {
      harness: 'codex', workspace, globalRoot: root, skillsRoot: skills,
      readiness: { ready: true, runtimePath: '/private/loam' }, runtimeRunner,
      env: { LOAM_INGEST_BACKGROUND: '1' },
    };

    const first = await prepareWorkerRun(options);
    const late = await prepareWorkerRun(options);
    assert.equal(first.action, 'run', order);
    assert.deepEqual(late, { action: 'skip', result: { reason: 'busy' } }, order);
    await finalizeWorkerRun(first, { launch: { background: false }, result: { code: 0 } });
    await assert.rejects(() => readFile(join(runRoot(root, workspace), 'lease.json')), { code: 'ENOENT' });
  }
});

test('Codex loam_ingestor binding prepares first and finalizes from verified state', async () => {
  const { root, workspace, wiki, skills } = await fixture();
  const { adapterRoot, integrationPath } = await nativeInstall(root);
  await writeFile(join(root, 'config.json'), JSON.stringify({ background_ingest: { visibility: 'native', min_interval_seconds: 0 } }));
  let ingested = false;
  const readiness = { ready: true, runtimePath: '/private/loam' };
  const runtimeRunner = async ({ args }) => args[0] === 'state'
    ? { code: 0, stdout: JSON.stringify({ wiki_root: wiki, hints: [{ kind: 'code_ingest_pending', evidence: { pending_count: ingested ? 0 : 1 } }] }), stderr: '' }
    : { code: 0, stdout: JSON.stringify(ingested ? [] : [{ path: 'src/a.js', mtime: '1', reason: 'new' }]), stderr: '' };

  const boundary = {
    harness: 'codex', payload: { cwd: workspace, session_id: 'parent-1' },
    globalRoot: root, skillsRoot: skills, readiness, runtimeRunner, env: {},
  };
  assert.equal((await dispatchBoundary(boundary)).native_continuation.decision, 'block');
  const bound = await bindNativeAgent({ globalRoot: root, workspace, agentId: 'agent-1' });
  assert.equal(bound.status, 'bound');
  assert.equal(bound.owns_claim, true);
  assert.equal(bound.integration_path, integrationPath);
  assert.equal(bound.adapter_path, adapterRoot);
  assert.equal(bound.worker_path, join(adapterRoot, 'ingest-worker.mjs'));
  const claim = JSON.parse(await readFile(join(runRoot(root, workspace), 'native-claim.json'), 'utf8'));
  assert.equal(claim.claim, 'agent');
  assert.equal(claim.agent_id, 'agent-1');

  const prepared = await prepareNativeAgentRun({
    globalRoot: root, workspace, agentId: 'agent-1', skillsRoot: skills,
    readiness, runtimeRunner, env: {},
  });
  assert.deepEqual(prepared, { action: 'run' });
  const lease = JSON.parse(await readFile(join(runRoot(root, workspace), 'lease.json'), 'utf8'));
  assert.equal(lease.launch_mode, 'codex_native');
  assert.equal(lease.child_identity.agent_id, 'agent-1');
  const agentFile = (await readdir(runRoot(root, workspace))).find((name) => name.startsWith('native-agent-'));
  const agentRecord = JSON.parse(await readFile(join(runRoot(root, workspace), agentFile), 'utf8'));
  await writeFile(join(runRoot(root, workspace), agentFile), JSON.stringify({ ...agentRecord, expires_at: 0 }));

  ingested = true;
  const finalized = await finalizeNativeAgentRun({
    globalRoot: root, workspace, agentId: 'agent-1', runtimeRunner, env: {},
  });
  assert.equal(finalized.reason, 'ok');
  assert.equal(finalized.owns_claim, true);
  const stored = JSON.parse(await readFile(join(runRoot(root, workspace), 'last-run.json'), 'utf8'));
  assert.equal(stored.status, 'ok');
  await assert.rejects(() => readFile(join(runRoot(root, workspace), 'lease.json')), { code: 'ENOENT' });
  assert.deepEqual(await finalizeNativeAgentRun({ globalRoot: root, workspace, agentId: 'agent-1', env: {} }), { reason: 'busy' });
});

test('Codex loam_ingestor preparation skips after fallback wins and missing or malformed intents stay inert', async () => {
  const { root, workspace, wiki, skills } = await fixture();
  await nativeInstall(root);
  await writeFile(join(root, 'config.json'), JSON.stringify({ background_ingest: { visibility: 'native', min_interval_seconds: 0 } }));
  const readiness = { ready: true, runtimePath: '/private/loam' };
  const runtimeRunner = async ({ args }) => args[0] === 'state'
    ? { code: 0, stdout: JSON.stringify({ wiki_root: wiki, hints: [{ kind: 'code_ingest_pending', evidence: { pending_count: 1 } }] }), stderr: '' }
    : { code: 0, stdout: JSON.stringify([{ path: 'src/a.js', mtime: '1', reason: 'new' }]), stderr: '' };
  const options = {
    harness: 'codex', payload: { cwd: workspace, session_id: 'parent-2' }, globalRoot: root,
    skillsRoot: skills, readiness, runtimeRunner, env: {}, spawn: () => ({ pid: 1 }),
  };
  assert.equal((await dispatchBoundary(options)).native_continuation.decision, 'block');
  assert.equal((await dispatchBoundary({ ...options, payload: { ...options.payload, stop_hook_active: true } })).native_fallback, true);
  const late = await bindNativeAgent({ globalRoot: root, workspace, agentId: 'agent-late' });
  assert.equal(late.status, 'late');
  assert.equal(late.owns_claim, false);

  const fallback = await prepareWorkerRun({
    harness: 'codex', workspace, globalRoot: root, skillsRoot: skills,
    readiness, runtimeRunner, env: {},
  });
  assert.equal(fallback.action, 'run');
  assert.deepEqual(await prepareNativeAgentRun({
    globalRoot: root, workspace, agentId: 'agent-late', skillsRoot: skills,
    readiness, runtimeRunner, env: {},
  }), { action: 'skip', reason: 'busy' });
  await finalizeWorkerRun(fallback, { launch: { background: false }, result: { code: 0 } });

  const missingWorkspace = join(root, 'missing-workspace');
  await mkdir(missingWorkspace);
  assert.deepEqual(await bindNativeAgent({ globalRoot: root, workspace: missingWorkspace, agentId: 'agent-none' }), { status: 'missing' });
  await mkdir(runRoot(root, missingWorkspace), { recursive: true });
  await writeFile(join(runRoot(root, missingWorkspace), 'native-intent.json'), '{malformed');
  assert.deepEqual(await bindNativeAgent({ globalRoot: root, workspace: missingWorkspace, agentId: 'agent-bad' }), { status: 'malformed' });
  assert.deepEqual(await bindNativeAgent({ globalRoot: root, workspace, agentId: 'bad\nagent' }), { status: 'invalid' });
});

test('Codex loam_ingestor stop aborts an unprepared bound intent without trusting assistant text', async () => {
  const { root, workspace, wiki, skills } = await fixture();
  await nativeInstall(root);
  await writeFile(join(root, 'config.json'), JSON.stringify({ background_ingest: { visibility: 'native' } }));
  assert.equal((await dispatchBoundary({
    harness: 'codex', payload: { cwd: workspace }, globalRoot: root, skillsRoot: skills, env: {},
    ...actionableRuntime(wiki),
  })).native_continuation.decision, 'block');
  await bindNativeAgent({ globalRoot: root, workspace, agentId: 'agent-abort' });
  const aborted = await finalizeNativeAgentRun({ globalRoot: root, workspace, agentId: 'agent-abort', env: {} });
  assert.equal(aborted.reason, 'unavailable');
  assert.equal(aborted.owns_claim, true);
});

test('worker leases before full state and issues the exact exclusions-aware diff argv', async () => {
  const { root, workspace, wiki, skills } = await fixture();
  const calls = [];
  let modelCalls = 0;
  const readiness = { ready: true, runtimePath: '/private/loam' };
  const runtimeRunner = async ({ args }) => {
    calls.push(args);
    if (args[0] === 'state') {
      return {
        code: 0,
        stdout: JSON.stringify({
          wiki_root: wiki,
          hints: [{ kind: 'code_ingest_pending', evidence: { pending_count: 1 } }],
        }),
        stderr: '',
      };
    }
    return { code: 0, stdout: JSON.stringify([{ path: 'src/a.js', mtime: '1', reason: 'new' }]), stderr: '' };
  };
  const result = await runWorker({
    harness: 'opencode', workspace, globalRoot: root, skillsRoot: skills,
    readiness, runtimeRunner, env: { LOAM_INGEST_BACKGROUND: '1' },
    modelRunner: async ({ lease }) => {
      modelCalls += 1;
      assert.equal(lease.launch_state, 'planned');
      assert.equal(lease.actionable_fingerprint.length, 64);
      return { completion: Promise.resolve({ code: 0 }) };
    },
  });
  assert.equal(result.reason, 'ok');
  assert.equal(modelCalls, 1);
  assert.deepEqual(calls[0], ['state', workspace]);
  assert.deepEqual(calls[1], [
    'codegraph', 'diff', workspace, wiki, '--exclusions', join(skills, 'loam-ingesting-codebase', 'references', 'ingestion-exclusions.md'),
  ]);
  assert.equal(calls[2][0], 'state');
  assert.equal(calls[2].includes('--fast'), false);
  assert.equal(await readFile(join(runRoot(root, workspace), 'last-run.json'), 'utf8').then((value) => value.includes('"status":"failed"')), true);
});

test('worker preparation and finalization expose one reusable safety lifecycle', async () => {
  const lifecycle = await import('../integration/ingest.mjs');
  assert.equal(typeof lifecycle.prepareWorkerRun, 'function');
  assert.equal(typeof lifecycle.finalizeWorkerRun, 'function');
  const { root, workspace, wiki, skills } = await fixture();
  let diffCalls = 0;
  const runtimeRunner = async ({ args }) => {
    if (args[0] === 'state') {
      return { code: 0, stdout: JSON.stringify({ wiki_root: wiki, hints: [{ kind: 'code_ingest_pending', evidence: { pending_count: 1 } }] }), stderr: '' };
    }
    diffCalls += 1;
    return {
      code: 0,
      stdout: JSON.stringify(diffCalls === 1 ? [{ path: 'src/a.js', mtime: '1', reason: 'new' }] : []),
      stderr: '',
    };
  };
  const prepared = await lifecycle.prepareWorkerRun({
    harness: 'codex', workspace, globalRoot: root, skillsRoot: skills,
    readiness: { ready: true, runtimePath: '/private/loam' }, runtimeRunner,
    env: { LOAM_INGEST_BACKGROUND: '1' },
  });
  assert.equal(prepared.action, 'run');
  assert.equal(prepared.intent.lease_id, prepared.lease.lease_id);
  assert.equal(prepared.intent.actionable_fingerprint.length, 64);
  const persistedIntent = JSON.parse(await readFile(join(runRoot(root, workspace), 'lease.json'), 'utf8'));
  assert.equal(persistedIntent.lease_id, prepared.intent.lease_id);
  assert.equal(persistedIntent.actionable_count, 1);

  const result = await lifecycle.finalizeWorkerRun(prepared, {
    launch: { background: false },
    result: { code: 0 },
  });
  assert.equal(result.reason, 'ok');
  // T5: the worker buffered a typed preparation-admitted then finalization event.
  assert.equal(prepared.events.length, 2);
  assert.deepEqual(prepared.events[0], {
    event: 'ingest_preparation', outcome: 'admitted',
    launch_mode: prepared.lease.launch_mode, lease_id: prepared.lease.lease_id,
    actionable_digest: prepared.fingerprint.fingerprint, actionable_count: 1,
    deadline_ms: prepared.events[0].deadline_ms,
  });
  assert.ok(Number.isInteger(prepared.events[0].deadline_ms) && prepared.events[0].deadline_ms > 0);
  assert.equal(prepared.events[1].event, 'ingest_finalization');
  assert.equal(prepared.events[1].outcome, 'ok');
  assert.equal(prepared.events[1].pre_digest, prepared.fingerprint.fingerprint);
  assert.equal(prepared.events[1].post_digest.length, 64);
  assert.equal(prepared.events[1].actionable_count, 1);
  assert.equal(JSON.parse(await readFile(join(runRoot(root, workspace), 'last-run.json'), 'utf8')).status, 'ok');
  await assert.rejects(() => readFile(join(runRoot(root, workspace), 'lease.json')), (error) => error.code === 'ENOENT');

  const skippedRoot = join(root, 'skip');
  const skipped = await lifecycle.prepareWorkerRun({
    harness: 'codex', workspace, globalRoot: skippedRoot, skillsRoot: skills,
    readiness: { ready: true, runtimePath: '/private/loam' },
    runtimeRunner: async () => ({ code: 0, stdout: JSON.stringify({ wiki_root: '', hints: [] }), stderr: '' }),
    env: { LOAM_INGEST_BACKGROUND: '1' },
  });
  assert.deepEqual(skipped, {
    action: 'skip',
    result: {
      reason: 'nothing_to_do',
      events: [{ event: 'ingest_preparation', outcome: 'skipped', reason: 'nothing_to_do' }],
    },
  });
  await assert.rejects(() => readFile(join(runRoot(skippedRoot, workspace), 'lease.json')), (error) => error.code === 'ENOENT');
});

test('worker preparation preserves skip classifications and never strands its lease', async () => {
  const { prepareWorkerRun } = await import('../integration/ingest.mjs');
  const cases = [
    { name: 'disabled', expected: 'disabled', detail: undefined, config: { enabled: false }, env: {} },
    { name: 'backoff', expected: 'too_soon', detail: 'backoff', previous: { schema: 1, backoff_until: Date.now() + 60_000 } },
    { name: 'debounced', expected: 'too_soon', detail: 'debounced', previous: { schema: 1, status: 'ok', completed_at: Date.now() } },
    { name: 'runtime', expected: 'unavailable', detail: 'runtime_missing', readiness: { ready: false, category: 'runtime_missing' } },
    { name: 'probe-timeout', expected: 'unavailable', detail: 'probe_timeout', stateResponse: { code: null, category: 'timeout', stdout: '', stderr: '' } },
    { name: 'probe-malformed', expected: 'unavailable', detail: 'malformed_state', stateResponse: { code: 0, stdout: '{', stderr: '' } },
    { name: 'probe-failed', expected: 'unavailable', detail: 'probe_failed', stateResponse: { code: 1, stdout: '', stderr: 'failed' } },
    { name: 'wiki', expected: 'nothing_to_do', detail: 'wiki_missing', state: { wiki_root: '', hints: [] } },
    { name: 'codegraph', expected: 'nothing_to_do', detail: 'codegraph_missing', removeCodegraph: true },
    { name: 'pending', expected: 'nothing_to_do', detail: 'no_pending', state: { hints: [] } },
    { name: 'exclusions', expected: 'unavailable', detail: 'exclusions_unavailable', missingSkills: true },
    { name: 'diff-timeout', expected: 'unavailable', detail: 'probe_timeout', diffResponse: { code: null, category: 'timeout', stdout: '', stderr: '' } },
    { name: 'diff-failed', expected: 'unavailable', detail: 'probe_failed', diffResponse: { code: 1, stdout: '', stderr: 'failed' } },
    { name: 'diff-malformed', expected: 'unavailable', detail: 'malformed_state', diffResponse: { code: 0, stdout: '{', stderr: '' } },
    { name: 'fingerprint', expected: 'unavailable', detail: 'fingerprint_unavailable', entries: [{ path: '../outside.js', mtime: '1', reason: 'new' }] },
    { name: 'empty', expected: 'nothing_to_do', detail: 'no_actionable_work', entries: [] },
  ];
  for (const item of cases) {
    const { root, workspace, wiki, skills } = await fixture();
    const globalRoot = join(root, `global-${item.name}`);
    await mkdir(globalRoot, { recursive: true });
    if (item.config) await writeFile(join(globalRoot, 'config.json'), JSON.stringify({ background_ingest: item.config }));
    if (item.previous) {
      await mkdir(runRoot(globalRoot, workspace), { recursive: true });
      await writeFile(join(runRoot(globalRoot, workspace), 'last-run.json'), JSON.stringify(item.previous));
    }
    if (item.removeCodegraph) await rm(join(wiki, 'code'), { recursive: true });
    const defaultState = {
      wiki_root: wiki,
      hints: [{ kind: 'code_ingest_pending', evidence: { pending_count: 1 } }],
    };
    const state = { ...defaultState, ...(item.state || {}) };
    const runtimeRunner = async ({ args }) => {
      if (args[0] === 'state') {
        return item.stateResponse || { code: 0, stdout: JSON.stringify(state), stderr: '' };
      }
      return item.diffResponse || {
        code: 0,
        stdout: JSON.stringify(item.entries === undefined ? [{ path: 'src/a.js', mtime: '1', reason: 'new' }] : item.entries),
        stderr: '',
      };
    };
    const skipped = await prepareWorkerRun({
      harness: 'codex', workspace, globalRoot,
      skillsRoot: item.missingSkills ? join(root, 'missing-skills') : skills,
      readiness: item.readiness || { ready: true, runtimePath: '/private/loam' },
      runtimeRunner, env: item.env || { LOAM_INGEST_BACKGROUND: '1' },
    });
    assert.deepEqual(skipped, {
      action: 'skip',
      result: {
        reason: item.expected,
        events: [{ event: 'ingest_preparation', outcome: 'skipped', reason: item.expected }],
      },
    }, item.name);
    const stored = JSON.parse(await readFile(join(runRoot(globalRoot, workspace), 'last-run.json'), 'utf8'));
    assert.equal(stored.status, 'skipped', item.name);
    assert.equal(stored.detail, item.detail, item.name);
    await assert.rejects(() => readFile(join(runRoot(globalRoot, workspace), 'lease.json')), (error) => error.code === 'ENOENT');
  }
});

test('maintenance worker distinguishes missing wiki and missing codegraph before diff or model launch', async () => {
  const { root, workspace, skills, wiki } = await fixture();
  const cases = [
    { name: 'wiki_missing', state: { wiki_root: '', hints: [{ kind: 'code_ingest_pending', evidence: { pending_count: 1 } }] } },
    { name: 'wiki_missing', state: { wiki_root: join(root, 'missing-wiki'), hints: [{ kind: 'code_ingest_pending', evidence: { pending_count: 1 } }] } },
    { name: 'codegraph_missing', state: { wiki_root: wiki, hints: [{ kind: 'code_ingest_pending', evidence: { pending_count: 1 } }] }, removeCode: true },
  ];
  for (const item of cases) {
    if (item.removeCode) await rm(join(wiki, 'code'), { recursive: true, force: true });
    const calls = [];
    let modelCalls = 0;
    const result = await runWorker({
      harness: 'codex', workspace, globalRoot: join(root, item.name), skillsRoot: skills,
      readiness: { ready: true, runtimePath: '/private/loam' }, env: { LOAM_INGEST_BACKGROUND: '1' },
      runtimeRunner: async ({ args }) => {
        calls.push(args);
        return { code: 0, stdout: JSON.stringify({ ...item.state, hints: item.state.hints }), stderr: '' };
      },
      modelRunner: async () => { modelCalls += 1; return { completion: Promise.resolve({ code: 0 }) }; },
    });
    assert.equal(result.reason, 'nothing_to_do');
    assert.equal(modelCalls, 0);
    assert.equal(calls.length, 1);
    assert.equal(calls[0][0], 'state');
    if (item.removeCode) await assert.rejects(() => readFile(join(wiki, 'code')));
  }
});

test('a live lease blocks before runtime, capability, or model work', async () => {
  const { root, workspace, skills } = await fixture();
  const leaseRoot = runRoot(root, workspace);
  await mkdir(leaseRoot, { recursive: true });
  const identity = await childIdentity(process.pid);
  await writeFile(join(leaseRoot, 'lease.json'), JSON.stringify({
    schema: 1, lease_id: 'live', owner_pid: process.pid, ...identity, started_at: Date.now(),
  }));
  let runtimeCalls = 0;
  const result = await runWorker({
    harness: 'codex', workspace, globalRoot: root, skillsRoot: skills,
    runtimeRunner: async () => { runtimeCalls += 1; return { code: 0, stdout: '{}', stderr: '' }; },
    modelRunner: async () => { throw new Error('model must not launch'); },
  });
  assert.equal(result.reason, 'busy');
  assert.equal(runtimeCalls, 0);
});

test('a complete no-progress attempt backs off without fingerprint suppression state', async () => {
  const { root, workspace, wiki, skills } = await fixture();
  let modelCalls = 0;
  const runtimeRunner = async ({ args }) => args[0] === 'state'
    ? { code: 0, stdout: JSON.stringify({ wiki_root: wiki, hints: [{ kind: 'code_ingest_pending', evidence: { pending_count: 1 } }] }), stderr: '' }
    : { code: 0, stdout: JSON.stringify([{ path: 'src/a.js', mtime: '1', reason: 'new' }]), stderr: '' };
  const run = () => runWorker({
    harness: 'codex', workspace, globalRoot: root, skillsRoot: skills,
    readiness: { ready: true, runtimePath: '/private/loam' }, runtimeRunner,
    env: { LOAM_INGEST_BACKGROUND: '1' },
    modelRunner: async () => { modelCalls += 1; return { completion: Promise.resolve({ code: 0 }) }; },
  });
  assert.equal((await run()).reason, 'ok');
  assert.equal((await run()).reason, 'too_soon');
  assert.equal(modelCalls, 1);
  const outcome = JSON.parse(await readFile(join(runRoot(root, workspace), 'last-run.json'), 'utf8'));
  assert.equal(outcome.no_progress_count, undefined);
  assert.equal(outcome.suppressed_fingerprint, undefined);
  assert.equal(typeof outcome.backoff_until, 'number');
});

test('a second admitted worker rechecks backoff after acquiring the lease', async () => {
  const { root, workspace, wiki, skills } = await fixture();
  let modelCalls = 0;
  const runtimeRunner = async ({ args }) => args[0] === 'state'
    ? { code: 0, stdout: JSON.stringify({ wiki_root: wiki, hints: [{ kind: 'code_ingest_pending', evidence: { pending_count: 1 } }] }), stderr: '' }
    : { code: 0, stdout: JSON.stringify([{ path: 'src/a.js', mtime: '1', reason: 'new' }]), stderr: '' };
  const options = {
    harness: 'codex', workspace, globalRoot: root, skillsRoot: skills,
    readiness: { ready: true, runtimePath: '/private/loam' }, runtimeRunner,
    env: { LOAM_INGEST_BACKGROUND: '1' },
    modelRunner: async () => {
      modelCalls += 1;
      return { completion: Promise.resolve({ code: 1 }) };
    },
  };
  assert.equal((await runWorker(options)).reason, 'ok');
  assert.equal((await runWorker(options)).reason, 'too_soon');
  assert.equal(modelCalls, 1);
});

test('OpenCode creates a child, records identity before prompt, and verifies terminal ownership', async () => {
  const { root, workspace, wiki, skills } = await fixture();
  const calls = [];
  let statusCalls = 0;
  const result = await runWorker({
    harness: 'opencode', workspace, globalRoot: root, skillsRoot: skills,
    readiness: { ready: true, runtimePath: '/private/loam' }, env: { LOAM_INGEST_BACKGROUND: '1' },
    runtimeRunner: async ({ args }) => args[0] === 'state'
      ? { code: 0, stdout: JSON.stringify({ wiki_root: wiki, hints: [{ kind: 'code_ingest_pending', evidence: { pending_count: 1 } }] }), stderr: '' }
      : { code: 0, stdout: JSON.stringify([{ path: 'src/a.js', mtime: '1', reason: 'new' }]), stderr: '' },
    openCodeSession: {
      parentSessionId: 'parent-1',
      createChild: async (input) => { calls.push(['create', input]); return { id: 'child-1' }; },
      promptAsync: async (input) => { calls.push(['prompt', input]); },
      status: async (id) => { calls.push(['status', id]); statusCalls += 1; return { type: statusCalls === 1 ? 'busy' : 'idle' }; },
      abort: async () => { calls.push(['abort']); },
    },
  });
  assert.equal(result.reason, 'ok');
  assert.equal(calls[0][0], 'create');
  assert.equal(calls[0][1].parentId, 'parent-1');
  assert.equal(calls[1][0], 'prompt');
  assert.equal(calls[1][1].sessionId, 'child-1');
  assert.ok(calls.some(([kind]) => kind === 'status'));
});

test('a dead worker and dead child lease is recovered before the first runtime probe', async () => {
  const { root, workspace, wiki, skills } = await fixture();
  const runPath = runRoot(root, workspace);
  await mkdir(runPath, { recursive: true });
  await writeFile(join(runPath, 'lease.json'), JSON.stringify({
    schema: 1, lease_id: 'old-lease', workspace, harness: 'codex', owner_pid: 999998,
    boot_id: await bootIdentity(), process_start: '1',
    actionable_fingerprint: 'old-fingerprint', launch_mode: 'codex_exec', launch_state: 'launched',
    planned_identity: {}, child_identity: { pid: 999999, boot_id: await bootIdentity(), process_start: '1' },
  }));
  const calls = [];
  const result = await runWorker({
    harness: 'codex', workspace, globalRoot: root, skillsRoot: skills,
    readiness: { ready: true, runtimePath: '/private/loam' }, env: { LOAM_INGEST_BACKGROUND: '1' },
    runtimeRunner: async ({ args }) => {
      calls.push(args);
      return args[0] === 'state'
        ? { code: 0, stdout: JSON.stringify({ wiki_root: wiki, hints: [{ kind: 'code_ingest_pending', evidence: { pending_count: 1 } }] }), stderr: '' }
        : { code: 0, stdout: JSON.stringify([{ path: 'src/a.js', mtime: '1', reason: 'new' }]), stderr: '' };
    },
    modelRunner: async () => ({ completion: Promise.resolve({ code: 0 }) }),
  });
  assert.equal(result.reason, 'ok');
  assert.equal(calls[0][0], 'state');
  assert.equal(await readFile(join(runPath, 'lease.json')).then(() => true).catch(() => false), false);
});

test('expired orphan deadlines are terminal across every launch mode', async () => {
  for (const launchMode of ['opencode_child', 'claude_bg', 'claude_print', 'codex_exec', 'codex_native']) {
    const intent = await inspectIntent({
      present: true,
      malformed: false,
      value: {
        schema: 1,
        launch_mode: launchMode,
        launch_state: 'launched',
        hard_deadline: '1970-01-01T00:00:00.000Z',
        child_identity: { agent_id: 'agent-1' },
      },
    }, '/workspace', undefined, { PATH: '/definitely-missing' });
    assert.equal(intent.state, 'terminal', launchMode);
  }
});

test('expired deadlines do not reclaim a child still known to be live', async () => {
  const processChild = await childIdentity(process.pid);
  const common = {
    schema: 1,
    launch_state: 'launched',
    hard_deadline: '1970-01-01T00:00:00.000Z',
  };
  const processIntent = await inspectIntent({
    present: true,
    malformed: false,
    value: { ...common, launch_mode: 'codex_exec', child_identity: processChild },
  }, '/workspace');
  const openCodeIntent = await inspectIntent({
    present: true,
    malformed: false,
    value: { ...common, launch_mode: 'opencode_child', child_identity: { session_id: 'child-live' } },
  }, '/workspace', { status: async () => ({ type: 'busy' }) });

  assert.equal(processIntent.state, 'live');
  assert.equal(openCodeIntent.state, 'live');
});

test('an expired unknown OpenCode orphan is reclaimed before a new run', async () => {
  const { root, workspace, wiki, skills } = await fixture();
  const runPath = runRoot(root, workspace);
  await mkdir(runPath, { recursive: true });
  await writeFile(join(runPath, 'lease.json'), JSON.stringify({
    schema: 1,
    lease_id: 'expired-opencode',
    workspace,
    harness: 'opencode',
    owner_pid: 999998,
    boot_id: await bootIdentity(),
    process_start: '1',
    launch_mode: 'opencode_child',
    launch_state: 'launched',
    hard_deadline: '1970-01-01T00:00:00.000Z',
    child_identity: { session_id: 'unknown-child' },
  }));
  let launches = 0;
  const result = await runWorker({
    harness: 'opencode', workspace, globalRoot: root, skillsRoot: skills,
    readiness: { ready: true, runtimePath: '/private/loam' }, env: { LOAM_INGEST_BACKGROUND: '1' },
    runtimeRunner: async ({ args }) => args[0] === 'state'
      ? { code: 0, stdout: JSON.stringify({ wiki_root: wiki, hints: [{ kind: 'code_ingest_pending', evidence: { pending_count: 1 } }] }), stderr: '' }
      : { code: 0, stdout: JSON.stringify([{ path: 'src/a.js', mtime: '1', reason: 'new' }]), stderr: '' },
    openCodeSession: {
      parentSessionId: 'new-parent',
      status: async () => ({ type: 'mystery' }),
    },
    modelRunner: async () => { launches += 1; return { completion: Promise.resolve({ code: 0 }) }; },
  });

  assert.equal(result.reason, 'ok');
  assert.equal(launches, 1);
  await assert.rejects(() => readFile(join(runPath, 'lease.json')), { code: 'ENOENT' });
});

test('OpenCode live child keeps the lease and intent when abort/requery cannot verify death', async () => {
  const { root, workspace, wiki, skills } = await fixture();
  const result = await runWorker({
    harness: 'opencode', workspace, globalRoot: root, skillsRoot: skills,
    readiness: { ready: true, runtimePath: '/private/loam' }, env: { LOAM_INGEST_BACKGROUND: '1', LOAM_INGEST_TIMEOUT: '1' },
    runtimeRunner: async ({ args }) => args[0] === 'state'
      ? { code: 0, stdout: JSON.stringify({ wiki_root: wiki, hints: [{ kind: 'code_ingest_pending', evidence: { pending_count: 1 } }] }), stderr: '' }
      : { code: 0, stdout: JSON.stringify([{ path: 'src/a.js', mtime: '1', reason: 'new' }]), stderr: '' },
    openCodeSession: {
      parentSessionId: 'parent-1',
      createChild: async () => ({ id: 'child-live' }),
      promptAsync: async () => {},
      status: async () => ({ type: 'busy' }),
    },
  });
  assert.equal(result.reason, 'busy');
  const runPath = runRoot(root, workspace);
  assert.equal(JSON.parse(await readFile(join(runPath, 'lease.json'), 'utf8')).child_identity.session_id, 'child-live');
});

test('OpenCode prompt transport failure keeps ownership after child identity was published', async () => {
  const { root, workspace, wiki, skills } = await fixture();
  const result = await runWorker({
    harness: 'opencode', workspace, globalRoot: root, skillsRoot: skills,
    readiness: { ready: true, runtimePath: '/private/loam' }, env: { LOAM_INGEST_BACKGROUND: '1' },
    runtimeRunner: async ({ args }) => args[0] === 'state'
      ? { code: 0, stdout: JSON.stringify({ wiki_root: wiki, hints: [{ kind: 'code_ingest_pending', evidence: { pending_count: 1 } }] }), stderr: '' }
      : { code: 0, stdout: JSON.stringify([{ path: 'src/a.js', mtime: '1', reason: 'new' }]), stderr: '' },
    openCodeSession: {
      parentSessionId: 'parent-1',
      createChild: async () => ({ id: 'child-transport-failed' }),
      promptAsync: async () => { throw new Error('ambiguous transport'); },
      status: async () => ({ type: 'busy' }),
    },
  });
  assert.equal(result.reason, 'busy');
  const runPath = runRoot(root, workspace);
  assert.equal(JSON.parse(await readFile(join(runPath, 'lease.json'), 'utf8')).child_identity.session_id, 'child-transport-failed');
});

test('published adapters load only the staged integration after the source tree disappears', async () => {
  const root = await mkdtemp(join(tmpdir(), 'loam-published-'));
  const integrationRoot = join(root, 'integration', 'version');
  const adapterRoot = join(root, 'plugins', 'version');
  await mkdir(integrationRoot, { recursive: true });
  await mkdir(adapterRoot, { recursive: true });
  for (const name of await readdir(new URL('../integration/', import.meta.url))) {
    if (name.endsWith('.mjs')) await writeFile(join(integrationRoot, name), await readFile(new URL(`../integration/${name}`, import.meta.url)));
  }
  for (const name of ['claude-stop.mjs', 'codex-stop.mjs', 'ingest-worker.mjs', 'ingest-modules.mjs', 'opencode.mjs']) {
    await writeFile(join(adapterRoot, name), await readFile(new URL(`../adapters/${name}`, import.meta.url)));
  }
  const previousPath = process.env.LOAM_INTEGRATION_PATH;
  process.env.LOAM_INTEGRATION_PATH = join(integrationRoot, 'loam.mjs');
  const env = { ...process.env, LOAM_INGEST_BACKGROUND: '0', LOAM_INGEST_GLOBAL_ROOT: join(root, 'global'), LOAM_INGEST_SKILLS_ROOT: join(root, 'skills') };
  try {
    const claude = await import(`${pathToFileURL(join(adapterRoot, 'claude-stop.mjs')).href}?test=claude`);
    assert.equal((await claude.main({ env, payload: { cwd: root } })).reason, 'disabled');
    assert.equal((await claude.main({
      env: { ...env, LOAM_INGEST_BACKGROUND: '1' },
      payload: { cwd: root, agent_type: 'loam:ingestor' },
    })).reason, 'disabled');
    const codex = await import(`${pathToFileURL(join(adapterRoot, 'codex-stop.mjs')).href}?test=codex`);
    assert.deepEqual(await codex.main({ env, input: { cwd: root } }), {});
    const stdout = await new Promise((resolvePromise, reject) => {
      const child = spawn(process.execPath, [join(adapterRoot, 'codex-stop.mjs')], {
        env, stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true,
      });
      let output = '';
      let error = '';
      child.stdout.on('data', (chunk) => { output += chunk; });
      child.stderr.on('data', (chunk) => { error += chunk; });
      child.once('error', reject);
      child.once('close', (code) => code === 0 ? resolvePromise(output) : reject(new Error(error)));
      child.stdin.end(JSON.stringify({ cwd: root, session_id: 'verify', stop_hook_active: false }));
    });
    assert.equal(stdout, '{}');
    const worker = await import(`${pathToFileURL(join(adapterRoot, 'ingest-worker.mjs')).href}?test=worker`);
    assert.equal(typeof worker.main, 'function');
    const opencode = await import(`${pathToFileURL(join(adapterRoot, 'opencode.mjs')).href}?test=opencode`);
    assert.equal(typeof opencode.createOpenCodeAdapter, 'function');
  } finally {
    if (previousPath === undefined) delete process.env.LOAM_INTEGRATION_PATH;
    else process.env.LOAM_INTEGRATION_PATH = previousPath;
  }
});

test('process descriptors quote Windows batch commands without a shell', async () => {
  const descriptor = processDescriptor({
    command: process.execPath,
    args: ['--version'],
    platform: process.platform,
  });
  assert.equal(descriptor.shell, false);
  assert.equal(descriptor.executable, process.execPath);
  const batch = join(tmpdir(), 'loam-tool.cmd');
  await writeFile(batch, '@echo off\n');
  const windows = processDescriptor({
    command: batch,
    args: ['path with spaces & punctuation'],
    platform: 'win32',
    env: { ComSpec: process.execPath, SystemRoot: '/Windows' },
  });
  assert.equal(windows.kind, 'cmd');
  assert.deepEqual(windows.args, ['/d', '/s', '/c', `""${batch}" "path with spaces & punctuation""`]);
  assert.equal(windows.windowsVerbatimArguments, true);
  const quoted = processDescriptor({
    command: batch,
    args: ['say "yes"', '!value!'],
    platform: 'win32',
    env: { ComSpec: process.execPath },
  });
  assert.equal(quoted.args[3], `""${batch}" "say ""yes""" "!value!""`);
  assert.throws(() => processDescriptor({
    command: batch,
    args: ['%PATH%'],
    platform: 'win32',
    env: { ComSpec: process.execPath },
  }), /percent expansion/);
  assert.throws(() => processDescriptor({
    command: batch, platform: 'win32', env: { ComSpec: join(tmpdir(), 'missing-cmd.exe') },
  }), /ComSpec/);
  assert.equal(resolveExecutable(process.execPath), process.execPath);
});

test('Windows batch launch executes from a spaced path', { skip: process.platform !== 'win32' }, async () => {
  const root = await mkdtemp(join(tmpdir(), 'loam batch '));
  const batch = join(root, 'echo args.cmd');
  try {
    await writeFile(batch, '@echo off\r\necho %~1^|%~2\r\n');
    const started = startTracked({
      command: batch,
      args: ['alpha beta', 'gamma'],
      cwd: root,
      env: { ...process.env },
      timeoutMs: 10_000,
    });
    const result = await started.completion;
    assert.equal(result.code, 0, result.stderr);
    assert.equal(result.stdout.trim(), 'alpha beta|gamma');
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test('Claude uses a help-only capability check, falls back cleanly, and receives the source-safety prompt', async () => {
  const { root, workspace, wiki, skills } = await fixture();
  await writeFile(join(root, 'config.json'), JSON.stringify({ background_ingest: { visibility: 'toast' } }));
  const bin = await mkdtemp(join(tmpdir(), 'loam-claude-'));
  const command = join(bin, process.platform === 'win32' ? 'claude.cmd' : 'claude');
  const script = process.platform === 'win32' ? join(bin, 'claude-shim.cjs') : command;
  const calls = join(bin, 'calls.jsonl');
  await writeFile(script, `#!/usr/bin/env node
const fs = require('node:fs');
const args = process.argv.slice(2);
fs.appendFileSync(process.env.LOAM_TEST_CALLS, JSON.stringify(args) + '\\n');
if (args[0] === '--help') { process.stdout.write('--bg'); process.exit(0); }
process.exit(args[0] === '--bg' ? 1 : 0);
`);
  if (process.platform === 'win32') {
    await writeFile(command, `@echo off\r\n"${process.execPath}" "${script}" %*\r\n`);
  } else {
    await chmod(command, 0o700);
  }
  const source = await readFile(join(workspace, 'src', 'a.js'), 'utf8');
  const notifications = [];
  const result = await runWorker({
    harness: 'claude', workspace, globalRoot: root, skillsRoot: skills,
    readiness: { ready: true, runtimePath: '/private/loam' },
    env: { ...process.env, PATH: [bin, process.env.PATH || ''].filter(Boolean).join(delimiter), LOAM_TEST_CALLS: calls, LOAM_INGEST_BACKGROUND: '1' },
    runtimeRunner: async ({ args }) => args[0] === 'state'
      ? { code: 0, stdout: JSON.stringify({ wiki_root: wiki, hints: [{ kind: 'code_ingest_pending', evidence: { pending_count: 1 } }] }), stderr: '' }
      : { code: 0, stdout: JSON.stringify([{ path: 'src/a.js', mtime: '1', reason: 'new' }]), stderr: '' },
    notify: async (event) => {
      if (event.phase === 'launch' && event.launchMode === 'claude_bg') notifications.push(event);
    },
  });
  const argv = (await readFile(calls, 'utf8')).trim().split('\n').map(JSON.parse);
  assert.equal(result.reason, 'ok');
  assert.deepEqual(argv.map((args) => args[0]), ['--help', '--bg', '-p']);
  assert.match(argv.at(-1)[1], /Do not modify source files, commit, or push/u);
  assert.match(argv.at(-1)[1], /Do not spawn other agents or subagents\./u);
  assert.equal(notifications.length, 0, 'failed background registration must not report a planned Agent View name');
  assert.equal(await readFile(join(workspace, 'src', 'a.js'), 'utf8'), source);
});

test('Claude loam:ingestor uses a unique registered Agent View session with non-interactive launch arguments', async () => {
  const { root, workspace, wiki, skills } = await fixture();
  const bin = await mkdtemp(join(tmpdir(), 'loam-claude-visible-'));
  const command = join(bin, process.platform === 'win32' ? 'claude.cmd' : 'claude');
  const script = process.platform === 'win32' ? join(bin, 'claude-shim.cjs') : command;
  const calls = join(bin, 'calls.jsonl');
  const state = join(bin, 'agent.json');
  await writeFile(script, `#!/usr/bin/env node
const fs = require('node:fs');
const args = process.argv.slice(2);
fs.appendFileSync(process.env.LOAM_TEST_CALLS, JSON.stringify(args) + '\\n');
if (args[0] === '--help') { process.stdout.write('--bg'); process.exit(0); }
if (args[0] === '--bg') {
  let agents = [];
  try { agents = JSON.parse(fs.readFileSync(process.env.LOAM_TEST_AGENT, 'utf8')); } catch {}
  agents.push({ name: args[args.indexOf('--name') + 1], id: 'agent-' + (agents.length + 1), queries: 0 });
  fs.writeFileSync(process.env.LOAM_TEST_AGENT, JSON.stringify(agents));
  process.stdout.write('backgrounded · stdout-wrong');
  process.exit(0);
}
if (args[0] === 'agents') {
  const agents = JSON.parse(fs.readFileSync(process.env.LOAM_TEST_AGENT, 'utf8'));
  agents[agents.length - 1].queries += 1;
  fs.writeFileSync(process.env.LOAM_TEST_AGENT, JSON.stringify(agents));
  process.stdout.write(JSON.stringify([
    { name: 'unrelated-agent', id: 'agent-wrong', status: 'done' },
    ...agents.map((agent, index) => ({
      ...agent,
      status: index === agents.length - 1 && agent.queries === 1 ? 'working' : 'done',
    })),
  ]));
  process.exit(0);
}
process.exit(0);
`);
  if (process.platform === 'win32') {
    await writeFile(command, `@echo off\r\n"${process.execPath}" "${script}" %*\r\n`);
  } else {
    await chmod(command, 0o700);
  }

  const namePrefix = claudeSessionName(workspace);
  const sessionNames = [];
  for (const configuredVisibility of ['toast', 'native', 'silent']) {
    const globalRoot = join(root, `global-${configuredVisibility}`);
    await mkdir(globalRoot, { recursive: true });
    await writeFile(join(globalRoot, 'config.json'), JSON.stringify({ background_ingest: { visibility: configuredVisibility } }));
    const launchEvents = [];
    const result = await runWorker({
      harness: 'claude', workspace, globalRoot, skillsRoot: skills,
      readiness: { ready: true, runtimePath: '/private/loam' },
      env: {
        ...process.env,
        PATH: [bin, process.env.PATH || ''].filter(Boolean).join(delimiter),
        LOAM_TEST_CALLS: calls,
        LOAM_TEST_AGENT: state,
        LOAM_INGEST_BACKGROUND: '1',
      },
      runtimeRunner: async ({ args }) => args[0] === 'state'
        ? { code: 0, stdout: JSON.stringify({ wiki_root: wiki, hints: [{ kind: 'code_ingest_pending', evidence: { pending_count: 1 } }] }), stderr: '' }
        : { code: 0, stdout: JSON.stringify([{ path: 'src/a.js', mtime: '1', reason: 'new' }]), stderr: '' },
      notify: async (event) => { if (event.phase === 'launch') launchEvents.push(event); },
    });
    const argv = (await readFile(calls, 'utf8')).trim().split('\n').map(JSON.parse);
    const background = argv.filter((args) => args[0] === '--bg').at(-1);
    const sessionName = background[background.indexOf('--name') + 1];
    const settings = background[background.indexOf('--settings') + 1];
    sessionNames.push(sessionName);
    assert.equal(result.reason, 'ok');
    assert.equal(background[background.indexOf('--agent') + 1], 'loam:ingestor');
    assert.match(sessionName, new RegExp(`^${namePrefix}-[a-f0-9]{8}$`, 'u'));
    assert.deepEqual(JSON.parse(settings), { worktree: { bgIsolation: 'none' } });
    assert.equal(background[background.indexOf('--setting-sources') + 1], 'user');
    assert.notEqual(background.indexOf('--strict-mcp-config'), -1);
    assert.equal(background[background.indexOf('--permission-mode') + 1], 'dontAsk');
    assert.match(background[background.indexOf('--allowedTools') + 1], /\bSkill\b/u);
    assert.equal(background.at(-2), '--');
    assert.match(background.at(-1), /Run the existing loam::ingesting-codebase skill/u);
    assert.equal(launchEvents.length, configuredVisibility === 'silent' ? 0 : 1);
    if (launchEvents.length) {
      assert.equal(launchEvents[0].launchMode, 'claude_bg');
      assert.equal(launchEvents[0].identity.manager_name, sessionName);
      assert.equal(launchEvents[0].identity.manager_id, `agent-${sessionNames.length}`);
    }
  }
  assert.equal(new Set(sessionNames).size, sessionNames.length);
});

test('Claude ingest sessions are pruned to the newest few and never touch records Loam did not create', async () => {
  const { root, workspace, wiki, skills } = await fixture();
  const bin = await mkdtemp(join(tmpdir(), 'loam-claude-prune-'));
  const command = join(bin, process.platform === 'win32' ? 'claude.cmd' : 'claude');
  const script = process.platform === 'win32' ? join(bin, 'claude-shim.cjs') : command;
  const calls = join(bin, 'calls.jsonl');
  const prefix = claudeSessionName(workspace);
  // Eight prior runs plus the one this worker registers; only the newest five may survive.
  const existing = Array.from({ length: 8 }, (unused, index) => ({
    name: `${prefix}-old${index + 1}`, id: `old-${index + 1}`, state: 'done', startedAt: index + 1,
  }));
  await writeFile(script, `#!/usr/bin/env node
const fs = require('node:fs');
const args = process.argv.slice(2);
fs.appendFileSync(process.env.LOAM_TEST_CALLS, JSON.stringify(args) + '\\n');
if (args[0] === '--help') { process.stdout.write('--bg'); process.exit(0); }
if (args[0] === '--bg') { fs.writeFileSync(process.env.LOAM_TEST_AGENT, args[args.indexOf('--name') + 1]); process.exit(0); }
if (args[0] === 'agents') {
  const current = fs.readFileSync(process.env.LOAM_TEST_AGENT, 'utf8');
  process.stdout.write(JSON.stringify([
    { name: 'someone-elses-agent', id: 'keep-me', state: 'done', startedAt: 0 },
    { name: ${JSON.stringify(prefix)} + '-busy', id: 'still-working', state: 'working', startedAt: 9 },
    ...${JSON.stringify(existing)},
    { name: current, id: 'current', state: 'done', startedAt: 100 },
  ]));
  process.exit(0);
}
process.exit(0);
`);
  if (process.platform === 'win32') {
    await writeFile(command, `@echo off\r\n"${process.execPath}" "${script}" %*\r\n`);
  } else {
    await chmod(command, 0o700);
  }
  const result = await runWorker({
    harness: 'claude', workspace, globalRoot: root, skillsRoot: skills,
    readiness: { ready: true, runtimePath: '/private/loam' },
    env: {
      ...process.env,
      PATH: [bin, process.env.PATH || ''].filter(Boolean).join(delimiter),
      LOAM_TEST_CALLS: calls,
      LOAM_TEST_AGENT: join(bin, 'current.txt'),
      LOAM_INGEST_BACKGROUND: '1',
    },
    runtimeRunner: async ({ args }) => args[0] === 'state'
      ? { code: 0, stdout: JSON.stringify({ wiki_root: wiki, hints: [{ kind: 'code_ingest_pending', evidence: { pending_count: 1 } }] }), stderr: '' }
      : { code: 0, stdout: JSON.stringify([{ path: 'src/a.js', mtime: '1', reason: 'new' }]), stderr: '' },
  });
  const argv = (await readFile(calls, 'utf8')).trim().split('\n').map(JSON.parse);
  const removed = argv.filter((args) => args[0] === 'rm').map((args) => args[1]);
  assert.equal(result.reason, 'ok');
  // Newest five of the nine terminal Loam records survive: current, old8, old7, old6, old5.
  assert.deepEqual(removed, ['old-4', 'old-3', 'old-2', 'old-1']);
  assert.equal(removed.includes('keep-me'), false, 'a record Loam did not create must never be removed');
  assert.equal(removed.includes('still-working'), false, 'a live session must never be removed');
  assert.equal(removed.includes('current'), false, 'this run must never remove its own record');
});

test('Claude downgrade reasons survive every visibility tier and require_visible_worker can refuse fallback', async () => {
  const cases = [
    ['disabled', 'agent_view_disabled', []],
    ['unavailable', 'agent_view_unavailable', ['--help']],
    ['launch_failed', 'agent_view_launch_failed', ['--help', '--bg']],
  ];
  for (const visibility of ['silent', 'toast', 'native']) {
    for (const [mode, reason, refusedCalls] of cases) {
      for (const requireVisibleWorker of [false, true]) {
        const { root, workspace, wiki, skills } = await fixture();
        await writeFile(join(root, 'config.json'), JSON.stringify({
          background_ingest: { visibility, require_visible_worker: requireVisibleWorker },
        }));
        const bin = await mkdtemp(join(tmpdir(), 'loam-claude-downgrade-'));
        const command = join(bin, process.platform === 'win32' ? 'claude.cmd' : 'claude');
        const script = process.platform === 'win32' ? join(bin, 'claude-shim.cjs') : command;
        const calls = join(bin, 'calls.jsonl');
        await writeFile(calls, '');
        await writeFile(script, `#!/usr/bin/env node
const fs = require('node:fs');
const args = process.argv.slice(2);
fs.appendFileSync(process.env.LOAM_TEST_CALLS, JSON.stringify(args) + '\\n');
if (args[0] === '--help') {
  process.stdout.write(process.env.LOAM_TEST_AGENT_VIEW_MODE === 'unavailable' ? 'usage' : '--bg');
  process.exit(0);
}
if (args[0] === '--bg') process.exit(process.env.LOAM_TEST_AGENT_VIEW_MODE === 'launch_failed' ? 1 : 0);
process.exit(0);
`);
        if (process.platform === 'win32') {
          await writeFile(command, `@echo off\r\n"${process.execPath}" "${script}" %*\r\n`);
        } else {
          await chmod(command, 0o700);
        }
        const result = await runWorker({
          harness: 'claude', workspace, globalRoot: root, skillsRoot: skills,
          readiness: { ready: true, runtimePath: '/private/loam' },
          env: {
            ...process.env,
            PATH: [bin, process.env.PATH || ''].filter(Boolean).join(delimiter),
            LOAM_TEST_CALLS: calls,
            LOAM_TEST_AGENT_VIEW_MODE: mode,
            LOAM_INGEST_BACKGROUND: '1',
            ...(mode === 'disabled' ? { CLAUDE_CODE_DISABLE_AGENT_VIEW: '1' } : {}),
          },
          runtimeRunner: async ({ args }) => args[0] === 'state'
            ? { code: 0, stdout: JSON.stringify({ wiki_root: wiki, hints: [{ kind: 'code_ingest_pending', evidence: { pending_count: 1 } }] }), stderr: '' }
            : { code: 0, stdout: JSON.stringify([{ path: 'src/a.js', mtime: '1', reason: 'new' }]), stderr: '' },
        });
        const argv = await readFile(calls, 'utf8').then((value) => value.trim() ? value.trim().split('\n').map(JSON.parse) : []);
        const stored = JSON.parse(await readFile(join(runRoot(root, workspace), 'last-run.json'), 'utf8'));
        assert.equal(stored.downgrade_reason, reason, `${visibility}/${mode}/${requireVisibleWorker}`);
        assert.equal(result.reason, requireVisibleWorker ? 'unavailable' : 'ok');
        assert.deepEqual(
          argv.map((args) => args[0]),
          requireVisibleWorker ? refusedCalls : [...refusedCalls, '-p'],
          `${visibility}/${mode}/${requireVisibleWorker}`,
        );
        if (requireVisibleWorker) assert.equal(stored.status, 'skipped');
      }
    }
  }
});

test('separate workspaces proceed independently while a live workspace lease remains held', async () => {
  const { root, workspace, skills } = await fixture();
  const identity = await childIdentity(process.pid);
  const heldRoot = runRoot(root, workspace);
  await mkdir(heldRoot, { recursive: true });
  await writeFile(join(heldRoot, 'lease.json'), JSON.stringify({
    schema: 1, lease_id: 'held', workspace, harness: 'codex', owner_pid: process.pid, ...identity,
  }));

  const second = join(root, 'workspace-two');
  const secondWiki = join(second, 'wiki');
  await mkdir(join(second, 'src'), { recursive: true });
  await mkdir(join(secondWiki, 'code'), { recursive: true });
  await writeFile(join(second, 'src', 'b.js'), 'export const b = 2;\n');
  let launches = 0;
  const result = await runWorker({
    harness: 'codex', workspace: second, globalRoot: root, skillsRoot: skills,
    readiness: { ready: true, runtimePath: '/private/loam' }, env: { LOAM_INGEST_BACKGROUND: '1' },
    runtimeRunner: async ({ args }) => args[0] === 'state'
      ? { code: 0, stdout: JSON.stringify({ wiki_root: secondWiki, hints: [{ kind: 'code_ingest_pending', evidence: { pending_count: 1 } }] }), stderr: '' }
      : { code: 0, stdout: JSON.stringify([{ path: 'src/b.js', mtime: '1', reason: 'new' }]), stderr: '' },
    modelRunner: async () => { launches += 1; return { completion: Promise.resolve({ code: 0 }) }; },
  });
  assert.equal(result.reason, 'ok');
  assert.equal(launches, 1);
  assert.ok(await readFile(join(heldRoot, 'lease.json'), 'utf8'));
});

test('a live model child on a dead worker lease prevents a second launch', async () => {
  const { root, workspace, skills } = await fixture();
  const child = await childIdentity(process.pid);
  const leaseRoot = runRoot(root, workspace);
  await mkdir(leaseRoot, { recursive: true });
  await writeFile(join(leaseRoot, 'lease.json'), JSON.stringify({
    schema: 1, lease_id: 'orphan-child', workspace, harness: 'codex',
    owner_pid: 999999, boot_id: child.boot_id, process_start: 'dead',
    launch_mode: 'codex_exec', launch_state: 'launched', child_identity: child,
  }));
  let launches = 0;
  const result = await runWorker({
    harness: 'codex', workspace, globalRoot: root, skillsRoot: skills,
    env: { LOAM_INGEST_BACKGROUND: '1' },
    runtimeRunner: async () => { throw new Error('must not probe while child is live'); },
    modelRunner: async () => { launches += 1; },
  });
  assert.equal(result.reason, 'busy');
  assert.equal(launches, 0);
});

test('ingestStatus always returns JSON-shaped state, including malformed leases', async () => {
  const { root, workspace } = await fixture();
  const leaseRoot = runRoot(root, workspace);
  await mkdir(leaseRoot, { recursive: true });
  await writeFile(join(leaseRoot, 'lease.json'), '{malformed');
  const status = await ingestStatus({ globalRoot: root, workspace, env: { LOAM_INGEST_BACKGROUND: '1' } });
  assert.equal(JSON.parse(JSON.stringify(status)).lease_state, 'unknown');
  assert.equal(status.intent_state, 'unknown');
});
