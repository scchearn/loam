import assert from 'node:assert/strict';
import { mkdir, mkdtemp, readFile, rm, stat, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { test } from 'node:test';

import { runConfigure } from '../setup/configure.mjs';

const packageRoot = fileURLToPath(new URL('..', import.meta.url));

function outputCapture() {
  const chunks = [];
  return { output: { write: (chunk) => chunks.push(String(chunk)) }, text: () => chunks.join('') };
}

// A federation runner that models the runtime's file-based definition so the
// symmetric-disable absence check (which stats the real definition file) is
// exercised truthfully: install writes the linux unit, uninstall removes it,
// status reflects the enabled flag. Records the verb order for assertions.
function federationRunner({ home, active = false } = {}) {
  const definitionPath = join(home, '.agents', 'loam', 'systemd', 'loam-connector.service');
  const state = { active };
  const calls = [];
  const runner = async (request) => {
    const verb = request.args[2];
    calls.push(verb);
    if (verb === 'install') {
      await mkdir(join(home, '.agents', 'loam', 'systemd'), { recursive: true });
      await writeFile(definitionPath, '[Unit]\n');
      return { code: 0, stdout: '', stderr: '' };
    }
    if (verb === 'enable') { state.active = true; return { code: 0, stdout: '', stderr: '' }; }
    if (verb === 'disable') { state.active = false; return { code: 0, stdout: '', stderr: '' }; }
    if (verb === 'uninstall') { await rm(definitionPath, { force: true }); return { code: 0, stdout: '', stderr: '' }; }
    if (verb === 'status') return { code: state.active ? 0 : 1, stdout: '', stderr: state.active ? '' : 'disabled' };
    return { code: 0, stdout: '', stderr: '' };
  };
  return { runner, calls, definitionPath, state };
}

async function installedFixture({ active = false } = {}) {
  const home = await mkdtemp(join(tmpdir(), 'loam-configure-home-'));
  const workspace = await mkdtemp(join(tmpdir(), 'loam-configure-workspace-'));
  const globalRoot = join(home, '.agents', 'loam');
  await mkdir(globalRoot, { recursive: true });
  const runtimePath = join(globalRoot, 'bin', '0.9.1', 'x86_64-unknown-linux-musl', 'loam');
  await writeFile(join(globalRoot, 'install.json'), JSON.stringify({
    schema_version: 1,
    plugin_version: '0.13.0',
    runtime_version: '0.9.1',
    runtime_path: runtimePath,
    configured_harnesses: [],
  }));
  const fed = federationRunner({ home, active });
  return { home, workspace, globalRoot, runtimePath, fed };
}

const baseOptions = (fixture, capture, fed) => ({
  home: fixture.home,
  workspace: fixture.workspace,
  packageRoot,
  platform: 'linux',
  federationRunner: (fed || fixture.fed).runner,
  output: capture.output,
  errorOutput: capture.output,
});

test('grace: bare setup with no install guides to install and mutates nothing', async () => {
  const home = await mkdtemp(join(tmpdir(), 'loam-configure-empty-home-'));
  const workspace = await mkdtemp(join(tmpdir(), 'loam-configure-empty-ws-'));
  const capture = outputCapture();
  const code = await runConfigure(
    { command: 'setup', federation: null, integrations: [], dryRun: false, yes: false, purge: false },
    { home, workspace, packageRoot, platform: 'linux', confirm: async () => false, output: capture.output, errorOutput: capture.output },
  );
  assert.equal(code, 1);
  assert.match(capture.text(), /No Loam installation found|install` first/);
  await assert.rejects(() => readFile(join(home, '.agents', 'loam', 'install.json')));
});

test('--yes with no component flags is a no-op', async () => {
  const fixture = await installedFixture();
  const capture = outputCapture();
  const code = await runConfigure(
    { command: 'setup', federation: null, integrations: [], dryRun: false, yes: true, purge: false },
    baseOptions(fixture, capture),
  );
  assert.equal(code, 0, capture.text());
  assert.match(capture.text(), /Nothing to configure/);
  assert.deepEqual(fixture.fed.calls, []);
});

test('federation enable installs, enables, and verifies the connector service', async () => {
  const fixture = await installedFixture();
  const capture = outputCapture();
  const code = await runConfigure(
    { command: 'setup', federation: 'enable', integrations: [], dryRun: false, yes: true, purge: false },
    baseOptions(fixture, capture),
  );
  assert.equal(code, 0, capture.text());
  assert.match(capture.text(), /Federation enabled/);
  assert.deepEqual(fixture.fed.calls, ['status', 'install', 'enable', 'status']);
  await assert.doesNotReject(() => stat(fixture.fed.definitionPath));
});

test('federation disable is symmetric, verified, and preserves identity', async () => {
  const fixture = await installedFixture({ active: true });
  // Pre-existing definition (enabled state).
  await mkdir(join(fixture.globalRoot, 'systemd'), { recursive: true });
  await writeFile(fixture.fed.definitionPath, '[Unit]\n');
  const capture = outputCapture();
  const code = await runConfigure(
    { command: 'setup', federation: 'disable', integrations: [], dryRun: false, yes: true, purge: false },
    baseOptions(fixture, capture),
  );
  assert.equal(code, 0, capture.text());
  assert.match(capture.text(), /Federation disabled/);
  assert.match(capture.text(), /identity and enrollment preserved/);
  assert.deepEqual(fixture.fed.calls, ['disable', 'uninstall', 'status']);
  await assert.rejects(() => stat(fixture.fed.definitionPath));
});

test('federation disable that leaves a definition behind reports the leftover and fails', async () => {
  const fixture = await installedFixture({ active: false });
  await mkdir(join(fixture.globalRoot, 'systemd'), { recursive: true });
  await writeFile(fixture.fed.definitionPath, '[Unit]\n');
  const capture = outputCapture();
  // A runner whose uninstall does NOT remove the file — simulates a partial disable.
  const stubborn = {
    runner: async (request) => {
      const verb = request.args[2];
      fixture.fed.calls.push(verb);
      if (verb === 'status') return { code: 1, stdout: '', stderr: 'disabled' };
      return { code: 0, stdout: '', stderr: '' }; // uninstall lies: file survives
    },
  };
  const code = await runConfigure(
    { command: 'setup', federation: 'disable', integrations: [], dryRun: false, yes: true, purge: false },
    baseOptions(fixture, capture, stubborn),
  );
  assert.equal(code, 1);
  assert.match(capture.text(), /Federation disable incomplete/);
  assert.match(capture.text(), /loam-connector\.service/);
});

test('federation round-trip enable -> disable -> enable converges with no residue', async () => {
  const fixture = await installedFixture();
  const run = async (action) => {
    const capture = outputCapture();
    const code = await runConfigure(
      { command: 'setup', federation: action, integrations: [], dryRun: false, yes: true, purge: false },
      baseOptions(fixture, capture),
    );
    return { code, text: capture.text() };
  };
  const first = await run('enable');
  assert.equal(first.code, 0, first.text);
  await assert.doesNotReject(() => stat(fixture.fed.definitionPath), 'enable leaves a definition');

  const off = await run('disable');
  assert.equal(off.code, 0, off.text);
  await assert.rejects(() => stat(fixture.fed.definitionPath), 'disable leaves NO definition (zero residue)');
  assert.equal(fixture.fed.state.active, false);

  const again = await run('enable');
  assert.equal(again.code, 0, again.text);
  await assert.doesNotReject(() => stat(fixture.fed.definitionPath), 're-enable converges to a working definition');
  assert.equal(fixture.fed.state.active, true);
});

test('federation dry-run mutates nothing and issues no runtime calls', async () => {
  const fixture = await installedFixture();
  const capture = outputCapture();
  const code = await runConfigure(
    { command: 'setup', federation: 'enable', integrations: [], dryRun: true, yes: true, purge: false },
    baseOptions(fixture, capture),
  );
  assert.equal(code, 0, capture.text());
  assert.match(capture.text(), /dry-run/i);
  assert.deepEqual(fixture.fed.calls, []);
  await assert.rejects(() => stat(fixture.fed.definitionPath));
});

test('unknown integration id fails with an actionable message', async () => {
  const fixture = await installedFixture();
  const capture = outputCapture();
  const code = await runConfigure(
    { command: 'setup', federation: null, integrations: ['nope'], dryRun: false, yes: true, purge: false },
    baseOptions(fixture, capture),
  );
  assert.equal(code, 1);
  assert.match(capture.text(), /Unknown integration: nope/);
});

test('win32 federation disable is clean when the Task Scheduler marker is gone', async () => {
  const fixture = await installedFixture();
  const capture = outputCapture();
  // status returns not-active; no windows-task.marker exists → clean disable.
  const code = await runConfigure(
    { command: 'setup', federation: 'disable', integrations: [], dryRun: false, yes: true, purge: false },
    { ...baseOptions(fixture, capture), platform: 'win32' },
  );
  assert.equal(code, 0, capture.text());
  assert.match(capture.text(), /Federation disabled/);
});

test('win32 federation disable names a leftover windows-task.marker', async () => {
  const fixture = await installedFixture();
  const markerPath = join(fixture.globalRoot, 'windows-task.marker');
  await writeFile(markerPath, 'task');
  const capture = outputCapture();
  // A runner whose uninstall does NOT remove the marker simulates a partial disable.
  const stubborn = {
    runner: async (request) => (request.args[2] === 'status'
      ? { code: 1, stdout: '', stderr: 'disabled' }
      : { code: 0, stdout: '', stderr: '' }),
  };
  const code = await runConfigure(
    { command: 'setup', federation: 'disable', integrations: [], dryRun: false, yes: true, purge: false },
    { ...baseOptions(fixture, capture, stubborn), platform: 'win32' },
  );
  assert.equal(code, 1);
  assert.match(capture.text(), /Federation disable incomplete/);
  assert.match(capture.text(), /windows-task\.marker/);
});

test('federation enable rolls back a newly-installed service that cannot be verified', async () => {
  const fixture = await installedFixture();
  const calls = [];
  const definitionPath = join(fixture.home, '.agents', 'loam', 'systemd', 'loam-connector.service');
  // install writes the unit and enable succeeds, but status is UNVERIFIABLE (a
  // hard runtime invocation error, not a clean disabled report). enable must not
  // leave the newly-created definition behind.
  const runner = async (request) => {
    const verb = request.args[2];
    calls.push(verb);
    if (verb === 'install') {
      await mkdir(join(fixture.home, '.agents', 'loam', 'systemd'), { recursive: true });
      await writeFile(definitionPath, 'unit');
      return { code: 0, stdout: '', stderr: '' };
    }
    if (verb === 'uninstall') { await rm(definitionPath, { force: true }); return { code: 0, stdout: '', stderr: '' }; }
    if (verb === 'status') return { code: null, category: 'process_error', stdout: '', stderr: 'no such runtime' };
    return { code: 0, stdout: '', stderr: '' };
  };
  const capture = outputCapture();
  const code = await runConfigure(
    { command: 'setup', federation: 'enable', integrations: [], dryRun: false, yes: true, purge: false },
    { ...baseOptions(fixture, capture, { runner }), platform: 'linux' },
  );
  assert.equal(code, 1);
  assert.match(capture.text(), /could not be verified/);
  // Atomic enable: the definition we created was rolled back (uninstall ran).
  assert.ok(calls.includes('uninstall'), 'a newly-created but unverifiable service is rolled back');
  await assert.rejects(() => stat(definitionPath));
});

test('setup --integration grep registers the MCP and --disable-integration grep removes it', async () => {
  const fixture = await installedFixture();
  // Give the install a set of configured harnesses to register into.
  const metadataPath = join(fixture.globalRoot, 'install.json');
  const install = JSON.parse(await readFile(metadataPath, 'utf8'));
  await writeFile(metadataPath, JSON.stringify({ ...install, configured_harnesses: ['claude', 'codex', 'opencode', 'cursor'] }));

  const capture = outputCapture();
  const on = await runConfigure(
    { command: 'setup', federation: null, integrations: ['grep'], disableIntegrations: [], dryRun: false, yes: true, purge: false },
    { home: fixture.home, workspace: fixture.workspace, packageRoot, platform: 'linux', output: capture.output, errorOutput: capture.output },
  );
  assert.equal(on, 0, capture.text());
  const claude = JSON.parse(await readFile(join(fixture.home, '.claude.json'), 'utf8'));
  assert.deepEqual(claude.mcpServers.grep, { type: 'http', url: 'https://mcp.grep.app' });
  const codex = await readFile(join(fixture.home, '.codex', 'config.toml'), 'utf8');
  assert.match(codex, /\[mcp_servers\.grep\]/);

  const offCapture = outputCapture();
  const off = await runConfigure(
    { command: 'setup', federation: null, integrations: [], disableIntegrations: ['grep'], dryRun: false, yes: true, purge: false },
    { home: fixture.home, workspace: fixture.workspace, packageRoot, platform: 'linux', output: offCapture.output, errorOutput: offCapture.output },
  );
  assert.equal(off, 0, offCapture.text());
  const claudeAfter = JSON.parse(await readFile(join(fixture.home, '.claude.json'), 'utf8'));
  assert.equal(claudeAfter.mcpServers.grep, undefined, 'grep MCP deregistered from claude');
  await assert.doesNotMatch(await readFile(join(fixture.home, '.codex', 'config.toml'), 'utf8').catch(() => ''), /\[mcp_servers\.grep\]/);
});
