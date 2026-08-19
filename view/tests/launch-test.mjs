import assert from 'node:assert/strict';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';

import { assertSupportedNode, launch, LaunchError } from '../launch.mjs';

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
