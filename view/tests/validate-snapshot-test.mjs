import assert from 'node:assert/strict';
import { readdir, readFile } from 'node:fs/promises';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { test } from 'node:test';

import { validateSnapshot } from '../server/validate-snapshot.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const fixturesDir = join(here, 'fixtures', 'snapshots');

async function loadJsonFixtures(subdir) {
  const dir = join(fixturesDir, subdir);
  const names = (await readdir(dir)).filter((name) => name.endsWith('.json'));
  assert.ok(names.length > 0, `expected at least one fixture in ${dir}`);
  const entries = [];
  for (const name of names) {
    const raw = await readFile(join(dir, name), 'utf8');
    entries.push([name, JSON.parse(raw)]);
  }
  return entries;
}

test('accepts every valid snapshot fixture', async () => {
  const fixtures = await loadJsonFixtures('valid');
  for (const [name, snapshot] of fixtures) {
    const result = validateSnapshot(snapshot);
    assert.equal(result.valid, true, `expected ${name} to be valid, got errors: ${JSON.stringify(result.errors)}`);
    assert.deepEqual(result.errors, []);
  }
});

test('rejects every invalid snapshot fixture', async () => {
  const fixtures = await loadJsonFixtures('invalid');
  for (const [name, snapshot] of fixtures) {
    const result = validateSnapshot(snapshot);
    assert.equal(result.valid, false, `expected ${name} to be invalid`);
    assert.ok(result.errors.length > 0, `expected ${name} to report at least one error`);
  }
});

test('rejects an unknown schema_version explicitly', async () => {
  const raw = await readFile(join(fixturesDir, 'invalid', 'unknown-schema-version.json'), 'utf8');
  const snapshot = JSON.parse(raw);
  assert.equal(snapshot.schema_version, 2);
  const result = validateSnapshot(snapshot);
  assert.equal(result.valid, false);
  assert.ok(
    result.errors.some((error) => error.includes('schema_version')),
    `expected a schema_version-specific error, got: ${JSON.stringify(result.errors)}`,
  );
});

test('rejects non-object input', () => {
  assert.equal(validateSnapshot(null).valid, false);
  assert.equal(validateSnapshot(undefined).valid, false);
  assert.equal(validateSnapshot('not an object').valid, false);
  assert.equal(validateSnapshot([]).valid, false);
});

test('validator logic actually checks fields, not just top-level shape', () => {
  // A minimal object with all required keys present but obviously wrong types
  // must be rejected -- this guards against a validator that only checks
  // Object.keys() without inspecting values.
  const result = validateSnapshot({
    profile: 'loam-view',
    schema_version: 1,
    generated_at: '2026-07-17T15:30:00+02:00',
    status: 'ready',
    workspace: {},
    capabilities: {},
    artifacts: 'not-an-array',
    relationships: [],
    events: [],
    metrics: {},
    signals: [],
    hints: [],
    probes: [],
  });
  assert.equal(result.valid, false);
});
