import assert from 'node:assert/strict';
import { test } from 'node:test';

import { provisionSealTrust } from '../setup/seal.mjs';

const ok = (created) => async (request) => ({
  code: 0,
  stdout: JSON.stringify({ principal: 'agent.abc@org', created }),
  stderr: '',
  request,
});

test('runs `seal trust init --json` exactly, with no interactive callbacks', async () => {
  const calls = [];
  const runner = async (request) => {
    calls.push(request);
    return ok(['agent_ed25519', 'agent_ed25519.pub'])(request);
  };
  const result = await provisionSealTrust({ runtimePath: '/rt/loam', cwd: '/g', runner });
  assert.equal(calls.length, 1);
  assert.deepEqual(calls[0].args, ['seal', 'trust', 'init', '--json']);
  assert.equal(calls[0].runtimePath, '/rt/loam');
  assert.equal(calls[0].input, undefined);
  assert.equal(result.ready, true);
  assert.equal(result.principal, 'agent.abc@org');
  assert.deepEqual(result.created, ['agent_ed25519', 'agent_ed25519.pub']);
});

test('a second run reports nothing created', async () => {
  const result = await provisionSealTrust({ runtimePath: '/rt/loam', runner: ok([]) });
  assert.equal(result.ready, true);
  assert.deepEqual(result.created, []);
});

test('a runner failure or unknown-command exit is skip seal_unavailable', async () => {
  for (const runner of [
    async () => ({ code: 2, stdout: '', stderr: 'unknown command: seal' }),
    async () => ({ code: null, category: 'runtime_error', stdout: '', stderr: 'ENOENT' }),
    async () => { throw new Error('boom'); },
    async () => ({ code: 0, stdout: 'not json', stderr: '' }),
    async () => ({ code: 0, stdout: '{}', stderr: '' }),
  ]) {
    const result = await provisionSealTrust({ runtimePath: '/rt/loam', runner });
    assert.equal(result.ready, false);
    assert.equal(result.category, 'seal_unavailable');
    assert.deepEqual(result.created, []);
  }
});

test('a missing runtime path is a skip and makes no call', async () => {
  const runner = async () => { throw new Error('should not run'); };
  const result = await provisionSealTrust({ runtimePath: null, runner });
  assert.equal(result.category, 'seal_unavailable');
});
