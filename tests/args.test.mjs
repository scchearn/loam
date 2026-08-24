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

test('package exposes the scoped setup executable and pinned Skills CLI', async () => {
  const packageJson = JSON.parse(await readFile(new URL('../package.json', import.meta.url), 'utf8'));

  assert.equal(packageJson.name, '@scchearn/loam');
  assert.equal(packageJson.version, PACKAGE_VERSION);
  assert.equal(packageJson.bin.loam, 'bin/loam.mjs');
  assert.equal(packageJson.dependencies.skills, SKILLS_CLI_VERSION);
  assert.match(HELP_TEXT, /@scchearn\/loam setup/);
});

test('setup is the configurator with federation and integration flags', () => {
  assert.deepEqual(parseArgs(['setup']), {
    command: 'setup',
    dryRun: false,
    yes: false,
    purge: false,
    federation: null,
    integrations: [],
    disableIntegrations: [],
  });
  assert.deepEqual(parseArgs(['setup', '--yes', '--dry-run', '--federation', 'enable']), {
    command: 'setup',
    dryRun: true,
    yes: true,
    purge: false,
    federation: 'enable',
    integrations: [],
    disableIntegrations: [],
  });
  assert.deepEqual(parseArgs(['setup', '--federation', 'disable', '--integration', 'qmd', '--integration', 'grep', '--purge']), {
    command: 'setup',
    dryRun: false,
    yes: false,
    purge: true,
    federation: 'disable',
    integrations: ['qmd', 'grep'],
    disableIntegrations: [],
  });
  assert.deepEqual(parseArgs(['setup', '--disable-integration', 'qmd', '--purge']), {
    command: 'setup',
    dryRun: false,
    yes: false,
    purge: true,
    federation: null,
    integrations: [],
    disableIntegrations: ['qmd'],
  });
});

test('--federation rejects a value that is not enable or disable', () => {
  assert.throws(() => parseArgs(['setup', '--federation', 'maybe']), UsageError);
  assert.throws(() => parseArgs(['setup', '--federation']), UsageError);
  assert.throws(() => parseArgs(['setup', '--integration']), UsageError);
});

test('install is a distinct command (not an alias) and doctor is supported', () => {
  assert.deepEqual(parseArgs(['install']), {
    command: 'install',
    dryRun: false,
    yes: false,
    purge: false,
  });
  assert.deepEqual(parseArgs(['doctor']), {
    command: 'doctor',
    dryRun: false,
    yes: false,
    purge: false,
  });
  assert.deepEqual(parseArgs(['install', '--integration', 'qmd', '--yes']), {
    command: 'install',
    dryRun: false,
    yes: true,
    purge: false,
    integrations: ['qmd'],
  });
  // Configurator value flags belong to setup only.
  assert.throws(() => parseArgs(['install', '--federation', 'enable']), UsageError);
  assert.throws(() => parseArgs(['update', '--integration', 'qmd']), UsageError);
});

test('update is a supported setup mode with dry-run', () => {
  assert.deepEqual(parseArgs(['update']), {
    command: 'update',
    dryRun: false,
    yes: false,
    purge: false,
  });
  assert.deepEqual(parseArgs(['update', '--dry-run']), {
    command: 'update',
    dryRun: true,
    yes: false,
    purge: false,
  });
});

test('uninstall accepts an explicit --purge flag', () => {
  assert.deepEqual(parseArgs(['uninstall', '--purge']), {
    command: 'uninstall',
    dryRun: false,
    yes: false,
    purge: true,
  });
});

test('native hook commands are not package installation commands', () => {
  assert.throws(() => parseArgs(['events']), UsageError);
  assert.throws(() => parseArgs(['hooks', 'list']), UsageError);
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
  assert.throws(() => parseArgs(['unknown']), (error) => {
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

test('the shipped view command carries the workspace root and --no-open', () => {
  assert.deepEqual(parseArgs(['view']), { command: 'view', workspace: undefined, open: true });
  assert.deepEqual(parseArgs(['view', '/ws']), { command: 'view', workspace: '/ws', open: true });
  // Harness agents background-spawn with --no-open and read the URL line.
  assert.deepEqual(parseArgs(['view', '/ws', '--no-open']), { command: 'view', workspace: '/ws', open: false });
  assert.throws(() => parseArgs(['view', '--nope']), UsageError);
  assert.throws(() => parseArgs(['view', '/a', '/b']), UsageError);
});
