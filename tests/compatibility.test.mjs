import assert from 'node:assert/strict';
import { execFile, spawn } from 'node:child_process';
import { cp, mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { promisify } from 'node:util';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { test } from 'node:test';

import { assertPackageAssets } from '../setup/package-check.mjs';

const execFileAsync = promisify(execFile);
const packageRoot = fileURLToPath(new URL('..', import.meta.url));
const loaderPath = join(packageRoot, '.opencode', 'plugins', 'loam.js');
const hookPath = join(packageRoot, 'hooks', 'session-start.mjs');

async function runHook(env, payload = {}) {
  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath, [hookPath], {
      cwd: packageRoot,
      env: { ...process.env, ...env },
    });
    let stdout = '';
    let stderr = '';
    child.stdout.on('data', (chunk) => { stdout += chunk; });
    child.stderr.on('data', (chunk) => { stderr += chunk; });
    child.once('error', reject);
    child.once('close', (code) => resolve({ code, stdout, stderr }));
    child.stdin.end(JSON.stringify(payload));
  });
}

test('legacy OpenCode entry delegates to the shared adapter without startup polling', async () => {
  const source = await readFile(loaderPath, 'utf8');
  assert.match(source, /adapters[\\/]opencode\.mjs/);
  assert.doesNotMatch(source, /git ls-remote|loamstate\.(sh|ps1)|findSkillPath/);

  const plugin = await import(pathToFileURL(loaderPath).href);
  assert.equal(typeof plugin.LoamPlugin, 'function');
  assert.equal(typeof plugin.default, 'function');
});

test('missing adapter in an existing clone returns setup recovery instead of a loader error', async () => {
  const clone = await mkdtemp(join(tmpdir(), 'loam-legacy-clone-'));
  const cloneLoader = join(clone, '.opencode', 'plugins', 'loam.js');
  await mkdir(dirname(cloneLoader), { recursive: true });
  await cp(loaderPath, cloneLoader);

  try {
    const loaded = await import(`${pathToFileURL(cloneLoader).href}?fixture=${Date.now()}`);
    const plugin = await loaded.LoamPlugin({ directory: clone });
    const output = { messages: [{ info: { role: 'user' }, parts: [{ type: 'text', text: 'hello' }] }] };
    await plugin['experimental.chat.messages.transform']({}, output);
    assert.match(output.messages[0].parts[0].text, /npx @scchearn\/loam setup/);
  } finally {
    await rm(clone, { recursive: true, force: true });
  }
});

test('packed tarball contains a loadable adapter through the preserved main entry', async () => {
  const destination = await mkdtemp(join(tmpdir(), 'loam-pack-'));
  const extracted = join(destination, 'extracted');
  await mkdir(extracted);
  try {
    const { stdout } = await execFileAsync('npm', [
      'pack',
      '--ignore-scripts',
      '--silent',
      '--pack-destination',
      destination,
    ], { cwd: packageRoot });
    const tarball = join(destination, stdout.trim().split(/\r?\n/).at(-1));
    await execFileAsync('tar', ['-xzf', tarball], { cwd: extracted });
    const packedRoot = join(extracted, 'package');
    const packedLoader = join(packedRoot, '.opencode', 'plugins', 'loam.js');
    const loaded = await import(`${pathToFileURL(packedLoader).href}?fixture=${Date.now()}`);
    const plugin = await loaded.LoamPlugin({ directory: packedRoot });
    assert.equal(typeof plugin['experimental.chat.messages.transform'], 'function');
  } finally {
    await rm(destination, { recursive: true, force: true });
  }
});

test('publication guard rejects a package fixture missing the shared integration', async () => {
  const fixture = await mkdtemp(join(tmpdir(), 'loam-package-fixture-'));
  try {
    const excluded = ['/node_modules/', '/.git/', '/cli/', '/plans/', '/specs/', '/target/', '/tests/'];
    await cp(packageRoot, fixture, {
      recursive: true,
      filter: (source) => !excluded.some((part) => source.includes(part)),
    });
    await rm(join(fixture, 'integration'), { recursive: true, force: true });
    await assert.rejects(
      () => assertPackageAssets({ packageRoot: fixture }),
      /package asset is missing: integration/,
    );
  } finally {
    await rm(fixture, { recursive: true, force: true });
  }
});

test('packaged session hook emits valid Claude, Cursor, and default envelopes', async () => {
  for (const [env, field] of [
    [{ CLAUDE_PLUGIN_ROOT: packageRoot }, 'hookSpecificOutput'],
    [{ CURSOR_PLUGIN_ROOT: packageRoot }, 'additional_context'],
    [{ COPILOT_CLI: '1' }, 'additionalContext'],
  ]) {
    const result = await runHook(env, { cwd: join(packageRoot, 'workspace') });
    assert.equal(result.code, 0, result.stderr);
    const parsed = JSON.parse(result.stdout);
    assert.ok(parsed[field]);
    const context = field === 'hookSpecificOutput' ? parsed[field].additionalContext : parsed[field];
    assert.match(context, /<LOAM_IMPORTANT>/);
    assert.match(context, /npx @scchearn\/loam setup/);
  }
});

test('plugin manifests point at the packaged Node hook entry', async () => {
  const claude = JSON.parse(await readFile(join(packageRoot, '.claude-plugin', 'plugin.json'), 'utf8'));
  const cursor = JSON.parse(await readFile(join(packageRoot, '.cursor-plugin', 'plugin.json'), 'utf8'));
  const claudeHooks = JSON.parse(await readFile(join(packageRoot, 'hooks', 'hooks.json'), 'utf8'));
  const cursorHooks = JSON.parse(await readFile(join(packageRoot, 'hooks', 'hooks-cursor.json'), 'utf8'));

  assert.equal(claude.hooks, './hooks/hooks.json');
  assert.equal(cursor.hooks, './hooks/hooks-cursor.json');
  assert.match(claudeHooks.hooks.SessionStart[0].hooks[0].command, /session-start\.mjs/);
  assert.match(cursorHooks.hooks.sessionStart[0].command, /session-start\.mjs/);
});
