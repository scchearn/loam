import assert from 'node:assert/strict';
import { chmod, mkdir, mkdtemp, stat, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

import { runDoctor } from '../setup/doctor.mjs';

const packageRoot = fileURLToPath(new URL('..', import.meta.url));

test('doctor reports missing installation without changing the home', async () => {
  const home = await mkdtemp(join(tmpdir(), 'loam-doctor-home-'));
  const workspace = await mkdtemp(join(tmpdir(), 'loam-doctor-workspace-'));
  let output = '';
  const code = await runDoctor({
    home,
    workspace,
    packageRoot,
    runner: async ({ args }) => {
      assert.deepEqual(args, ['--yes', 'skills@1.5.20', 'list', '--json', '--global']);
      return { code: 0, stdout: '[]', stderr: '' };
    },
    output: { write: (value) => { output += value; } },
    errorOutput: { write: (value) => { output += value; } },
  });

  assert.equal(code, 1);
  assert.match(output, /Loam Doctor/);
  assert.match(output, /Install metadata: failed/);
  assert.match(output, /Result: not ready/);
  await assert.rejects(() => stat(join(home, '.agents')));
});

test('doctor reports installed plugin and CLI versions', async () => {
  const home = await mkdtemp(join(tmpdir(), 'loam-doctor-ready-home-'));
  const workspace = await mkdtemp(join(tmpdir(), 'loam-doctor-ready-workspace-'));
  const globalRoot = join(home, '.agents', 'loam');
  await mkdir(globalRoot, { recursive: true });
  await writeFile(join(globalRoot, 'install.json'), JSON.stringify({
    schema_version: 1,
    plugin_version: '1.2.3',
    runtime_version: '4.5.6',
    target: 'test-target',
    runtime_path: join(globalRoot, 'runtime'),
    runtime_sha256: '0'.repeat(64),
    adapter_root: join(globalRoot, 'plugins'),
    integration_path: join(globalRoot, 'integration.mjs'),
    skills_scope: 'global',
    skills_source: 'scchearn/loam',
    configured_harnesses: [],
  }));

  let output = '';
  const code = await runDoctor({
    home,
    workspace,
    packageRoot,
    output: { write: (value) => { output += value; } },
    errorOutput: { write: (value) => { output += value; } },
  });

  assert.equal(code, 1, output);
  assert.match(output, /Plugin version: 1\.2\.3/);
  assert.match(output, /CLI version: 4\.5\.6/);
  assert.match(output, /Result: not ready/);
});

test('doctor reports every catalog integration, detection-only ones included, without touching the exit code', async () => {
  const home = await mkdtemp(join(tmpdir(), 'loam-doctor-integrations-home-'));
  const workspace = await mkdtemp(join(tmpdir(), 'loam-doctor-integrations-workspace-'));
  const globalRoot = join(home, '.agents', 'loam');
  await mkdir(globalRoot, { recursive: true });
  await writeFile(join(globalRoot, 'install.json'), JSON.stringify({
    schema_version: 1,
    plugin_version: '1.2.3',
    runtime_version: '4.5.6',
    target: 'test-target',
    runtime_path: join(globalRoot, 'runtime'),
    runtime_sha256: '0'.repeat(64),
    adapter_root: join(globalRoot, 'plugins'),
    integration_path: join(globalRoot, 'integration.mjs'),
    skills_scope: 'global',
    skills_source: 'scchearn/loam',
    configured_harnesses: ['claude'],
  }));
  const binDir = join(home, 'bin');
  await mkdir(binDir, { recursive: true });
  await writeFile(join(binDir, 'hcom'), '#!/bin/sh\necho "hcom 0.7.25"\n');
  await chmod(join(binDir, 'hcom'), 0o755);

  const run = async (path) => {
    let output = '';
    const code = await runDoctor({
      home,
      workspace,
      packageRoot,
      env: { PATH: path },
      toolRunner: async () => ({ code: 0, stdout: 'hcom 0.7.25\n', stderr: '' }),
      output: { write: (value) => { output += value; } },
      errorOutput: { write: (value) => { output += value; } },
    });
    return { code, output };
  };

  const present = await run(binDir);
  assert.match(present.output, /Optional integrations \(informational\):/);
  // Catalog-driven: the section reports the entry without doctor knowing it.
  assert.match(present.output, /hcom \(agent-messaging\): tool present \(PATH, hcom 0\.7\.25\); MCP not applicable/);
  // The MCP-backed entries still report per-harness state.
  assert.match(present.output, /grep \(code-search\): no tool; MCP claude:no/);

  const absent = await run('');
  assert.match(absent.output, /hcom \(agent-messaging\): tool absent; MCP not applicable/);
  // Informational only: an absent optional integration is a choice, not a failure.
  assert.equal(absent.code, present.code, 'integration state must not move the exit code');
});
