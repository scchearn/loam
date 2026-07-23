import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdir, readFile, stat, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { mkdtemp } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { test } from 'node:test';

import { detectTarget, runtimePath } from '../setup/target.mjs';
import { installRuntime } from '../setup/runtime.mjs';

const target = detectTarget();

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

async function rootFixture() {
  const home = await mkdtemp(join(tmpdir(), 'loam-runtime-'));
  return join(home, '.agents', 'loam');
}

test('runtime installation verifies, smoke-tests, and atomically publishes staged bytes', async () => {
  const release = await releaseFixture();
  const globalRoot = await rootFixture();
  const smokeCalls = [];
  const result = await installRuntime({
    globalRoot,
    version: '0.9.1',
    target,
    releaseBaseUrl: release.url,
    smokeRunner: async (request) => {
      smokeCalls.push(request);
      assert.equal(await readFile(request.runtimePath, 'utf8'), release.bytes);
      return { code: 0, stdout: '{"exists":false}', stderr: '' };
    },
  });

  const destination = runtimePath(globalRoot, '0.9.1', target);
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
      globalRoot,
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
        globalRoot,
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
        globalRoot,
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
    globalRoot,
    version: '0.9.1',
    target,
    releaseBaseUrl: release.url,
    smokeRunner: async () => ({ code: 0, stdout: '{}', stderr: '' }),
  });
  const result = await installRuntime({
    globalRoot,
    version: '0.9.1',
    target,
    releaseBaseUrl: 'file:///missing-release',
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
    globalRoot,
    version: '0.9.1',
    target,
    releaseBaseUrl: firstRelease.url,
    smokeRunner: async () => ({ code: 0, stdout: '{}', stderr: '' }),
  });
  const result = await installRuntime({
    globalRoot,
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
  const destination = runtimePath(globalRoot, '0.9.1', target);
  await mkdir(join(globalRoot, 'bin', '0.9.1', target), { recursive: true });
  await writeFile(destination, 'known good runtime');

  await assert.rejects(
    () => installRuntime({
      globalRoot,
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
      globalRoot,
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
      globalRoot,
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
      globalRoot: invalidRoot,
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
      globalRoot: missingTargetRoot,
      version: '0.9.1',
      target: 'aarch64-unknown-linux-musl',
      releaseBaseUrl: missingTarget.url,
      smokeRunner: async () => ({ code: 0, stdout: '{}', stderr: '' }),
    }),
    /manifest has no runtime for target/,
  );
});
