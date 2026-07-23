import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { chmod, mkdir, readFile, rm, symlink, writeFile } from 'node:fs/promises';
import { mkdtemp } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';

import { formatContext, formatNativeRuntimeCommand } from '../integration/context.mjs';
import { runIntegration } from '../integration/loam.mjs';
import { detectLegacyShadow } from '../integration/shadow.mjs';
import { detectTarget, SUPPORTED_TARGETS, runtimePath } from '../integration/paths.mjs';
import { invokeRuntime, probeState } from '../integration/runtime.mjs';

const target = detectTarget();

async function fixture({ runtimeVersion = '0.9.1', includeRuntime = true } = {}) {
  const home = await mkdtemp(join(tmpdir(), 'loam-integration-'));
  const globalRoot = join(home, '.agents', 'loam');
  const skillsRoot = join(home, '.agents', 'skills');
  const runtimeFile = runtimePath(globalRoot, runtimeVersion, target);
  const integrationPath = join(globalRoot, 'integration', 'loam.mjs');
  const runtimeBytes = 'fixture runtime';
  const adapterRoot = join(globalRoot, 'plugins', '0.8.3');

  await mkdir(join(globalRoot, 'integration'), { recursive: true });
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
  if (includeRuntime) {
    await mkdir(join(globalRoot, 'bin', runtimeVersion, target), { recursive: true });
    await writeFile(runtimeFile, runtimeBytes);
  }
  await mkdir(adapterRoot, { recursive: true });

  return { home, globalRoot, skillsRoot, runtimePath: runtimeFile, integrationPath, target };
}

const state = {
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

  const context = formatContext({
    skillContent: '---\nname: loam::using\n---\n\n# Using loam\n',
    state: result.state,
    pluginVersion: '0.8.3',
    runtimePath: fixtureData.runtimePath,
    platform: 'linux',
  });
  assert.match(context, /^<LOAM_IMPORTANT>/);
  assert.match(context, /You have loam \(v0\.8\.3\)/);
  assert.match(context, /Native runtime command: '/);
  assert.match(context, new RegExp(fixtureData.runtimePath.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')));
  assert.match(context, /project-wiki/);
  assert.doesNotMatch(context, /^name: loam::using$/m);
});

test('quotes native runtime paths for POSIX and PowerShell commands', () => {
  const posixPath = '/home/Sam User/.agents/loam/bin/0.9.1/loam';
  const windowsPath = String.raw`C:\Users\Sam User\.agents\loam\bin\0.9.1\loam.exe`;

  assert.equal(formatNativeRuntimeCommand(posixPath, 'linux'), `'${posixPath}'`);
  assert.equal(formatNativeRuntimeCommand(windowsPath, 'win32'), `& '${windowsPath}'`);
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
  const context = formatContext({
    skillContent: '# Using loam\n',
    unavailable: result,
    pluginVersion: '0.8.3',
  });
  assert.match(context, /npx @scchearn\/loam setup/);
  assert.match(context, /No workspace state was generated/);
  assert.doesNotMatch(context, /## Workspace state/);
});

test('runtime version mismatch fails readiness before execution', async () => {
  const fixtureData = await fixture({ runtimeVersion: '0.8.2' });
  const result = await probeState({
    ...fixtureData,
    workspace: fixtureData.home,
    runner: async () => {
      throw new Error('must not run');
    },
  });

  assert.equal(result.ready, false);
  assert.equal(result.category, 'runtime_version_mismatch');
  assert.equal(result.expected, '0.9.1');
  assert.equal(result.actual, '0.8.2');
});

test('runtime readiness rejects tampered bytes before invoking native state', async () => {
  const fixtureData = await fixture();
  await writeFile(fixtureData.runtimePath, 'tampered runtime');
  let calls = 0;
  const result = await probeState({
    ...fixtureData,
    workspace: fixtureData.home,
    runner: async () => {
      calls += 1;
      return { code: 0, stdout: JSON.stringify(state), stderr: '' };
    },
  });

  assert.equal(result.ready, false);
  assert.equal(result.category, 'runtime_untrusted');
  assert.equal(calls, 0);
});

test('runtime readiness rejects symlinked executables', async (t) => {
  if (process.platform === 'win32') return t.skip('symlink privileges vary on Windows');
  const fixtureData = await fixture();
  const outside = join(fixtureData.home, 'outside-runtime');
  await writeFile(outside, 'fixture runtime');
  await rm(fixtureData.runtimePath);
  await symlink(outside, fixtureData.runtimePath);

  const result = await probeState({ ...fixtureData, workspace: fixtureData.home });
  assert.equal(result.ready, false);
  assert.equal(result.category, 'runtime_untrusted');
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

test('status and hook commands share one read-only integration boundary', async () => {
  const fixtureData = await fixture();
  const workspace = fixtureData.home;
  const before = await readFile(join(fixtureData.skillsRoot, 'loam-using', 'SKILL.md'), 'utf8');
  const statusChunks = [];
  const statusCode = await runIntegration(['status'], {
    globalRoot: fixtureData.globalRoot,
    skillsRoot: fixtureData.skillsRoot,
    integrationPath: fixtureData.integrationPath,
    target,
    runner: async () => ({ code: 0, signal: null, stdout: JSON.stringify(state), stderr: '' }),
    output: { write: (chunk) => statusChunks.push(String(chunk)) },
  });
  assert.equal(statusCode, 0);
  assert.equal(JSON.parse(statusChunks.join('')).ready, true);

  await assert.rejects(
    () => runIntegration(['run', '--', 'check', 'versions', fixtureData.home], { globalRoot: fixtureData.globalRoot }),
    /usage: loam\.mjs status \| hook/,
  );

  const contexts = [];
  for (const harness of ['opencode', 'claude', 'cursor']) {
    const chunks = [];
    const code = await runIntegration(
      ['hook', '--harness', harness, '--workspace', workspace],
      {
        globalRoot: fixtureData.globalRoot,
        skillsRoot: fixtureData.skillsRoot,
        integrationPath: fixtureData.integrationPath,
        target,
        runner: async () => ({ code: 0, signal: null, stdout: JSON.stringify(state), stderr: '' }),
        output: { write: (chunk) => chunks.push(String(chunk)) },
      },
    );
    assert.equal(code, 0);
    contexts.push(chunks.join(''));
  }

  assert.ok(contexts.every((context) => context.includes('<LOAM_IMPORTANT>')));
  assert.ok(contexts.every((context) => context.includes(`Native runtime command: '${fixtureData.runtimePath}'`)));
  assert.deepEqual(new Set(contexts).size, 1);
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
    runner: async () => ({ code: 1, signal: null, stdout: '', stderr: 'not executable' }),
    output: { write: (chunk) => chunks.push(String(chunk)) },
  });

  assert.equal(code, 1);
  assert.equal(JSON.parse(chunks.join('')).category, 'runtime_failed');
});

test('status rejects a correctly hashed non-executable runtime', async (t) => {
  if (process.platform === 'win32') return t.skip('execute permissions are not portable on Windows');
  const fixtureData = await fixture();
  await chmod(fixtureData.runtimePath, 0o600);
  const chunks = [];
  const code = await runIntegration(['status'], {
    globalRoot: fixtureData.globalRoot,
    skillsRoot: fixtureData.skillsRoot,
    integrationPath: fixtureData.integrationPath,
    target,
    output: { write: (chunk) => chunks.push(String(chunk)) },
  });

  assert.equal(code, 1);
  assert.equal(JSON.parse(chunks.join('')).category, 'runtime_failed');
});
