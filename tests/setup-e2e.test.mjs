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
import { discover } from '../setup/discovery.mjs';
import { verifyInstallation } from '../setup/verify.mjs';
import { detectTarget, runtimePath } from '../setup/target.mjs';

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

test('clean --yes setup completes and publishes verified install metadata', async () => {
  const fixture = await baseFixture();
  const capture = outputCapture();
  const code = await runSetup(parseArgs(['setup', '--yes']), {
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
  await runSetup(parseArgs(['setup', '--yes']), { ...fixture, packageRoot, output: outputCapture().output });
  const capture = outputCapture();
  const code = await runSetup(parseArgs(['setup', '--yes']), {
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

test('dry-run is valid and byte-stable without creating roots, backups, or invoking mutators', async () => {
  const fixture = await baseFixture();
  const capture = outputCapture();
  const code = await runSetup(parseArgs(['setup', '--dry-run']), {
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

test('Skills CLI failure prevents readiness and install metadata publication', async () => {
  const fixture = await baseFixture();
  const capture = outputCapture();
  const code = await runSetup(parseArgs(['setup', '--yes']), {
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
  const code = await runSetup(parseArgs(['setup']), {
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
  const code = await runSetup(parseArgs(['setup', '--yes']), {
    ...fixture,
    packageRoot,
    output: capture.output,
    errorOutput: capture.output,
  });

  assert.equal(code, 1);
  assert.match(capture.text(), /Harness integration is incomplete/);
  await assert.rejects(() => readFile(join(fixture.home, '.agents', 'loam', 'install.json')));
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
  const code = await runSetup(parseArgs(['setup', '--yes']), { ...fixture, packageRoot, runner, output: outputCapture().output });

  assert.equal(code, 1);
  await assert.rejects(() => readFile(join(fixture.home, '.agents', 'loam', 'install.json')));
  await assert.doesNotReject(() => readFile(join(projectSkill, 'SKILL.md')));
});

test('interrupted runtime smoke cleans staging and publishes no metadata', async () => {
  const fixture = await baseFixture();
  const code = await runSetup(parseArgs(['setup', '--yes']), {
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

  const code = await runSetup(parseArgs(['setup', '--yes']), {
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
  const code = await runSetup(parseArgs(['setup', '--yes']), {
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
  await runSetup(parseArgs(['setup', '--yes']), { ...fixture, packageRoot, output: outputCapture().output });
  const globalRoot = join(fixture.home, '.agents', 'loam');
  const metadataPath = join(globalRoot, 'install.json');
  const previous = await readFile(metadataPath, 'utf8');
  const previousMetadata = JSON.parse(previous);
  await writeFile(previousMetadata.integration_path, 'previous integration');
  await writeFile(runtimePath(globalRoot, '0.9.1', target), 'tampered runtime');

  const code = await runSetup(parseArgs(['setup', '--yes']), {
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
  await runSetup(parseArgs(['setup', '--yes']), { ...fixture, packageRoot, output: outputCapture().output });
  const discovery = await discover({
    home: fixture.home,
    workspace: fixture.workspace,
    packageRoot,
    runner: fixture.runner,
  });
  return { fixture, discovery };
}

test('harness readiness rejects stale owned paths and unrelated Loam strings', async () => {
  const { fixture, discovery } = await readyHarnessFixture();
  const settingsPath = join(fixture.home, '.claude', 'settings.json');
  const settings = JSON.parse(await readFile(settingsPath, 'utf8'));
  settings.hooks.SessionStart = [{ hooks: [{ type: 'command', command: 'node "/old/claude-session-start.mjs"' }] }];
  await writeFile(settingsPath, JSON.stringify(settings));

  let result = await verifyInstallation({ discovery, packageRoot, runtimeRunner: fixture.smokeRunner });
  assert.equal(result.ready, false);
  assert.equal(result.harnesses.claude.ready, false);

  settings.hooks.SessionStart = [{ hooks: [{ type: 'command', command: 'node unrelated-loam-hook' }] }];
  await writeFile(settingsPath, JSON.stringify(settings));
  result = await verifyInstallation({ discovery, packageRoot, runtimeRunner: fixture.smokeRunner });
  assert.equal(result.ready, false);
  assert.equal(result.harnesses.claude.ready, false);
});

test('harness readiness rejects duplicate registrations and malformed adapters', async () => {
  const { fixture, discovery } = await readyHarnessFixture();
  const metadata = JSON.parse(await readFile(join(fixture.home, '.agents', 'loam', 'install.json'), 'utf8'));
  const assetPath = join(metadata.adapter_root, 'claude-session-start.mjs');
  const settingsPath = join(fixture.home, '.claude', 'settings.json');
  const settings = JSON.parse(await readFile(settingsPath, 'utf8'));
  const command = `node ${JSON.stringify(assetPath)}`;
  settings.hooks.SessionStart = [{ hooks: [{ type: 'command', command }, { type: 'command', command }] }];
  await writeFile(settingsPath, JSON.stringify(settings));

  let result = await verifyInstallation({ discovery, packageRoot, runtimeRunner: fixture.smokeRunner });
  assert.equal(result.ready, false);
  assert.equal(result.harnesses.claude.ready, false);

  await writeFile(assetPath, 'export default {};\n');
  settings.hooks.SessionStart = [{ hooks: [{ type: 'command', command }] }];
  await writeFile(settingsPath, JSON.stringify(settings));
  result = await verifyInstallation({ discovery, packageRoot, runtimeRunner: fixture.smokeRunner });
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
    join(fixture.home, '.config', 'opencode', 'plugins', 'loam.mjs'),
    join(adapterRoot, 'opencode.mjs'),
    join(adapterRoot, 'claude-session-start.mjs'),
    join(adapterRoot, 'cursor-session-start.mjs'),
    join(fixture.home, '.claude', 'settings.json'),
    join(fixture.home, '.cursor', 'hooks.json'),
  ];
  await writeFile(files[2], 'previous OpenCode adapter');
  await writeFile(files[3], 'previous OpenCode asset');
  await writeFile(files[4], 'previous Claude asset');
  await writeFile(files[5], 'previous Cursor asset');
  await writeFile(files[6], '{"unrelated":true}');
  await writeFile(files[7], '{"unrelated":true}');
  const before = new Map(await Promise.all(files.map(async (file) => [file, await readFile(file, 'utf8')])));
  const beforePluginEntries = await readdir(join(globalRoot, 'plugins'));
  const beforeClaudeEntries = await readdir(join(fixture.home, '.claude'));
  const beforeCursorEntries = await readdir(join(fixture.home, '.cursor'));

  const code = await runSetup(parseArgs(['setup', '--yes']), {
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
  const code = await runSetup(parseArgs(['setup', '--yes']), {
    ...fixture,
    packageRoot,
    finalVerify: async () => ({ ready: false, category: 'controlled-fresh-harness-failure' }),
    output: outputCapture().output,
  });

  assert.equal(code, 1);
  await assert.rejects(() => readFile(join(fixture.home, '.config', 'opencode', 'plugins', 'loam.mjs')), { code: 'ENOENT' });
  await assert.rejects(() => readFile(join(fixture.home, '.claude', 'settings.json')), { code: 'ENOENT' });
  await assert.rejects(() => readFile(join(fixture.home, '.cursor', 'hooks.json')), { code: 'ENOENT' });
});
