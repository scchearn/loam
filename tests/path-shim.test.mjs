import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { writeSync } from 'node:fs';
import { chmod, cp, mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { test } from 'node:test';

import { ledgerPath, writeLedger } from '../integration/ledger.mjs';
import {
  installShim,
  removeShim,
  shimLocations,
  verifyShim,
} from '../setup/shim.mjs';

const execFileAsync = promisify(execFile);
const packageRoot = fileURLToPath(new URL('..', import.meta.url));
const target = process.platform === 'win32' ? 'x86_64-pc-windows-msvc' : 'x86_64-unknown-linux-musl';

async function stageIntegration(globalRoot) {
  const source = join(packageRoot, 'integration');
  const destination = join(globalRoot, 'integration', 'active');
  await mkdir(destination, { recursive: true });
  await cp(source, destination, { recursive: true });
  return join(destination, 'loam.mjs');
}

async function runtimeFixture({ home, configDir, version }) {
  const runtimePath = join(configDir, 'runtime', version, target, process.platform === 'win32' ? 'loam.exe' : 'loam');
  await mkdir(dirname(runtimePath), { recursive: true });
  if (process.platform === 'win32') {
    // Windows CI exercises the real native runtime. The platform-agnostic test
    // below verifies the resolver and lifecycle without trying to fake a PE.
    await writeFile(runtimePath, 'fixture runtime');
  } else {
    await writeFile(runtimePath, `#!/usr/bin/env node\nconst { writeSync } = require('node:fs');\nwriteSync(1, process.argv[2] === '--version' ? '${version}\\n' : '');\n`);
    await chmod(runtimePath, 0o700);
  }
  const bytes = await readFile(runtimePath);
  await writeLedger({
    channel: 'pinned',
    target: version,
    sha256: createHash('sha256').update(bytes).digest('hex'),
    store_path: runtimePath,
  }, { root: configDir });
  return runtimePath;
}

async function fixture() {
  const home = await mkdtemp(join(tmpdir(), 'loam-shim-home-'));
  const globalRoot = join(home, '.agents', 'loam');
  const configDir = join(home, '.config', 'loam');
  await mkdir(globalRoot, { recursive: true });
  const integrationPath = await stageIntegration(globalRoot);
  await writeFile(join(globalRoot, 'install.json'), JSON.stringify({ integration_path: integrationPath }));
  await runtimeFixture({ home, configDir, version: '1.0.0' });
  return { home, globalRoot, configDir };
}

function envFor({ home, configDir, path = '' } = {}) {
  return {
    ...process.env,
    HOME: home,
    USERPROFILE: home,
    LOAM_HOME: join(home, '.agents', 'loam'),
    LOAM_CONFIG_DIR: configDir,
    PATH: path,
  };
}

test('install creates one dynamic shim that follows a runtime version bump without changing bytes', async (t) => {
  const fx = await fixture();
  const locations = shimLocations({ home: fx.home, platform: process.platform });
  const installed = await installShim({
    home: fx.home,
    globalRoot: fx.globalRoot,
    platform: process.platform,
    env: envFor(fx),
  });
  assert.equal(installed.path, locations.shimPath);
  const before = await readFile(locations.shimPath);

  if (process.platform === 'win32') {
    t.skip('native runtime execution is covered by the Windows Actions job');
    return;
  }

  const path = `${locations.binDir}:${process.env.PATH || ''}`;
  const first = await execFileAsync(locations.shimPath, ['--version'], {
    env: envFor({ ...fx, path }),
    cwd: fx.home,
  });
  assert.equal(first.stdout.trim(), '1.0.0');

  await runtimeFixture({ home: fx.home, configDir: fx.configDir, version: '1.1.0' });
  const second = await execFileAsync(locations.shimPath, ['--version'], {
    env: envFor({ ...fx, path }),
    cwd: fx.home,
  });
  assert.equal(second.stdout.trim(), '1.1.0');
  assert.deepEqual(await readFile(locations.shimPath), before);

  const update = await installShim({
    home: fx.home,
    globalRoot: fx.globalRoot,
    platform: process.platform,
    env: envFor({ ...fx, path }),
    update: true,
  });
  assert.equal(update.action, 'unchanged');
  assert.deepEqual(await readFile(locations.shimPath), before);
});

test('doctor verifies a healthy shim and gives actionable missing/off-PATH/runtime failures', async () => {
  const fx = await fixture();
  const locations = shimLocations({ home: fx.home, platform: process.platform });
  const env = envFor({ ...fx, path: locations.binDir });
  const pathRunner = process.platform === 'win32'
    ? async ({ action, env }) => {
      if (action === 'read') {
        const entry = String(env.PATH || '').replaceAll('\\', '/').toUpperCase();
        return { code: 0, stdout: `C:/Windows/System32${entry ? `;${entry}` : ''}\n` };
      }
      return { code: 0, stdout: '' };
    }
    : undefined;
  const shimOptions = pathRunner ? { pathRunner } : {};
  await installShim({ home: fx.home, globalRoot: fx.globalRoot, platform: process.platform, env, ...shimOptions });

  const healthy = await verifyShim({
    home: fx.home,
    globalRoot: fx.globalRoot,
    platform: process.platform,
    env,
    ...shimOptions,
    requireOnPath: true,
  });
  assert.equal(healthy.ready, true, healthy.detail);

  const offPath = await verifyShim({
    home: fx.home,
    globalRoot: fx.globalRoot,
    platform: process.platform,
    env: envFor(fx),
    ...shimOptions,
    requireOnPath: true,
  });
  assert.equal(offPath.ready, false);
  assert.equal(offPath.category, 'shim_off_path');
  assert.match(offPath.detail, /PATH|path/i);

  await removeShim({ home: fx.home, globalRoot: fx.globalRoot, platform: process.platform, env, ...shimOptions });
  const missing = await verifyShim({
    home: fx.home,
    globalRoot: fx.globalRoot,
    platform: process.platform,
    env,
    ...shimOptions,
    requireOnPath: true,
  });
  assert.equal(missing.ready, false);
  assert.equal(missing.category, 'shim_missing');
  assert.match(missing.detail, /install/i);

  await installShim({ home: fx.home, globalRoot: fx.globalRoot, platform: process.platform, env, ...shimOptions });
  await writeFile(await ledgerPath({ root: fx.configDir }), '{"broken":true}');
  const stale = await verifyShim({
    home: fx.home,
    globalRoot: fx.globalRoot,
    platform: process.platform,
    env,
    ...shimOptions,
    requireOnPath: true,
  });
  assert.equal(stale.ready, false);
  assert.match(stale.detail, /runtime|ledger|install/i);
});

test('Windows install records only its own per-user PATH entry and uninstall reverses it', async () => {
  const fx = await fixture();
  const locations = shimLocations({
    home: fx.home,
    platform: 'win32',
    env: { LOCALAPPDATA: join(fx.home, 'AppData', 'Local') },
  });
  let userPath = 'C:\\Windows\\System32';
  const pathRunner = async ({ action, entry }) => {
    if (action === 'read') return { code: 0, stdout: `${userPath}\n` };
    if (action === 'add') userPath = `${userPath};${entry}`;
    if (action === 'remove') userPath = userPath.split(';').filter((part) => part !== entry).join(';');
    return { code: 0, stdout: '' };
  };
  const env = { ...envFor(fx), LOCALAPPDATA: join(fx.home, 'AppData', 'Local') };
  const installed = await installShim({ home: fx.home, globalRoot: fx.globalRoot, platform: 'win32', env, pathRunner });

  assert.equal(installed.pathAdded, true);
  assert.match(userPath, new RegExp(locations.binDir.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')));
  assert.equal(await verifyShim({
    home: fx.home,
    globalRoot: fx.globalRoot,
    platform: 'win32',
    env,
    pathRunner,
    requireOnPath: true,
  }).then((result) => result.ready), true);

  await rm(locations.shimPath, { force: true });
  await rm(locations.scriptPath, { force: true });
  const repaired = await installShim({
    home: fx.home,
    globalRoot: fx.globalRoot,
    platform: 'win32',
    env,
    pathRunner,
    existing: installed.record,
    update: true,
  });
  assert.equal(repaired.pathAdded, true);

  await removeShim({ home: fx.home, globalRoot: fx.globalRoot, platform: 'win32', env, pathRunner, record: installed });
  assert.doesNotMatch(userPath, new RegExp(locations.binDir.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')));
});
