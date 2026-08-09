import assert from 'node:assert/strict';
import { cp, mkdir, mkdtemp, readFile, stat, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { test } from 'node:test';

import { detectHarnesses, installHarnesses } from '../setup/harnesses.mjs';

const packageRoot = fileURLToPath(new URL('..', import.meta.url));

// The files a harness actually loads or executes at session time. Everything
// else in the repo is setup-time or test-time and is not the runtime path.
const RUNTIME_PATH_FILES = [
  '.opencode/plugins/loam.js',
  'adapters/opencode.mjs',
  'plugins/loam-adapter/adapter.mjs',
  'plugins/loam-adapter/hooks/stop.mjs',
];

// Each pattern is one structural claim the slice makes about the runtime path.
// The scanner is only worth trusting because the positive-control test below
// plants each of these deliberately and proves the scan catches it.
const FORBIDDEN = [
  ['retired-node-integration', /integration\/(loam|context)\.mjs|formatContext|runIntegration/],
  ['broker', /rumqttc|MqttTransport|MqttSession|DeliveryProcessor|\bmqtts?:\/\//],
  ['connector-ipc', /node:net\b|UnixStream|createConnection|federation\.snapshot/],
  ['registry-or-dedupe', /record_response|causation_id|ChannelRegistry|SnapshotStore/],
  ['collaboration-state', /sender_scope|handler-allowlist|publication:\s*verified/],
  ['service-manager', /systemctl|launchctl|\bsc\.exe|LaunchAgents|systemd/],
];

function scanSource(source) {
  return FORBIDDEN.filter(([, pattern]) => pattern.test(source)).map(([name]) => name);
}

async function missing(path) {
  try {
    await stat(path);
    return false;
  } catch (error) {
    return error?.code === 'ENOENT';
  }
}

async function harnessFixture(prefix, { pluginVersion = '0.9.10' } = {}) {
  const home = await mkdtemp(join(tmpdir(), prefix));
  const globalRoot = join(home, '.agents', 'loam');
  const runtimePath = join(globalRoot, 'bin', '0.9.10', 'x86_64-unknown-linux-gnu', 'loam');
  await mkdir(join(home, '.config', 'opencode'), { recursive: true });
  await mkdir(join(home, '.cursor'), { recursive: true });
  await mkdir(join(home, '.claude'), { recursive: true });
  await mkdir(join(home, '.codex'), { recursive: true });
  await writeFile(join(home, '.claude', 'settings.json'), JSON.stringify({ enabledPlugins: { 'loam@loam': true } }));
  await writeFile(join(home, '.codex', 'config.toml'), '[plugins."loam@loam"]\nenabled = true\n');
  await writeFile(join(home, '.codex', 'hooks.json'), JSON.stringify({ hooks: {} }));

  const claudeCache = join(home, '.claude', 'plugins', 'cache', 'loam', 'loam', pluginVersion);
  const codexCache = join(home, '.codex', 'plugins', 'cache', 'loam', 'loam', pluginVersion);
  for (const cache of [claudeCache, codexCache]) {
    await mkdir(join(cache, 'hooks'), { recursive: true });
    await cp(join(packageRoot, 'plugins', 'loam-adapter', 'hooks', 'hooks.json'), join(cache, 'hooks', 'hooks.json'));
  }
  await writeFile(join(home, '.claude', 'plugins', 'installed_plugins.json'), JSON.stringify({
    version: 2,
    plugins: { 'loam@loam': [{ scope: 'user', installPath: claudeCache, version: pluginVersion }] },
  }));

  const detected = await detectHarnesses({ home, pluginVersion });
  const result = await installHarnesses({ home, globalRoot, pluginVersion, runtimePath, detected });
  return { home, globalRoot, runtimePath, claudeCache, codexCache, result };
}

function sessionEntries(hooksConfig, key) {
  const groups = Array.isArray(hooksConfig?.hooks?.[key]) ? hooksConfig.hooks[key] : [];
  return groups.flatMap((group) => (Array.isArray(group?.hooks) ? group.hooks : [group]));
}

test('every harness registration invokes the absolute native runtime, not a Node shim', async () => {
  const { home, runtimePath, claudeCache, codexCache, result } = await harnessFixture('loam-native-registration-');

  const cursor = JSON.parse(await readFile(join(home, '.cursor', 'hooks.json'), 'utf8'));
  const cursorOwned = sessionEntries(cursor, 'sessionStart').filter((entry) => entry.command === runtimePath);
  assert.equal(cursorOwned.length, 1);
  assert.deepEqual(cursorOwned[0].args, ['hook', 'cursor', '--event', 'sessionStart']);

  for (const [id, cache] of [['claude', claudeCache], ['codex', codexCache]]) {
    const hooks = JSON.parse(await readFile(join(cache, 'hooks', 'hooks.json'), 'utf8'));
    const start = sessionEntries(hooks, 'SessionStart');
    const refresh = sessionEntries(hooks, 'UserPromptSubmit');
    assert.equal(start.length, 1, `${id} SessionStart`);
    assert.equal(refresh.length, 1, `${id} UserPromptSubmit`);
    assert.equal(start[0].command, runtimePath);
    assert.deepEqual(start[0].args, ['hook', id, '--event', 'SessionStart']);
    assert.deepEqual(refresh[0].args, ['hook', id, '--event', 'UserPromptSubmit']);
    // Stop stays Node: it is the ingestion boundary, not the read path.
    assert.match(JSON.stringify(sessionEntries(hooks, 'Stop')), /stop\.mjs/);
  }

  const openCode = await readFile(join(home, '.config', 'opencode', 'plugins', 'loam.js'), 'utf8');
  assert.match(openCode, new RegExp(JSON.stringify(runtimePath).replaceAll('\\', '\\\\')));
  assert.doesNotMatch(openCode, /__LOAM_RUNTIME_PATH__/);
  assert.equal(result.opencode.state, 'ready');
  assert.equal(result.cursor.state, 'ready');
  assert.equal(result.claude.state, 'ready');
  assert.equal(result.codex.state, 'ready');
});

test('the rendered command tracks the staged runtime rather than a constant', async () => {
  // Positive control for the assertion above: a different staged runtime must
  // produce a different rendered command, so "contains the absolute path" is
  // not satisfied by any fixed string the renderer could have hardcoded.
  const first = await harnessFixture('loam-native-registration-a-');
  const second = await harnessFixture('loam-native-registration-b-');
  assert.notEqual(first.runtimePath, second.runtimePath);

  const read = async ({ home }) => JSON.parse(await readFile(join(home, '.cursor', 'hooks.json'), 'utf8'));
  const command = (config) => sessionEntries(config, 'sessionStart').map((entry) => entry.command);
  assert.deepEqual(command(await read(first)), [first.runtimePath]);
  assert.deepEqual(command(await read(second)), [second.runtimePath]);

  const openCode = async ({ home }) => readFile(join(home, '.config', 'opencode', 'plugins', 'loam.js'), 'utf8');
  assert.ok((await openCode(first)).includes(JSON.stringify(first.runtimePath)));
  assert.ok(!(await openCode(first)).includes(JSON.stringify(second.runtimePath)));
});

test('installation without a staged runtime refuses rather than writing a Node shim', async () => {
  const home = await mkdtemp(join(tmpdir(), 'loam-native-registration-missing-'));
  await mkdir(join(home, '.cursor'), { recursive: true });
  const detected = await detectHarnesses({ home });
  await assert.rejects(
    () => installHarnesses({
      home,
      globalRoot: join(home, '.agents', 'loam'),
      pluginVersion: '0.9.10',
      detected,
    }),
    /runtime path/,
  );
});

test('no runtime-path file reaches the retired Node integration, a broker, IPC, or the service manager', async () => {
  for (const relative of RUNTIME_PATH_FILES) {
    const source = await readFile(join(packageRoot, relative), 'utf8');
    assert.deepEqual(scanSource(source), [], `${relative} must stay a thin registration surface`);
  }
});

test('the structural scan catches a deliberately planted violation', async () => {
  // Positive control: without this, a scan that matched nothing would pass for
  // the wrong reason — a broken pattern reads exactly like a clean tree.
  const clean = await readFile(join(packageRoot, 'adapters', 'opencode.mjs'), 'utf8');
  const planted = [
    ['retired-node-integration', "import { formatContext } from '../integration/context.mjs';"],
    ['broker', 'const url = "mqtts://broker.example";'],
    ['connector-ipc', "import net from 'node:net';"],
    ['registry-or-dedupe', 'const key = causation_id;'],
    ['collaboration-state', 'const scope = { sender_scope: [] };'],
    ['service-manager', 'spawn("systemctl", ["--user", "start", "loam"]);'],
  ];
  for (const [expected, line] of planted) {
    assert.deepEqual(scanSource(`${clean}\n${line}\n`), [expected], `planted ${expected} must be caught`);
  }
  assert.deepEqual(scanSource(clean), []);
});

test('the shared Node session integration is gone from the tree', async () => {
  for (const relative of [
    'integration/context.mjs',
    'hooks/session-start.mjs',
    'hooks/hooks.json',
    'hooks/hooks-cursor.json',
    'adapters/claude-session-start.mjs',
    'adapters/cursor-session-start.mjs',
    'plugins/loam-adapter/hooks/session-start.mjs',
  ]) {
    assert.equal(await missing(join(packageRoot, relative)), true, `${relative} must be retired`);
  }

  const { runIntegration } = await import('../integration/loam.mjs');
  await assert.rejects(() => runIntegration(['hook', '--harness', 'claude', '--workspace', packageRoot]), /usage/);
});
