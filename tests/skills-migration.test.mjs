import assert from 'node:assert/strict';
import { mkdir, readFile, stat, writeFile } from 'node:fs/promises';
import { mkdtemp } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

import { loadSkillInventory } from '../setup/inventory.mjs';
import { discover } from '../setup/discovery.mjs';
import { detectLegacyProject, migrateLegacyProject } from '../setup/migration.mjs';
import { npxCommand, runSkills } from '../setup/process.mjs';
import { ensureGlobalSkills } from '../setup/skills.mjs';

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

test('Skills CLI commands use an argument array and the exact pinned global add', async () => {
  const calls = [];
  await runSkills(
    ['add', 'scchearn/loam', '--global', '--agent', 'claude-code', '--yes'],
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
    'skills@1.5.20',
    'add',
    'scchearn/loam',
    '--global',
    '--agent',
    'claude-code',
    '--yes',
  ]);
  assert.equal(calls[0].shell, false);
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
  assert.deepEqual(calls[0].args, ['--yes', 'skills@1.5.20', 'list', '--json', '--global']);
});

test('incomplete global inventory invokes the pinned public add and re-verifies', async () => {
  const skillsRoot = await skillsRootFixture();
  const full = await completeList();
  let listCalls = 0;
  const calls = [];
  const result = await ensureGlobalSkills({
    packageRoot,
    skillsRoot,
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
    'skills@1.5.20',
    'add',
    'scchearn/loam',
    '--global',
    '--agent',
    'claude-code',
    '--yes',
  ]);
});

test('fresh global setup adds skills when local CLI_VERSION and skill content are absent', async () => {
  const skillsRoot = await mkdtemp(join(tmpdir(), 'loam-skills-empty-'));
  const full = await completeList();
  const calls = [];
  const result = await ensureGlobalSkills({
    packageRoot,
    skillsRoot,
    runner: async (request) => {
      calls.push(request);
      if (request.args.includes('add')) {
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
  assert.deepEqual(remove[0].args, ['--yes', 'skills@1.5.20', 'remove', 'loam::using', '--yes']);
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
