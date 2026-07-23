import assert from 'node:assert/strict';
import { mkdtemp, readFile, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';

import { acquireSetupLock, withSetupLock } from '../setup/lock.mjs';

test('setup lock is exclusive and releases its own metadata', async () => {
  const globalRoot = await mkdtemp(join(tmpdir(), 'loam-lock-'));
  const first = await acquireSetupLock({ globalRoot, timeoutMs: 20, pollMs: 1 });
  const metadata = JSON.parse(await readFile(join(globalRoot, 'setup.lock'), 'utf8'));
  assert.equal(metadata.pid, process.pid);
  assert.ok(metadata.token);

  await assert.rejects(
    () => acquireSetupLock({ globalRoot, timeoutMs: 10, pollMs: 1, isProcessAlive: async () => true }),
    /setup lock is held/,
  );

  await first.release();
  await assert.rejects(() => readFile(join(globalRoot, 'setup.lock')), { code: 'ENOENT' });
});

test('stale lock metadata is reclaimed only when its owner is gone', async () => {
  const globalRoot = await mkdtemp(join(tmpdir(), 'loam-lock-stale-'));
  await writeFile(
    join(globalRoot, 'setup.lock'),
    JSON.stringify({ pid: 42424242, started_at: new Date(Date.now() - 60_000).toISOString(), token: 'old' }),
  );
  const lock = await acquireSetupLock({
    globalRoot,
    timeoutMs: 20,
    pollMs: 1,
    staleMs: 1_000,
    isProcessAlive: async () => false,
  });
  assert.notEqual(JSON.parse(await readFile(join(globalRoot, 'setup.lock'), 'utf8')).token, 'old');
  await lock.release();
});

test('withSetupLock releases the lock after a failed transaction', async () => {
  const globalRoot = await mkdtemp(join(tmpdir(), 'loam-lock-finally-'));
  await assert.rejects(
    () => withSetupLock({ globalRoot }, async () => { throw new Error('transaction failed'); }),
    /transaction failed/,
  );
  await assert.rejects(() => readFile(join(globalRoot, 'setup.lock')), { code: 'ENOENT' });
});
