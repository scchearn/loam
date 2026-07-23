import assert from 'node:assert/strict';
import { execFile, spawn } from 'node:child_process';
import { chmod, mkdtemp, readFile, readdir, rm, writeFile } from 'node:fs/promises';
import { promisify } from 'node:util';
import { tmpdir } from 'node:os';
import { delimiter, join } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { test } from 'node:test';

const execFileAsync = promisify(execFile);
const packageRoot = fileURLToPath(new URL('..', import.meta.url));
const npmCommand = process.platform === 'win32' ? 'npm.cmd' : 'npm';

async function packedRoot() {
  const directory = await mkdtemp(join(tmpdir(), 'loam-packaged-contract-'));
  const { stdout } = await execFileAsync(npmCommand, ['pack', '--silent', '--pack-destination', directory], {
    cwd: packageRoot,
  });
  const tarball = join(directory, stdout.trim().split(/\r?\n/).at(-1));
  await execFileAsync('tar', ['-xzf', tarball], { cwd: directory });
  return { directory, root: join(directory, 'package') };
}

function runClosedStdin(command, args, options = {}) {
  const timeoutMs = options.timeoutMs || 5000;
  return new Promise((resolvePromise, reject) => {
    const child = spawn(command, args, { ...options, stdio: ['ignore', 'pipe', 'pipe'] });
    let stdout = '';
    let stderr = '';
    let settled = false;
    const timer = setTimeout(() => {
      if (settled) return;
      settled = true;
      child.kill();
      reject(new Error(`subprocess exceeded ${timeoutMs}ms`));
    }, timeoutMs);
    child.stdout.on('data', (chunk) => { stdout += chunk; });
    child.stderr.on('data', (chunk) => { stderr += chunk; });
    child.once('error', (error) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      reject(error);
    });
    child.once('close', (code, signal) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      resolvePromise({ code, signal, stdout, stderr });
    });
  });
}

test('packed setup is offline, direct-native, and preserves the legacy entry', async () => {
  const fixture = await packedRoot();
  const home = await mkdtemp(join(tmpdir(), 'loam-packaged-home-'));
  const workspace = await mkdtemp(join(tmpdir(), 'loam-packaged-workspace-'));
  try {
    const env = { ...process.env, HOME: home, USERPROFILE: home };
    const dryRun = await execFileAsync(process.execPath, [join(fixture.root, 'bin', 'loam.mjs'), 'setup', '--dry-run', '--yes'], {
      cwd: workspace,
      env,
    });
    assert.match(`${dryRun.stdout}${dryRun.stderr}`, /dry.?run/i);
    await assert.rejects(() => readdir(join(home, '.agents')));

    const integration = await readFile(join(fixture.root, 'integration', 'loam.mjs'), 'utf8');
    assert.doesNotMatch(integration, /run --|command === ['"]run['"]/);
    assert.match(integration, /status/);
    assert.match(integration, /hook/);

    const context = await import(pathToFileURL(join(fixture.root, 'integration', 'context.mjs')).href);
    assert.equal(
      context.formatNativeRuntimeCommand(String.raw`C:\Users\Sam User\.agents\loam\bin\loam.exe`, 'win32'),
      String.raw`& 'C:\Users\Sam User\.agents\loam\bin\loam.exe'`,
    );

    const legacy = await import(pathToFileURL(join(fixture.root, '.opencode', 'plugins', 'loam.js')).href);
    assert.equal(typeof legacy.LoamPlugin, 'function');
  } finally {
    await rm(fixture.directory, { recursive: true, force: true });
    await rm(home, { recursive: true, force: true });
    await rm(workspace, { recursive: true, force: true });
  }
});

test('packed setup --yes exits at a controlled Skills CLI failure with closed stdin', async () => {
  const fixture = await packedRoot();
  const home = await mkdtemp(join(tmpdir(), 'loam-closed-stdin-home-'));
  const workspace = await mkdtemp(join(tmpdir(), 'loam-closed-stdin-workspace-'));
  const fakeBin = await mkdtemp(join(tmpdir(), 'loam-closed-stdin-bin-'));
  try {
    const fakeNpx = `#!/usr/bin/env node
process.stderr.write('controlled Skills CLI failure\\n');
process.exit(1);
`;
    if (process.platform === 'win32') {
      await writeFile(join(fakeBin, 'npx.cmd'), '@node "%~dp0fake-npx.mjs" %*\r\n');
    }
    await writeFile(join(fakeBin, 'fake-npx.mjs'), fakeNpx);
    if (process.platform !== 'win32') await writeFile(join(fakeBin, 'npx'), fakeNpx, { mode: 0o700 });
    if (process.platform !== 'win32') await chmod(join(fakeBin, 'npx'), 0o700);

    const started = Date.now();
    const result = await runClosedStdin(
      process.execPath,
      [join(fixture.root, 'bin', 'loam.mjs'), 'setup', '--yes'],
      {
        cwd: workspace,
        env: {
          ...process.env,
          HOME: home,
          USERPROFILE: home,
          PATH: `${fakeBin}${delimiter}${process.env.PATH || ''}`,
          npm_config_update_notifier: 'false',
          npm_config_fund: 'false',
          npm_config_audit: 'false',
        },
        timeoutMs: 5000,
      },
    );

    assert.equal(result.code, 1, `${result.stdout}${result.stderr}`);
    assert.ok(Date.now() - started < 5000, 'closed-stdin setup exceeded its subprocess bound');
    assert.match(`${result.stdout}${result.stderr}`, /Skills CLI:/);
    await assert.rejects(() => readFile(join(home, '.agents', 'loam', 'install.json')));
  } finally {
    await rm(fixture.directory, { recursive: true, force: true });
    await rm(home, { recursive: true, force: true });
    await rm(workspace, { recursive: true, force: true });
    await rm(fakeBin, { recursive: true, force: true });
  }
});
