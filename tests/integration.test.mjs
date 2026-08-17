import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { chmod, mkdir, readFile, rm, symlink, writeFile } from 'node:fs/promises';
import { mkdtemp } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { test } from 'node:test';

import {
  beginHookRun, finishHookRun, finishHookWorker, startHookWorker,
} from '../integration/hooks.mjs';
import { runIntegration } from '../integration/loam.mjs';
import { readInstallMetadata, validateInstallMetadata } from '../integration/metadata.mjs';
import { detectLegacyShadow } from '../integration/shadow.mjs';
import { detectTarget, resolveGlobalRoot, SUPPORTED_TARGETS, runtimePath } from '../integration/paths.mjs';
import { invokeRuntime, probeState, verifyRuntimeFile } from '../integration/runtime.mjs';
import { runtimeStorePath, writeLedger } from '../integration/ledger.mjs';

const target = detectTarget();

async function fixture({ runtimeVersion = '0.9.1', includeRuntime = true } = {}) {
  const home = await mkdtemp(join(tmpdir(), 'loam-integration-'));
  const globalRoot = join(home, '.agents', 'loam');
  const skillsRoot = join(home, '.agents', 'skills');
  const runtimeFile = runtimePath(globalRoot, runtimeVersion, target);
  const integrationPath = join(globalRoot, 'integration', '0.8.3-fixture', 'loam.mjs');
  const runtimeBytes = 'fixture runtime';
  const adapterRoot = join(globalRoot, 'plugins', '0.8.3');

  await mkdir(join(globalRoot, 'integration', '0.8.3-fixture'), { recursive: true });
  await mkdir(join(skillsRoot, 'loam-using', 'scripts'), { recursive: true });
  await writeFile(integrationPath, 'export {};\n');
  await writeFile(
    join(globalRoot, 'install.json'),
    JSON.stringify({
      schema_version: 1,
      plugin_version: '0.8.3',
      runtime_version: runtimeVersion,
      target,
      runtime_path: runtimeFile,
      runtime_sha256: createHash('sha256').update(runtimeBytes).digest('hex'),
      adapter_root: adapterRoot,
      integration_path: integrationPath,
      skills_scope: 'global',
      skills_source: 'scchearn/loam',
      configured_harnesses: [],
    }),
  );
  await writeFile(join(skillsRoot, 'loam-using', 'scripts', 'CLI_VERSION'), '0.9.1\n');
  await writeFile(
    join(skillsRoot, 'loam-using', 'SKILL.md'),
    '---\nname: loam::using\nmetadata:\n  version: "1.7.2"\n---\n\n# Using loam\n',
  );
  // The config-dir store + ledger are the readiness authority. install.json's
  // bin/ runtime_path is still written (pre-T6) but no longer consulted for the
  // version; the ledger's store binary is.
  const configDir = join(home, 'config');
  const storePath = runtimeStorePath({ version: runtimeVersion, target, root: configDir });
  const runtimeSha = createHash('sha256').update(runtimeBytes).digest('hex');
  if (includeRuntime) {
    await mkdir(join(globalRoot, 'bin', runtimeVersion, target), { recursive: true });
    await writeFile(runtimeFile, runtimeBytes);
    await mkdir(dirname(storePath), { recursive: true });
    await writeFile(storePath, runtimeBytes);
  }
  await writeLedger(
    { channel: runtimeVersion.includes('-') ? 'next' : 'latest', target: runtimeVersion, sha256: runtimeSha, store_path: storePath },
    { root: configDir },
  );
  await mkdir(adapterRoot, { recursive: true });

  return {
    home, globalRoot, skillsRoot, runtimePath: runtimeFile, storePath, integrationPath, target,
    configDir, env: { LOAM_CONFIG_DIR: configDir }, runtimeVersion,
  };
}

const state = {
  version: '0.9.1',
  wiki_root: '/tmp/wiki',
  exists: true,
  qmd_ready: true,
  collection: 'project-wiki',
  latest_checkpoint: null,
  recent_checkpoints: [],
  checkpoint_count: 0,
  git_status: null,
  drift_count: 0,
  hints: [],
};

test('ready state invokes native state once and formats one common context', async () => {
  const fixtureData = await fixture();
  const calls = [];
  const result = await probeState({
    ...fixtureData,
    workspace: fixtureData.home,
    runner: async (request) => {
      calls.push(request);
      return { code: 0, signal: null, stdout: JSON.stringify(state), stderr: '' };
    },
  });

  assert.equal(result.ready, true);
  assert.equal(calls.length, 1);
  assert.deepEqual(calls[0].args, ['state', '--fast', fixtureData.home]);

  assert.equal(result.state.collection, 'project-wiki');
});

test('metadata validation accepts prerelease versions and rejects build metadata', async () => {
  const home = await mkdtemp(join(tmpdir(), 'loam-meta-'));
  const globalRoot = join(home, '.agents', 'loam');
  await mkdir(join(globalRoot, 'bin', '0.9.1-next.0', target), { recursive: true });
  const base = {
    schema_version: 1,
    plugin_version: '0.8.3',
    runtime_version: '0.9.1',
    target,
    runtime_path: runtimePath(globalRoot, '0.9.1', target),
    runtime_sha256: 'a'.repeat(64),
    adapter_root: join(globalRoot, 'plugins', '0.8.3'),
    integration_path: join(globalRoot, 'integration', '0.8.3-fixture', 'loam.mjs'),
    skills_scope: 'global',
    skills_source: 'scchearn/loam',
    configured_harnesses: [],
  };
  const valid = await validateInstallMetadata(globalRoot, {
    ...base,
    runtime_version: '0.9.1-next.0',
    runtime_path: runtimePath(globalRoot, '0.9.1-next.0', target),
  });
  assert.equal(valid.runtime_version, '0.9.1-next.0');
  for (const bad of ['0.9.1+build', '0.9.1-next.0+build', '0.9.1-', '0.9.1-next.01', 'not-a-version']) {
    assert.throws(
      () => validateInstallMetadata(globalRoot, { ...base, runtime_version: bad }),
      /runtime_version is invalid/,
      `runtime_version ${bad} should be rejected`,
    );
  }
});

test('readInstallMetadata passes a prerelease fixture through', async () => {
  const fixtureData = await fixture({ runtimeVersion: '0.9.1-next.0' });
  const install = await readInstallMetadata(fixtureData.globalRoot);
  assert.equal(install.runtime_version, '0.9.1-next.0');
});

test('versioned integration path resolves the global Loam root', async () => {
  const fixtureData = await fixture();
  const chunks = [];
  const code = await runIntegration(['status'], {
    skillsRoot: fixtureData.skillsRoot,
    integrationPath: fixtureData.integrationPath,
    target,
    env: fixtureData.env,
    runner: async () => ({ code: 0, signal: null, stdout: JSON.stringify(state), stderr: '' }),
    output: { write: (chunk) => chunks.push(String(chunk)) },
  });

  assert.equal(code, 0);
  assert.equal(JSON.parse(chunks.join('')).globalRoot, fixtureData.globalRoot);
});

test('legacy integration path still resolves the global Loam root', () => {
  const globalRoot = join(tmpdir(), 'loam-root');
  const integrationPath = join(globalRoot, 'integration', 'loam.mjs');

  assert.equal(resolveGlobalRoot({ env: {}, integrationPath }), globalRoot);
});

test('target detection accepts the five release targets and rejects unsupported hosts', () => {
  assert.equal(detectTarget({ platform: 'linux', arch: 'x64' }), 'x86_64-unknown-linux-musl');
  assert.equal(detectTarget({ platform: 'linux', arch: 'arm64' }), 'aarch64-unknown-linux-musl');
  assert.equal(detectTarget({ platform: 'darwin', arch: 'x64' }), 'x86_64-apple-darwin');
  assert.equal(detectTarget({ platform: 'darwin', arch: 'arm64' }), 'aarch64-apple-darwin');
  assert.equal(detectTarget({ platform: 'win32', arch: 'x64' }), 'x86_64-pc-windows-msvc');
  assert.throws(() => detectTarget({ platform: 'win32', arch: 'arm64' }), /unsupported runtime target/);
  assert.equal(SUPPORTED_TARGETS.length, 5);
});

test('missing runtime reports unavailable without invoking state or fabricating workspace data', async () => {
  const fixtureData = await fixture({ includeRuntime: false });
  let calls = 0;
  const result = await probeState({
    ...fixtureData,
    workspace: fixtureData.home,
    runner: async () => {
      calls += 1;
      throw new Error('must not run');
    },
  });

  assert.equal(result.ready, false);
  assert.equal(result.category, 'runtime_missing');
  assert.equal(result.state, undefined);
  assert.equal(calls, 0);
});

test('a runtime self-report that differs from the ledger target is stale, not run-once', async () => {
  const fixtureData = await fixture();
  const result = await probeState({
    ...fixtureData,
    workspace: fixtureData.home,
    // The store binary is intact; only the self-reported version disagrees.
    runner: async () => ({ code: 0, stdout: JSON.stringify({ ...state, version: '0.8.2' }), stderr: '' }),
  });

  assert.equal(result.ready, false);
  assert.equal(result.category, 'runtime_stale');
  assert.equal(result.hint, 'update');
  assert.equal(result.expected, '0.9.1');
  assert.equal(result.actual, '0.8.2');
});

test('a stale or absent skills CLI_VERSION cannot change readiness', async () => {
  const fixtureData = await fixture();
  const scripts = join(fixtureData.skillsRoot, 'loam-using', 'scripts');
  // Poison the skills CLI_VERSION: readiness must ignore it entirely.
  await writeFile(join(scripts, 'CLI_VERSION'), '0.0.0\n');
  const poisoned = await probeState({
    ...fixtureData, workspace: fixtureData.home,
    runner: async () => ({ code: 0, stdout: JSON.stringify(state), stderr: '' }),
  });
  assert.equal(poisoned.ready, true);
  // And even when the file is gone.
  await rm(join(scripts, 'CLI_VERSION'));
  const absent = await probeState({
    ...fixtureData, workspace: fixtureData.home,
    runner: async () => ({ code: 0, stdout: JSON.stringify(state), stderr: '' }),
  });
  assert.equal(absent.ready, true);
});

test('runtime readiness rejects a store binary whose sha differs from the ledger', async () => {
  const fixtureData = await fixture();
  await writeFile(fixtureData.storePath, 'tampered runtime');
  let calls = 0;
  const result = await probeState({
    ...fixtureData,
    workspace: fixtureData.home,
    runner: async () => {
      calls += 1;
      return { code: 0, stdout: JSON.stringify(state), stderr: '' };
    },
  });

  // A sha divergence from the ledger means the store binary is not the target;
  // stale (update), caught before ever spawning the runtime.
  assert.equal(result.ready, false);
  assert.equal(result.category, 'runtime_stale');
  assert.equal(calls, 0);
});

test('runtime readiness rejects symlinked executables', async (t) => {
  if (process.platform === 'win32') return t.skip('symlink privileges vary on Windows');
  const fixtureData = await fixture();
  const outside = join(fixtureData.home, 'outside-runtime');
  await writeFile(outside, 'fixture runtime');
  await rm(fixtureData.storePath);
  await symlink(outside, fixtureData.storePath);

  const result = await probeState({ ...fixtureData, workspace: fixtureData.home });
  assert.equal(result.ready, false);
  assert.equal(result.category, 'runtime_untrusted');
});

test('runtime trust accepts a regular file through an aliased root', async (t) => {
  if (process.platform === 'win32') return t.skip('symlink privileges vary on Windows');
  const physicalRoot = await mkdtemp(join(tmpdir(), 'loam-physical-root-'));
  const aliasParent = await mkdtemp(join(tmpdir(), 'loam-alias-parent-'));
  const aliasRoot = join(aliasParent, 'loam');
  const runtimeFile = join(aliasRoot, 'runtime');
  const bytes = 'fixture runtime';
  await writeFile(join(physicalRoot, 'runtime'), bytes);
  await symlink(physicalRoot, aliasRoot);

  const result = await verifyRuntimeFile({
    globalRoot: aliasRoot,
    runtimePath: runtimeFile,
    expectedSha256: createHash('sha256').update(bytes).digest('hex'),
  });

  assert.equal(result.ready, true);
});

test('runtime invocation times out and bounds stderr diagnostics', async () => {
  const result = await invokeRuntime({
    runtimePath: '/contained/runtime',
    args: ['state', '--fast', '/workspace'],
    timeoutMs: 10,
    runner: () => new Promise(() => {}),
  });

  assert.equal(result.code, null);
  assert.equal(result.category, 'timeout');
  assert.match(result.stderr, /timed out/);
});

test('hook-run logging invokes the installed private runtime with fixed validated argv', async () => {
  const fixtureData = await fixture();
  const calls = [];
  const runner = async (request) => {
    calls.push(request);
    return { code: 0, signal: null, stdout: calls.length === 1 ? '42\n' : '', stderr: '' };
  };
  const run = await beginHookRun({
    globalRoot: fixtureData.globalRoot,
    harness: 'codex',
    hook: 'stop',
    workspace: fixtureData.home,
    sessionId: 'session-42',
    runner,
  });
  const finished = await finishHookRun({
    run,
    status: 'succeeded',
    action: 'skip',
    reason: 'nothing_to_do',
    detail: 'no actionable files',
    runner,
  });

  assert.deepEqual(run, {
    id: 42,
    globalRoot: fixtureData.globalRoot,
    runtimePath: fixtureData.runtimePath,
    workspace: fixtureData.home,
  });
  assert.equal(finished, true);
  assert.equal(calls.length, 2);
  assert.equal(calls[0].runtimePath, fixtureData.runtimePath);
  assert.deepEqual(calls[0].args, [
    'hooks', 'begin', fixtureData.globalRoot,
    '--harness', 'codex',
    '--hook', 'stop',
    '--workspace', fixtureData.home,
    '--plugin-version', '0.8.3',
    '--session-id', 'session-42',
  ]);
  assert.deepEqual(calls[1].args, [
    'hooks', 'finish', fixtureData.globalRoot,
    '--id', '42',
    '--status', 'succeeded',
    '--action', 'skip',
    '--reason', 'nothing_to_do',
    '--detail', 'no actionable files',
  ]);
  assert.equal(calls[0].cwd, fixtureData.home);
  assert.equal(calls[0].timeoutMs, 300);
  assert.equal(calls[1].timeoutMs, 300);
});

test('worker logging uses the same run and maps normalized results to native status', async () => {
  const fixtureData = await fixture();
  const calls = [];
  const run = {
    id: 42,
    globalRoot: fixtureData.globalRoot,
    runtimePath: fixtureData.runtimePath,
    workspace: fixtureData.home,
  };
  const runner = async (request) => {
    calls.push(request);
    return { code: 0, signal: null, stdout: '', stderr: '' };
  };

  assert.equal(await startHookWorker({ run, sessionId: 'worker-42', runner }), true);
  assert.equal(await finishHookWorker({ run, reason: 'busy', detail: 'lease held', runner }), true);
  assert.deepEqual(calls.map(({ args }) => args), [
    [
      'hooks', 'worker-start', fixtureData.globalRoot,
      '--id', '42',
      '--session-id', 'worker-42',
    ],
    [
      'hooks', 'worker-finish', fixtureData.globalRoot,
      '--id', '42',
      '--status', 'skipped',
      '--reason', 'busy',
      '--detail', 'lease held',
    ],
  ]);
  assert.equal(calls[0].timeoutMs, 300);
  assert.equal(calls[1].timeoutMs, 300);
  assert.equal(await finishHookWorker({ run, reason: 'unknown', runner }), false);
});

test('hook-run logging is fail-open for invalid input, malformed IDs, runtime failure, and timeout', async () => {
  const fixtureData = await fixture();
  let calls = 0;
  assert.equal(await beginHookRun({
    globalRoot: fixtureData.globalRoot,
    harness: 'Claude Code',
    hook: 'stop',
    workspace: fixtureData.home,
    runner: async () => { calls += 1; },
  }), null);
  assert.equal(calls, 0);
  assert.equal(await beginHookRun({
    globalRoot: fixtureData.globalRoot,
    harness: 'claude',
    hook: 'stop',
    workspace: fixtureData.home,
    runner: async () => ({ code: 1, stdout: '', stderr: 'locked' }),
  }), null);
  assert.equal(await beginHookRun({
    globalRoot: fixtureData.globalRoot,
    harness: 'claude',
    hook: 'stop',
    workspace: fixtureData.home,
    runner: async () => ({ code: 0, stdout: 'not-an-id\n', stderr: '' }),
  }), null);
  assert.equal(await beginHookRun({
    globalRoot: fixtureData.globalRoot,
    harness: 'claude',
    hook: 'stop',
    workspace: fixtureData.home,
    timeoutMs: 10,
    runner: () => new Promise(() => {}),
  }), null);
  assert.equal(await finishHookRun({ run: null, status: 'succeeded' }), false);
  assert.equal(await finishHookRun({
    run: { id: 1, globalRoot: fixtureData.globalRoot, runtimePath: fixtureData.runtimePath, workspace: fixtureData.home },
    status: 'succeeded',
    action: 'skip',
  }), false);
});

test('failed hook-run logging bounds and sanitizes its diagnostic', async () => {
  const fixtureData = await fixture();
  const calls = [];
  const run = await beginHookRun({
    globalRoot: fixtureData.globalRoot,
    harness: 'claude',
    hook: 'stop',
    workspace: fixtureData.home,
    runner: async () => ({ code: 0, stdout: '7\n', stderr: '' }),
  });
  assert.equal(await finishHookRun({
    run,
    status: 'failed',
    detail: 'bad\u0000' + 'x'.repeat(2000),
    runner: async (request) => {
      calls.push(request);
      return { code: 0, stdout: '', stderr: '' };
    },
  }), true);
  const detail = calls[0].args.at(-1);
  assert.equal(calls[0].args.at(-2), '--detail');
  assert.ok([...detail].length <= 1024);
  assert.doesNotMatch(detail, /[\u0000-\u001F\u007F]/);
});

test('direct runtime invocation closes stdin', async () => {
  const result = await invokeRuntime({
    runtimePath: process.execPath,
    args: ['-e', "process.stdin.resume(); process.stdin.on('end', () => process.stdout.write('closed'))"],
    cwd: process.cwd(),
    // Generous timeout: node startup under a loaded pre-commit run can exceed a
    // tight bound and this test is about the closed-stdin contract, not latency.
    timeoutMs: 15000,
  });

  assert.equal(result.code, 0, result.stderr);
  assert.equal(result.stdout, 'closed');
});

test('direct runtime invocation writes a supplied stdin payload then closes it', async () => {
  const result = await invokeRuntime({
    runtimePath: process.execPath,
    args: ['-e', "let d=''; process.stdin.on('data', (c) => d += c); process.stdin.on('end', () => process.stdout.write(d))"],
    cwd: process.cwd(),
    // Generous timeout: node startup under a loaded pre-commit run can exceed a
    // tight bound and this test is about the write path, not latency.
    timeoutMs: 15000,
    input: '{"schema":1,"events":[]}',
  });

  assert.equal(result.code, 0, result.stderr);
  assert.equal(result.stdout, '{"schema":1,"events":[]}');
});

test('invokeRuntime threads input through the runner seam', async () => {
  let seen;
  const result = await invokeRuntime({
    runtimePath: '/runtime',
    args: ['hooks', 'finish'],
    cwd: process.cwd(),
    timeoutMs: 100,
    input: 'payload',
    runner: async (request) => {
      seen = request;
      return { code: 0, signal: null, stdout: '', stderr: '' };
    },
  });

  assert.equal(result.code, 0);
  assert.equal(seen.input, 'payload');
  // A call without input still reaches the runner with input undefined.
  await invokeRuntime({
    runtimePath: '/runtime',
    args: ['hooks', 'begin'],
    cwd: process.cwd(),
    timeoutMs: 100,
    runner: async (request) => {
      seen = request;
      return { code: 0, signal: null, stdout: '', stderr: '' };
    },
  });
  assert.equal(seen.input, undefined);
});

test('a stdin write to a child that exits without reading stays fail-open', async () => {
  const result = await invokeRuntime({
    runtimePath: process.execPath,
    args: ['-e', 'process.exit(0)'],
    cwd: process.cwd(),
    timeoutMs: 15000,
    input: 'x'.repeat(4096),
  });

  assert.equal(result.code, 0, result.stderr);
});

const syntheticRun = () => ({ id: 42, globalRoot: '/g', workspace: '/w', runtimePath: '/r' });

test('finishHookRun attaches a bounded event batch on the same subprocess call', async () => {
  const calls = [];
  const runner = async (request) => {
    calls.push(request);
    return { code: 0, signal: null, stdout: '', stderr: '' };
  };
  const events = [
    { event: 'ingest_visibility', phase: 'launch', outcome: 'started', visibility: 'native', launch_mode: 'claude_bg' },
  ];
  const ok = await finishHookRun({
    run: syntheticRun(), status: 'succeeded', action: 'spawn_worker', events, runner,
  });

  assert.equal(ok, true);
  assert.equal(calls.length, 1);
  assert.ok(calls[0].args.includes('--events-stdin'));
  assert.equal(calls[0].input, JSON.stringify({ schema: 1, events }));
});

test('finishHookRun without events keeps the closed-stdin call unchanged', async () => {
  const calls = [];
  const runner = async (request) => {
    calls.push(request);
    return { code: 0, signal: null, stdout: '', stderr: '' };
  };
  const ok = await finishHookRun({
    run: syntheticRun(), status: 'succeeded', action: 'skip', reason: 'nothing_to_do', runner,
  });

  assert.equal(ok, true);
  assert.equal(calls.length, 1);
  assert.ok(!calls[0].args.includes('--events-stdin'));
  assert.equal(calls[0].input, undefined);
});

test('an out-of-bounds event batch is dropped without a flag, process, or lost transition', async () => {
  for (const events of [
    Array.from({ length: 17 }, () => ({ event: 'ingest_visibility' })),
    [{ event: 'ingest_visibility', detail: 'z'.repeat(20000) }],
    [{ event: 'ingest_visibility' }, 'not-an-object'],
    [],
  ]) {
    const calls = [];
    const runner = async (request) => {
      calls.push(request);
      return { code: 0, signal: null, stdout: '', stderr: '' };
    };
    const ok = await finishHookRun({
      run: syntheticRun(), status: 'succeeded', action: 'spawn_worker', events, runner,
    });

    assert.equal(ok, true);
    assert.equal(calls.length, 1, 'the transition still runs as one subprocess');
    assert.ok(!calls[0].args.includes('--events-stdin'));
    assert.equal(calls[0].input, undefined);
  }
});

test('emitting events keeps the lifecycle subprocess count constant', async () => {
  // Each wrapper makes exactly one runtime invocation whether or not it carries
  // an event batch — the batch rides that call, never a per-observation process.
  const run = syntheticRun();
  const events = [
    { event: 'ingest_visibility', phase: 'launch', outcome: 'started', visibility: 'native', launch_mode: 'claude_bg' },
  ];
  for (const withEvents of [false, true]) {
    for (const [label, invoke] of [
      ['finish', (extra) => finishHookRun({ run, status: 'succeeded', action: 'spawn_worker', ...extra })],
      ['worker-start', (extra) => startHookWorker({ run, sessionId: 'w', ...extra })],
      ['worker-finish', (extra) => finishHookWorker({ run, reason: 'ok', ...extra })],
    ]) {
      const calls = [];
      const runner = async (request) => {
        calls.push(request);
        return { code: 0, signal: null, stdout: '', stderr: '' };
      };
      await invoke({ runner, ...(withEvents ? { events } : {}) });
      assert.equal(calls.length, 1, `${label} is one subprocess (events=${withEvents})`);
      assert.equal(calls[0].args.includes('--events-stdin'), withEvents, label);
    }
  }
});

test('worker lifecycle wrappers forward their batch on their own single call', async () => {
  const calls = [];
  const runner = async (request) => {
    calls.push(request);
    return { code: 0, signal: null, stdout: '', stderr: '' };
  };
  const run = syntheticRun();
  const startEvents = [
    { event: 'subagent', phase: 'start', outcome: 'observed', agent_type: 'loam_ingestor', session_id: 'c1' },
  ];
  const finishEvents = [
    { event: 'subagent', phase: 'stop', outcome: 'succeeded', agent_type: 'loam_ingestor', session_id: 'c1' },
  ];

  assert.equal(await startHookWorker({ run, sessionId: 'c1', events: startEvents, runner }), true);
  assert.equal(await finishHookWorker({ run, reason: 'ok', events: finishEvents, runner }), true);

  assert.equal(calls.length, 2);
  assert.ok(calls[0].args.includes('--events-stdin'));
  assert.equal(calls[0].input, JSON.stringify({ schema: 1, events: startEvents }));
  assert.ok(calls[1].args.includes('--events-stdin'));
  assert.equal(calls[1].input, JSON.stringify({ schema: 1, events: finishEvents }));
});

test('malformed native state is reported without synthetic fields', async () => {
  const fixtureData = await fixture();
  const result = await probeState({
    ...fixtureData,
    workspace: fixtureData.home,
    runner: async () => ({ code: 0, signal: null, stdout: '{not-json', stderr: '' }),
  });

  assert.equal(result.ready, false);
  assert.equal(result.category, 'malformed_state');
  assert.equal(result.state, undefined);
  assert.match(result.detail, /JSON/);
});

test('hostile runtime diagnostics are truncated and control characters are removed', async () => {
  const fixtureData = await fixture();
  const result = await probeState({
    ...fixtureData,
    workspace: fixtureData.home,
    runner: async () => ({
      code: 1,
      signal: null,
      stdout: '',
      stderr: `bad\u0000${'<script>'.repeat(2000)}`,
    }),
  });

  assert.equal(result.ready, false);
  assert.equal(result.category, 'runtime_failed');
  assert.ok(result.detail.length <= 4096);
  assert.doesNotMatch(result.detail, /\u0000/);
});

test('legacy shadow detection is report-only and rejects escaping symlinks', async (t) => {
  const fixtureData = await fixture();
  const workspace = join(fixtureData.home, 'workspace');
  await mkdir(join(workspace, '.agents', 'skills', 'loam-using'), { recursive: true });
  await writeFile(join(workspace, '.agents', 'skills', 'loam-using', 'SKILL.md'), '# legacy\n');

  const report = await detectLegacyShadow(workspace);
  assert.equal(report.shadows.length, 1);
  assert.equal(report.unsafe.length, 0);

  if (process.platform === 'win32') return t.skip('symlink privileges vary on Windows');
  const outside = join(fixtureData.home, 'outside');
  await mkdir(outside);
  await symlink(outside, join(workspace, '.agents', 'loam'));
  const escaped = await detectLegacyShadow(workspace);
  assert.equal(escaped.unsafe.length, 1);
  assert.equal(escaped.shadows.length, 1);
});

test('legacy shadow detection handles a workspace with no shadow directories', async () => {
  const workspace = await mkdtemp(join(tmpdir(), 'loam-empty-workspace-'));
  const report = await detectLegacyShadow(workspace);

  assert.deepEqual(report, { shadows: [], unsafe: [] });
});

test('the integration boundary is status-only and refuses the retired hook command', async () => {
  const fixtureData = await fixture();
  const before = await readFile(join(fixtureData.skillsRoot, 'loam-using', 'SKILL.md'), 'utf8');
  const statusChunks = [];
  const statusCode = await runIntegration(['status'], {
    globalRoot: fixtureData.globalRoot,
    skillsRoot: fixtureData.skillsRoot,
    integrationPath: fixtureData.integrationPath,
    target,
    env: fixtureData.env,
    runner: async () => ({ code: 0, signal: null, stdout: JSON.stringify(state), stderr: '' }),
    output: { write: (chunk) => statusChunks.push(String(chunk)) },
  });
  assert.equal(statusCode, 0);
  assert.equal(JSON.parse(statusChunks.join('')).ready, true);

  // The harness read path is the native `loam hook <harness>` command now;
  // the shared Node integration no longer serves one at any spelling.
  for (const argv of [
    ['run', '--', 'check', 'versions', fixtureData.home],
    ['hook', '--harness', 'claude', '--workspace', fixtureData.home],
    ['hook', '--harness', 'opencode', '--workspace', fixtureData.home],
  ]) {
    await assert.rejects(
      () => runIntegration(argv, { globalRoot: fixtureData.globalRoot }),
      /usage: loam\.mjs status \| ingest-status/,
    );
  }
  assert.equal(await readFile(join(fixtureData.skillsRoot, 'loam-using', 'SKILL.md'), 'utf8'), before);
});

test('status rejects a correctly hashed runtime that fails bounded execution', async () => {
  const fixtureData = await fixture();
  const chunks = [];
  const code = await runIntegration(['status'], {
    globalRoot: fixtureData.globalRoot,
    skillsRoot: fixtureData.skillsRoot,
    integrationPath: fixtureData.integrationPath,
    target,
    env: fixtureData.env,
    runner: async () => ({ code: 1, signal: null, stdout: '', stderr: 'not executable' }),
    output: { write: (chunk) => chunks.push(String(chunk)) },
  });

  assert.equal(code, 1);
  assert.equal(JSON.parse(chunks.join('')).category, 'runtime_failed');
});

test('status rejects a correctly hashed non-executable runtime', async (t) => {
  if (process.platform === 'win32') return t.skip('execute permissions are not portable on Windows');
  const fixtureData = await fixture();
  await chmod(fixtureData.storePath, 0o600);
  const chunks = [];
  const code = await runIntegration(['status'], {
    globalRoot: fixtureData.globalRoot,
    skillsRoot: fixtureData.skillsRoot,
    integrationPath: fixtureData.integrationPath,
    target,
    env: fixtureData.env,
    output: { write: (chunk) => chunks.push(String(chunk)) },
  });

  assert.equal(code, 1);
  assert.equal(JSON.parse(chunks.join('')).category, 'runtime_failed');
});
