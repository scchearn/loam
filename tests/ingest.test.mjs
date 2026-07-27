import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { chmod, mkdir, mkdtemp, readdir, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { test } from 'node:test';

import { fingerprintActionable } from '../integration/ingest-fingerprint.mjs';
import { gate, ingestStatus, runRoot, runWorker } from '../integration/ingest.mjs';
import { bootIdentity, childIdentity, processDescriptor, resolveExecutable } from '../integration/ingest-process.mjs';

async function fixture() {
  const root = await mkdtemp(join(tmpdir(), 'loam-ingest-'));
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

test('boundary gate is default-off and does not probe native state', async () => {
  const result = await gate({
    harness: 'claude',
    payload: { cwd: await mkdtemp(join(tmpdir(), 'loam-gate-')), event_id: 'same-event' },
    globalRoot: join(tmpdir(), 'loam-gate-global-missing'),
    env: {},
  });
  assert.deepEqual(result, { action: 'skip', reason: 'disabled' });
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
    modelRunner: async ({ intent }) => {
      modelCalls += 1;
      assert.equal(intent.launch_state, 'planned');
      assert.equal(intent.actionable_fingerprint.length, 64);
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
      status: async (id) => { calls.push(['status', id]); statusCalls += 1; return { status: statusCalls === 1 ? 'running' : 'idle' }; },
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
      status: async () => ({ status: 'running' }),
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

test('process descriptors resolve executables and pass Windows batch arguments directly', async () => {
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
  assert.deepEqual(windows.args, ['/d', '/s', '/c', batch, 'path with spaces & punctuation']);
  const quoted = processDescriptor({
    command: batch,
    args: ['{"worktree":{"bgIsolation":"none"}}', '%PATH%', '!value!'],
    platform: 'win32',
    env: { ComSpec: process.execPath },
  });
  assert.deepEqual(quoted.args.slice(4), ['{"worktree":{"bgIsolation":"none"}}', '%PATH%', '!value!']);
  assert.throws(() => processDescriptor({
    command: batch, platform: 'win32', env: { ComSpec: join(tmpdir(), 'missing-cmd.exe') },
  }), /ComSpec/);
  assert.equal(resolveExecutable(process.execPath), process.execPath);
});

test('Claude uses a help-only capability check, falls back cleanly, and receives the source-safety prompt', async () => {
  const { root, workspace, wiki, skills } = await fixture();
  const bin = await mkdtemp(join(tmpdir(), 'loam-claude-'));
  const command = join(bin, 'claude');
  const calls = join(bin, 'calls.jsonl');
  await writeFile(command, `#!/usr/bin/env node
const fs = require('node:fs');
const args = process.argv.slice(2);
fs.appendFileSync(process.env.LOAM_TEST_CALLS, JSON.stringify(args) + '\\n');
if (args[0] === '--help') { process.stdout.write('--bg'); process.exit(0); }
process.exit(args[0] === '--bg' ? 1 : 0);
`);
  await chmod(command, 0o700);
  const source = await readFile(join(workspace, 'src', 'a.js'), 'utf8');
  const result = await runWorker({
    harness: 'claude', workspace, globalRoot: root, skillsRoot: skills,
    readiness: { ready: true, runtimePath: '/private/loam' },
    env: { ...process.env, PATH: `${bin}:${process.env.PATH || ''}`, LOAM_TEST_CALLS: calls, LOAM_INGEST_BACKGROUND: '1' },
    runtimeRunner: async ({ args }) => args[0] === 'state'
      ? { code: 0, stdout: JSON.stringify({ wiki_root: wiki, hints: [{ kind: 'code_ingest_pending', evidence: { pending_count: 1 } }] }), stderr: '' }
      : { code: 0, stdout: JSON.stringify([{ path: 'src/a.js', mtime: '1', reason: 'new' }]), stderr: '' },
  });
  const argv = (await readFile(calls, 'utf8')).trim().split('\n').map(JSON.parse);
  assert.equal(result.reason, 'ok');
  assert.deepEqual(argv.map((args) => args[0]), ['--help', '--bg', '-p']);
  assert.match(argv.at(-1)[1], /Do not modify source files, commit, or push/u);
  assert.equal(await readFile(join(workspace, 'src', 'a.js'), 'utf8'), source);
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
