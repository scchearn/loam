import assert from 'node:assert/strict';
import { mkdir, mkdtemp, readFile, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';

import { migrateEnrollment } from '../setup/migration.mjs';

// LOAM_CONFIG_DIR is pinned in the PASSED env (never process.env), so the whole
// test resolves inside a temp dir and never touches the developer's real config
// dir / federation registry.
async function fixture() {
  const home = await mkdtemp(join(tmpdir(), 'loam-enroll-mig-'));
  const globalRoot = join(home, '.agents', 'loam');
  const configDir = join(home, 'config');
  await mkdir(globalRoot, { recursive: true });
  return { home, globalRoot, configDir, env: { LOAM_CONFIG_DIR: configDir } };
}

test('migrateEnrollment copies the legacy global-root registry into the config dir', async () => {
  const f = await fixture();
  await writeFile(join(f.globalRoot, 'loam.sqlite3'), 'legacy-enrollment-bytes');

  const result = await migrateEnrollment({ globalRoot: f.globalRoot, env: f.env });

  const dest = join(f.configDir, 'federation', 'loam.sqlite3');
  assert.equal(result.migrated, true);
  assert.equal(result.to, dest);
  assert.equal(await readFile(dest, 'utf8'), 'legacy-enrollment-bytes');
});

test('migrateEnrollment is idempotent — it never clobbers a live config-dir registry', async () => {
  const f = await fixture();
  await writeFile(join(f.globalRoot, 'loam.sqlite3'), 'stale-legacy-copy');
  // The config-dir registry already exists (a migrated, now-live store). A
  // re-run must never overwrite it with the stale legacy copy.
  await mkdir(join(f.configDir, 'federation'), { recursive: true });
  const dest = join(f.configDir, 'federation', 'loam.sqlite3');
  await writeFile(dest, 'current-config-dir-registry');

  const result = await migrateEnrollment({ globalRoot: f.globalRoot, env: f.env });

  assert.equal(result.migrated, false);
  assert.equal(result.reason, 'registry_present');
  assert.equal(await readFile(dest, 'utf8'), 'current-config-dir-registry');
});

test('migrateEnrollment is a no-op on a machine with no legacy registry', async () => {
  const f = await fixture();

  const result = await migrateEnrollment({ globalRoot: f.globalRoot, env: f.env });

  assert.equal(result.migrated, false);
  assert.equal(result.reason, 'no_legacy_registry');
});
