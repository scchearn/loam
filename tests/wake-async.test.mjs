import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { join } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { test } from 'node:test';

const packageRoot = fileURLToPath(new URL('..', import.meta.url));
const wakePath = join(packageRoot, 'plugins', 'loam-adapter', 'hooks', 'wake.mjs');
const wake = await import(pathToFileURL(wakePath).href);

function runWake(env, payload = {}) {
  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath, [wakePath], {
      cwd: packageRoot,
      env: { ...process.env, ...env },
    });
    let stdout = '';
    let stderr = '';
    child.stdout.on('data', (c) => { stdout += c; });
    child.stderr.on('data', (c) => { stderr += c; });
    child.once('error', reject);
    child.once('close', (code) => resolve({ code, stdout, stderr }));
    child.stdin.end(JSON.stringify(payload));
  });
}

test('isHeadlessClaude flags the Agent SDK entrypoint, not an interactive terminal', () => {
  assert.equal(wake.isHeadlessClaude({ CLAUDE_CODE_ENTRYPOINT: 'sdk-cli' }), true);
  assert.equal(wake.isHeadlessClaude({ CLAUDE_CODE_ENTRYPOINT: 'cli' }), false);
  assert.equal(wake.isHeadlessClaude({}), false);
});

test('handleWake short-circuits headless Claude to allow-stop without polling (#140)', async () => {
  const result = await wake.handleWake({ session_id: 's' }, { CLAUDE_CODE_ENTRYPOINT: 'sdk-cli' });
  assert.deepEqual(result, {});
});

test('renderWakeOutput: Claude wakes on a frame via stderr + exit 2 (asyncRewake), silent otherwise', () => {
  assert.deepEqual(
    wake.renderWakeOutput({ decision: 'block', reason: 'BODY' }, 'claude'),
    { exitCode: 2, stdout: '', stderr: 'BODY' },
  );
  assert.deepEqual(wake.renderWakeOutput({}, 'claude'), { exitCode: 0, stdout: '', stderr: '' });
  // A block with an empty body is not a delivery — stay silent, never exit 2 on nothing.
  assert.deepEqual(wake.renderWakeOutput({ decision: 'block', reason: '' }, 'claude'), { exitCode: 0, stdout: '', stderr: '' });
});

test('renderWakeOutput: Codex keeps the synchronous stdout block-decision contract', () => {
  assert.deepEqual(
    wake.renderWakeOutput({ decision: 'block', reason: 'BODY' }, 'codex'),
    { exitCode: 0, stdout: '{"decision":"block","reason":"BODY"}\n', stderr: '' },
  );
  assert.deepEqual(wake.renderWakeOutput({}, 'codex'), { exitCode: 0, stdout: '{}\n', stderr: '' });
});

test('wake.mjs headless Claude run exits 0 with no output (never holds the Stop pipeline)', async () => {
  const { code, stdout, stderr } = await runWake({ CLAUDE_CODE_ENTRYPOINT: 'sdk-cli' }, { session_id: 's' });
  assert.equal(code, 0);
  assert.equal(stdout, '');
  assert.equal(stderr, '');
});

test('wake.mjs Codex run with no session degrades to the stdout allow-stop sentinel', async () => {
  // PLUGIN_ROOT selects the Codex harness; no session_id => immediate fallback.
  const { code, stdout } = await runWake({ PLUGIN_ROOT: packageRoot, CLAUDE_CODE_ENTRYPOINT: '' }, {});
  assert.equal(code, 0);
  assert.equal(stdout, '{}\n');
});
