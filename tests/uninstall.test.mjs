import assert from 'node:assert/strict';
import { mkdir, mkdtemp, readFile, rm, stat, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { PassThrough } from 'node:stream';
import { test } from 'node:test';

import { installHarnesses, detectHarnesses } from '../setup/harnesses.mjs';
import { uninstall } from '../setup/uninstall.mjs';
import { childIdentity } from '../integration/ingest-process.mjs';
import { confirmUninstall } from '../setup/wizard.mjs';

function skillsRunner({ installed = true, calls = [] } = {}) {
  let active = installed;
  return async ({ args }) => {
    calls.push(args);
    if (args.includes('list')) {
      return {
        code: 0,
        stdout: JSON.stringify(active ? [{ name: 'loam::using', source: 'https://github.com/scchearn/loam' }] : []),
        stderr: '',
      };
    }
    if (args.includes('remove')) {
      active = false;
      return { code: 0, stdout: '', stderr: '' };
    }
    return { code: 1, stdout: '', stderr: 'unexpected Skills CLI command' };
  };
}

async function readyFixture({ codexProfile } = {}) {
  const home = await mkdtemp(join(tmpdir(), 'loam-uninstall-'));
  const globalRoot = join(home, '.agents', 'loam');
  await mkdir(join(home, '.config', 'opencode'), { recursive: true });
  await mkdir(join(home, '.claude'), { recursive: true });
  await mkdir(join(home, '.codex'), { recursive: true });
  await mkdir(join(home, '.cursor'), { recursive: true });
  await mkdir(join(home, '.agents', 'skills', 'loam-using'), { recursive: true });
  if (codexProfile !== undefined) {
    await mkdir(join(home, '.codex', 'agents'), { recursive: true });
    await writeFile(join(home, '.codex', 'agents', 'loam_ingestor.toml'), codexProfile);
  }
  await writeFile(join(home, '.agents', 'skills', 'loam-using', 'SKILL.md'), '# using\n');
  const runtimePath = join(globalRoot, 'bin', '0.9.1', 'x86_64-unknown-linux-musl', 'loam');
  const detected = await detectHarnesses({ home });
  const installed = await installHarnesses({ home, globalRoot, pluginVersion: '0.8.3', runtimePath, detected });
  const install = {
    schema_version: 1,
    plugin_version: '0.8.3',
    runtime_version: '0.9.1',
    target: 'x86_64-unknown-linux-musl',
    runtime_path: runtimePath,
    runtime_sha256: 'a'.repeat(64),
    adapter_root: installed.versionRoot,
    integration_path: join(globalRoot, 'integration', 'loam.mjs'),
    skills_scope: 'global',
    skills_source: 'scchearn/loam',
    configured_harnesses: ['opencode', 'claude', 'codex', 'cursor'],
  };
  await mkdir(join(globalRoot, 'integration'), { recursive: true });
  await writeFile(install.integration_path, 'export async function runIntegration() { return 0; }\n');
  await mkdir(join(globalRoot, 'bin', '0.9.1', 'x86_64-unknown-linux-musl'), { recursive: true });
  await writeFile(install.runtime_path, 'fake runtime\n');
  await writeFile(join(globalRoot, 'install.json'), `${JSON.stringify(install, null, 2)}\n`);
  return { home, globalRoot, install, installed };
}

async function exists(path) {
  try {
    await stat(path);
    return true;
  } catch {
    return false;
  }
}

test('uninstall removes the global root, skills, adapter, and Loam-owned hooks; preserves unrelated config', async () => {
  const { home, globalRoot, install, installed } = await readyFixture();
  const unrelatedClaude = { type: 'command', command: 'node "/usr/local/bin/other-hook.mjs"' };
  const unrelatedCodexSession = { type: 'command', command: 'node "/usr/local/bin/other-codex-session.mjs"' };
  const unrelatedCodexStop = { type: 'command', command: 'node "/usr/local/bin/other-codex-stop.mjs"' };
  const unrelatedCursor = { type: 'command', command: 'node "/opt/other-tools/cursor-hook.mjs"' };
  const claudePath = join(home, '.claude', 'settings.json');
  const codexPath = join(home, '.codex', 'hooks.json');
  const cursorPath = join(home, '.cursor', 'hooks.json');
  // Both registration generations: the retired Node shim a previous install
  // left in user config, and the native `hook` command this one writes.
  const legacyClaude = { type: 'command', command: 'node', args: [join(globalRoot, 'plugins', 'old', 'claude-session-start.mjs')] };
  const nativeClaude = { type: 'command', command: install.runtime_path, args: ['hook', 'claude', '--event', 'SessionStart'] };
  const legacyCodexSession = { type: 'command', command: `node ${JSON.stringify(join(globalRoot, 'plugins', 'old', 'codex-session-start.mjs'))}` };
  const claude = { hooks: {
    SessionStart: [{ hooks: [legacyClaude, nativeClaude] }],
    Stop: [],
  } };
  const codex = { hooks: {
    SessionStart: [{ hooks: [legacyCodexSession] }],
    Stop: [{ hooks: [{ type: 'command', command: `node ${JSON.stringify(installed.codex.stopPath)}` }] }],
  } };
  const cursor = JSON.parse(await readFile(cursorPath, 'utf8'));
  claude.hooks.SessionStart[0].hooks.unshift(unrelatedClaude);
  codex.hooks.SessionStart[0].hooks.unshift(unrelatedCodexSession);
  codex.hooks.Stop[0].hooks.unshift(unrelatedCodexStop);
  cursor.hooks.sessionStart.unshift(unrelatedCursor);
  await writeFile(claudePath, JSON.stringify(claude));
  await writeFile(codexPath, JSON.stringify(codex));
  await writeFile(cursorPath, JSON.stringify(cursor));
  await writeFile(join(home, '.config', 'opencode', 'plugins', 'loam.mjs'), 'legacy adapter');
  const databasePath = join(globalRoot, 'loam.sqlite3');
  await writeFile(databasePath, 'operational history');
  let output = '';

  const code = await uninstall({
    home,
    globalRoot,
    yes: true,
    runner: skillsRunner(),
    output: { write: (chunk) => { output += chunk; } },
  });

  assert.equal(code, 0);
  assert.equal(await exists(globalRoot), false, 'global root removed');
  assert.equal(await exists(databasePath), false, 'operational history removed with the global root');
  assert.match(output, /local operational history/);
  assert.equal(await exists(join(home, '.config', 'opencode', 'plugins', 'loam.js')), false, 'opencode adapter removed');
  assert.equal(await exists(join(home, '.config', 'opencode', 'plugins', 'loam.mjs')), false, 'legacy opencode adapter removed');
  assert.equal(await exists(join(home, '.codex', 'agents', 'loam_ingestor.toml')), false, 'Loam Codex agent profile removed');
  assert.equal(await exists(join(home, '.agents', 'skills', 'loam-using', 'SKILL.md')), true, 'fixture skill tree is not touched directly');

  const claudeAfter = JSON.parse(await readFile(claudePath, 'utf8'));
  const codexAfter = JSON.parse(await readFile(codexPath, 'utf8'));
  const cursorAfter = JSON.parse(await readFile(cursorPath, 'utf8'));
  const claudeHooks = claudeAfter.hooks.SessionStart.flatMap((e) => e.hooks || []);
  const codexSessionHooks = codexAfter.hooks.SessionStart.flatMap((e) => e.hooks || []);
  const codexStopHooks = codexAfter.hooks.Stop.flatMap((e) => e.hooks || []);
  const cursorHooks = cursorAfter.hooks.sessionStart;
  assert.deepEqual(claudeHooks[0], unrelatedClaude, 'unrelated claude hook preserved');
  assert.deepEqual(codexSessionHooks[0], unrelatedCodexSession, 'unrelated codex SessionStart hook preserved');
  assert.deepEqual(codexStopHooks[0], unrelatedCodexStop, 'unrelated codex Stop hook preserved');
  assert.deepEqual(cursorHooks[0], unrelatedCursor, 'unrelated cursor hook preserved');
  assert.equal(claudeHooks.length, 1, 'both the legacy shim and the native claude hook removed');
  assert.equal(codexSessionHooks.filter((h) => h.command === legacyCodexSession.command).length, 0, 'loam codex SessionStart hook removed');
  assert.equal(codexStopHooks.filter((h) => h.command === `node ${JSON.stringify(installed.codex.stopPath)}`).length, 0, 'loam codex Stop hook removed');
  assert.equal(cursorHooks.length, 1, 'the native cursor hook removed');
  assert.equal(cursorHooks.filter((h) => h.command === install.runtime_path).length, 0, 'no native runtime command survives uninstall');
});

test('uninstall restores a Codex profile preserved during setup', async () => {
  const original = 'name = "personal_ingestor"\ndescription = "User profile"\ndeveloper_instructions = "Keep me"\n';
  const { home, globalRoot } = await readyFixture({ codexProfile: original });
  const profilePath = join(home, '.codex', 'agents', 'loam_ingestor.toml');
  assert.notEqual(await readFile(profilePath, 'utf8'), original, 'setup installed the Loam profile');

  const code = await uninstall({ home, globalRoot, yes: true, runner: skillsRunner(), output: { write: () => {} } });

  assert.equal(code, 0);
  assert.equal(await readFile(profilePath, 'utf8'), original);
  assert.equal(await exists(`${profilePath}.loam-backup`), false);
});

test('uninstall preserves a Codex profile the user replaced after setup', async () => {
  const { home, globalRoot } = await readyFixture();
  const profilePath = join(home, '.codex', 'agents', 'loam_ingestor.toml');
  const replacement = 'name = "personal_ingestor"\ndescription = "User replacement"\ndeveloper_instructions = "Keep me"\n';
  await writeFile(profilePath, replacement);

  const code = await uninstall({ home, globalRoot, yes: true, runner: skillsRunner(), output: { write: () => {} } });

  assert.equal(code, 0);
  assert.equal(await readFile(profilePath, 'utf8'), replacement);
});

test('uninstall removes backup files created by setup', async () => {
  const { home, globalRoot } = await readyFixture();
  const claudeBackup = join(home, '.claude', 'settings.json.backup-deadbeef');
  const cursorBackup = join(home, '.cursor', 'hooks.json.backup-cafebabe');
  await writeFile(claudeBackup, '{"old":true}');
  await writeFile(cursorBackup, '{"old":true}');

  const code = await uninstall({ home, globalRoot, yes: true, runner: skillsRunner(), output: { write: () => {} } });

  assert.equal(code, 0);
  assert.equal(await exists(claudeBackup), false, 'claude backup removed');
  assert.equal(await exists(cursorBackup), false, 'cursor backup removed');
});

test('uninstall without install.json reports nothing to remove', async () => {
  const home = await mkdtemp(join(tmpdir(), 'loam-uninstall-empty-'));
  const globalRoot = join(home, '.agents', 'loam');
  let message = '';
  const code = await uninstall({ home, globalRoot, yes: true, runner: skillsRunner({ installed: false }), output: { write: (s) => { message += s; } } });

  assert.equal(code, 0);
  assert.match(message, /Nothing to uninstall/);
});

test('harvest_packaging: uninstall removes harvest state and window files with the global root', async () => {
  const { home, globalRoot } = await readyFixture();
  const runRoot = join(globalRoot, 'run', 'workspace-hash');
  await mkdir(join(runRoot, 'harvest'), { recursive: true });
  await writeFile(join(runRoot, 'harvest', 'abc123.window.md'), '# window\n');
  await writeFile(join(runRoot, 'harvest', 'abc123.json'), '{"schema":1}\n');
  await writeFile(join(runRoot, 'harvest-last-run.json'), '{"schema":1}\n');
  await writeFile(join(runRoot, 'lease.json'), JSON.stringify({
    schema: 1, lease_id: 'lease-dead', workspace: '/workspace', harness: 'opencode',
    owner_pid: 99999999, boot_id: 'dead', process_start: 'dead',
    started_at: 0, hard_deadline: new Date(0).toISOString(),
    launch_mode: 'opencode_child', launch_state: 'launched', planned_identity: null, child_identity: null,
  }) + '\n');

  const code = await uninstall({ home, globalRoot, yes: true, runner: skillsRunner(), output: { write: () => {} } });

  assert.equal(code, 0);
  assert.equal(await exists(join(runRoot, 'harvest', 'abc123.window.md')), false, 'window file removed');
  assert.equal(await exists(join(runRoot, 'harvest-last-run.json')), false, 'harvest last-run removed');
  assert.equal(await exists(join(globalRoot, 'run')), false, 'run root removed');
});

test('harvest_packaging: uninstall is blocked while a harvest lease is live', async () => {
  const { home, globalRoot } = await readyFixture();
  const runRoot = join(globalRoot, 'run', 'workspace-hash');
  await mkdir(join(runRoot, 'harvest'), { recursive: true });
  const identity = await childIdentity(process.pid);
  await writeFile(join(runRoot, 'lease.json'), JSON.stringify({
    schema: 1, lease_id: 'lease-live', workspace: '/workspace', harness: 'opencode',
    owner_pid: identity.pid, boot_id: identity.boot_id, process_start: identity.process_start,
    started_at: Date.now(), hard_deadline: new Date(Date.now() + 60000).toISOString(),
    launch_mode: 'opencode_child', launch_state: 'launched', planned_identity: null, child_identity: null,
  }) + '\n');

  const code = await uninstall({ home, globalRoot, yes: true, runner: skillsRunner(), output: { write: () => {} } });

  assert.equal(code, 1, 'uninstall blocked by live lease');
  assert.equal(await exists(join(runRoot, 'harvest')), true, 'harvest state retained while a worker is live');
});

test('uninstall cancelled without --yes returns 130', async () => {
  const { home, globalRoot } = await readyFixture();
  const code = await uninstall({
    home,
    globalRoot,
    yes: false,
    confirm: async () => false,
    runner: skillsRunner(),
    output: { write: () => {} },
  });

  assert.equal(code, 130);
  assert.equal(await exists(join(globalRoot, 'install.json')), true, 'global root preserved on cancel');
});

test('uninstall confirmation prompts on an interactive input', async () => {
  const input = new PassThrough();
  input.isTTY = true;
  let output = '';
  const confirmation = confirmUninstall({
    input,
    output: { write: (value) => { output += value; } },
  });
  input.end('y\n');

  assert.equal(await confirmation, true);
  assert.match(output, /Proceed with global Loam uninstall/);
});

test('uninstall removes globally installed Loam skills through the Skills CLI', async () => {
  const { home, globalRoot } = await readyFixture();
  const skillsPath = join(home, '.agents', 'skills', 'loam-using', 'SKILL.md');
  const calls = [];
  await uninstall({ home, globalRoot, yes: true, runner: skillsRunner({ calls }), output: { write: () => {} } });
  assert.deepEqual(calls.find((args) => args.includes('remove')), [
    '--yes', '--package', 'skills@1.5.20', 'skills', 'remove', 'loam::using', '--global', '--yes',
  ]);
  assert.equal(await exists(skillsPath), true, 'the Skills CLI owns removal of skill files');
});

test('uninstall detects skills installed from a prerelease tag tree URL', async () => {
  const { home, globalRoot } = await readyFixture();
  const calls = [];
  let active = true;
  const runner = async ({ args }) => {
    calls.push(args);
    if (args.includes('list')) {
      return {
        code: 0,
        stdout: JSON.stringify(active ? [{ name: 'loam::using', source: 'https://github.com/scchearn/loam/tree/v0.13.0-next.0' }] : []),
        stderr: '',
      };
    }
    if (args.includes('remove')) {
      active = false;
      return { code: 0, stdout: '', stderr: '' };
    }
    return { code: 1, stdout: '', stderr: 'unexpected Skills CLI command' };
  };
  const code = await uninstall({ home, globalRoot, yes: true, runner, output: { write: () => {} } });
  assert.equal(code, 0);
  assert.deepEqual(calls.find((args) => args.includes('remove')), [
    '--yes', '--package', 'skills@1.5.20', 'skills', 'remove', 'loam::using', '--global', '--yes',
  ]);
});

test('uninstall delegates marketplace plugin removal to Claude and Codex', async () => {
  const { home, globalRoot } = await readyFixture();
  const claudeCache = join(home, '.claude', 'plugins', 'cache', 'loam', 'loam', '0.9.2');
  await mkdir(claudeCache, { recursive: true });
  await writeFile(join(home, '.claude', 'settings.json'), JSON.stringify({ enabledPlugins: { 'loam@loam': true } }));
  await writeFile(join(home, '.claude', 'plugins', 'installed_plugins.json'), JSON.stringify({
    version: 2,
    plugins: { 'loam@loam': [{ scope: 'user', installPath: claudeCache, version: '0.9.2' }] },
  }));
  await writeFile(join(home, '.codex', 'config.toml'), '[plugins."loam@loam"]\nenabled = true\n');
  await mkdir(join(home, '.codex', 'plugins', 'cache', 'loam', 'loam', '0.9.2'), { recursive: true });
  const calls = [];
  const skills = skillsRunner();
  const runner = async (request) => {
    if (request.command === 'claude' || request.command === 'codex') {
      calls.push({ command: request.command, args: request.args });
      return { code: 0, stdout: '', stderr: '' };
    }
    return skills(request);
  };

  const code = await uninstall({ home, globalRoot, yes: true, runner, output: { write: () => {} } });

  assert.equal(code, 0);
  assert.deepEqual(calls, [
    { command: 'claude', args: ['plugin', 'uninstall', 'loam@loam', '--scope', 'user', '--yes'] },
    { command: 'codex', args: ['plugin', 'remove', 'loam@loam'] },
  ]);
});

test('plugin-only uninstall succeeds without install metadata', async () => {
  const home = await mkdtemp(join(tmpdir(), 'loam-uninstall-plugin-only-'));
  const globalRoot = join(home, '.agents', 'loam');
  const cache = join(home, '.claude', 'plugins', 'cache', 'loam', 'loam', '0.9.2');
  await mkdir(cache, { recursive: true });
  await writeFile(join(home, '.claude', 'settings.json'), JSON.stringify({ enabledPlugins: { 'loam@loam': false } }));
  await writeFile(join(home, '.claude', 'plugins', 'installed_plugins.json'), JSON.stringify({
    version: 2,
    plugins: { 'loam@loam': [{ scope: 'user', installPath: cache, version: '0.9.2' }] },
  }));
  const calls = [];
  const runner = async (request) => {
    if (request.command === 'claude') {
      calls.push(request.args);
      return { code: 0, stdout: '', stderr: '' };
    }
    return skillsRunner({ installed: false })(request);
  };

  const code = await uninstall({ home, globalRoot, yes: true, runner, output: { write: () => {} } });

  assert.equal(code, 0);
  assert.deepEqual(calls, [['plugin', 'uninstall', 'loam@loam', '--scope', 'user', '--yes']]);
});

test('uninstall deletes a fresh harness config created by setup, not leaving an empty husk', async () => {
  const home = await mkdtemp(join(tmpdir(), 'loam-uninstall-fresh-'));
  const globalRoot = join(home, '.agents', 'loam');
  const adapterRoot = join(globalRoot, 'plugins', '0.8.3-abc');
  await mkdir(join(home, '.cursor'), { recursive: true });
  // No pre-existing hooks.json — setup created it fresh, no backup
  await writeFile(join(home, '.cursor', 'hooks.json'), JSON.stringify({
    hooks: { sessionStart: [{ type: 'command', command: `node ${JSON.stringify(join(adapterRoot, 'cursor-session-start.mjs'))}` }] },
  }));
  await mkdir(join(globalRoot, 'integration'), { recursive: true });
  await mkdir(join(globalRoot, 'bin', '0.9.1', 'x86_64-unknown-linux-musl'), { recursive: true });
  await mkdir(adapterRoot, { recursive: true });
  await writeFile(join(globalRoot, 'install.json'), `${JSON.stringify({
    schema_version: 1, plugin_version: '0.8.3', runtime_version: '0.9.1',
    target: 'x86_64-unknown-linux-musl', runtime_path: join(globalRoot, 'bin/0.9.1/x86_64-unknown-linux-musl/loam'),
    runtime_sha256: 'a'.repeat(64), adapter_root: adapterRoot,
    integration_path: join(globalRoot, 'integration/loam.mjs'), skills_scope: 'global',
    skills_source: 'scchearn/loam', configured_harnesses: ['cursor'],
  }, null, 2)}\n`);

  const code = await uninstall({ home, globalRoot, yes: true, runner: skillsRunner({ installed: false }), output: { write: () => {} } });

  assert.equal(code, 0);
  assert.equal(await exists(join(home, '.cursor', 'hooks.json')), false, 'fresh config deleted, not left as empty husk');
});

test('uninstall preserves a pre-existing harness config that setup modified', async () => {
  const home = await mkdtemp(join(tmpdir(), 'loam-uninstall-modified-'));
  const globalRoot = join(home, '.agents', 'loam');
  const adapterRoot = join(globalRoot, 'plugins', '0.8.3-abc');
  await mkdir(join(home, '.cursor'), { recursive: true });
  // Pre-existing config with unrelated content + a backup from setup
  await writeFile(join(home, '.cursor', 'hooks.json'), JSON.stringify({
    hooks: { sessionStart: [{ type: 'command', command: 'node "/opt/other/hook.mjs"' }] },
  }));
  await writeFile(join(home, '.cursor', 'hooks.json.backup-deadbeef'), JSON.stringify({
    hooks: { sessionStart: [{ type: 'command', command: 'node "/opt/other/hook.mjs"' }] },
  }));
  await mkdir(join(globalRoot, 'integration'), { recursive: true });
  await mkdir(join(globalRoot, 'bin', '0.9.1', 'x86_64-unknown-linux-musl'), { recursive: true });
  await mkdir(adapterRoot, { recursive: true });
  await writeFile(join(globalRoot, 'install.json'), `${JSON.stringify({
    schema_version: 1, plugin_version: '0.8.3', runtime_version: '0.9.1',
    target: 'x86_64-unknown-linux-musl', runtime_path: join(globalRoot, 'bin/0.9.1/x86_64-unknown-linux-musl/loam'),
    runtime_sha256: 'a'.repeat(64), adapter_root: adapterRoot,
    integration_path: join(globalRoot, 'integration/loam.mjs'), skills_scope: 'global',
    skills_source: 'scchearn/loam', configured_harnesses: ['cursor'],
  }, null, 2)}\n`);

  const code = await uninstall({ home, globalRoot, yes: true, runner: skillsRunner({ installed: false }), output: { write: () => {} } });
  const cursor = JSON.parse(await readFile(join(home, '.cursor', 'hooks.json'), 'utf8'));

  assert.equal(code, 0);
  assert.equal(await exists(join(home, '.cursor', 'hooks.json')), true, 'pre-existing config preserved');
  assert.deepEqual(cursor.hooks.sessionStart, [{ type: 'command', command: 'node "/opt/other/hook.mjs"' }], 'unrelated hook preserved');
  assert.equal(await exists(join(home, '.cursor', 'hooks.json.backup-deadbeef')), false, 'backup removed');
});

test('uninstall blocks on malformed worker ownership state', async () => {
  const { home, globalRoot } = await readyFixture();
  const runPath = join(globalRoot, 'run', 'malformed');
  await mkdir(runPath, { recursive: true });
  await writeFile(join(runPath, 'lease.json'), '{not-json');

  const code = await uninstall({ home, globalRoot, yes: true, runner: skillsRunner(), output: { write: () => {} } });

  assert.equal(code, 1);
  assert.equal(await exists(globalRoot), true);
});

test('uninstall blocks while a background worker lease is live', async () => {
  const { home, globalRoot } = await readyFixture();
  const runPath = join(globalRoot, 'run', 'active');
  const identity = await childIdentity(process.pid);
  await mkdir(runPath, { recursive: true });
  await writeFile(join(runPath, 'lease.json'), JSON.stringify({
    schema: 1, lease_id: 'active', workspace: home, harness: 'codex',
    owner_pid: process.pid, boot_id: identity.boot_id, process_start: identity.process_start,
  }));

  const code = await uninstall({ home, globalRoot, yes: true, runner: skillsRunner(), output: { write: () => {} } });

  assert.equal(code, 1);
  assert.equal(await exists(globalRoot), true);
});
