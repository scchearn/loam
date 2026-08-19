// Loam View producer/validator agreement contract (T7). This is the Node
// half of the same claim cli/tests/view_contract.rs proves on the Rust side:
// `loam state --view` over every cli/tests/fixtures/view/ workspace produces
// a snapshot that `validate-snapshot.mjs` -- the runtime validation path,
// since no JSON-Schema engine is vendored for the product -- accepts. This
// is what actually proves the Rust producer and the Node validator agree,
// including on the `degraded` fixture's invalid-UTF-8 artifact.
//
// The native binary path comes from LOAM_NATIVE_BIN, defaulting to
// <repo>/target/debug/loam (the workspace-level target/ dir, not
// cli/target/) -- build it first with `cargo build --locked --package loam`.

import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { existsSync } from 'node:fs';
import { cp, mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';
import { before, test } from 'node:test';

import { validateSnapshot } from '../server/validate-snapshot.mjs';

const execFileAsync = promisify(execFile);
const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = join(here, '..', '..');
const fixturesDir = join(repoRoot, 'cli', 'tests', 'fixtures', 'view');

// Same fixture set covered by view_contract.rs's FIXTURES constant --
// documented in cli/tests/fixtures/view/README.md.
const FIXTURES = ['sparse', 'healthy', 'code-drift', 'broken-links', 'malformed', 'degraded', 'chronicle'];

const defaultBinary = join(repoRoot, 'target', 'debug', process.platform === 'win32' ? 'loam.exe' : 'loam');
const binary = process.env.LOAM_NATIVE_BIN || defaultBinary;

before(() => {
  assert.ok(
    existsSync(binary),
    `no native runtime at ${binary} -- build it first (cargo build --locked --package loam) or set LOAM_NATIVE_BIN`,
  );
});

async function runView(name) {
  const workspace = await mkdtemp(join(tmpdir(), `loam-view-node-contract-${name}-`));
  const configDir = await mkdtemp(join(tmpdir(), `loam-view-node-contract-cfg-${name}-`));
  try {
    await cp(join(fixturesDir, name), workspace, { recursive: true });
    // LOAM_CONFIG_DIR isn't read by this producer today, but every
    // child-process test in this repo pins it so that never becomes an
    // accidental hermeticity gap against a developer's real enrollment
    // registry.
    const { stdout } = await execFileAsync(binary, ['state', '--view', workspace], {
      env: { ...process.env, LOAM_CONFIG_DIR: configDir },
    });
    return JSON.parse(stdout);
  } finally {
    await rm(workspace, { recursive: true, force: true });
    await rm(configDir, { recursive: true, force: true });
  }
}

for (const name of FIXTURES) {
  test(`loam state --view over the ${name} fixture validates against the snapshot v1 contract`, async () => {
    const snapshot = await runView(name);
    const result = validateSnapshot(snapshot);
    assert.equal(result.valid, true, `fixture \`${name}\` failed validation: ${JSON.stringify(result.errors, null, 2)}`);
  });
}
