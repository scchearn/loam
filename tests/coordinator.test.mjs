import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { createServer } from 'node:http';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { promisify } from 'node:util';

import {
  CoordinatorUnavailableError,
  invokeCoordinator,
  loadCoordinator,
  parseCoordinatorArgs,
  runCoordinator,
} from '../integration/coordinator.mjs';

const METHODS = ['registerIdentity', 'send', 'subscribe', 'spawn', 'status', 'cancel', 'resume'];
const execFileAsync = promisify(execFile);
const packageRoot = fileURLToPath(new URL('..', import.meta.url));

function fakeCoordinator(calls = []) {
  return Object.fromEntries(METHODS.map((method) => [method, async (input) => {
    calls.push({ method, input });
    return { method, input };
  }]));
}

test('coordinator invocation calls the owned interface directly', async () => {
  const calls = [];
  const result = await invokeCoordinator('send', { body: 'hello' }, { coordinator: fakeCoordinator(calls) });

  assert.deepEqual(result, { method: 'send', input: { body: 'hello' } });
  assert.deepEqual(calls, [{ method: 'send', input: { body: 'hello' } }]);
});

test('coordinator loader accepts a ready module and validates the complete interface', async () => {
  const dir = await mkdtemp(join(tmpdir(), 'loam-coordinator-'));
  const modulePath = join(dir, 'coordinator.mjs');
  try {
    await writeFile(modulePath, `export default ${JSON.stringify({})};\n`);
    await assert.rejects(
      () => loadCoordinator({ modulePath }),
      (error) => error instanceof CoordinatorUnavailableError && /missing methods|ready Coordinator/.test(error.message),
    );

    const readyModulePath = join(dir, 'ready-coordinator.mjs');
    await writeFile(readyModulePath, `export const coordinator = {\n${METHODS.map((method) => `  ${method}: async () => ({ ok: true }),`).join('\n')}\n};\n`);
    const coordinator = await loadCoordinator({ modulePath: pathToFileURL(readyModulePath).href });
    assert.deepEqual(await coordinator.status({}), { ok: true });
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test('coordinator loader fails closed when no module is configured', async () => {
  await assert.rejects(
    () => loadCoordinator({ modulePath: '', baseUrl: '' }),
    (error) => error instanceof CoordinatorUnavailableError && /LOAM_COORDINATOR_MODULE/.test(error.message),
  );
});

test('coordinator loader uses the authenticated loopback service directly', async () => {
  const requests = [];
  const server = createServer(async (request, response) => {
    const chunks = [];
    for await (const chunk of request) chunks.push(chunk);
    requests.push({ url: request.url, auth: request.headers.authorization, body: JSON.parse(Buffer.concat(chunks).toString('utf8')) });
    response.setHeader('Content-Type', 'application/json');
    response.end(JSON.stringify({ state: 'idle' }));
  });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  const port = server.address().port;
  try {
    const coordinator = await loadCoordinator({ modulePath: '', baseUrl: `http://127.0.0.1:${port}`, token: 'test-token' });
    assert.deepEqual(await coordinator.status({ actorId: 'actor-1' }), { state: 'idle' });
    assert.deepEqual(requests, [{
      url: '/v1/status',
      auth: 'Bearer test-token',
      body: { actorId: 'actor-1' },
    }]);
  } finally {
    await new Promise((resolve) => server.close(resolve));
  }
});

test('coordinator CLI parses one JSON payload and emits the direct result', async () => {
  assert.deepEqual(parseCoordinatorArgs(['status', '--json', '{"actorId":"a"}']), {
    operation: 'status',
    payload: { actorId: 'a' },
  });
  assert.throws(() => parseCoordinatorArgs(['send']), /usage:/);

  const chunks = [];
  const result = await runCoordinator(['send', '--json', '{"body":"hello"}'], {
    output: { write: (chunk) => chunks.push(chunk) },
    coordinator: fakeCoordinator(),
  });
  assert.deepEqual(result, { method: 'send', input: { body: 'hello' } });
  assert.equal(chunks.join(''), '{"method":"send","input":{"body":"hello"}}\n');
});

test('coordinator CLI completes a direct Loam operation with hcom absent from PATH', async () => {
  const dir = await mkdtemp(join(tmpdir(), 'loam-coordinator-cli-'));
  const modulePath = join(dir, 'coordinator.mjs');
  const binPath = join(packageRoot, 'bin', 'coordinator.mjs');
  try {
    await writeFile(modulePath, `export const coordinator = {\n${METHODS.map((method) => `  ${method}: async (input) => ({ method: "${method}", input }),`).join('\n')}\n};\n`);
    const result = await execFileAsync(process.execPath, [binPath, 'send', '--json', '{"body":"no-hcom-proof"}'], {
      cwd: dir,
      env: { ...process.env, PATH: '', LOAM_COORDINATOR_MODULE: modulePath },
    });
    assert.deepEqual(JSON.parse(result.stdout), { method: 'send', input: { body: 'no-hcom-proof' } });
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});
