import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdir, readFile, readdir, writeFile } from 'node:fs/promises';
import { mkdtemp } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { fileURLToPath } from 'node:url';
import { test } from 'node:test';

import { loadSkillInventory } from '../setup/inventory.mjs';
import { runSetup } from '../setup/main.mjs';
import { parseArgs } from '../setup/args.mjs';
import { PACKAGE_VERSION } from '../setup/constants.mjs';
import { discover } from '../setup/discovery.mjs';
import { verifyInstallation } from '../setup/verify.mjs';
import { detectTarget, runtimePath } from '../setup/target.mjs';
import { uninstall } from '../setup/uninstall.mjs';
import { federationDefinitionPath } from '../setup/federation.mjs';

const packageRoot = fileURLToPath(new URL('..', import.meta.url));
const target = detectTarget();

async function releaseFixture() {
  const release = await mkdtemp(join(tmpdir(), 'loam-setup-release-'));
  const bytes = 'verified runtime';
  const file = `loam-${target}${target.includes('windows') ? '.exe' : ''}`;
  await writeFile(join(release, file), bytes);
  await writeFile(
    join(release, 'loam-runtime-manifest.json'),
    JSON.stringify({ version: '0.9.1', runtimes: [{ target, file, sha256: createHash('sha256').update(bytes).digest('hex') }] }),
  );
  return { url: pathToFileURL(release).href, bytes };
}

async function fullList() {
  const inventory = await loadSkillInventory({ packageRoot });
  return {
    skills: inventory.skills.map((skill) => ({ name: skill.frontmatterName, source: 'https://github.com/scchearn/loam' })),
  };
}

function outputCapture() {
  const chunks = [];
  return { output: { write: (chunk) => chunks.push(String(chunk)) }, text: () => chunks.join('') };
}

async function baseFixture() {
  const home = await mkdtemp(join(tmpdir(), 'loam-setup-home-'));
  const workspace = await mkdtemp(join(tmpdir(), 'loam-setup-workspace-'));
  const release = await releaseFixture();
  const list = await fullList();
  const runner = async (request) => {
    if (request.args.includes('list')) return { code: 0, stdout: JSON.stringify(list), stderr: '' };
    if (request.args.includes('add')) {
      const skillsRoot = join(home, '.agents', 'skills', 'loam-using');
      await mkdir(join(skillsRoot, 'scripts'), { recursive: true });
      await writeFile(join(skillsRoot, 'scripts', 'CLI_VERSION'), '0.9.1\n');
      await writeFile(join(skillsRoot, 'SKILL.md'), '---\nname: loam::using\n---\n# using\n');
      const ingestRoot = join(home, '.agents', 'skills', 'loam-ingesting-codebase', 'references');
      await mkdir(ingestRoot, { recursive: true });
      await writeFile(join(ingestRoot, 'ingestion-exclusions.md'), '# exclusions\n');
      return { code: 0, stdout: '', stderr: '' };
    }
    if (request.args.includes('remove')) return { code: 0, stdout: '', stderr: '' };
    return { code: 0, stdout: '', stderr: '' };
  };
  return {
    home,
    workspace,
    release,
    releaseBaseUrl: release.url,
    runner,
    detected: {
      opencode: { id: 'opencode', state: 'absent' },
      claude: { id: 'claude', state: 'absent' },
      cursor: { id: 'cursor', state: 'absent' },
    },
    smokeRunner: async () => ({ code: 0, stdout: '{"exists":false}', stderr: '' }),
  };
}

test('harvest_packaging: fresh install stages every harvest module and the harvest worker', async () => {
  const fixture = await baseFixture();
  const capture = outputCapture();
  const code = await runSetup(parseArgs(['install', '--yes']), {
    ...fixture,
    packageRoot,
    output: capture.output,
    errorOutput: capture.output,
  });
  assert.equal(code, 0, capture.text());
  const globalRoot = join(fixture.home, '.agents', 'loam');
  const install = JSON.parse(await readFile(join(globalRoot, 'install.json'), 'utf8'));
  const integrationPath = install.integration_path;
  const integrationRoot = join(integrationPath, '..');
  for (const module of [
    'harvest-state.mjs', 'harvest-window.mjs', 'harvest-store.mjs',
    'harvest-claude.mjs', 'harvest-codex.mjs', 'harvest-opencode.mjs', 'harvest.mjs',
  ]) {
    assert.ok(
      (await readdir(integrationRoot)).includes(module),
      `fresh install must stage ${module}`,
    );
  }
  const adapterRoot = install.adapter_root;
  assert.ok(
    (await readdir(adapterRoot)).includes('harvest-worker.mjs'),
    'fresh install must stage harvest-worker.mjs in the adapter root',
  );
});

test('harvest_packaging: upgrade stages harvest modules idempotently and final verification passes', async () => {
  const fixture = await baseFixture();
  const first = outputCapture();
  await runSetup(parseArgs(['install', '--yes']), { ...fixture, packageRoot, output: first.output, errorOutput: first.output });
  const second = outputCapture();
  const code = await runSetup(parseArgs(['install', '--yes']), {
    ...fixture,
    packageRoot,
    output: second.output,
    errorOutput: second.output,
  });
  assert.equal(code, 0, second.text());
  const globalRoot = join(fixture.home, '.agents', 'loam');
  const install = JSON.parse(await readFile(join(globalRoot, 'install.json'), 'utf8'));
  const integrationRoot = join(install.integration_path, '..');
  assert.ok((await readdir(integrationRoot)).includes('harvest.mjs'), 'upgrade is idempotent');
});

test('clean --yes setup completes and publishes verified install metadata', async () => {
  const fixture = await baseFixture();
  const capture = outputCapture();
  const code = await runSetup(parseArgs(['install', '--yes']), {
    ...fixture,
    packageRoot,
    output: capture.output,
    errorOutput: capture.output,
  });

  assert.equal(code, 0, capture.text());
  assert.match(capture.text(), /Loam is ready/);
  const globalRoot = join(fixture.home, '.agents', 'loam');
  const metadata = JSON.parse(await readFile(join(globalRoot, 'install.json'), 'utf8'));
  assert.equal(metadata.schema_version, 1);
  assert.equal(metadata.runtime_version, '0.9.1');
  assert.equal(metadata.target, target);
  assert.equal(metadata.runtime_sha256, createHash('sha256').update(fixture.release.bytes).digest('hex'));
  assert.equal(metadata.skills_scope, 'global');
  assert.equal(await readFile(runtimePath(globalRoot, '0.9.1', target), 'utf8'), fixture.release.bytes);
});

test('complete ready rerun is local-only and does not call Skills CLI or download', async () => {
  const fixture = await baseFixture();
  await runSetup(parseArgs(['install', '--yes']), { ...fixture, packageRoot, output: outputCapture().output });
  const capture = outputCapture();
  const code = await runSetup(parseArgs(['install', '--yes']), {
    ...fixture,
    packageRoot,
    releaseBaseUrl: 'file:///missing-release',
    runner: async () => { throw new Error('offline rerun invoked Skills CLI'); },
    output: capture.output,
    errorOutput: capture.output,
  });

  assert.equal(code, 0, capture.text());
  assert.match(capture.text(), /already ready|Loam is ready/);
});

test('update refreshes a ready installation without prompting', async () => {
  const fixture = await baseFixture();
  await runSetup(parseArgs(['install', '--yes']), { ...fixture, packageRoot, output: outputCapture().output });
  const metadataPath = join(fixture.home, '.agents', 'loam', 'install.json');
  const previous = JSON.parse(await readFile(metadataPath, 'utf8'));
  let skillAdds = 0;
  const capture = outputCapture();
  const code = await runSetup(parseArgs(['update']), {
    ...fixture,
    packageRoot,
    runner: async (request) => {
      if (request.command.includes('npx') && request.args.includes('skills') && request.args.includes('add')) skillAdds += 1;
      return fixture.runner(request);
    },
    confirm: async () => { throw new Error('update prompted for confirmation'); },
    output: capture.output,
    errorOutput: capture.output,
  });

  assert.equal(code, 0, capture.text());
  assert.equal(skillAdds, 1);
  assert.match(capture.text(), /Loam Update/);
  const current = JSON.parse(await readFile(metadataPath, 'utf8'));
  assert.notEqual(current.integration_path, previous.integration_path);
});

test('setup reconciles an install from an older plugin version', async () => {
  const fixture = await baseFixture();
  await runSetup(parseArgs(['install', '--yes']), { ...fixture, packageRoot, output: outputCapture().output });
  const metadataPath = join(fixture.home, '.agents', 'loam', 'install.json');
  const previous = JSON.parse(await readFile(metadataPath, 'utf8'));
  const databasePath = join(fixture.home, '.agents', 'loam', 'loam.sqlite3');
  await writeFile(databasePath, 'operational history');
  await writeFile(metadataPath, JSON.stringify({ ...previous, plugin_version: '0.0.0' }));

  const capture = outputCapture();
  const code = await runSetup(parseArgs(['install', '--yes']), {
    ...fixture,
    packageRoot,
    output: capture.output,
    errorOutput: capture.output,
  });

  assert.equal(code, 0, capture.text());
  assert.doesNotMatch(capture.text(), /already ready/);
  const current = JSON.parse(await readFile(metadataPath, 'utf8'));
  assert.equal(current.plugin_version, PACKAGE_VERSION);
  assert.notEqual(current.integration_path, previous.integration_path);
  assert.equal(await readFile(databasePath, 'utf8'), 'operational history');
});

test('marketplace-owned Claude and Codex satisfy readiness with the Codex agent profile and without user hooks', async () => {
  const fixture = await baseFixture();
  await mkdir(join(fixture.home, '.claude'), { recursive: true });
  await mkdir(join(fixture.home, '.codex'), { recursive: true });
  await writeFile(join(fixture.home, '.claude', 'settings.json'), JSON.stringify({
    enabledPlugins: { 'loam@loam': true },
  }));
  await writeFile(join(fixture.home, '.codex', 'config.toml'), '[plugins."loam@loam"]\nenabled = true\n');
  const claudeCache = join(fixture.home, '.claude', 'plugins', 'cache', 'loam', 'loam', PACKAGE_VERSION);
  await mkdir(join(claudeCache, 'hooks'), { recursive: true });
  await writeFile(join(claudeCache, 'hooks', 'hooks.json'), JSON.stringify({ hooks: { Stop: [{}] } }));
  await writeFile(join(fixture.home, '.claude', 'plugins', 'installed_plugins.json'), JSON.stringify({
    version: 2,
    plugins: { 'loam@loam': [{ scope: 'user', installPath: claudeCache, version: PACKAGE_VERSION }] },
  }));
  const codexCache = join(fixture.home, '.codex', 'plugins', 'cache', 'loam', 'loam', PACKAGE_VERSION);
  await mkdir(join(codexCache, 'hooks'), { recursive: true });
  await writeFile(join(codexCache, 'hooks', 'hooks.json'), JSON.stringify({ hooks: { Stop: [{}] } }));
  const capture = outputCapture();

  const code = await runSetup(parseArgs(['install', '--yes']), {
    ...fixture,
    packageRoot,
    output: capture.output,
    errorOutput: capture.output,
  });

  assert.equal(code, 0, capture.text());
  const metadata = JSON.parse(await readFile(join(fixture.home, '.agents', 'loam', 'install.json'), 'utf8'));
  assert.ok(metadata.configured_harnesses.includes('claude'));
  assert.ok(metadata.configured_harnesses.includes('codex'));
  const claude = JSON.parse(await readFile(join(fixture.home, '.claude', 'settings.json'), 'utf8'));
  assert.deepEqual(claude.hooks.SessionStart, []);
  assert.equal((claude.hooks.Stop || []).flatMap((entry) => entry.hooks || []).length, 0);
  await assert.rejects(() => readFile(join(fixture.home, '.codex', 'hooks.json')), { code: 'ENOENT' });
  assert.equal(
    await readFile(join(fixture.home, '.codex', 'agents', 'loam_ingestor.toml'), 'utf8'),
    await readFile(join(packageRoot, 'adapters', 'loam_ingestor.toml'), 'utf8'),
  );

  const profilePath = join(fixture.home, '.codex', 'agents', 'loam_ingestor.toml');
  await writeFile(profilePath, '# Managed by @scchearn/loam setup.\nname = "loam_ingestor"\n');
  const tampered = await verifyInstallation({
    discovery: await discover({ home: fixture.home, workspace: fixture.workspace, packageRoot }),
    packageRoot,
    runtimeRunner: fixture.smokeRunner,
  });
  assert.equal(tampered.harnesses.codex.category, 'agent_profile_mismatch');

  const updateCode = await runSetup(parseArgs(['update']), {
    ...fixture,
    packageRoot,
    output: outputCapture().output,
  });
  assert.equal(updateCode, 0);
  assert.equal(
    await readFile(profilePath, 'utf8'),
    await readFile(join(packageRoot, 'adapters', 'loam_ingestor.toml'), 'utf8'),
  );
});

test('failed setup restores a pre-existing Codex agent profile collision', async () => {
  const fixture = await baseFixture();
  const profilePath = join(fixture.home, '.codex', 'agents', 'loam_ingestor.toml');
  const original = 'name = "personal_ingestor"\ndescription = "User profile"\ndeveloper_instructions = "Keep me"\n';
  await mkdir(join(fixture.home, '.codex', 'agents'), { recursive: true });
  await writeFile(profilePath, original);

  let observedManagedProfile = false;
  const code = await runSetup(parseArgs(['install', '--yes']), {
    ...fixture,
    packageRoot,
    beforeActivate: async () => {
      observedManagedProfile = (await readFile(profilePath, 'utf8')).includes('name = "loam_ingestor"');
      assert.equal(await readFile(`${profilePath}.loam-backup`, 'utf8'), original);
      throw new Error('controlled interruption');
    },
    output: outputCapture().output,
  });

  assert.equal(code, 1);
  assert.equal(observedManagedProfile, true);
  assert.equal(await readFile(profilePath, 'utf8'), original);
  await assert.rejects(() => readFile(`${profilePath}.loam-backup`), { code: 'ENOENT' });
});

test('setup verifies an updated marketplace plugin from disk instead of trusting CLI success', async () => {
  const fixture = await baseFixture();
  const cache = join(fixture.home, '.claude', 'plugins', 'cache', 'loam', 'loam', '0.8.6');
  await mkdir(join(cache, 'hooks'), { recursive: true });
  await writeFile(join(cache, 'hooks', 'hooks.json'), JSON.stringify({ hooks: { Stop: [{}] } }));
  await writeFile(join(fixture.home, '.claude', 'settings.json'), JSON.stringify({ enabledPlugins: { 'loam@loam': true } }));
  await writeFile(join(fixture.home, '.claude', 'plugins', 'installed_plugins.json'), JSON.stringify({
    version: 2,
    plugins: { 'loam@loam': [{ scope: 'user', installPath: cache, version: '0.8.6' }] },
  }));
  const capture = outputCapture();

  const code = await runSetup(parseArgs(['install', '--yes']), {
    ...fixture,
    packageRoot,
    output: capture.output,
    errorOutput: capture.output,
  });

  assert.equal(code, 1, capture.text());
  assert.match(capture.text(), /verification failed|incomplete/i);
});

test('setup --yes installs a missing Codex plugin in one pass', async () => {
  const fixture = await baseFixture();
  await mkdir(join(fixture.home, '.codex'), { recursive: true });
  const calls = [];
  const runner = async (request) => {
    if (request.command !== 'codex') return fixture.runner(request);
    calls.push(request.args);
    if (request.args[1] === 'add') {
      await writeFile(join(fixture.home, '.codex', 'config.toml'), '[plugins."loam@loam"]\nenabled = true\n');
      const cache = join(fixture.home, '.codex', 'plugins', 'cache', 'loam', 'loam', PACKAGE_VERSION);
      await mkdir(join(cache, 'hooks'), { recursive: true });
      await writeFile(join(cache, 'hooks', 'hooks.json'), JSON.stringify({ hooks: { Stop: [{}] } }));
    }
    return { code: 0, stdout: '', stderr: '' };
  };

  const code = await runSetup(parseArgs(['install', '--yes']), {
    ...fixture,
    packageRoot,
    runner,
    output: outputCapture().output,
  });

  assert.equal(code, 0);
  assert.deepEqual(calls, [
    ['plugin', 'marketplace', 'add', 'scchearn/loam'],
    ['plugin', 'add', 'loam@loam'],
  ]);
  await assert.rejects(() => readFile(join(fixture.home, '.codex', 'hooks.json')), { code: 'ENOENT' });
});

test('partial marketplace failure keeps successful installs and removes legacy hooks', async () => {
  const fixture = await baseFixture();
  const globalRoot = join(fixture.home, '.agents', 'loam');
  const oldRoot = join(globalRoot, 'plugins', 'old');
  await mkdir(join(fixture.home, '.claude'), { recursive: true });
  await mkdir(join(fixture.home, '.codex'), { recursive: true });
  await writeFile(join(fixture.home, '.claude', 'settings.json'), JSON.stringify({
    hooks: { SessionStart: [{ hooks: [{ type: 'command', command: `node ${JSON.stringify(join(oldRoot, 'claude-session-start.mjs'))}` }] }] },
  }));
  await writeFile(join(fixture.home, '.codex', 'hooks.json'), JSON.stringify({
    hooks: { Stop: [{ hooks: [{ type: 'command', command: `node ${JSON.stringify(join(oldRoot, 'codex-stop.mjs'))}` }] }] },
  }));
  const runner = async (request) => {
    if (request.command === 'claude') {
      return request.args.includes('install')
        ? { code: 1, stdout: '', stderr: 'claude install failed' }
        : { code: 0, stdout: '', stderr: '' };
    }
    if (request.command === 'codex') {
      if (request.args[1] === 'add') {
        await writeFile(join(fixture.home, '.codex', 'config.toml'), '[plugins."loam@loam"]\nenabled = true\n');
        const cache = join(fixture.home, '.codex', 'plugins', 'cache', 'loam', 'loam', PACKAGE_VERSION);
        await mkdir(join(cache, 'hooks'), { recursive: true });
        await writeFile(join(cache, 'hooks', 'hooks.json'), JSON.stringify({ hooks: { Stop: [{}] } }));
      }
      return { code: 0, stdout: '', stderr: '' };
    }
    return fixture.runner(request);
  };

  const code = await runSetup(parseArgs(['install', '--yes']), {
    ...fixture,
    packageRoot,
    runner,
    output: outputCapture().output,
  });

  assert.equal(code, 1);
  const claude = JSON.parse(await readFile(join(fixture.home, '.claude', 'settings.json'), 'utf8'));
  const codex = JSON.parse(await readFile(join(fixture.home, '.codex', 'hooks.json'), 'utf8'));
  assert.equal(claude.hooks.SessionStart.length, 0);
  assert.equal(codex.hooks.Stop.flatMap((entry) => entry.hooks || []).length, 0);
  await assert.doesNotReject(() => readFile(join(fixture.home, '.codex', 'config.toml')));
});

test('dry-run is valid and byte-stable without creating roots, backups, or invoking mutators', async () => {
  const fixture = await baseFixture();
  const capture = outputCapture();
  const code = await runSetup(parseArgs(['install', '--dry-run']), {
    ...fixture,
    packageRoot,
    runner: async () => { throw new Error('dry-run invoked Skills CLI'); },
    output: capture.output,
    errorOutput: capture.output,
  });

  assert.equal(code, 0);
  assert.match(capture.text(), /Dry run|dry-run/i);
  await assert.rejects(() => readdir(join(fixture.home, '.agents')));
});

test('update dry-run is valid without mutation or confirmation', async () => {
  const fixture = await baseFixture();
  await runSetup(parseArgs(['install', '--yes']), { ...fixture, packageRoot, output: outputCapture().output });
  const metadataPath = join(fixture.home, '.agents', 'loam', 'install.json');
  const before = await readFile(metadataPath, 'utf8');
  const capture = outputCapture();
  const code = await runSetup(parseArgs(['update', '--dry-run']), {
    ...fixture,
    packageRoot,
    runner: async () => { throw new Error('update dry-run invoked Skills CLI'); },
    confirm: async () => { throw new Error('update dry-run prompted for confirmation'); },
    output: capture.output,
    errorOutput: capture.output,
  });

  assert.equal(code, 0, capture.text());
  assert.match(capture.text(), /Loam Update \(dry-run\)/);
  // Dry run mutates nothing: the install metadata is byte-identical afterward.
  assert.equal(await readFile(metadataPath, 'utf8'), before);
});

test('update refuses on a machine with no install and points to install', async () => {
  const fixture = await baseFixture();
  const capture = outputCapture();
  const code = await runSetup(parseArgs(['update']), {
    ...fixture,
    packageRoot,
    runner: async () => { throw new Error('update with no install invoked Skills CLI'); },
    output: capture.output,
    errorOutput: capture.output,
  });

  assert.equal(code, 1);
  assert.match(capture.text(), /No Loam installation found/);
  assert.match(capture.text(), /install` first/);
  await assert.rejects(() => readdir(join(fixture.home, '.agents')));
});

test('Skills CLI failure prevents readiness and install metadata publication', async () => {
  const fixture = await baseFixture();
  const capture = outputCapture();
  const code = await runSetup(parseArgs(['install', '--yes']), {
    ...fixture,
    packageRoot,
    runner: async (request) => {
      if (request.args.includes('list')) return { code: 0, stdout: JSON.stringify({ skills: [] }), stderr: '' };
      return { code: 1, stdout: '', stderr: 'skills unavailable' };
    },
    output: capture.output,
    errorOutput: capture.output,
  });

  assert.equal(code, 1);
  await assert.rejects(() => readFile(join(fixture.home, '.agents', 'loam', 'install.json')));
});

test('closed non-interactive setup cancels before mutation without --yes', async () => {
  const fixture = await baseFixture();
  const capture = outputCapture();
  const code = await runSetup(parseArgs(['install']), {
    ...fixture,
    packageRoot,
    confirm: async () => false,
    output: capture.output,
    errorOutput: capture.output,
  });

  assert.equal(code, 130);
  await assert.rejects(() => readdir(join(fixture.home, '.agents')));
});

test('managed harness failure prevents the setup transaction from claiming readiness', async () => {
  const fixture = await baseFixture();
  await mkdir(join(fixture.home, '.claude'), { recursive: true });
  await writeFile(join(fixture.home, '.claude', 'settings.json'), JSON.stringify({ managed: true }));
  const capture = outputCapture();
  const code = await runSetup(parseArgs(['install', '--yes']), {
    ...fixture,
    packageRoot,
    output: capture.output,
    errorOutput: capture.output,
  });

  assert.equal(code, 1);
  assert.match(capture.text(), /Harness integration is incomplete/);
  await assert.rejects(() => readFile(join(fixture.home, '.agents', 'loam', 'install.json')));
});

test('marketplace failure cannot mask policy-owned legacy hook cleanup', async () => {
  const fixture = await baseFixture();
  await mkdir(join(fixture.home, '.claude'), { recursive: true });
  await writeFile(join(fixture.home, '.claude', 'settings.json'), JSON.stringify({ managed: true }));
  const runner = async (request) => request.command === 'claude'
    ? { code: 1, stdout: '', stderr: 'plugin failed' }
    : fixture.runner(request);

  const code = await runSetup(parseArgs(['install', '--yes']), {
    ...fixture,
    packageRoot,
    runner,
    output: outputCapture().output,
  });

  assert.equal(code, 1);
  await assert.rejects(() => readFile(join(fixture.home, '.agents', 'loam', 'install.json')), { code: 'ENOENT' });
});

test('migration failure preserves the global installation without publishing metadata', async () => {
  const fixture = await baseFixture();
  const projectSkill = join(fixture.workspace, '.agents', 'skills', 'loam-using');
  await mkdir(projectSkill, { recursive: true });
  await mkdir(join(fixture.workspace, '.agents', 'loam'), { recursive: true });
  await writeFile(join(projectSkill, 'SKILL.md'), '# legacy\n');
  const full = await fullList();
  const runner = async (request) => {
    if (request.args.includes('list') && request.args.includes('--global')) return { code: 0, stdout: JSON.stringify(full), stderr: '' };
    if (request.args.includes('list')) return { code: 0, stdout: JSON.stringify({ skills: [{ name: 'loam::using', path: projectSkill }] }), stderr: '' };
    if (request.args.includes('add')) {
      const skillsRoot = join(fixture.home, '.agents', 'skills', 'loam-using');
      await mkdir(join(skillsRoot, 'scripts'), { recursive: true });
      await writeFile(join(skillsRoot, 'scripts', 'CLI_VERSION'), '0.9.1\n');
      await writeFile(join(skillsRoot, 'SKILL.md'), '# using\n');
      return { code: 0, stdout: '', stderr: '' };
    }
    return { code: 1, stdout: '', stderr: 'project remove failed' };
  };
  const code = await runSetup(parseArgs(['install', '--yes']), { ...fixture, packageRoot, runner, output: outputCapture().output });

  assert.equal(code, 1);
  await assert.rejects(() => readFile(join(fixture.home, '.agents', 'loam', 'install.json')));
  await assert.doesNotReject(() => readFile(join(projectSkill, 'SKILL.md')));
});

test('interrupted runtime smoke cleans staging and publishes no metadata', async () => {
  const fixture = await baseFixture();
  const code = await runSetup(parseArgs(['install', '--yes']), {
    ...fixture,
    packageRoot,
    smokeRunner: async () => ({ code: 1, stdout: '', stderr: 'controlled smoke failure' }),
    output: outputCapture().output,
  });

  assert.equal(code, 1);
  const globalRoot = join(fixture.home, '.agents', 'loam');
  await assert.rejects(() => readFile(join(globalRoot, 'install.json')));
  const entries = await readdir(join(globalRoot, 'staging'));
  assert.deepEqual(entries, []);
});

test('final verification failure restores the previous install metadata', async () => {
  const fixture = await baseFixture();
  const globalRoot = join(fixture.home, '.agents', 'loam');
  const previous = '{"schema_version":1,"sentinel":"previous"}\n';
  await mkdir(globalRoot, { recursive: true });
  await writeFile(join(globalRoot, 'install.json'), previous);

  const code = await runSetup(parseArgs(['install', '--yes']), {
    ...fixture,
    packageRoot,
    finalVerify: async () => ({ ready: false, category: 'controlled-final-failure' }),
    output: outputCapture().output,
  });

  assert.equal(code, 1);
  assert.equal(await readFile(join(globalRoot, 'install.json'), 'utf8'), previous);
});

test('candidate metadata remains inactive during the activation boundary', async () => {
  const fixture = await baseFixture();
  let observed;
  const code = await runSetup(parseArgs(['install', '--yes']), {
    ...fixture,
    packageRoot,
    beforeActivate: async ({ metadataPath, integrationPath }) => {
      observed = { metadataPath, integrationPath };
      await assert.rejects(() => readFile(metadataPath), { code: 'ENOENT' });
      throw new Error('controlled interruption');
    },
    output: outputCapture().output,
  });

  assert.equal(code, 1);
  assert.ok(observed);
  await assert.rejects(() => readFile(observed.metadataPath), { code: 'ENOENT' });
  await assert.rejects(() => readFile(observed.integrationPath), { code: 'ENOENT' });
});

test('failed later setup stages preserve the active integration and metadata', async () => {
  const fixture = await baseFixture();
  await runSetup(parseArgs(['install', '--yes']), { ...fixture, packageRoot, output: outputCapture().output });
  const globalRoot = join(fixture.home, '.agents', 'loam');
  const metadataPath = join(globalRoot, 'install.json');
  const previous = await readFile(metadataPath, 'utf8');
  const previousMetadata = JSON.parse(previous);
  await writeFile(previousMetadata.integration_path, 'previous integration');
  await writeFile(runtimePath(globalRoot, '0.9.1', target), 'tampered runtime');

  const code = await runSetup(parseArgs(['install', '--yes']), {
    ...fixture,
    packageRoot,
    finalVerify: async () => ({ ready: false, category: 'controlled-later-failure' }),
    output: outputCapture().output,
  });

  assert.equal(code, 1);
  assert.equal(await readFile(metadataPath, 'utf8'), previous);
  assert.equal(await readFile(previousMetadata.integration_path, 'utf8'), 'previous integration');
});

async function readyHarnessFixture() {
  const fixture = await baseFixture();
  await mkdir(join(fixture.home, '.config', 'opencode'), { recursive: true });
  await mkdir(join(fixture.home, '.claude'), { recursive: true });
  await mkdir(join(fixture.home, '.cursor'), { recursive: true });
  const runner = async (request) => {
    if (request.command !== 'claude') return fixture.runner(request);
    if (request.args.includes('install')) {
      const cache = join(fixture.home, '.claude', 'plugins', 'cache', 'loam', 'loam', PACKAGE_VERSION);
      await mkdir(join(cache, 'hooks'), { recursive: true });
      await writeFile(join(cache, 'hooks', 'hooks.json'), JSON.stringify({ hooks: { Stop: [{}] } }));
      await writeFile(join(fixture.home, '.claude', 'settings.json'), JSON.stringify({ enabledPlugins: { 'loam@loam': true } }));
      await writeFile(join(fixture.home, '.claude', 'plugins', 'installed_plugins.json'), JSON.stringify({
        version: 2,
        plugins: { 'loam@loam': [{ scope: 'user', installPath: cache, version: PACKAGE_VERSION }] },
      }));
    }
    return { code: 0, stdout: '', stderr: '' };
  };
  await runSetup(parseArgs(['install', '--yes']), { ...fixture, packageRoot, runner, output: outputCapture().output });
  const discovery = await discover({
    home: fixture.home,
    workspace: fixture.workspace,
    packageRoot,
    runner: fixture.runner,
  });
  return { fixture, discovery };
}

test('harness readiness ignores hook paths outside the setup-owned root', async () => {
  const { fixture, discovery } = await readyHarnessFixture();
  const settingsPath = join(fixture.home, '.claude', 'settings.json');
  const settings = JSON.parse(await readFile(settingsPath, 'utf8'));
  settings.hooks.SessionStart = [{ hooks: [{ type: 'command', command: 'node "/old/claude-session-start.mjs"' }] }];
  await writeFile(settingsPath, JSON.stringify(settings));

  let result = await verifyInstallation({ discovery, packageRoot, runtimeRunner: fixture.smokeRunner });
  assert.equal(result.ready, true);
  assert.equal(result.harnesses.claude.ready, true);

  settings.hooks.SessionStart = [{ hooks: [{ type: 'command', command: 'node unrelated-loam-hook' }] }];
  await writeFile(settingsPath, JSON.stringify(settings));
  result = await verifyInstallation({ discovery, packageRoot, runtimeRunner: fixture.smokeRunner });
  assert.equal(result.ready, true);
  assert.equal(result.harnesses.claude.ready, true);
});

test('harness readiness rejects duplicate setup-owned registrations', async () => {
  const { fixture, discovery } = await readyHarnessFixture();
  const metadata = JSON.parse(await readFile(join(fixture.home, '.agents', 'loam', 'install.json'), 'utf8'));
  const settingsPath = join(fixture.home, '.claude', 'settings.json');
  const settings = JSON.parse(await readFile(settingsPath, 'utf8'));
  const command = `node ${JSON.stringify(join(metadata.adapter_root, 'claude-stop.mjs'))}`;
  settings.hooks.Stop = [{ hooks: [{ type: 'command', command }, { type: 'command', command }] }];
  await writeFile(settingsPath, JSON.stringify(settings));

  let result = await verifyInstallation({ discovery, packageRoot, runtimeRunner: fixture.smokeRunner });
  assert.equal(result.ready, false);
  assert.equal(result.harnesses.claude.ready, false);

});

test('failed post-harness setup restores every active harness mutation', async () => {
  const { fixture } = await readyHarnessFixture();
  const globalRoot = join(fixture.home, '.agents', 'loam');
  const metadataPath = join(globalRoot, 'install.json');
  const metadata = JSON.parse(await readFile(metadataPath, 'utf8'));
  const adapterRoot = metadata.adapter_root || join(globalRoot, 'plugins', metadata.plugin_version);
  const files = [
    metadataPath,
    metadata.integration_path,
    join(fixture.home, '.config', 'opencode', 'plugins', 'loam.js'),
    join(adapterRoot, 'opencode.mjs'),
    join(adapterRoot, 'claude-stop.mjs'),
    join(fixture.home, '.claude', 'settings.json'),
    join(fixture.home, '.cursor', 'hooks.json'),
  ];
  await writeFile(files[2], 'previous OpenCode adapter');
  await writeFile(files[3], 'previous OpenCode asset');
  await writeFile(files[4], 'previous Claude asset');
  await writeFile(files[5], '{"unrelated":true}');
  await writeFile(files[6], '{"unrelated":true}');
  const before = new Map(await Promise.all(files.map(async (file) => [file, await readFile(file, 'utf8')])));
  const beforePluginEntries = await readdir(join(globalRoot, 'plugins'));
  const beforeClaudeEntries = await readdir(join(fixture.home, '.claude'));
  const beforeCursorEntries = await readdir(join(fixture.home, '.cursor'));

  const code = await runSetup(parseArgs(['install', '--yes']), {
    ...fixture,
    packageRoot,
    finalVerify: async () => ({ ready: false, category: 'controlled-post-harness-failure' }),
    output: outputCapture().output,
  });

  assert.equal(code, 1);
  for (const [file, contents] of before) assert.equal(await readFile(file, 'utf8'), contents, file);
  assert.deepEqual(await readdir(join(globalRoot, 'plugins')), beforePluginEntries);
  assert.deepEqual(await readdir(join(fixture.home, '.claude')), beforeClaudeEntries);
  assert.deepEqual(await readdir(join(fixture.home, '.cursor')), beforeCursorEntries);
});

test('failed fresh harness setup removes originally absent harness files', async () => {
  const fixture = await baseFixture();
  await mkdir(join(fixture.home, '.config', 'opencode'), { recursive: true });
  await mkdir(join(fixture.home, '.claude'), { recursive: true });
  await mkdir(join(fixture.home, '.cursor'), { recursive: true });
  const code = await runSetup(parseArgs(['install', '--yes']), {
    ...fixture,
    packageRoot,
    finalVerify: async () => ({ ready: false, category: 'controlled-fresh-harness-failure' }),
    output: outputCapture().output,
  });

  assert.equal(code, 1);
  await assert.rejects(() => readFile(join(fixture.home, '.config', 'opencode', 'plugins', 'loam.js')), { code: 'ENOENT' });
  await assert.rejects(() => readFile(join(fixture.home, '.claude', 'settings.json')), { code: 'ENOENT' });
  await assert.rejects(() => readFile(join(fixture.home, '.cursor', 'hooks.json')), { code: 'ENOENT' });
});

// --- Verb-boundary contract tests: #100 (service refresh) and #97 (safe update)

// A recording federation runner that models the runtime's file-based definition:
// install writes the platform unit under the global root, uninstall removes it,
// status reflects the enabled flag. Lets update's #100 refresh be asserted.
function fedRunner(globalRoot, platform, { active = false } = {}) {
  const definitionPath = federationDefinitionPath({ globalRoot, platform });
  const state = { active };
  const calls = [];
  const runner = async (request) => {
    const verb = request.args[2];
    calls.push({ verb, runtimePath: request.runtimePath });
    if (verb === 'install' && definitionPath) {
      await mkdir(join(globalRoot, definitionPath.includes('launchagents') ? 'launchagents' : 'systemd'), { recursive: true });
      await writeFile(definitionPath, 'unit');
      return { code: 0, stdout: '', stderr: '' };
    }
    if (verb === 'enable') { state.active = true; return { code: 0, stdout: '', stderr: '' }; }
    if (verb === 'status') return { code: state.active ? 0 : 1, stdout: '', stderr: '' };
    return { code: 0, stdout: '', stderr: '' };
  };
  return { runner, calls, definitionPath, state };
}

test('#100: update refreshes an existing service definition against the committed runtime', async () => {
  const fixture = await baseFixture();
  await runSetup(parseArgs(['install', '--yes']), { ...fixture, packageRoot, output: outputCapture().output });
  const globalRoot = join(fixture.home, '.agents', 'loam');
  const definitionPath = federationDefinitionPath({ globalRoot, platform: process.platform });
  if (!definitionPath) return; // win32: no file-based definition to refresh.

  // Simulate federation having been enabled (an active definition exists).
  const fed = fedRunner(globalRoot, process.platform, { active: true });
  await mkdir(join(globalRoot, definitionPath.includes('launchagents') ? 'launchagents' : 'systemd'), { recursive: true });
  await writeFile(definitionPath, 'stale');

  const install = JSON.parse(await readFile(join(globalRoot, 'install.json'), 'utf8'));
  const capture = outputCapture();
  const code = await runSetup(parseArgs(['update']), {
    ...fixture,
    packageRoot,
    federationRunner: fed.runner,
    output: capture.output,
    errorOutput: capture.output,
  });
  assert.equal(code, 0, capture.text());
  // The definition was re-rendered (status -> install) and re-enabled (was active),
  // every verb targeting the committed runtime path.
  const verbs = fed.calls.map((c) => c.verb);
  assert.ok(verbs.includes('install'), 'update must re-render the definition');
  assert.ok(verbs.includes('enable'), 'an active service is re-enabled after the refresh');
  for (const call of fed.calls) assert.equal(call.runtimePath, install.runtime_path);
  await assert.doesNotReject(() => readFile(definitionPath));
});

test('#100: update never CREATES federation state on a machine that never enabled it', async () => {
  const fixture = await baseFixture();
  await runSetup(parseArgs(['install', '--yes']), { ...fixture, packageRoot, output: outputCapture().output });
  const globalRoot = join(fixture.home, '.agents', 'loam');
  const fed = fedRunner(globalRoot, process.platform);

  const code = await runSetup(parseArgs(['update']), {
    ...fixture,
    packageRoot,
    federationRunner: fed.runner,
    output: outputCapture().output,
  });
  assert.equal(code, 0);
  assert.deepEqual(fed.calls, [], 'no definition present → update leaves federation entirely alone');
});

test('#97: a failed final verification names the failing check instead of a bare message', async () => {
  const fixture = await baseFixture();
  await runSetup(parseArgs(['install', '--yes']), { ...fixture, packageRoot, output: outputCapture().output });
  const capture = outputCapture();
  const code = await runSetup(parseArgs(['update']), {
    ...fixture,
    packageRoot,
    finalVerify: async () => ({
      ready: false,
      install: { plugin_version: PACKAGE_VERSION },
      skills: { ready: false, category: 'skills_missing' },
      runtime: { ready: true },
      harnesses: {},
      ingestExclusions: { ready: true },
      migration: { ready: true },
    }),
    output: capture.output,
    errorOutput: capture.output,
  });
  assert.equal(code, 1);
  assert.match(capture.text(), /Final readiness verification failed: .*skills \(skills_missing\)/);
});

test('#97: a failed update does not destroy the install root, registry, or metadata', async () => {
  const fixture = await baseFixture();
  await runSetup(parseArgs(['install', '--yes']), { ...fixture, packageRoot, output: outputCapture().output });
  const globalRoot = join(fixture.home, '.agents', 'loam');
  const metadataPath = join(globalRoot, 'install.json');
  const registryPath = join(globalRoot, 'loam.sqlite3');
  await writeFile(registryPath, 'enrollment row + federation registry');
  const before = await readFile(metadataPath, 'utf8');

  const code = await runSetup(parseArgs(['update']), {
    ...fixture,
    packageRoot,
    finalVerify: async () => ({ ready: false, category: 'controlled-update-failure' }),
    output: outputCapture().output,
  });

  assert.equal(code, 1);
  // Rollback blast radius is contained: the install root, the registry, and the
  // previous metadata all survive a failed update (#97).
  await assert.doesNotReject(() => readFile(join(globalRoot, 'install.json')));
  assert.equal(await readFile(registryPath, 'utf8'), 'enrollment row + federation registry');
  assert.equal(await readFile(metadataPath, 'utf8'), before);
});

test('setup harness reconciliation delegates without moving versions (touches no versions)', async () => {
  const { runConfigure } = await import('../setup/configure.mjs');
  const fixture = await baseFixture();
  await runSetup(parseArgs(['install', '--yes']), { ...fixture, packageRoot, output: outputCapture().output });
  const metadataPath = join(fixture.home, '.agents', 'loam', 'install.json');
  const before = JSON.parse(await readFile(metadataPath, 'utf8'));

  const capture = outputCapture();
  const code = await runConfigure(
    { command: 'setup', federation: null, integrations: [], dryRun: false, yes: false, purge: false },
    {
      ...fixture,
      packageRoot,
      select: async () => ({ harnesses: [] }),
      output: capture.output,
      errorOutput: capture.output,
    },
  );
  assert.equal(code, 0, capture.text());
  assert.match(capture.text(), /Harness selection reconciled/);
  const after = JSON.parse(await readFile(metadataPath, 'utf8'));
  // The configurator never bumps versions — the whole point of the verb split.
  assert.equal(after.plugin_version, before.plugin_version);
  assert.equal(after.runtime_version, before.runtime_version);
});

test('setup harness reconciliation refuses a version mismatch and points to update', async () => {
  const { runConfigure } = await import('../setup/configure.mjs');
  const fixture = await baseFixture();
  await runSetup(parseArgs(['install', '--yes']), { ...fixture, packageRoot, output: outputCapture().output });
  const metadataPath = join(fixture.home, '.agents', 'loam', 'install.json');
  const install = JSON.parse(await readFile(metadataPath, 'utf8'));
  // Simulate a NEWER npx package touching an install pinned at an older version.
  await writeFile(metadataPath, JSON.stringify({ ...install, plugin_version: '0.0.1' }));

  const capture = outputCapture();
  const code = await runConfigure(
    { command: 'setup', federation: null, integrations: [], dryRun: false, yes: false, purge: false },
    {
      ...fixture,
      packageRoot,
      // A runner that fails loudly proves the transaction is never entered.
      runner: async () => { throw new Error('version mismatch must refuse before staging'); },
      select: async () => ({ harnesses: [] }),
      output: capture.output,
      errorOutput: capture.output,
    },
  );
  assert.equal(code, 1);
  assert.match(capture.text(), /never moves versions/);
  assert.match(capture.text(), /update` first/);
  // The install version pin is untouched by the refusal.
  assert.equal(JSON.parse(await readFile(metadataPath, 'utf8')).plugin_version, '0.0.1');
});
