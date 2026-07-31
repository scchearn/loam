import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { chmod, mkdir, mkdtemp, readdir, readFile, realpath, rm, stat, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { delimiter, join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { test } from 'node:test';

import { fingerprintActionable } from '../integration/ingest-fingerprint.mjs';
import { claudeSessionName, gate, ingestStatus, readIngestConfig, runRoot, runWorker, startWorker } from '../integration/ingest.mjs';
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
  assert.deepEqual(await gate({ ...options, env: { LOAM_INGEST_WORKER: '1' } }), { action: 'skip', reason: 'disabled' });
  assert.deepEqual(await gate({ ...options, payload: { cwd: workspace, agent_type: 'loam:ingestor' }, env: {} }), { action: 'skip', reason: 'disabled' });
  assert.equal((await gate({ ...options, payload: { cwd: workspace, agent_type: 'other-agent' }, env: {} })).action, 'spawn_worker');
});

test('visibility config accepts supported values and silently normalizes everything else', async () => {
  const globalRoot = await mkdtemp(join(tmpdir(), 'loam-visibility-config-'));
  assert.equal((await readIngestConfig(globalRoot, {})).visibility, 'silent');
  for (const visibility of ['silent', 'toast', 'native']) {
    await writeFile(join(globalRoot, 'config.json'), JSON.stringify({ background_ingest: { visibility } }));
    assert.equal((await readIngestConfig(globalRoot, {})).visibility, visibility);
  }
  await writeFile(join(globalRoot, 'config.json'), JSON.stringify({ background_ingest: { visibility: 'loud' } }));
  assert.equal((await readIngestConfig(globalRoot, {})).visibility, 'silent');
});

test('Claude session name is deterministic, Loam-attributable, and workspace scoped', () => {
  const first = claudeSessionName('/workspace/one');
  assert.equal(first, claudeSessionName('/workspace/one'));
  assert.notEqual(first, claudeSessionName('/workspace/two'));
  assert.match(first, /^loam-ingest-[a-f0-9]{10}$/u);
  assert.doesNotMatch(first, /workspace/u);
});

test('notification launch is non-blocking and terminal status follows persisted outcome', async () => {
  const { root, workspace, wiki, skills } = await fixture();
  await writeFile(join(root, 'config.json'), JSON.stringify({ background_ingest: { visibility: 'toast' } }));
  const calls = [];
  let modelCompleted = false;
  let markLaunchCalled;
  const launchCalled = new Promise((resolvePromise) => { markLaunchCalled = resolvePromise; });
  const startedAt = Date.now();
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
      if (event.phase === 'launch') { markLaunchCalled(); return new Promise(() => {}); }
    },
  });

  await launchCalled;
  assert.equal(modelCompleted, true, 'launch notification must not block worker completion');
  const result = await resultPromise;
  const elapsed = Date.now() - startedAt;
  const stored = JSON.parse(await readFile(join(runRoot(root, workspace), 'last-run.json'), 'utf8'));
  assert.equal(result.reason, 'ok');
  assert.equal(stored.status, 'failed');
  assert.deepEqual(calls.map(({ phase }) => phase), ['launch', 'terminal']);
  assert.equal(calls[0].signal.aborted, true, 'deadline must cancel a hanging notification resource');
  assert.equal(calls[1].signal.aborted, false);
  assert.equal(calls[1].status, stored.status);
  assert.ok(elapsed >= 200 && elapsed < 1000, `bounded notification elapsed ${elapsed}ms`);
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

test('Claude loam:ingestor visibility uses one registered Agent View session name for command and launch event', async () => {
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
  fs.writeFileSync(process.env.LOAM_TEST_AGENT, JSON.stringify({ name: args[args.indexOf('--name') + 1] }));
  process.stdout.write('backgrounded · stdout-wrong');
  process.exit(0);
}
if (args[0] === 'agents') {
  const agent = JSON.parse(fs.readFileSync(process.env.LOAM_TEST_AGENT, 'utf8'));
  process.stdout.write(JSON.stringify([
    { name: 'unrelated-agent', id: 'agent-wrong', status: 'done' },
    { ...agent, id: 'agent-42', status: 'done' },
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

  const expectedName = claudeSessionName(workspace);
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
    assert.equal(result.reason, 'ok');
    assert.equal(background[background.indexOf('--agent') + 1], 'loam:ingestor');
    assert.equal(background[background.indexOf('--name') + 1], expectedName);
    assert.equal(background[background.indexOf('--permission-mode') + 1], 'dontAsk');
    assert.equal(launchEvents.length, configuredVisibility === 'silent' ? 0 : 1);
    if (launchEvents.length) {
      assert.equal(launchEvents[0].launchMode, 'claude_bg');
      assert.equal(launchEvents[0].identity.manager_name, expectedName);
      assert.equal(launchEvents[0].identity.manager_id, 'agent-42');
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
