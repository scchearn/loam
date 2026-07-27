import { execFile } from 'node:child_process';
import { mkdir, mkdtemp, readFile, rm } from 'node:fs/promises';
import { promisify } from 'node:util';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const execFileAsync = promisify(execFile);
const semver = /^\d+\.\d+\.\d+$/;
const pluginVersion = process.env.LOAM_PLUGIN_VERSION || '';
const runtimeVersion = process.env.LOAM_RUNTIME_VERSION || '';

if (!semver.test(pluginVersion)) throw new Error(`invalid plugin version: ${pluginVersion}`);
if (!semver.test(runtimeVersion)) throw new Error(`invalid runtime version: ${runtimeVersion}`);

const tempRoot = await mkdtemp(join(tmpdir(), 'loam-packaged-smoke-'));
const home = join(tempRoot, 'home');
const workspace = join(tempRoot, 'workspace');
const npx = process.platform === 'win32' ? 'npx.cmd' : 'npx';
const env = { ...process.env, HOME: home, USERPROFILE: home };

try {
  await mkdir(home, { recursive: true });
  await mkdir(workspace, { recursive: true });

  const setup = await execFileAsync(npx, ['--yes', `@scchearn/loam@${pluginVersion}`, 'setup', '--yes'], {
    cwd: workspace,
    env,
    maxBuffer: 4 * 1024 * 1024,
  });
  const metadataPath = join(home, '.agents', 'loam', 'install.json');
  const metadata = JSON.parse(await readFile(metadataPath, 'utf8'));
  if (metadata.runtime_version !== runtimeVersion) {
    throw new Error(`setup selected runtime ${metadata.runtime_version}, expected ${runtimeVersion}`);
  }

  const state = await execFileAsync(metadata.runtime_path, ['state', '--fast', workspace], {
    cwd: workspace,
    env,
    maxBuffer: 4 * 1024 * 1024,
  });
  const parsed = JSON.parse(state.stdout);
  if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) throw new Error('runtime state is not an object');
  process.stdout.write(`${setup.stdout}${state.stdout}`);
  process.stdout.write(`packaged setup smoke passed for @scchearn/loam@${pluginVersion} and cli-v${runtimeVersion}\n`);
} catch (error) {
  process.stderr.write(`${error instanceof Error ? error.message : String(error)}\n`);
  process.exitCode = 1;
} finally {
  await rm(tempRoot, { recursive: true, force: true });
}
