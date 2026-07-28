import assert from 'node:assert/strict';
import { mkdtemp, stat } from 'node:fs/promises';
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
