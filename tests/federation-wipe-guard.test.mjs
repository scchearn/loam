import assert from 'node:assert/strict';
import { mkdir, mkdtemp, stat, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

import { detectLegacyProject, migrateEnrollment, migrateLegacyProject } from '../setup/migration.mjs';
import { verifyInstallation } from '../setup/verify.mjs';

const packageRoot = fileURLToPath(new URL('..', import.meta.url));
// listSkills succeeds with an empty inventory, so the ONLY thing standing
// between the sweep and the global root is the #125 guard — remove it and these
// tests delete the global root and fail.
const emptyList = async () => ({ code: 0, stdout: '[]', stderr: '' });

async function exists(path) {
  try {
    await stat(path);
    return true;
  } catch {
    return false;
  }
}

// Seed a global root that looks like a live enrolled install: enrollment DB,
// a service definition, a legacy bin, a connector log.
async function seedGlobalRoot(globalRoot) {
  await mkdir(join(globalRoot, 'bin'), { recursive: true });
  await mkdir(join(globalRoot, 'launchagents'), { recursive: true });
  await mkdir(join(globalRoot, 'systemd'), { recursive: true });
  await writeFile(join(globalRoot, 'loam.sqlite3'), 'ENROLLMENT');
  await writeFile(join(globalRoot, 'launchagents', 'io.loam.connector.plist'), 'PLIST');
  await writeFile(join(globalRoot, 'systemd', 'loam-connector.service'), 'UNIT');
  await writeFile(join(globalRoot, 'connector.stderr.log'), 'LOG');
}

test('#125 wipe guard: a setup run from $HOME never sweeps the global root', async () => {
  const home = await mkdtemp(join(tmpdir(), 'loam-wipe-'));
  const globalRoot = join(home, '.agents', 'loam');
  await seedGlobalRoot(globalRoot);

  // workspace === home, so <workspace>/.agents/loam resolves ONTO the global root.
  const report = await detectLegacyProject({ workspace: home, packageRoot, protectedRoots: [globalRoot], runner: emptyList });
  assert.equal(report.protectedWorkspace, true, 'workspace overlapping the global root must be marked protected');
  assert.equal(report.ready, true);
  assert.deepEqual(report.paths, [], 'the global root must never be collected as a removable project-runtime path');

  // The full migration is a no-op and leaves every piece of live state intact.
  const result = await migrateLegacyProject({ workspace: home, packageRoot, protectedRoots: [globalRoot], yes: true, runner: emptyList });
  assert.equal(result.migrated, false);
  assert.equal(await exists(join(globalRoot, 'loam.sqlite3')), true, 'enrollment DB survives');
  assert.equal(await exists(join(globalRoot, 'launchagents', 'io.loam.connector.plist')), true, 'service definition survives');
  assert.equal(await exists(join(globalRoot, 'connector.stderr.log')), true, 'connector log survives');
});

test('#125 a real project workspace is still swept (guard does not over-reach)', async () => {
  const home = await mkdtemp(join(tmpdir(), 'loam-proj-'));
  const globalRoot = join(home, '.agents', 'loam');
  await mkdir(globalRoot, { recursive: true });
  // A distinct project dir that does NOT overlap the global root.
  const workspace = join(home, 'projects', 'app');
  await mkdir(join(workspace, '.agents', 'loam'), { recursive: true });

  const report = await detectLegacyProject({ workspace, packageRoot, protectedRoots: [globalRoot], runner: emptyList });
  assert.notEqual(report.protectedWorkspace, true);
  assert.ok(
    report.paths.some((entry) => entry.kind === 'project-runtime'),
    'a genuine project runtime dir must still be collected for removal',
  );
});

test('#125 verify fails when an enabled federation definition vanished', async () => {
  const home = await mkdtemp(join(tmpdir(), 'loam-verify-'));
  const globalRoot = join(home, '.agents', 'loam');
  await mkdir(globalRoot, { recursive: true }); // no systemd/loam-connector.service

  const discovery = {
    globalRoot,
    home,
    workspace: home,
    platform: 'linux',
    arch: process.arch,
    skillsRoot: join(home, '.agents', 'skills'),
    target: 'x86_64-unknown-linux-gnu',
    packageVersion: '0.0.0-test',
    harnesses: {},
    federationEnabled: true,
    legacy: { ready: true },
  };

  const gutted = await verifyInstallation({ discovery, packageRoot, runner: emptyList });
  assert.equal(gutted.federation.ready, false, 'a vanished definition must fail verification');
  assert.equal(gutted.federation.category, 'federation_definition_missing');
  assert.equal(gutted.ready, false, 'overall readiness must reflect the federation failure');

  // Positive control: with the definition present, the assertion passes.
  await mkdir(join(globalRoot, 'systemd'), { recursive: true });
  await writeFile(join(globalRoot, 'systemd', 'loam-connector.service'), 'UNIT');
  const intact = await verifyInstallation({ discovery, packageRoot, runner: emptyList });
  assert.equal(intact.federation.ready, true, 'a present definition must not trip the guard');
});

test('#125 salvage-before-destroy: enrollment reaches the config dir even from $HOME', async () => {
  const home = await mkdtemp(join(tmpdir(), 'loam-salvage-'));
  const globalRoot = join(home, '.agents', 'loam');
  await seedGlobalRoot(globalRoot);
  const configDir = join(home, '.config', 'loam');
  const env = { LOAM_CONFIG_DIR: configDir };

  // Transaction order: salvage the enrollment registry BEFORE the sweep.
  const salvage = await migrateEnrollment({ globalRoot, env, home, platform: 'linux' });
  assert.equal(salvage.migrated, true);
  await migrateLegacyProject({ workspace: home, packageRoot, protectedRoots: [globalRoot], yes: true, runner: emptyList });

  // The durable config-dir registry is populated and the legacy copy still exists.
  assert.equal(await exists(join(configDir, 'federation', 'loam.sqlite3')), true, 'config-dir registry populated by salvage');
  assert.equal(await exists(join(globalRoot, 'loam.sqlite3')), true, 'guard left the legacy registry intact');
});
