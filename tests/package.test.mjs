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
    shell: process.platform === 'win32',
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

    const update = await execFileAsync(process.execPath, [join(fixture.root, 'bin', 'loam.mjs'), 'update', '--dry-run'], {
      cwd: workspace,
      env,
    });
    assert.match(`${update.stdout}${update.stderr}`, /Loam Update \(dry-run\)/);
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

test('packed marketplaces expose loam:ingestor, the loam_ingestor profile, and subagent hooks', async () => {
  const fixture = await packedRoot();
  try {
    const claudeMarketplace = JSON.parse(await readFile(join(fixture.root, '.claude-plugin', 'marketplace.json'), 'utf8'));
    const codexMarketplace = JSON.parse(await readFile(join(fixture.root, '.agents', 'plugins', 'marketplace.json'), 'utf8'));
    const adapterRoot = join(fixture.root, 'plugins', 'loam-adapter');
    const claudePlugin = JSON.parse(await readFile(join(adapterRoot, '.claude-plugin', 'plugin.json'), 'utf8'));
    const codexPlugin = JSON.parse(await readFile(join(adapterRoot, '.codex-plugin', 'plugin.json'), 'utf8'));

    assert.equal(claudeMarketplace.plugins[0].source, './plugins/loam-adapter');
    assert.equal(codexMarketplace.plugins[0].source, './plugins/loam-adapter');
    assert.equal('skills' in claudePlugin, false);
    assert.equal('skills' in codexPlugin, false);
    await assert.rejects(() => readdir(join(adapterRoot, 'skills')));
    const agent = await readFile(join(adapterRoot, 'agents', 'ingestor.md'), 'utf8');
    assert.match(agent, /^---\r?\n[\s\S]*?^name: ingestor$/mu);
    assert.match(agent, /^tools: .*\bSkill\b.*$/mu);
    // Frontmatter outranks every other model source, so this pins both --bg and any future dispatch form.
    assert.match(agent, /^model: haiku$/mu);
    assert.doesNotMatch(agent.match(/^tools: (.*)$/mu)?.[1] || '', /(?:^|,\s*)Agent(?:,|$)/u);
    assert.match(agent, /Never spawn or delegate to another agent or subagent/u);
    const harvester = await readFile(join(adapterRoot, 'agents', 'harvester.md'), 'utf8');
    assert.match(harvester, /^---\r?\n[\s\S]*?^name: harvester$/mu);
    assert.match(harvester, /^tools: .*\bSkill\b.*$/mu);
    assert.match(harvester, /^model: haiku$/mu);
    assert.match(harvester, /loam::learning-from-session/u);
    assert.match(harvester, /Never spawn or delegate to another agent or subagent/u);
    const codexAgent = await readFile(join(fixture.root, 'adapters', 'loam_ingestor.toml'), 'utf8');
    assert.match(codexAgent, /^name = "loam_ingestor"$/mu);
    assert.match(codexAgent, /^description = "[^"\r\n]+"$/mu);
    assert.match(codexAgent, /^developer_instructions = """$/mu);
    assert.doesNotMatch(codexAgent, /^(?:model|model_reasoning_effort|sandbox_mode)\s*=/mu);
    await assert.doesNotReject(() => readFile(join(adapterRoot, 'hooks', 'session-start.mjs'), 'utf8'));
    await assert.doesNotReject(() => readFile(join(adapterRoot, 'hooks', 'stop.mjs'), 'utf8'));
    await assert.doesNotReject(() => readFile(join(adapterRoot, 'hooks', 'subagent-start.mjs'), 'utf8'));
    await assert.doesNotReject(() => readFile(join(adapterRoot, 'hooks', 'subagent-stop.mjs'), 'utf8'));
  } finally {
    await rm(fixture.directory, { recursive: true, force: true });
  }
});

test('the packed session-start adapter sources context through the federation-aware runtime hook', async () => {
  const fixture = await packedRoot();
  try {
    const adapterRoot = join(fixture.root, 'plugins', 'loam-adapter');
    // Main's session-start structure is intact: the marketplace adapter and its
    // SessionStart hook file both ship.
    const adapter = await readFile(join(adapterRoot, 'adapter.mjs'), 'utf8');
    await assert.doesNotReject(() => readFile(join(adapterRoot, 'hooks', 'session-start.mjs'), 'utf8'));
    assert.match(adapter, /createMarketplaceAdapter/);
    // ...and additively, the injected context is sourced from the native runtime
    // `<runtime> hook <harness> --body`, so the baseline it carries now includes
    // the sanitized federation section (federation experience present — layered
    // on main's path, not the baseline-only Node shim). The runtime renders and
    // sanitizes the federation data; the adapter only relays its output.
    assert.match(adapter, /defaultRuntimePath/, 'adapter must resolve the staged runtime path');
    assert.match(adapter, /runtime_path/, 'runtime path comes from install.json');
    assert.match(adapter, /'hook'[\s\S]{0,120}--body/, 'context is sourced from `hook … --body`');
  } finally {
    await rm(fixture.directory, { recursive: true, force: true });
  }
});
