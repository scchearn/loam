import assert from 'node:assert/strict';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';

import { assertSupportedNode, launch, LaunchError, parseArgs } from '../launch.mjs';

test('assertSupportedNode gates on Node >= 22 with a clear error', () => {
  assert.throws(() => assertSupportedNode('v20.12.0'), (error) => {
    assert.ok(error instanceof LaunchError);
    assert.match(error.message, /Node\.js >= 22/);
    return true;
  });
  assert.doesNotThrow(() => assertSupportedNode('v22.14.0'));
  assert.doesNotThrow(() => assertSupportedNode('v23.0.0'));
});

test('launch fails with a setup recovery message when no loam runtime is installed', async () => {
  const home = await mkdtemp(join(tmpdir(), 'loam-view-launch-home-'));
  try {
    await assert.rejects(
      launch({ workspace: home, home, env: {} }),
      (error) => {
        assert.ok(error instanceof LaunchError);
        assert.match(error.message, /Loam is unavailable/);
        assert.match(error.message, /npx @scchearn\/loam setup/);
        return true;
      },
    );
  } finally {
    await rm(home, { recursive: true, force: true });
  }
});

const SNAPSHOT = {
  profile: 'loam-view',
  schema_version: 1,
  generated_at: '2026-08-19T00:00:00+00:00',
  status: 'ready',
  posture: 'healthy',
  workspace: {
    root: '/ws',
    name: 'ws',
    platform: 'linux',
    git: { state: 'clean', branch: 'main', dirty: false, changed_count: 0 },
  },
  capabilities: {
    wiki: { state: 'ready', required: true, reason: null, evidence: null },
    code_graph: { state: 'absent', required: false, reason: null, evidence: null },
    goals: { state: 'absent', required: false, reason: null, evidence: null },
    work: { state: 'absent', required: false, reason: null, evidence: null },
    checkpoints: { state: 'absent', required: false, reason: null, evidence: null },
    git: { state: 'ready', required: false, reason: null, evidence: null },
    qmd: { state: 'absent', required: false, reason: null, evidence: null },
    search_corpus: { state: 'ready', required: true, reason: null, evidence: null },
  },
  artifacts: [],
  relationships: [],
  events: [],
  metrics: {},
  signals: [],
  hints: [],
  probes: [],
};

async function fakeRuntimeHome(prefix) {
  const home = await mkdtemp(join(tmpdir(), prefix));
  const bin = join(home, 'loam');
  await writeFile(bin, '#!/bin/sh\nexit 0\n', { mode: 0o755 });
  return { home, bin };
}

const snapshotRunner = (calls) => async ({ args }) => {
  calls.push(args);
  return { code: 0, stdout: JSON.stringify({ ...SNAPSHOT, workspace: { ...SNAPSHOT.workspace } }), stderr: '' };
};

test('parseArgs reads the workspace positional and the --no-open flag', () => {
  assert.deepEqual(parseArgs([]), { workspace: undefined, open: true });
  assert.deepEqual(parseArgs(['/ws']), { workspace: '/ws', open: true });
  assert.deepEqual(parseArgs(['/ws', '--no-open']), { workspace: '/ws', open: false });
  assert.deepEqual(parseArgs(['--no-open', '/ws']), { workspace: '/ws', open: false });
  assert.throws(() => parseArgs(['--nope']), LaunchError);
  assert.throws(() => parseArgs(['/a', '/b']), LaunchError);
});

test('LOAM_NATIVE_BIN must be an absolute path to an executable file', async () => {
  const home = await mkdtemp(join(tmpdir(), 'loam-view-launch-bin-'));
  try {
    await assert.rejects(
      launch({ workspace: home, home, env: { LOAM_NATIVE_BIN: 'loam' } }),
      (error) => {
        assert.ok(error instanceof LaunchError);
        assert.match(error.message, /absolute path/);
        return true;
      },
    );
    await assert.rejects(
      launch({ workspace: home, home, env: { LOAM_NATIVE_BIN: join(home, 'missing-loam') } }),
      (error) => {
        assert.match(error.message, /not an executable file/);
        return true;
      },
    );
  } finally {
    await rm(home, { recursive: true, force: true });
  }
});

test('LOAM_NATIVE_BIN bypasses installed-runtime resolution', async () => {
  const { home } = await fakeRuntimeHome('loam-view-launch-override-');
  const calls = [];
  try {
    // No installed runtime under this home: without the override, launch() rejects.
    const { server } = await launch({
      workspace: home,
      home,
      env: { LOAM_NATIVE_BIN: join(home, 'loam') },
      runner: snapshotRunner(calls),
      listen: false,
    });
    assert.deepEqual(calls, [['state', '--view', home]]);
    server.close();
  } finally {
    await rm(home, { recursive: true, force: true });
  }
});

test('the serving URL prints as a parseable line before any browser open, and --no-open suppresses the opener', async () => {
  const { home } = await fakeRuntimeHome('loam-view-launch-url-');
  const lines = [];
  const output = { write: (chunk) => { lines.push(String(chunk)); return true; } };
  let openedWith = null;
  let outputAtOpen = null;

  const run = (open) => launch({
    workspace: home,
    home,
    env: { LOAM_NATIVE_BIN: join(home, 'loam') },
    runner: snapshotRunner([]),
    output,
    openBrowserFn: (url) => { openedWith = url; outputAtOpen = lines.slice(); },
    open,
  });

  try {
    const pending = run(true);
    while (lines.length === 0) await new Promise((r) => setTimeout(r, 5));
    const match = /^Loam View: (http:\/\/127\.0\.0\.1:\d+\/)$/m.exec(lines.join(''));
    assert.ok(match, `expected a parseable URL line, got ${JSON.stringify(lines)}`);
    assert.equal(openedWith, match[1]);
    assert.ok(outputAtOpen.join('').includes(match[1]), 'URL must be printed before the browser opens');
    process.emit('SIGINT');
    await pending;

    lines.length = 0;
    openedWith = null;
    const pendingNoOpen = run(false);
    while (lines.length === 0) await new Promise((r) => setTimeout(r, 5));
    assert.match(lines.join(''), /^Loam View: http:\/\/127\.0\.0\.1:\d+\/$/m);
    assert.equal(openedWith, null, '--no-open must not spawn a browser opener');
    process.emit('SIGINT');
    await pendingNoOpen;
  } finally {
    await rm(home, { recursive: true, force: true });
  }
});
