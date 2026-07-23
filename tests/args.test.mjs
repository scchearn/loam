import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { mkdtemp, readFile, rm, symlink } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { promisify } from 'node:util';
import { fileURLToPath } from 'node:url';

const execFileAsync = promisify(execFile);

import { EXIT_CODES, HELP_TEXT, PACKAGE_VERSION, SKILLS_CLI_VERSION } from '../setup/constants.mjs';
import { parseArgs, UsageError } from '../setup/args.mjs';

const root = fileURLToPath(new URL('..', import.meta.url));

test('package exposes the scoped setup executable and pinned Skills CLI', async () => {
  const packageJson = JSON.parse(await readFile(new URL('../package.json', import.meta.url), 'utf8'));

  assert.equal(packageJson.name, '@scchearn/loam');
  assert.equal(packageJson.version, PACKAGE_VERSION);
  assert.equal(packageJson.bin.loam, 'bin/loam.mjs');
  assert.equal(packageJson.dependencies.skills, SKILLS_CLI_VERSION);
  assert.match(HELP_TEXT, /@scchearn\/loam setup/);
  assert.ok(root.endsWith('/loam/'));
});

test('setup accepts the confirmation and dry-run flags', () => {
  assert.deepEqual(parseArgs(['setup']), {
    command: 'setup',
    dryRun: false,
    yes: false,
  });
  assert.deepEqual(parseArgs(['setup', '--yes', '--dry-run']), {
    command: 'setup',
    dryRun: true,
    yes: true,
  });
});

test('help and version are read-only command modes', () => {
  assert.deepEqual(parseArgs(['--help']), { command: 'help' });
  assert.deepEqual(parseArgs(['--version']), { command: 'version' });
  assert.deepEqual(parseArgs(['setup', '--help']), { command: 'help' });
  assert.deepEqual(parseArgs(['setup', '--version']), { command: 'version' });
  assert.deepEqual(parseArgs(['help']), { command: 'help' });
  assert.deepEqual(parseArgs(['version']), { command: 'version' });
});

// Regression: npx runs the bin through a node_modules/.bin symlink. The
// entrypoint guard must resolve real paths, or main() never runs and every
// command silently prints nothing.
test('the bin runs when invoked through a symlink (npx path)', async () => {
  const binPath = fileURLToPath(new URL('../bin/loam.mjs', import.meta.url));
  const dir = await mkdtemp(join(tmpdir(), 'loam-bin-'));
  const link = join(dir, 'loam');
  try {
    await symlink(binPath, link);
    const { stdout } = await execFileAsync(process.execPath, [link, '--help']);
    assert.match(stdout, /@scchearn\/loam setup/);
    const viaVerb = await execFileAsync(process.execPath, [link, 'help']);
    assert.match(viaVerb.stdout, /@scchearn\/loam setup/);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test('invalid setup arguments expose the public usage status', () => {
  assert.throws(() => parseArgs(['install']), (error) => {
    assert.ok(error instanceof UsageError);
    assert.equal(error.exitCode, EXIT_CODES.USAGE);
    return true;
  });
  assert.throws(() => parseArgs(['setup', '--unknown']), (error) => {
    assert.ok(error instanceof UsageError);
    assert.equal(error.exitCode, EXIT_CODES.USAGE);
    return true;
  });
});
