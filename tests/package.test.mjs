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

test('packed install is offline, direct-native, and preserves the legacy entry', async () => {
  const fixture = await packedRoot();
  const home = await mkdtemp(join(tmpdir(), 'loam-packaged-home-'));
  const workspace = await mkdtemp(join(tmpdir(), 'loam-packaged-workspace-'));
  try {
    const env = { ...process.env, HOME: home, USERPROFILE: home };
    const dryRun = await execFileAsync(process.execPath, [join(fixture.root, 'bin', 'loam.mjs'), 'install', '--dry-run', '--yes'], {
      cwd: workspace,
      env,
    });
    assert.match(`${dryRun.stdout}${dryRun.stderr}`, /dry.?run/i);
    await assert.rejects(() => readdir(join(home, '.agents')));

    // update on a machine with no install refuses with a hint to `install`
    // (the verb split: update bumps an EXISTING install, never a first install).
    const update = await execFileAsync(
      process.execPath,
      [join(fixture.root, 'bin', 'loam.mjs'), 'update', '--dry-run'],
      { cwd: workspace, env },
    ).then(() => { throw new Error('update should have refused without an install'); }, (error) => error);
    assert.equal(update.code, 1);
    assert.match(`${update.stdout}${update.stderr}`, /No Loam installation found/);
    await assert.rejects(() => readdir(join(home, '.agents')));

    const integration = await readFile(join(fixture.root, 'integration', 'loam.mjs'), 'utf8');
    assert.doesNotMatch(integration, /run --|command === ['"]run['"]/);
    assert.match(integration, /status/);
    // The harness read path is the native runtime now; the packed integration
    // must not ship a `hook` command or the retired context renderer.
    assert.doesNotMatch(integration, /command === 'hook'/);
    await assert.rejects(() => readFile(join(fixture.root, 'integration', 'context.mjs')));

    const legacy = await import(pathToFileURL(join(fixture.root, '.opencode', 'plugins', 'loam.js')).href);
    assert.equal(typeof legacy.LoamPlugin, 'function');
  } finally {
    await rm(fixture.directory, { recursive: true, force: true });
    await rm(home, { recursive: true, force: true });
    await rm(workspace, { recursive: true, force: true });
  }
});

test('packed install --yes exits at a controlled Skills CLI failure with closed stdin', async () => {
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
      [join(fixture.root, 'bin', 'loam.mjs'), 'install', '--yes'],
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
    await assert.rejects(() => readFile(join(adapterRoot, 'hooks', 'session-start.mjs'), 'utf8'));
    await assert.doesNotReject(() => readFile(join(adapterRoot, 'hooks', 'stop.mjs'), 'utf8'));
    await assert.doesNotReject(() => readFile(join(adapterRoot, 'hooks', 'subagent-start.mjs'), 'utf8'));
    await assert.doesNotReject(() => readFile(join(adapterRoot, 'hooks', 'subagent-stop.mjs'), 'utf8'));
  } finally {
    await rm(fixture.directory, { recursive: true, force: true });
  }
});
