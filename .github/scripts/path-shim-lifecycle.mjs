import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { execFile } from 'node:child_process';
import { chmod, mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { promisify } from 'node:util';

import { parseArgs } from '../../setup/args.mjs';
import { detectTarget } from '../../setup/target.mjs';
import { runDoctor } from '../../setup/doctor.mjs';
import { runSetup } from '../../setup/main.mjs';
import { uninstall } from '../../setup/uninstall.mjs';
import { shimLocations, windowsPowerShellPath } from '../../setup/shim.mjs';
import { loadSkillInventory } from '../../setup/inventory.mjs';
import { processDescriptor } from '../../integration/ingest-process.mjs';

const execFileAsync = promisify(execFile);
const packageRoot = fileURLToPath(new URL('../..', import.meta.url));
const target = detectTarget();
const delimiter = process.platform === 'win32' ? ';' : ':';

async function buildFixture(version, outputPath, sourceRoot) {
  const sourcePath = join(sourceRoot, `runtime-${version}.rs`);
  await writeFile(sourcePath, `
const VERSION: &str = "${version}";
fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("--version") => println!("{}", VERSION),
        Some("state") => println!("{{\\"version\\":\\"{}\\"}}", VERSION),
        _ => std::process::exit(1),
    }
}
`);
  await execFileAsync('rustup', [
    'run', '1.94.1', 'rustc', '--crate-name', 'runtimefixture', '--edition', '2021', sourcePath, '-O', '-o', outputPath,
  ]);
}

async function publishRelease(releaseDir, binaryPath, version) {
  const filename = `loam-${target}${target.includes('windows') ? '.exe' : ''}`;
  const destination = join(releaseDir, filename);
  await rm(destination, { force: true });
  await writeFile(destination, await readFile(binaryPath));
  if (process.platform !== 'win32') await chmod(destination, 0o700);
  const sha256 = createHash('sha256').update(await readFile(destination)).digest('hex');
  await writeFile(join(releaseDir, 'loam-runtime-manifest.json'), JSON.stringify({
    version,
    runtimes: [{ target, file: filename, sha256 }],
  }));
}

function capture() {
  const chunks = [];
  return { output: { write: (value) => chunks.push(String(value)) }, text: () => chunks.join('') };
}

async function skillsRunner(home) {
  const inventory = await loadSkillInventory({ packageRoot });
  const entries = inventory.skills.map((skill) => ({
    name: skill.frontmatterName,
    source: 'https://github.com/scchearn/loam',
  }));
  let installed = false;
  return async ({ args }) => {
    if (args.includes('list')) {
      return { code: 0, stdout: JSON.stringify(installed ? entries : []), stderr: '' };
    }
    if (args.includes('add')) {
      installed = true;
      await mkdir(join(home, '.agents', 'skills', 'loam-using'), { recursive: true });
      await writeFile(join(home, '.agents', 'skills', 'loam-using', 'SKILL.md'), '# using\n');
      await mkdir(join(home, '.agents', 'skills', 'loam-ingesting-codebase', 'references'), { recursive: true });
      await writeFile(join(home, '.agents', 'skills', 'loam-ingesting-codebase', 'references', 'ingestion-exclusions.md'), '# exclusions\n');
      return { code: 0, stdout: '', stderr: '' };
    }
    if (args.includes('remove')) {
      installed = false;
      return { code: 0, stdout: '', stderr: '' };
    }
    return { code: 0, stdout: '', stderr: '' };
  };
}

async function freshShellEnv(base, binDir) {
  if (process.platform !== 'win32') return { ...base, PATH: `${binDir}${delimiter}${base.PATH || ''}` };
  const { stdout } = await execFileAsync(windowsPowerShellPath(base), [
    '-NoProfile', '-NonInteractive', '-Command', "[Environment]::GetEnvironmentVariable('Path', 'User')",
  ]);
  const path = stdout.trim();
  const inherited = base.Path || base.PATH || '';
  const combined = [path, inherited].filter(Boolean).join(';');
  return { ...base, PATH: combined, Path: combined };
}

async function runShim(path, env, cwd) {
  if (process.platform === 'win32') {
    const descriptor = processDescriptor({ command: path, args: ['--version'], env });
    const result = await execFileAsync(descriptor.executable, descriptor.args, {
      env: descriptor.env || env,
      cwd,
      maxBuffer: 1024 * 1024,
      windowsVerbatimArguments: descriptor.windowsVerbatimArguments === true,
    });
    return result.stdout.trim();
  }
  const result = await execFileAsync(path, ['--version'], { env, cwd, maxBuffer: 1024 * 1024 });
  return result.stdout.trim();
}

const tempRoot = await mkdtemp(join(tmpdir(), 'loam-path-shim-ci-'));
const home = join(tempRoot, 'home');
const workspace = join(tempRoot, 'workspace');
const configDir = join(tempRoot, 'config');
const releaseDir = join(tempRoot, 'release');
const sourceRoot = join(tempRoot, 'source');
const globalRoot = join(home, '.agents', 'loam');
await Promise.all([home, workspace, configDir, releaseDir, sourceRoot].map((path) => mkdir(path, { recursive: true })));
const env = {
  ...process.env,
  HOME: home,
  USERPROFILE: home,
  LOAM_HOME: globalRoot,
  LOAM_CONFIG_DIR: configDir,
  LOCALAPPDATA: join(home, 'AppData', 'Local'),
  APPDATA: join(home, 'AppData', 'Roaming'),
  LOAM_RUNTIME_VERSION: '1.0.0',
};
const runner = await skillsRunner(home);
const fixtureOutput = capture();
let installed = false;

try {
  const firstBinary = join(sourceRoot, process.platform === 'win32' ? 'runtime-1.0.0.exe' : 'runtime-1.0.0');
  const secondBinary = join(sourceRoot, process.platform === 'win32' ? 'runtime-1.1.0.exe' : 'runtime-1.1.0');
  await buildFixture('1.0.0', firstBinary, sourceRoot);
  await buildFixture('1.1.0', secondBinary, sourceRoot);
  await publishRelease(releaseDir, firstBinary, '1.0.0');

  const installCode = await runSetup(parseArgs(['install', '--yes']), {
    home,
    workspace,
    packageRoot,
    releaseBaseUrl: pathToFileURL(releaseDir).href,
    output: fixtureOutput.output,
    errorOutput: fixtureOutput.output,
    env,
    runner,
  });
  assert.equal(installCode, 0, fixtureOutput.text());
  installed = true;

  const locations = shimLocations({ home, env, platform: process.platform });
  const shimBefore = await readFile(locations.shimPath);
  const shellEnv = await freshShellEnv(env, locations.binDir);
  assert.equal(await runShim(locations.shimPath, shellEnv, workspace), '1.0.0');

  await publishRelease(releaseDir, secondBinary, '1.1.0');
  env.LOAM_RUNTIME_VERSION = '1.1.0';
  const updateOutput = capture();
  const updateCode = await runSetup(parseArgs(['update']), {
    home,
    workspace,
    packageRoot,
    releaseBaseUrl: pathToFileURL(releaseDir).href,
    output: updateOutput.output,
    errorOutput: updateOutput.output,
    env,
    runner,
  });
  assert.equal(updateCode, 0, updateOutput.text());
  assert.equal(await runShim(locations.shimPath, await freshShellEnv(env, locations.binDir), workspace), '1.1.0');
  assert.deepEqual(await readFile(locations.shimPath), shimBefore, 'update must not rewrite the stable shim');

  const doctorOutput = capture();
  const doctorCode = await runDoctor({
    home,
    workspace,
    packageRoot,
    env: await freshShellEnv(env, locations.binDir),
    runner,
    output: doctorOutput.output,
    errorOutput: doctorOutput.output,
  });
  assert.equal(doctorCode, 0, doctorOutput.text());
  assert.match(doctorOutput.text(), /PATH launcher: ok/);

  const missingOutput = capture();
  await rm(locations.shimPath, { force: true });
  const missingDoctor = await runDoctor({
    home,
    workspace,
    packageRoot,
    env: await freshShellEnv(env, locations.binDir),
    runner,
    output: missingOutput.output,
    errorOutput: missingOutput.output,
  });
  assert.equal(missingDoctor, 1);
  assert.match(missingOutput.text(), /PATH launcher: failed/);
  assert.match(missingOutput.text(), /install/i);

  // Reinstall repairs the deliberately missing shim before the final uninstall.
  env.LOAM_RUNTIME_VERSION = '1.1.0';
  const repairOutput = capture();
  assert.equal(await runSetup(parseArgs(['install', '--yes']), {
    home,
    workspace,
    packageRoot,
    releaseBaseUrl: pathToFileURL(releaseDir).href,
    output: repairOutput.output,
    errorOutput: repairOutput.output,
    env,
    runner,
  }), 0, repairOutput.text());

  const uninstallOutput = capture();
  assert.equal(await uninstall({
    home,
    globalRoot,
    env,
    platform: process.platform,
    yes: true,
    runner,
    output: uninstallOutput.output,
  }), 0, uninstallOutput.text());
  installed = false;
  assert.equal(await runShim(locations.shimPath, await freshShellEnv(env, locations.binDir), workspace).catch(() => null), null);
  if (process.platform === 'win32') {
    const userPath = (await execFileAsync(windowsPowerShellPath(env), [
      '-NoProfile', '-NonInteractive', '-Command', "[Environment]::GetEnvironmentVariable('Path', 'User')",
    ])).stdout;
    assert.doesNotMatch(userPath, new RegExp(locations.binDir.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')));
  }
  process.stdout.write('path shim lifecycle passed\n');
} finally {
  if (installed) {
    await uninstall({ home, globalRoot, env, platform: process.platform, yes: true, runner, output: { write: () => {} } }).catch(() => {});
  }
  await rm(tempRoot, { recursive: true, force: true });
}
