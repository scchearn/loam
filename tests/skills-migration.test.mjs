import assert from 'node:assert/strict';
import { mkdir, readFile, rm, stat, symlink, writeFile } from 'node:fs/promises';
import { mkdtemp } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

import { loadSkillInventory } from '../setup/inventory.mjs';
import { discover } from '../setup/discovery.mjs';
import { detectLegacyProject, migrateLegacyProject } from '../setup/migration.mjs';
import { npxCommand, runCommand, runSkills } from '../setup/process.mjs';
import { ensureGlobalSkills, skillsAgentsFor, skillsSourceFor } from '../setup/skills.mjs';

const packageRoot = fileURLToPath(new URL('..', import.meta.url));

async function skillsRootFixture() {
  const root = await mkdtemp(join(tmpdir(), 'loam-skills-'));
  await mkdir(join(root, 'loam-using', 'scripts'), { recursive: true });
  await writeFile(join(root, 'loam-using', 'scripts', 'CLI_VERSION'), '0.9.1\n');
  await writeFile(join(root, 'loam-using', 'SKILL.md'), '---\nname: loam::using\n---\n# using\n');
  return root;
}

async function completeList() {
  const inventory = await loadSkillInventory({ packageRoot });
  return {
    skills: inventory.skills.map((skill) => ({
      name: skill.frontmatterName,
      path: skill.sourcePath,
      source: 'https://github.com/scchearn/loam',
    })),
  };
}

test('skillsSourceFor pins the tag for prerelease packages and keeps the bare repo for finals', () => {
  assert.equal(skillsSourceFor('0.12.0'), 'scchearn/loam');
  assert.equal(skillsSourceFor('0.13.0-next.0'), 'https://github.com/scchearn/loam/tree/v0.13.0-next.0');
  assert.equal(skillsSourceFor('0.13.0-rc.1'), 'https://github.com/scchearn/loam/tree/v0.13.0-rc.1');
});

test('Skills CLI commands use an argument array and the exact pinned global add', async () => {  const calls = [];
  await runSkills(
    ['add', 'scchearn/loam', '--global', '--agent', '*', '--yes'],
    {
      cwd: '/workspace',
      runner: async (request) => {
        calls.push(request);
        return { code: 0, stdout: '', stderr: '' };
      },
    },
  );

  assert.equal(calls[0].command, npxCommand());
  assert.deepEqual(calls[0].args, [
    '--yes',
    '--package',
    'skills@1.5.20',
    'skills',
    'add',
    'scchearn/loam',
    '--global',
    '--agent',
    '*',
    '--yes',
  ]);
  assert.equal(calls[0].shell, false);
});

test('setup commands execute a Windows batch file from a spaced path', { skip: process.platform !== 'win32' }, async () => {
  const root = await mkdtemp(join(tmpdir(), 'loam setup '));
  const batch = join(root, 'echo args.cmd');
  try {
    await writeFile(batch, '@echo off\r\necho %~1^|%~2\r\n');
    const result = await runCommand({
      command: batch,
      args: ['alpha beta', 'gamma'],
      cwd: root,
      timeoutMs: 10_000,
    });
    assert.equal(result.ok, true, result.stderr);
    assert.equal(result.stdout.trim(), 'alpha beta|gamma');
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test('a timed-out command tears down the whole child tree on Windows (issue #50)', { skip: process.platform !== 'win32' }, async () => {
  // Reproduces the #50 symptom: the Skills CLI verification hangs and Loam's
  // timeout fires. Before the fix, child.kill() terminated only the cmd.exe/npx.cmd
  // wrapper and left the node/skills descendants orphaned. Here the batch launches
  // a long-lived grandchild that child.kill() would miss; the fix's taskkill /T
  // must take the whole tree down.
  const root = await mkdtemp(join(tmpdir(), 'loam-treekill-'));
  const grandchild = join(root, 'grandchild.cjs');
  const pidFile = join(root, 'pid.txt');
  const batch = join(root, 'hang.cmd');
  const alive = (p) => {
    try { process.kill(p, 0); return true; } catch (error) { return error.code === 'EPERM'; }
  };
  try {
    await writeFile(grandchild, "require('fs').writeFileSync(process.argv[2], String(process.pid));\nsetInterval(() => {}, 1 << 30);\n");
    // start "" /b runs the grandchild as a child of this cmd.exe, then ping keeps
    // the batch itself alive so the timeout (not a natural exit) is what stops it.
    await writeFile(batch, '@echo off\r\nstart "" /b "%~1" "%~2" "%~3"\r\nping -n 120 127.0.0.1 >nul\r\n');

    const result = await runCommand({
      command: batch,
      args: [process.execPath, grandchild, pidFile],
      cwd: root,
      timeoutMs: 6000,
    });
    assert.equal(result.category, 'timeout');

    const raw = await readFile(pidFile, 'utf8').catch(() => '');
    const pid = Number(raw.trim());
    assert.ok(Number.isInteger(pid) && pid > 0, `grandchild recorded its pid (got ${JSON.stringify(raw)})`);

    // terminateChild awaits taskkill /T, so the descendant should already be gone;
    // poll briefly to absorb teardown lag.
    const deadline = Date.now() + 8000;
    while (alive(pid) && Date.now() < deadline) await new Promise((resolve) => setTimeout(resolve, 150));
    assert.equal(alive(pid), false, 'orphaned descendant survived the timeout: child tree was not killed');
  } finally {
    const pid = Number((await readFile(pidFile, 'utf8').catch(() => '')).trim());
    if (Number.isInteger(pid) && pid > 0) { try { process.kill(pid, 'SIGKILL'); } catch {} }
    await rm(root, { recursive: true, force: true });
  }
});

test('complete global Skills CLI inventory skips mutation and verifies CLI_VERSION/source metadata', async () => {
  const skillsRoot = await skillsRootFixture();
  const calls = [];
  const result = await ensureGlobalSkills({
    packageRoot,
    skillsRoot,
    runner: async (request) => {
      calls.push(request);
      return { code: 0, stdout: JSON.stringify(await completeList()), stderr: '' };
    },
  });

  assert.equal(result.ready, true);
  assert.equal(result.changed, false);
  assert.equal(calls.length, 1);
  assert.deepEqual(calls[0].args, ['--yes', '--package', 'skills@1.5.20', 'skills', 'list', '--json', '--global']);
});

test('skillsAgentsFor maps detected harnesses to Skills CLI agent ids and falls back safely', () => {
  assert.deepEqual(
    skillsAgentsFor({ claude: { state: 'detected' }, codex: { state: 'detected' }, cursor: { state: 'absent' } }),
    ['claude-code', 'codex'],
  );
  // No detected harness must never revert to an unbounded (unfiltered) scan.
  assert.deepEqual(skillsAgentsFor({}), ['claude-code']);
  assert.deepEqual(skillsAgentsFor({ opencode: { state: 'absent' } }), ['claude-code']);
});

test('verification scopes the global list to the selected harness agents (issue #50)', async () => {
  const skillsRoot = await skillsRootFixture();
  const calls = [];
  const result = await ensureGlobalSkills({
    packageRoot,
    skillsRoot,
    agents: ['claude-code', 'codex'],
    runner: async (request) => {
      calls.push(request);
      return { code: 0, stdout: JSON.stringify(await completeList()), stderr: '' };
    },
  });

  assert.equal(result.ready, true);
  assert.deepEqual(calls[0].args, [
    '--yes', '--package', 'skills@1.5.20', 'skills', 'list', '--json', '--global', '--agent', 'claude-code', 'codex',
  ]);
});

test('refresh forces the Loam source add even when the global inventory is complete', async () => {
  const skillsRoot = await skillsRootFixture();
  const calls = [];
  const result = await ensureGlobalSkills({
    packageRoot,
    skillsRoot,
    refresh: true,
    // Finals assert the bare-repo source; pin the version so the case is
    // independent of whatever version the checkout happens to carry.
    packageVersion: '0.12.0',
    runner: async (request) => {
      calls.push(request);
      return { code: 0, stdout: JSON.stringify(await completeList()), stderr: '' };
    },
  });

  assert.equal(result.ready, true);
  assert.equal(result.changed, true);
  assert.equal(calls.filter((call) => call.args.includes('add')).length, 1);
  assert.deepEqual(calls[1].args, [
    '--yes',
    '--package',
    'skills@1.5.20',
    'skills',
    'add',
    'scchearn/loam',
    '--global',
    '--agent',
    '*',
    '--yes',
  ]);
});

test('incomplete global inventory invokes the pinned public add and re-verifies', async () => {
  const skillsRoot = await skillsRootFixture();
  const full = await completeList();
  let listCalls = 0;
  const calls = [];
  const result = await ensureGlobalSkills({
    packageRoot,
    skillsRoot,
    // Finals assert the bare-repo source; pin the version so the case is
    // independent of whatever version the checkout happens to carry.
    packageVersion: '0.12.0',
    runner: async (request) => {
      calls.push(request);
      if (request.args.includes('list')) {
        listCalls += 1;
        return {
          code: 0,
          stdout: JSON.stringify(listCalls === 1 ? { skills: full.skills.slice(0, 1) } : full),
          stderr: '',
        };
      }
      return { code: 0, stdout: 'installed', stderr: '' };
    },
  });

  assert.equal(result.ready, true);
  assert.equal(result.changed, true);
  assert.deepEqual(calls[1].args, [
    '--yes',
    '--package',
    'skills@1.5.20',
    'skills',
    'add',
    'scchearn/loam',
    '--global',
    '--agent',
    '*',
    '--yes',
  ]);
});

test('a prerelease package adds skills from the pinned tag tree URL', async () => {
  const skillsRoot = await skillsRootFixture();
  const full = await completeList();
  let listCalls = 0;
  const calls = [];
  const result = await ensureGlobalSkills({
    packageRoot,
    skillsRoot,
    packageVersion: '0.13.0-next.0',
    runner: async (request) => {
      calls.push(request);
      if (request.args.includes('list')) {
        listCalls += 1;
        return {
          code: 0,
          stdout: JSON.stringify(listCalls === 1 ? { skills: full.skills.slice(0, 1) } : full),
          stderr: '',
        };
      }
      return { code: 0, stdout: 'installed', stderr: '' };
    },
  });

  assert.equal(result.ready, true);
  assert.equal(result.changed, true);
  assert.deepEqual(calls[1].args, [
    '--yes',
    '--package',
    'skills@1.5.20',
    'skills',
    'add',
    'https://github.com/scchearn/loam/tree/v0.13.0-next.0',
    '--global',
    '--agent',
    '*',
    '--yes',
  ]);
});

test('fresh global setup preserves the canonical skills root', async () => {
  const skillsRoot = await mkdtemp(join(tmpdir(), 'loam-skills-empty-'));
  const full = await completeList();
  const calls = [];
  const result = await ensureGlobalSkills({
    packageRoot,
    skillsRoot,
    runner: async (request) => {
      calls.push(request);
      if (request.args.includes('add')) {
        assert.deepEqual(request.args.slice(-3), ['--agent', '*', '--yes']);
        await mkdir(join(skillsRoot, 'loam-using', 'scripts'), { recursive: true });
        await writeFile(join(skillsRoot, 'loam-using', 'scripts', 'CLI_VERSION'), '0.9.1\n');
        await writeFile(join(skillsRoot, 'loam-using', 'SKILL.md'), '# installed\n');
      }
      return { code: 0, stdout: JSON.stringify(full), stderr: '' };
    },
  });

  assert.equal(result.ready, true);
  assert.equal(result.changed, true);
  assert.equal(calls.filter((call) => call.args.includes('add')).length, 1);
  assert.equal(await readFile(join(skillsRoot, 'loam-using', 'scripts', 'CLI_VERSION'), 'utf8'), '0.9.1\n');
});

test('migration removes only exact current-workspace Loam skills and owned runtime', async () => {
  const workspace = await mkdtemp(join(tmpdir(), 'loam-project-'));
  const projectSkills = join(workspace, '.agents', 'skills');
  const runtime = join(workspace, '.agents', 'loam');
  await mkdir(join(projectSkills, 'loam-using'), { recursive: true });
  await mkdir(runtime, { recursive: true });
  await writeFile(join(projectSkills, 'loam-using', 'SKILL.md'), '# legacy\n');
  const unrelatedLock = join(workspace, 'skills-lock.json');
  await writeFile(unrelatedLock, '{"unrelated":true}\n');
  const list = {
    skills: [
      { name: 'loam::using', path: join(projectSkills, 'loam-using') },
      { name: 'unrelated-skill', path: join(projectSkills, 'unrelated-skill') },
    ],
  };
  const calls = [];
  let removed = false;
  const result = await migrateLegacyProject({
    workspace,
    packageRoot,
    yes: true,
    runner: async (request) => {
      calls.push(request);
      if (request.args.includes('list')) return { code: 0, stdout: JSON.stringify(removed ? { skills: [] } : list), stderr: '' };
      removed = true;
      return { code: 0, stdout: '', stderr: '' };
    },
  });

  assert.equal(result.migrated, true);
  assert.equal(result.ready, true);
  const remove = calls.filter((call) => call.args.includes('remove'));
  assert.equal(remove.length, 1);
  assert.deepEqual(remove[0].args, ['--yes', '--package', 'skills@1.5.20', 'skills', 'remove', 'loam::using', '--yes']);
  assert.equal(remove[0].args.includes('--all'), false);
  assert.equal(remove[0].args.includes('--global'), false);
  assert.equal(remove[0].cwd, workspace);
  assert.equal(await readFile(unrelatedLock, 'utf8'), '{"unrelated":true}\n');
  await assert.rejects(() => readFile(runtime), { code: 'ENOENT' });
});

test('declined migration leaves all project artifacts and reports not ready', async () => {
  const workspace = await mkdtemp(join(tmpdir(), 'loam-project-decline-'));
  const projectSkills = join(workspace, '.agents', 'skills', 'loam-using');
  await mkdir(projectSkills, { recursive: true });
  await writeFile(join(projectSkills, 'SKILL.md'), '# legacy\n');
  const calls = [];
  const result = await migrateLegacyProject({
    workspace,
    packageRoot,
    runner: async (request) => {
      calls.push(request);
      return { code: 0, stdout: JSON.stringify({ skills: [{ name: 'loam::using', path: projectSkills }] }), stderr: '' };
    },
    prompt: async () => false,
  });

  assert.equal(result.ready, false);
  assert.equal(result.category, 'migration_declined');
  assert.equal(calls.some((call) => call.args.includes('remove')), false);
  assert.equal(await readFile(join(projectSkills, 'SKILL.md'), 'utf8'), '# legacy\n');
});

test('partial Skills CLI removal leaves leftovers and never cleans the runtime', async () => {
  const workspace = await mkdtemp(join(tmpdir(), 'loam-project-partial-'));
  const projectSkills = join(workspace, '.agents', 'skills');
  const runtime = join(workspace, '.agents', 'loam');
  await mkdir(join(projectSkills, 'loam-using'), { recursive: true });
  await mkdir(join(projectSkills, 'loam-work'), { recursive: true });
  await mkdir(runtime, { recursive: true });
  const entries = [
    { name: 'loam::using', path: join(projectSkills, 'loam-using') },
    { name: 'loam::planning', path: join(projectSkills, 'loam-work') },
  ];
  let removals = 0;
  const result = await migrateLegacyProject({
    workspace,
    packageRoot,
    yes: true,
    runner: async (request) => {
      if (request.args.includes('list')) return { code: 0, stdout: JSON.stringify({ skills: entries }), stderr: '' };
      removals += 1;
      return removals === 1 ? { code: 0, stdout: '', stderr: '' } : { code: 1, stdout: '', stderr: 'remove failed' };
    },
  });

  assert.equal(result.ready, false);
  assert.equal(result.category, 'migration_failed');
  assert.ok(result.leftovers.length > 0);
  assert.equal(removals, 2);
  await assert.doesNotReject(() => stat(runtime));
});

test('escaping project skill paths are blocked and home projects are never scanned', async () => {
  const workspace = await mkdtemp(join(tmpdir(), 'loam-project-unsafe-'));
  const outside = await mkdtemp(join(tmpdir(), 'loam-outside-'));
  const calls = [];
  const report = await detectLegacyProject({
    workspace,
    packageRoot,
    runner: async (request) => {
      calls.push(request);
      return { code: 0, stdout: JSON.stringify({ skills: [{ name: 'loam::using', path: join(outside, 'loam-using') }] }), stderr: '' };
    },
  });
  assert.equal(report.unsafe.length, 1);
  assert.equal(report.skillNames.length, 0);
  assert.ok(calls.every((call) => call.cwd === workspace));
});

test('legacy detection accepts project paths through an aliased workspace root', async (t) => {
  if (process.platform === 'win32') return t.skip('symlink privileges vary on Windows');
  const physical = await mkdtemp(join(tmpdir(), 'loam-project-physical-'));
  const aliasParent = await mkdtemp(join(tmpdir(), 'loam-project-alias-'));
  const workspace = join(aliasParent, 'workspace');
  await symlink(physical, workspace);
  const projectSkill = join(workspace, '.agents', 'skills', 'loam-using');
  await mkdir(projectSkill, { recursive: true });

  const report = await detectLegacyProject({
    workspace,
    packageRoot,
    runner: async () => ({
      code: 0,
      stdout: JSON.stringify({ skills: [{ name: 'loam::using', path: projectSkill }] }),
      stderr: '',
    }),
  });

  assert.deepEqual(report.unsafe, []);
  assert.equal(report.paths.length, 1);
});

test('unrelated plugin metadata is not treated as an owned Loam marker', async () => {
  const workspace = await mkdtemp(join(tmpdir(), 'loam-project-unrelated-marker-'));
  await mkdir(join(workspace, '.claude-plugin'), { recursive: true });
  await writeFile(
    join(workspace, '.claude-plugin', 'plugin.json'),
    JSON.stringify({ name: 'other', repository: 'https://github.com/scchearn/loam-fork' }),
  );

  const report = await detectLegacyProject({
    workspace,
    packageRoot,
    runner: async () => ({ code: 0, stdout: JSON.stringify({ skills: [] }), stderr: '' }),
  });

  assert.equal(report.ready, true);
  assert.deepEqual(report.markers, []);
});

test('migration removes owned plugin markers and re-detects a clean workspace', async () => {
  const workspace = await mkdtemp(join(tmpdir(), 'loam-project-owned-marker-'));
  await mkdir(join(workspace, '.opencode', 'plugins'), { recursive: true });
  const marker = join(workspace, '.opencode', 'plugins', 'loam.js');
  await writeFile(marker, 'export async function LoamPlugin() {}\n');

  const result = await migrateLegacyProject({
    workspace,
    packageRoot,
    yes: true,
    runner: async () => ({ code: 0, stdout: JSON.stringify({ skills: [] }), stderr: '' }),
  });

  assert.equal(result.ready, true);
  await assert.rejects(() => readFile(marker), { code: 'ENOENT' });
});

test('the package source repository is not classified as a legacy project', async () => {
  const home = await mkdtemp(join(tmpdir(), 'loam-source-home-'));
  const result = await discover({
    workspace: packageRoot,
    packageRoot,
    home,
    runner: async () => ({ code: 0, stdout: JSON.stringify({ skills: [] }), stderr: '' }),
  });

  assert.equal(result.legacy.needed, false);
  assert.equal(result.legacy.ready, true);
});

test('migration stops when Skills CLI removal exits zero but public list still reports the skill', async () => {
  const workspace = await mkdtemp(join(tmpdir(), 'loam-project-stale-list-'));
  const projectSkill = join(workspace, '.agents', 'skills', 'loam-using');
  const runtime = join(workspace, '.agents', 'loam');
  await mkdir(projectSkill, { recursive: true });
  await mkdir(runtime, { recursive: true });
  await writeFile(join(projectSkill, 'SKILL.md'), '# legacy\n');
  const listed = { skills: [{ name: 'loam::using', path: projectSkill }] };

  const result = await migrateLegacyProject({
    workspace,
    packageRoot,
    yes: true,
    runner: async (request) => request.args.includes('list')
      ? { code: 0, stdout: JSON.stringify(listed), stderr: '' }
      : { code: 0, stdout: '', stderr: '' },
  });

  assert.equal(result.ready, false);
  assert.equal(result.category, 'migration_incomplete');
  await assert.doesNotReject(() => stat(runtime));
});
