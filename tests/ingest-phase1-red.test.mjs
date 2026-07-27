import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { spawn } from 'node:child_process';
import { chmod, mkdir, mkdtemp, readFile, readdir, stat, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';

import { createOpenCodeAdapter } from '../adapters/opencode.mjs';
import { main as codexStop } from '../adapters/codex-stop.mjs';
import { ingestStatus } from '../integration/ingest.mjs';
import { childIdentity, terminateChild } from '../integration/ingest-process.mjs';
import { installHarnesses, detectHarnesses } from '../setup/harnesses.mjs';
import { runRoot, runWorker } from '../integration/ingest.mjs';

async function fixture() {
  const root = await mkdtemp(join(tmpdir(), 'loam-phase1-'));
  const workspace = join(root, 'workspace');
  const wiki = join(workspace, 'wiki');
  const skills = join(root, 'skills');
  await mkdir(join(workspace, 'src'), { recursive: true });
  await mkdir(join(wiki, 'code'), { recursive: true });
  await mkdir(join(skills, 'loam-ingesting-codebase', 'references'), { recursive: true });
  await writeFile(join(workspace, 'src', 'a.js'), 'export const a = 1;\n');
  await writeFile(join(skills, 'loam-ingesting-codebase', 'references', 'ingestion-exclusions.md'), '# exclusions\n');
  return { root, workspace, wiki, skills };
}

async function runtime(wiki, args) {
  return args[0] === 'state'
    ? { code: 0, stdout: JSON.stringify({ wiki_root: wiki, hints: [{ kind: 'code_ingest_pending', evidence: { pending_count: 1 } }] }), stderr: '' }
    : { code: 0, stdout: JSON.stringify([{ path: 'src/a.js', mtime: '1', reason: 'new' }]), stderr: '' };
}

test('OpenCode uses the official child/create, 204 prompt, and all-session status contracts', async () => {
  const calls = [];
  let observed;
  const plugin = await createOpenCodeAdapter({
    client: {
      session: {
        create: async (input) => { calls.push(['create', input]); return { id: 'child-204' }; },
        promptAsync: async (input) => { calls.push(['promptAsync', input]); return undefined; },
        status: async (input) => {
          calls.push(['status', input]);
          return { data: { 'child-204': { type: 'idle' } } };
        },
      },
    },
    ingestion: {
      gate: async () => ({ action: 'spawn_worker', workspace: '/workspace' }),
      resolveGlobalRoot: () => '/tmp/loam-phase1-global',
      resolveSkillsRoot: () => '/tmp/loam-phase1-skills',
      runWorker: async ({ openCodeSession }) => {
        const child = await openCodeSession.createChild({ parentId: 'parent-204', title: 'Loam background code ingestion' });
        await openCodeSession.promptAsync({ sessionId: child.id, parts: [{ type: 'text', text: 'prompt' }] });
        observed = await openCodeSession.status(child.id);
      },
    },
  })({ directory: '/workspace' });
  await plugin.event({ event: { type: 'session.idle', sessionID: 'parent-204', id: 'event-204' } });
  await new Promise((resolve) => setImmediate(resolve));
  assert.deepEqual(calls[0], ['create', { query: { directory: '/workspace' }, body: { parentID: 'parent-204', title: 'Loam background code ingestion' } }]);
  assert.deepEqual(calls[1], ['promptAsync', { path: { id: 'child-204' }, query: { directory: '/workspace' }, body: { parts: [{ type: 'text', text: 'prompt' }] } }]);
  assert.deepEqual(calls[2], ['status', { query: { directory: '/workspace' } }]);
  assert.deepEqual(observed, { type: 'idle' });
});

test('OpenCode idle returns before even a slow admission gate settles', async () => {
  let release;
  const gatePromise = new Promise((resolve) => { release = resolve; });
  const plugin = await createOpenCodeAdapter({
    ingestion: {
      gate: async () => gatePromise,
      resolveGlobalRoot: () => '/tmp/loam-phase1-global',
      resolveSkillsRoot: () => '/tmp/loam-phase1-skills',
      runWorker: async () => undefined,
    },
  })({ directory: '/workspace' });
  const event = plugin.event({ event: { type: 'session.idle', sessionID: 'parent', id: 'event' } });
  assert.equal(event, undefined);
  release({ action: 'skip', reason: 'test' });
  await new Promise((resolve) => setImmediate(resolve));
});

test('completed children stay excluded while recent IDs are bounded and evict oldest entries', async () => {
  let gateCalls = 0;
  let runs = 0;
  let childNumber = 0;
  const plugin = await createOpenCodeAdapter({
    client: {
      session: {
        create: async () => ({ id: `child-${++childNumber}` }),
        promptAsync: async () => undefined,
      },
    },
    ingestion: {
      gate: async () => { gateCalls += 1; return { action: 'spawn_worker', workspace: '/workspace' }; },
      resolveGlobalRoot: () => '/tmp/loam-phase1-global',
      resolveSkillsRoot: () => '/tmp/loam-phase1-skills',
      runWorker: async ({ openCodeSession }) => {
        const child = await openCodeSession.createChild({ parentId: 'parent', title: 'test' });
        await openCodeSession.promptAsync({ sessionId: child.id, parts: [] });
        runs += 1;
      },
    },
  })({ directory: '/workspace' });
  await plugin.event({ event: { type: 'session.idle', sessionID: 'parent', id: 'event-1' } });
  await new Promise((resolve) => setImmediate(resolve));
  await plugin.event({ event: { type: 'session.idle', sessionID: 'child-1', id: 'event-2' } });
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(runs, 1);
  assert.equal(gateCalls, 1);

  // Contract: retain at most 64 recent completed child IDs, evicting oldest first.
  for (let index = 2; index <= 65; index += 1) {
    await plugin.event({ event: { type: 'session.idle', sessionID: `parent-${index}`, id: `event-${index}` } });
    await new Promise((resolve) => setImmediate(resolve));
  }
  await plugin.event({ event: { type: 'session.idle', sessionID: 'child-1', id: 'event-evicted' } });
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(runs, 66);
  assert.equal(gateCalls, 66);
});

test('ambiguous OpenCode launch persists host identity with the child identity', async () => {
  const { root, workspace, wiki, skills } = await fixture();
  let statusCalls = 0;
  const result = await runWorker({
    harness: 'opencode', workspace, globalRoot: root, skillsRoot: skills,
    readiness: { ready: true, runtimePath: '/private/loam' },
    env: { LOAM_INGEST_BACKGROUND: '1' },
    runtimeRunner: ({ args }) => runtime(wiki, args),
    openCodeSession: {
      parentSessionId: 'parent',
      createChild: async () => ({ id: 'child-ambiguous' }),
      promptAsync: async () => { throw new Error('transport ambiguous'); },
      status: async () => { statusCalls += 1; return { type: 'busy' }; },
    },
  });
  assert.equal(result.reason, 'orphan_live');
  const runPath = runRoot(root, workspace);
  const intent = JSON.parse(await readFile(join(runPath, 'intent.json'), 'utf8'));
  const lease = JSON.parse(await readFile(join(runPath, 'lease.json'), 'utf8'));
  assert.equal(intent.schema, 1);
  assert.equal(intent.state, 'active');
  assert.equal(intent.launch_state, 'launched');
  assert.equal(intent.child_identity.session_id, 'child-ambiguous');
  assert.equal(intent.child_identity.parent_session_id, 'parent');
  assert.ok(intent.child_identity.host_identity?.pid);
  assert.ok(intent.child_identity.host_identity?.boot_id);
  assert.ok(intent.child_identity.host_identity?.process_start);
  assert.equal(intent.child_identity.launch_token, intent.launch_token);
  assert.equal(lease.lease_id, intent.lease_id);
  assert.ok(statusCalls >= 1);
});

test('Claude capability probe stops and removes its exact registration and avoids inline settings JSON', async () => {
  const { root, workspace, wiki, skills } = await fixture();
  const fakeRoot = await mkdtemp(join(tmpdir(), 'loam-fake-claude-'));
  const fake = join(fakeRoot, 'claude');
  const log = join(fakeRoot, 'calls.jsonl');
  const settingsLog = join(fakeRoot, 'settings.jsonl');
  const observationsLog = join(fakeRoot, 'observations.jsonl');
  const state = join(fakeRoot, 'state.json');
  await writeFile(fake, `#!/usr/bin/env node
const fs = require('node:fs');
const args = process.argv.slice(2);
const log = process.env.LOAM_FAKE_CLAUDE_LOG;
const settingsLog = process.env.LOAM_FAKE_CLAUDE_SETTINGS_LOG;
const observationsLog = process.env.LOAM_FAKE_CLAUDE_OBSERVATIONS_LOG;
const statePath = process.env.LOAM_FAKE_CLAUDE_STATE;
const append = (value) => fs.appendFileSync(log, JSON.stringify(value) + '\\n');
const observe = (value) => {
  const callIndex = fs.readFileSync(log, 'utf8').trim().split('\\n').length - 1;
  fs.appendFileSync(observationsLog, JSON.stringify({ ...value, callIndex }) + '\\n');
};
const read = () => { try { return JSON.parse(fs.readFileSync(statePath, 'utf8')); } catch { return null; } };
const save = (value) => fs.writeFileSync(statePath, JSON.stringify(value));
append(args);
const settingsIndex = args.indexOf('--settings');
if (settingsIndex >= 0) {
  const path = args[settingsIndex + 1];
  let value = null;
  try { value = JSON.parse(fs.readFileSync(path, 'utf8')); } catch {}
  fs.appendFileSync(settingsLog, JSON.stringify({ path, value }) + '\\n');
}
if (args[0] === '--help') { process.stdout.write('--bg --background'); process.exit(0); }
if (args[0] === '--bg') { const name = args[args.indexOf('--name') + 1]; save({ id: 'manager-1', name, status: 'working', queries: 0 }); process.exit(0); }
if (args[0] === 'agents') {
  const current = read();
  if (current?.removed_name) { observe({ state: 'absent', name: current.removed_name, records: [] }); process.stdout.write('[]'); }
  else if (current && current.status === 'stopped' && current.queries >= 2) {
    save({ ...current, terminal_observed: true });
    observe({ state: 'terminal', name: current.name, records: [current] });
    process.stdout.write(JSON.stringify([current]));
  } else if (current && current.queries < 2) {
    save({ ...current, queries: current.queries + 1 });
    observe({ state: 'registration', name: current.name, records: [] });
    process.stdout.write('[]');
  } else {
    observe({ state: 'live', name: current?.name, records: current ? [current] : [] });
    process.stdout.write(JSON.stringify(current ? [current] : []));
  }
  process.exit(0);
}
if (args[0] === 'stop') { const current = read(); save({ ...current, status: 'stopped', queries: 0 }); process.exit(0); }
if (args[0] === 'rm') {
  const current = read();
  if (args[1] === 'manager-1' && current?.terminal_observed) save({ removed_name: current.name });
  else process.exit(2);
  process.exit(0);
}
process.exit(0);
`, { mode: 0o700 });
  await chmod(fake, 0o700);
  const oldPath = process.env.PATH;
  const oldLog = process.env.LOAM_FAKE_CLAUDE_LOG;
  const oldSettingsLog = process.env.LOAM_FAKE_CLAUDE_SETTINGS_LOG;
  const oldObservationsLog = process.env.LOAM_FAKE_CLAUDE_OBSERVATIONS_LOG;
  const oldState = process.env.LOAM_FAKE_CLAUDE_STATE;
  process.env.PATH = `${fakeRoot}:${oldPath || ''}`;
  process.env.LOAM_FAKE_CLAUDE_LOG = log;
  process.env.LOAM_FAKE_CLAUDE_SETTINGS_LOG = settingsLog;
  process.env.LOAM_FAKE_CLAUDE_OBSERVATIONS_LOG = observationsLog;
  process.env.LOAM_FAKE_CLAUDE_STATE = state;
  try {
    await runWorker({
      harness: 'claude', workspace, globalRoot: root, skillsRoot: skills,
      readiness: { ready: true, runtimePath: '/private/loam' },
      env: { ...process.env, LOAM_INGEST_BACKGROUND: '1', LOAM_INGEST_TIMEOUT: '1' },
      runtimeRunner: ({ args }) => runtime(wiki, args),
    });
    const calls = (await readFile(log, 'utf8')).trim().split('\n').map((line) => JSON.parse(line));
    const bgCalls = calls.filter((args) => args[0] === '--bg');
    assert.ok(bgCalls.length >= 2);
    const firstBg = calls.findIndex((args) => args[0] === '--bg');
    const secondBg = calls.findIndex((args, index) => index > firstBg && args[0] === '--bg');
    const settings = (await readFile(settingsLog, 'utf8')).trim().split('\n').filter(Boolean).map((line) => JSON.parse(line));
    assert.ok(settings.length >= 1);
    assert.ok(settings.every((entry) => entry.path.startsWith(runRoot(root, workspace) + '/')));
    assert.ok(settings.every((entry) => entry.value && entry.value.worktree?.bgIsolation === 'none'));
    const probe = calls.slice(firstBg + 1, secondBg);
    const stopIndex = probe.findIndex((args) => args[0] === 'stop');
    const removeIndex = probe.findIndex((args) => args[0] === 'rm');
    const probeName = calls[firstBg][calls[firstBg].indexOf('--name') + 1];
    const observations = (await readFile(observationsLog, 'utf8')).trim().split('\n').filter(Boolean).map((line) => JSON.parse(line));
    const probeObservations = observations.filter((entry) => entry.name === probeName);
    const terminal = probeObservations.find((entry) => entry.state === 'terminal');
    const absent = probeObservations.find((entry) => entry.state === 'absent');
    const removeAbsoluteIndex = firstBg + 1 + removeIndex;
    assert.ok(probe.filter((args) => args[0] === 'agents').length >= 3);
    assert.ok(stopIndex >= 0);
    assert.equal(probe[stopIndex][1], 'manager-1');
    assert.ok(terminal);
    assert.ok(absent);
    assert.ok(terminal.callIndex < removeAbsoluteIndex);
    assert.ok(absent.callIndex > removeAbsoluteIndex);
    assert.ok(removeIndex > stopIndex);
    assert.ok(probe.slice(removeIndex + 1).some((args) => args[0] === 'agents'));
  } finally {
    if (oldPath === undefined) delete process.env.PATH; else process.env.PATH = oldPath;
    if (oldLog === undefined) delete process.env.LOAM_FAKE_CLAUDE_LOG; else process.env.LOAM_FAKE_CLAUDE_LOG = oldLog;
    if (oldSettingsLog === undefined) delete process.env.LOAM_FAKE_CLAUDE_SETTINGS_LOG; else process.env.LOAM_FAKE_CLAUDE_SETTINGS_LOG = oldSettingsLog;
    if (oldObservationsLog === undefined) delete process.env.LOAM_FAKE_CLAUDE_OBSERVATIONS_LOG; else process.env.LOAM_FAKE_CLAUDE_OBSERVATIONS_LOG = oldObservationsLog;
    if (oldState === undefined) delete process.env.LOAM_FAKE_CLAUDE_STATE; else process.env.LOAM_FAKE_CLAUDE_STATE = oldState;
  }
});

test('Codex refuses every shell metacharacter in an installed command path', async () => {
  const metacharacters = [';', '&', '|', '$', '`', '<', '>', '^', '%', '!', '"', "'", '\t', '\n'];
  for (const character of metacharacters) {
    const home = await mkdtemp(join(tmpdir(), 'loam-codex-shell-path-'));
    await mkdir(join(home, '.codex'), { recursive: true });
    const original = { hooks: { Stop: [{ hooks: [{ type: 'command', command: 'node "/opt/other.mjs"' }] }] } };
    await writeFile(join(home, '.codex', 'hooks.json'), JSON.stringify(original));
    const globalRoot = join(home, `global${character}root`);
    const detected = await detectHarnesses({ home });
    const result = await installHarnesses({ home, globalRoot, pluginVersion: '0.8.4', detected });
    assert.equal(result.codex.state, 'partial', `character ${JSON.stringify(character)}`);
    assert.match(result.codex.detail, /unsafe/i);
    assert.deepEqual(JSON.parse(await readFile(join(home, '.codex', 'hooks.json'), 'utf8')), original);
    for (const assetPath of Object.values(result.assets)) assert.equal(await readFile(assetPath).catch(() => null), null);
    assert.equal(await stat(result.versionRoot).catch(() => null), null);
  }
});

test('Codex projects only its three declared fields before resolving workspace state', async () => {
  const root = await mkdtemp(join(tmpdir(), 'loam-codex-payload-'));
  const outside = await mkdtemp(join(tmpdir(), 'loam-codex-outside-'));
  const globalRoot = join(root, 'global');
  await codexStop({
    env: { ...process.env, LOAM_INGEST_BACKGROUND: '0', LOAM_INGEST_GLOBAL_ROOT: globalRoot },
    input: { workspace: { root: outside }, session_id: 'declared-only', undeclared: 'ignore-me' },
  });
  assert.equal(await readFile(join(runRoot(globalRoot, outside), 'log.jsonl'), 'utf8').catch(() => null), null);
});

test('failed Windows termination is reported instead of being treated as death', async () => {
  const root = await mkdtemp(join(tmpdir(), 'loam-taskkill-'));
  const systemRoot = join(root, 'windows');
  const taskkill = join(systemRoot, 'System32', 'taskkill.exe');
  await mkdir(join(systemRoot, 'System32'), { recursive: true });
  await writeFile(taskkill, '#!/usr/bin/env node\nprocess.exit(1);\n', { mode: 0o700 });
  await chmod(taskkill, 0o700);
  const previous = process.env.SystemRoot;
  process.env.SystemRoot = systemRoot;
  try {
    assert.equal(await terminateChild({ pid: 12345 }, { platform: 'win32' }), false);
  } finally {
    if (previous === undefined) delete process.env.SystemRoot; else process.env.SystemRoot = previous;
  }
});

test('status reports malformed ownership as unknown and keeps bounded diagnostics private', async () => {
  const { root, workspace } = await fixture();
  const runPath = runRoot(root, workspace);
  await mkdir(runPath, { recursive: true });
  await writeFile(join(runPath, 'intent.json'), '{malformed');
  const status = await ingestStatus({ globalRoot: root, workspace, env: { LOAM_INGEST_BACKGROUND: '1' } });
  assert.equal(status.intent_state, 'unknown');
  assert.equal(status.orphan, true);
  assert.ok(!JSON.stringify(status.records).includes('Run the existing loam::ingesting-codebase skill'));
});

test('live lease remains held when zero TTL makes its deadline immediately expired', async () => {
  const { root, workspace, skills } = await fixture();
  const identity = await childIdentity(process.pid);
  const runPath = runRoot(root, workspace);
  await mkdir(runPath, { recursive: true });
  await writeFile(join(runPath, 'lease.json'), JSON.stringify({
    schema: 1, lease_id: 'live-zero-ttl', owner_pid: process.pid, ...identity,
    started_at: Date.now(), hard_deadline: new Date(0).toISOString(),
  }));
  const result = await runWorker({
    harness: 'codex', workspace, globalRoot: root, skillsRoot: skills,
    readiness: { ready: true, runtimePath: '/private/loam' },
    env: { ...process.env, LOAM_INGEST_BACKGROUND: '1', LOAM_INGEST_LEASE_TTL: '0' },
    runtimeRunner: async () => { throw new Error('live owner must block before runtime'); },
  });
  assert.equal(result.reason, 'lease_held');
});

test('live OpenCode lease returns held without querying the active child', async () => {
  const { root, workspace, skills } = await fixture();
  const identity = await childIdentity(process.pid);
  const runPath = runRoot(root, workspace);
  await mkdir(runPath, { recursive: true });
  await writeFile(join(runPath, 'lease.json'), JSON.stringify({
    schema: 1, lease_id: 'live-opencode', owner_pid: process.pid, ...identity,
    started_at: Date.now(), hard_deadline: new Date(0).toISOString(),
  }));
  await writeFile(join(runPath, 'intent.json'), JSON.stringify({
    schema: 1, state: 'active', attempt_id: 'active-opencode', lease_id: 'live-opencode',
    launch_token: 'launch-opencode', launch_mode: 'opencode_child', launch_state: 'launched',
    child_identity: { session_id: 'child-live', parent_session_id: 'parent-live' },
  }));
  let statusCalls = 0;
  const result = await runWorker({
    harness: 'opencode', workspace, globalRoot: root, skillsRoot: skills,
    readiness: { ready: true, runtimePath: '/private/loam' },
    env: { ...process.env, LOAM_INGEST_BACKGROUND: '1' },
    runtimeRunner: async () => { throw new Error('live owner must block before runtime'); },
    openCodeSession: {
      parentSessionId: 'parent-live',
      status: async () => { statusCalls += 1; throw new Error('status must not be queried'); },
    },
  });
  assert.equal(result.reason, 'lease_held');
  assert.equal(statusCalls, 0);
});

test('simultaneous stale reclaimers recover a crashed takeover and preserve every event record', async () => {
  const { root, workspace } = await fixture();
  const moduleUrl = new URL('../integration/ingest.mjs', import.meta.url).href;
  const runPath = runRoot(root, workspace);
  await mkdir(runPath, { recursive: true });
  await writeFile(join(runPath, '.ownership.lock'), JSON.stringify({
    schema: 1, token: 'stale-owner', generation: 7, pid: 999999,
    boot_id: 'stale-boot', process_start: 'stale-start', acquired_at: Date.now() - 60000,
  }));
  const takeoverName = `.ownership.lock.takeover-${createHash('sha256').update('stale-owner').digest('hex')}-7`;
  await writeFile(join(runPath, takeoverName), JSON.stringify({
    schema: 1, token: 'crashed-takeover', expected_token: 'stale-owner', expected_generation: 7,
    pid: 999998, boot_id: 'crashed-boot', process_start: 'crashed-start', acquired_at: Date.now() - 60000,
  }));
  await writeFile(join(root, 'config.json'), JSON.stringify({ background_ingest: { enabled: true } }));
  const script = `import { gate } from ${JSON.stringify(moduleUrl)};
const result = await gate({
  harness: 'codex', payload: { cwd: process.env.LOAM_TEST_WORKSPACE, event_id: process.env.LOAM_TEST_EVENT },
  globalRoot: process.env.LOAM_TEST_GLOBAL_ROOT,
  env: { ...process.env, LOAM_INGEST_BACKGROUND: '1', LOAM_INGEST_MIN_INTERVAL: '0' },
});
process.stdout.write(JSON.stringify(result) + '\\n');`;
  const launch = (index) => {
    const child = spawn(process.execPath, ['--input-type=module', '-e', script], {
      env: { ...process.env, LOAM_TEST_WORKSPACE: workspace, LOAM_TEST_GLOBAL_ROOT: root, LOAM_TEST_EVENT: `stale-${index}` },
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    let output = '';
    child.stdout.on('data', (chunk) => { output += chunk; });
    return new Promise((resolve) => child.once('close', (code, signal) => resolve({ code, signal, output })));
  };
  const results = await Promise.all(Array.from({ length: 12 }, (_, index) => launch(index)));
  assert.ok(results.every(({ code }) => code === 0));
  assert.ok(results.every(({ output }) => JSON.parse(output).action === 'spawn_worker'));
  const events = JSON.parse(await readFile(join(runPath, 'events.json'), 'utf8'));
  assert.equal(events.entries.length, 12);
  assert.equal(await readFile(join(runPath, '.ownership.lock')).catch(() => null), null);
  assert.deepEqual((await readdir(runPath)).filter((name) => name.startsWith('.ownership.lock.takeover-')), []);
});

test('workspace ownership distinguishes unknown launch crashes from recoverable dead-owner claims', async () => {
  const moduleUrl = new URL('../integration/ingest.mjs', import.meta.url).href;
  const script = `import { writeFileSync } from 'node:fs';
import { runWorker } from ${JSON.stringify(moduleUrl)};
const workspace = process.env.LOAM_TEST_WORKSPACE;
const globalRoot = process.env.LOAM_TEST_GLOBAL_ROOT;
const wiki = process.env.LOAM_TEST_WIKI;
const skillsRoot = process.env.LOAM_TEST_SKILLS;
const marker = process.env.LOAM_TEST_MARKER;
const result = await runWorker({
  harness: 'codex', workspace, globalRoot, skillsRoot,
  readiness: { ready: true, runtimePath: '/private/loam' },
  env: { ...process.env, LOAM_INGEST_BACKGROUND: '1' },
  runtimeRunner: async ({ args }) => {
    if (args[0] === 'state' && process.env.LOAM_TEST_BLOCK_STATE === '1') {
      writeFileSync(marker, 'state-blocked');
      process.stdout.write('STATE_BLOCKED\\n');
      await new Promise(() => {});
    }
    return args[0] === 'state'
      ? { code: 0, stdout: JSON.stringify({ wiki_root: wiki, hints: [{ kind: 'code_ingest_pending', evidence: { pending_count: 1 } }] }), stderr: '' }
      : { code: 0, stdout: JSON.stringify([{ path: 'src/a.js', mtime: '1', reason: 'new' }]), stderr: '' };
  },
  modelRunner: async () => {
    writeFileSync(marker, 'claimed');
    process.stdout.write('CLAIMED\\n');
    if (process.env.LOAM_TEST_CRASH === '1') await new Promise(() => {});
    await new Promise((resolve) => setTimeout(resolve, Number(process.env.LOAM_TEST_HOLD_MS || 0)));
    return { completion: Promise.resolve({ code: 0 }) };
  },
});
process.stdout.write(JSON.stringify(result) + '\\n');`;
  const launch = ({ root, workspace, wiki, skills }, marker, extra = {}) => {
    const child = spawn(process.execPath, ['--input-type=module', '-e', script], {
      env: {
        ...process.env,
        LOAM_TEST_WORKSPACE: workspace,
        LOAM_TEST_GLOBAL_ROOT: root,
        LOAM_TEST_WIKI: wiki,
        LOAM_TEST_SKILLS: skills,
        LOAM_TEST_MARKER: marker,
        ...extra,
      },
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    let output = '';
    let claimResolve;
    let blockedResolve;
    const claimed = new Promise((resolve) => { claimResolve = resolve; });
    const blocked = new Promise((resolve) => { blockedResolve = resolve; });
    child.stdout.on('data', (chunk) => {
      output += chunk;
      if (output.includes('CLAIMED')) claimResolve();
      if (output.includes('STATE_BLOCKED')) blockedResolve();
    });
    const done = new Promise((resolve) => child.once('close', (code, signal) => {
      claimResolve();
      blockedResolve();
      resolve({ code, signal, output });
    }));
    return { child, claimed, blocked, done };
  };
  const resultOf = ({ output }) => JSON.parse(output.trim().split('\n').at(-1));

  const crashContext = await fixture();
  const crashed = launch(crashContext, join(crashContext.root, 'crashed.claim'), { LOAM_TEST_CRASH: '1' });
  await crashed.claimed;
  crashed.child.kill('SIGKILL');
  await crashed.done;

  const unsafeRecovery = launch(crashContext, join(crashContext.root, 'unsafe-recovery.claim'));
  const unsafeRecoveryResult = resultOf(await unsafeRecovery.done);

  const raceContext = await fixture();
  const owner = launch(raceContext, join(raceContext.root, 'owner.claim'), { LOAM_TEST_BLOCK_STATE: '1' });
  await owner.blocked;
  assert.equal(await readFile(join(raceContext.root, 'owner.claim'), 'utf8'), 'state-blocked');
  const contender = launch(raceContext, join(raceContext.root, 'contender.claim'));
  const contenderResult = resultOf(await contender.done);
  assert.equal(contenderResult.reason, 'lease_held');
  assert.equal(await readFile(join(raceContext.root, 'contender.claim')).catch(() => null), null);

  owner.child.kill('SIGKILL');
  await owner.done;
  const recovered = launch(raceContext, join(raceContext.root, 'recovered.claim'));
  const recoveredResult = resultOf(await recovered.done);
  assert.equal(recoveredResult.reason, 'ok');
  assert.equal(await readFile(join(raceContext.root, 'recovered.claim'), 'utf8'), 'claimed');
  assert.equal(unsafeRecoveryResult.reason, 'orphan_unknown');
});
