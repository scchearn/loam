import assert from 'node:assert/strict';
import { mkdtemp, mkdir, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawnSync } from 'node:child_process';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

import { assertPackageAssets } from '../setup/package-check.mjs';

const root = new URL('..', import.meta.url);
const rootPath = fileURLToPath(root);
const npmCommand = process.platform === 'win32' ? 'npm.cmd' : 'npm';

test('dry-run package contains the scoped executable and legacy OpenCode entry', () => {
  const result = spawnSync(npmCommand, ['pack', '--dry-run', '--json'], {
    cwd: rootPath,
    encoding: 'utf8',
  });

  assert.equal(result.status, 0, result.stderr);
  const packageJson = JSON.parse(result.stdout)[0];
  const files = new Set(packageJson.files.map(({ path }) => path));

  assert.equal(packageJson.name, '@scchearn/loam');
  assert.equal(packageJson.version, '0.8.3');
  assert.ok(packageJson.files.length > 0);
  assert.ok(files.has('bin/loam.mjs'));
  assert.ok(files.has('.opencode/plugins/loam.js'));
  assert.ok(files.has('.claude-plugin/plugin.json'));
});

test('publication guard rejects missing main, integration, and adapter assets', async () => {
  const packageRoot = await mkdtemp(join(tmpdir(), 'loam-package-'));
  await writeFile(
    join(packageRoot, 'package.json'),
    JSON.stringify({ main: '.opencode/plugins/loam.js', files: ['integration', 'adapters'] }),
  );

  await assert.rejects(
    () => assertPackageAssets({ packageRoot }),
    /package main asset is missing/,
  );

  await mkdir(join(packageRoot, '.opencode', 'plugins'), { recursive: true });
  await writeFile(join(packageRoot, '.opencode', 'plugins', 'loam.js'), 'export {};\n');
  await assert.rejects(
    () => assertPackageAssets({ packageRoot }),
    /package asset is missing: integration/,
  );

  await mkdir(join(packageRoot, 'integration'));
  await writeFile(join(packageRoot, 'integration', 'loam.mjs'), 'export {};\n');
  await assert.rejects(
    () => assertPackageAssets({ packageRoot }),
    /package asset is missing: adapters/,
  );
});
