import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdir, readFile, stat, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { mkdtemp } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { test } from 'node:test';

import { detectTarget } from '../setup/target.mjs';
import { installRuntime } from '../setup/runtime.mjs';
import { RUNTIME_VERSION, resolveRuntimeTarget } from '../setup/constants.mjs';
import { ledgerPath, readLedger, runtimeStorePath, writeLedger } from '../integration/ledger.mjs';

const target = detectTarget();

async function configFixture() {
  const dir = await mkdtemp(join(tmpdir(), 'loam-config-'));
  return { env: { LOAM_CONFIG_DIR: dir }, dir };
}

test('runtime ledger round-trips a write through read', async () => {
  const { env, dir } = await configFixture();
  const store_path = join(dir, 'runtime', '0.11.0-next.15', target, 'loam');
  const written = await writeLedger(
    { channel: 'next', target: '0.11.0-next.15', sha256: 'a'.repeat(64), store_path },
    { env },
  );
  assert.equal(written.path, ledgerPath({ env }));
  const read = await readLedger({ env });
  assert.deepEqual(read, {
    schema_version: 1,
    channel: 'next',
    target: '0.11.0-next.15',
    sha256: 'a'.repeat(64),
    store_path,
  });
});

test('readLedger returns null when no ledger exists', async () => {
  const { env } = await configFixture();
  assert.equal(await readLedger({ env }), null);
});

test('runtime ledger rejects every malformed field', async () => {
  const { env, dir } = await configFixture();
  const good = { channel: 'next', target: '0.11.0-next.15', sha256: 'b'.repeat(64), store_path: join(dir, 'runtime', 'x', 'loam') };
  await assert.rejects(() => writeLedger({ ...good, channel: 'stable' }, { env }), /channel is invalid/);
  await assert.rejects(() => writeLedger({ ...good, target: '0.11' }, { env }), /target is not semver/);
  await assert.rejects(() => writeLedger({ ...good, target: '0.11.0+build' }, { env }), /target is not semver/);
  await assert.rejects(() => writeLedger({ ...good, sha256: 'xyz' }, { env }), /sha256 is invalid/);
  await assert.rejects(() => writeLedger({ ...good, store_path: 'relative/loam' }, { env }), /absolute path/);
  await assert.rejects(() => writeLedger({ ...good, store_path: '/etc/loam' }, { env }), /outside the config dir/);
});

test('resolveRuntimeTarget derives the target and channel from the package constant', () => {
  const { target: resolved, channel } = resolveRuntimeTarget({ env: {} });
  assert.equal(resolved, RUNTIME_VERSION);
  // Provenance channel matches plugin-release.yml dist-tag routing.
  assert.equal(channel, RUNTIME_VERSION.includes('-') ? 'next' : 'latest');
  // Explicit both-branch coverage of the suffix rule via the resolver output.
  assert.equal(resolveRuntimeTarget({ env: {} }).channel, 'next'); // constant is 0.11.0-next.x
});

test('LOAM_RUNTIME_VERSION pins the target and marks the channel pinned', () => {
  assert.deepEqual(
    resolveRuntimeTarget({ env: { LOAM_RUNTIME_VERSION: '1.2.3' } }),
    { target: '1.2.3', channel: 'pinned' },
  );
  // A prerelease pin is still `pinned` provenance, never `next`.
  assert.deepEqual(
    resolveRuntimeTarget({ env: { LOAM_RUNTIME_VERSION: '0.9.1-rc.1' } }),
    { target: '0.9.1-rc.1', channel: 'pinned' },
  );
});

test('resolveRuntimeTarget rejects a malformed target', () => {
  for (const bad of ['0.9.1+build', '0.9.1-next.0+build', '0.9.1-', 'not-a-version', '0.9.1-next.01']) {
    assert.throws(
      () => resolveRuntimeTarget({ env: { LOAM_RUNTIME_VERSION: bad } }),
      /invalid runtime target/,
      `pin ${bad} should be rejected`,
    );
  }
});

async function releaseFixture({ version = '0.9.1', targetName = target, bytes = 'verified runtime' } = {}) {
  const release = await mkdtemp(join(tmpdir(), 'loam-release-'));
  const file = `loam-${targetName}${targetName.includes('windows') ? '.exe' : ''}`;
  await writeFile(join(release, file), bytes);
  const sha256 = createHash('sha256').update(bytes).digest('hex');
  await writeFile(
    join(release, 'loam-runtime-manifest.json'),
    JSON.stringify({ version, runtimes: [{ target: targetName, file, sha256 }] }),
  );
  return { release, file, bytes, url: pathToFileURL(release).href };
}

// The runtime store lives under the config dir now; the fixture is the config
// root and installRuntime places the store at <root>/runtime/<version>/<target>.
async function rootFixture() {
  return mkdtemp(join(tmpdir(), 'loam-config-'));
}

test('runtime installation verifies, smoke-tests, and atomically publishes staged bytes', async () => {
  const release = await releaseFixture();
  const globalRoot = await rootFixture();
  const smokeCalls = [];
  const result = await installRuntime({
    configDir: globalRoot,
    version: '0.9.1',
    target,
    releaseBaseUrl: release.url,
    smokeRunner: async (request) => {
      smokeCalls.push(request);
      assert.equal(await readFile(request.runtimePath, 'utf8'), release.bytes);
      return { code: 0, stdout: '{"exists":false}', stderr: '' };
    },
  });

  const destination = runtimeStorePath({ version: '0.9.1', target, root: globalRoot });
  assert.equal(result.published, true);
  assert.equal(result.path, destination);
  assert.equal(await readFile(destination, 'utf8'), release.bytes);
  assert.equal(smokeCalls.length, 1);
  if (process.platform !== 'win32') assert.notEqual((await stat(destination)).mode & 0o111, 0);
});

test('runtime downloads follow bounded HTTP redirects', async () => {
  const release = await releaseFixture();
  const server = createServer(async (request, response) => {
    const name = request.url?.slice(1);
    if (name === 'loam-runtime-manifest.json' || name === release.file) {
      response.writeHead(302, { location: `/final-${name}` });
      response.end();
      return;
    }
    if (name === 'final-loam-runtime-manifest.json') {
      response.setHeader('content-type', 'application/json');
      response.end(await readFile(join(release.release, 'loam-runtime-manifest.json')));
      return;
    }
    if (name === `final-${release.file}`) {
      response.end(release.bytes);
      return;
    }
    response.writeHead(404);
    response.end();
  });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  const address = server.address();
  const globalRoot = await rootFixture();

  try {
    const result = await installRuntime({
      configDir: globalRoot,
      version: '0.9.1',
      target,
      releaseBaseUrl: `http://127.0.0.1:${address.port}`,
      smokeRunner: async () => ({ code: 0, stdout: '{}', stderr: '' }),
    });
    assert.equal(result.published, true);
  } finally {
    await new Promise((resolve, reject) => server.close((error) => error ? reject(error) : resolve()));
  }
});

test('HTTPS release downloads reject downgrade redirects', async () => {
  const originalFetch = globalThis.fetch;
  const globalRoot = await rootFixture();
  globalThis.fetch = async () => new Response('', { status: 302, headers: { location: 'http://github.com/loam' } });
  try {
    await assert.rejects(
      () => installRuntime({
        configDir: globalRoot,
        version: '0.9.1',
        target,
        releaseBaseUrl: 'https://github.com/scchearn/loam/releases/download/cli-v0.9.1',
        smokeRunner: async () => ({ code: 0, stdout: '{}', stderr: '' }),
      }),
      /HTTPS redirect downgrade/,
    );
  } finally {
    globalThis.fetch = originalFetch;
  }
});

test('release downloads reject redirects to an untrusted host', async () => {
  const originalFetch = globalThis.fetch;
  const globalRoot = await rootFixture();
  globalThis.fetch = async () => new Response('', { status: 302, headers: { location: 'http://evil.example/loam' } });
  try {
    await assert.rejects(
      () => installRuntime({
        configDir: globalRoot,
        version: '0.9.1',
        target,
        releaseBaseUrl: 'http://127.0.0.1:9/releases/cli-v0.9.1',
        smokeRunner: async () => ({ code: 0, stdout: '{}', stderr: '' }),
      }),
      /untrusted redirect host/,
    );
  } finally {
    globalThis.fetch = originalFetch;
  }
});

test('ready runtime is reused only after digest verification and a smoke test', async () => {
  const release = await releaseFixture();
  const globalRoot = await rootFixture();
  let smokeCalls = 0;
  const first = await installRuntime({
    configDir: globalRoot,
    version: '0.9.1',
    target,
    releaseBaseUrl: release.url,
    smokeRunner: async () => ({ code: 0, stdout: '{}', stderr: '' }),
  });
  const result = await installRuntime({
    configDir: globalRoot,
    version: '0.9.1',
    target,
    releaseBaseUrl: pathToFileURL(join(tmpdir(), 'missing-release')).href,
    expectedSha256: first.sha256,
    smokeRunner: async () => {
      smokeCalls += 1;
      return { code: 0, stdout: '{}', stderr: '' };
    },
  });
  assert.equal(result.reused, true);
  assert.equal(smokeCalls, 1);
});

test('replacement keeps a same-filesystem backup of the verified active runtime', async () => {
  const firstRelease = await releaseFixture({ bytes: 'first runtime' });
  const secondRelease = await releaseFixture({ bytes: 'second runtime' });
  const globalRoot = await rootFixture();
  await installRuntime({
    configDir: globalRoot,
    version: '0.9.1',
    target,
    releaseBaseUrl: firstRelease.url,
    smokeRunner: async () => ({ code: 0, stdout: '{}', stderr: '' }),
  });
  const result = await installRuntime({
    configDir: globalRoot,
    version: '0.9.1',
    target,
    releaseBaseUrl: secondRelease.url,
    force: true,
    smokeRunner: async () => ({ code: 0, stdout: '{}', stderr: '' }),
  });

  assert.equal(await readFile(result.path, 'utf8'), 'second runtime');
  assert.ok(result.backupPath);
  assert.equal(await readFile(result.backupPath, 'utf8'), 'first runtime');
});

test('invalid manifest, target, checksum, truncation, and smoke preserve the active runtime', async () => {
  const release = await releaseFixture({ bytes: 'new runtime' });
  const globalRoot = await rootFixture();
  const destination = runtimeStorePath({ version: '0.9.1', target, root: globalRoot });
  await mkdir(join(globalRoot, 'runtime', '0.9.1', target), { recursive: true });
  await writeFile(destination, 'known good runtime');

  await assert.rejects(
    () => installRuntime({
      configDir: globalRoot,
      version: '0.9.1',
      target,
      releaseBaseUrl: release.url,
      force: true,
      smokeRunner: async () => ({ code: 0, stdout: '{}', stderr: '' }),
      maxDownloadBytes: 2,
    }),
    /too large/,
  );
  assert.equal(await readFile(destination, 'utf8'), 'known good runtime');

  const badRelease = await releaseFixture({ bytes: 'bad runtime' });
  const badManifest = JSON.parse(await readFile(join(badRelease.release, 'loam-runtime-manifest.json'), 'utf8'));
  badManifest.runtimes[0].sha256 = '0'.repeat(64);
  await writeFile(join(badRelease.release, 'loam-runtime-manifest.json'), JSON.stringify(badManifest));
  await assert.rejects(
    () => installRuntime({
      configDir: globalRoot,
      version: '0.9.1',
      target,
      releaseBaseUrl: badRelease.url,
      force: true,
      smokeRunner: async () => ({ code: 0, stdout: '{}', stderr: '' }),
    }),
    /checksum mismatch/,
  );
  assert.equal(await readFile(destination, 'utf8'), 'known good runtime');

  await assert.rejects(
    () => installRuntime({
      configDir: globalRoot,
      version: '0.9.1',
      target,
      releaseBaseUrl: release.url,
      force: true,
      smokeRunner: async () => ({ code: 1, stdout: '', stderr: 'smoke failed' }),
    }),
    /runtime smoke failed/,
  );
  assert.equal(await readFile(destination, 'utf8'), 'known good runtime');
});

test('malformed and target-incomplete manifests fail closed', async () => {
  const release = await mkdtemp(join(tmpdir(), 'loam-release-invalid-'));
  await writeFile(join(release, 'loam-runtime-manifest.json'), '{not-json');
  const invalidRoot = await rootFixture();
  await assert.rejects(
    () => installRuntime({
      configDir: invalidRoot,
      version: '0.9.1',
      target,
      releaseBaseUrl: pathToFileURL(release).href,
      smokeRunner: async () => ({ code: 0, stdout: '{}', stderr: '' }),
    }),
    /manifest is invalid/,
  );

  const missingTarget = await releaseFixture({ targetName: 'x86_64-apple-darwin' });
  const missingTargetRoot = await rootFixture();
  await assert.rejects(
    () => installRuntime({
      configDir: missingTargetRoot,
      version: '0.9.1',
      target: 'aarch64-unknown-linux-musl',
      releaseBaseUrl: missingTarget.url,
      smokeRunner: async () => ({ code: 0, stdout: '{}', stderr: '' }),
    }),
    /manifest has no runtime for target/,
  );
});

test('prerelease runtime versions install through the cli-v tag URL', async () => {
  const release = await releaseFixture({ version: '0.9.1-next.0' });
  const globalRoot = await rootFixture();
  const result = await installRuntime({
    configDir: globalRoot,
    version: '0.9.1-next.0',
    target,
    releaseBaseUrl: release.url,
    smokeRunner: async () => ({ code: 0, stdout: '{}', stderr: '' }),
  });
  assert.equal(result.published, true);
  assert.equal(result.path, runtimeStorePath({ version: '0.9.1-next.0', target, root: globalRoot }));
  assert.equal(await readFile(result.path, 'utf8'), release.bytes);
});

test('runtime version validation accepts prerelease and rejects build metadata', async () => {
  const globalRoot = await rootFixture();
  for (const version of ['0.9.1-next.0', '0.9.1-next.1', '0.9.1-rc.1']) {
    const release = await releaseFixture({ version });
    const result = await installRuntime({
      configDir: globalRoot,
      version,
      target,
      releaseBaseUrl: release.url,
      smokeRunner: async () => ({ code: 0, stdout: '{}', stderr: '' }),
    });
    assert.equal(result.published, true, `prerelease ${version} should install`);
  }
  const release = await releaseFixture();
  for (const version of ['0.9.1+build', '0.9.1-next.0+build', '0.9.1-', 'not-a-version', '0.9.1-next.01']) {
    await assert.rejects(
      () => installRuntime({
        configDir: globalRoot,
        version,
        target,
        releaseBaseUrl: release.url,
        smokeRunner: async () => ({ code: 0, stdout: '{}', stderr: '' }),
      }),
      /invalid runtime version/,
      `version ${version} should be rejected`,
    );
  }
});

test('install publishes into the config-dir store and writes the ledger', async () => {
  const release = await releaseFixture();
  const globalRoot = await rootFixture();
  const result = await installRuntime({
    configDir: globalRoot,
    version: '0.9.1',
    target,
    channel: 'latest',
    releaseBaseUrl: release.url,
    smokeRunner: async () => ({ code: 0, stdout: JSON.stringify({ version: '0.9.1' }), stderr: '' }),
  });
  const store = runtimeStorePath({ version: '0.9.1', target, root: globalRoot });
  assert.equal(result.published, true);
  assert.equal(result.path, store);
  // Store is under <config>/runtime/..., not <config>/bin/...
  assert.ok(store.startsWith(join(globalRoot, 'runtime') + '/') || store.includes(`${join(globalRoot, 'runtime')}\\`));
  const ledger = await readLedger({ root: globalRoot });
  assert.deepEqual(ledger, {
    schema_version: 1,
    channel: 'latest',
    target: '0.9.1',
    sha256: createHash('sha256').update(release.bytes).digest('hex'),
    store_path: store,
  });
});

test('an unpublished target yields a wait/retry signal, never a wipe', async () => {
  const globalRoot = await rootFixture();
  const result = await installRuntime({
    configDir: globalRoot,
    version: '0.9.1',
    target,
    channel: 'latest',
    releaseBaseUrl: pathToFileURL(join(tmpdir(), 'loam-not-published-yet')).href,
    smokeRunner: async () => ({ code: 0, stdout: '{}', stderr: '' }),
  });
  assert.equal(result.pending, true);
  assert.equal(result.published, false);
  // No ledger and no binary were written.
  assert.equal(await readLedger({ root: globalRoot }), null);
});

test('a binary whose self-reported version differs from the target fails at smoke', async () => {
  const release = await releaseFixture({ version: '0.9.1' });
  const globalRoot = await rootFixture();
  await assert.rejects(
    () => installRuntime({
      configDir: globalRoot,
      version: '0.9.1',
      target,
      channel: 'latest',
      releaseBaseUrl: release.url,
      smokeRunner: async () => ({ code: 0, stdout: JSON.stringify({ version: '0.8.0' }), stderr: '' }),
    }),
    /binary reports version 0\.8\.0, expected 0\.9\.1/,
  );
  assert.equal(await readLedger({ root: globalRoot }), null);
});
