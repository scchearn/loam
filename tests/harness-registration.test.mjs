import assert from 'node:assert/strict';
import { cp, mkdir, mkdtemp, readFile, readdir, stat, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { test } from 'node:test';

import { detectHarnesses, installHarnesses } from '../setup/harnesses.mjs';
import { runtimeStorePath } from '../integration/ledger.mjs';

const packageRoot = fileURLToPath(new URL('..', import.meta.url));

// The files a harness actually loads or executes at session time. Everything
// else in the repo is setup-time or test-time and is not the runtime path.
const RUNTIME_PATH_FILES = [
  '.opencode/plugins/loam.js',
  'adapters/opencode.mjs',
  'plugins/loam-adapter/adapter.mjs',
  'plugins/loam-adapter/hooks/stop.mjs',
];

// ...and the subtree those reach *in the same process*: they resolve an
// integration root and then `import(new URL('<sibling>.mjs', root).href)`, and
// `integration/ingest.mjs` spawns `adapters/ingest-worker.mjs`. Scanned as whole
// directories rather than a hand-written closure — a maintained list would fall
// silently behind a new sibling and stop covering code that runs in the plugin.
const INGESTION_SUBTREE_DIRS = ['integration', 'adapters'];

// The live structural security guard. Each pattern is one class the in-process
// plugin/adapter code must NEVER reach: the retired Node integration, a raw
// broker connection, raw IPC, the registry/dedupe ledger, collaboration state,
// or the OS service manager. Calling the trusted runtime binary as a subprocess
// (the native hook wiring) is explicitly NOT a violation — the runtime owns the
// broker/IPC/service lifecycle behind its own authenticated boundary and the
// plugin only ever consumes its already-sanitized output. The scanner is only
// worth trusting because the positive-control tests below plant each pattern
// and prove the scan catches it.
const FORBIDDEN = [
  ['retired-node-integration', /integration\/(loam|context)\.mjs|formatContext|runIntegration/],
  ['broker', /rumqttc|MqttTransport|MqttSession|DeliveryProcessor|\bmqtts?:\/\//],
  // The connector is reached only through the native runtime's `federation
  // inject`/`hook` commands. `node:net` appears in the OpenCode adapter for
  // the localhost notify wake listener only (live-push T4): it never connects
  // to the connector socket and never opens the broker.
  ['connector-ipc', /UnixStream|createConnection|federation\.snapshot|createClient|connect\(\{/],
  ['registry-or-dedupe', /record_response|causation_id|ChannelRegistry|SnapshotStore/],
  ['collaboration-state', /sender_scope|handler-allowlist|publication:\s*verified/],
  ['service-manager', /systemctl|launchctl|\bsc\.exe|LaunchAgents|systemd/],
];

// The ingestion subtree is reached through computed dynamic imports, so it is
// scanned for the same raw surfaces — but NOT for the retired-integration
// pattern: `integration/loam.mjs` legitimately exports `runIntegration` as its
// status/ingest-status entry point, and the subtree is not the session-time
// runtime path.
const FORBIDDEN_SUBTREE = FORBIDDEN.filter(([name]) => name !== 'retired-node-integration');

function scanSource(source) {
  return FORBIDDEN.filter(([, pattern]) => pattern.test(source)).map(([name]) => name);
}

function scanSubtreeSource(source) {
  return FORBIDDEN_SUBTREE.filter(([, pattern]) => pattern.test(source)).map(([name]) => name);
}

async function ingestionSubtreeFiles() {
  const files = [];
  for (const dir of INGESTION_SUBTREE_DIRS) {
    for (const name of await readdir(join(packageRoot, dir))) {
      if (name.endsWith('.mjs')) files.push(`${dir}/${name}`);
    }
  }
  return files.sort();
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
  // The injected native path is the config-dir store binary, not <globalRoot>/bin.
  const configDir = join(home, 'config');
  const runtimePath = runtimeStorePath({ version: '0.9.10', target: 'x86_64-unknown-linux-gnu', root: configDir });
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
  return { home, globalRoot, configDir, runtimePath, claudeCache, codexCache, result };
}

function sessionEntries(hooksConfig, key) {
  const groups = Array.isArray(hooksConfig?.hooks?.[key]) ? hooksConfig.hooks[key] : [];
  return groups.flatMap((group) => (Array.isArray(group?.hooks) ? group.hooks : [group]));
}

test('claude and codex carry self-resolving hook shims; cursor invokes the runtime directly', async () => {
  const { home, configDir, runtimePath, claudeCache, codexCache, result } = await harnessFixture('loam-native-registration-');

  // The injected path points into the config-dir runtime store.
  assert.ok(
    runtimePath.startsWith(join(configDir, 'runtime') + '/') || runtimePath.startsWith(join(configDir, 'runtime') + '\\'),
    'injected runtime path is inside the config-dir store',
  );

  const cursor = JSON.parse(await readFile(join(home, '.cursor', 'hooks.json'), 'utf8'));
  const cursorOwned = sessionEntries(cursor, 'sessionStart').filter((entry) => entry.command === runtimePath);
  assert.equal(cursorOwned.length, 1);
  assert.deepEqual(cursorOwned[0].args, ['hook', 'cursor', '--event', 'sessionStart']);

  // #137: claude/codex load hooks from the marketplace SOURCE hooks.json, which
  // setup no longer rewrites — the installed cache copy is the shipped file
  // unchanged, carrying one self-resolving node shim per native read surface
  // (SessionStart baseline + UserPromptSubmit drain; #114).
  const NATIVE_SHIMS = [
    ['SessionStart', 'session-start.mjs'],
    ['UserPromptSubmit', 'user-prompt-submit.mjs'],
  ];
  for (const [id, cache] of [['claude', claudeCache], ['codex', codexCache]]) {
    const hooks = JSON.parse(await readFile(join(cache, 'hooks', 'hooks.json'), 'utf8'));
    for (const [event, file] of NATIVE_SHIMS) {
      const entries = sessionEntries(hooks, event);
      assert.equal(entries.length, 1, `${id} ${event}`);
      assert.equal(entries[0].command, `node "\${CLAUDE_PLUGIN_ROOT}/hooks/${file}"`);
      assert.equal(entries[0].args, undefined, `${id} ${event} shim carries no args`);
    }
    // SessionStart must also match a forked session (#137 doc-verify).
    assert.match(hooks.hooks.SessionStart[0].matcher, /(^|\|)fork(\||$)/);
    // Stop carries the ingestion boundary AND the wake long-poll (#114).
    const stopJson = JSON.stringify(sessionEntries(hooks, 'Stop'));
    assert.match(stopJson, /stop\.mjs/);
    assert.match(stopJson, /wake\.mjs/);
    assert.ok(Array.isArray(hooks.hooks.SubagentStart), `${id} keeps SubagentStart`);
    assert.ok(Array.isArray(hooks.hooks.SubagentStop), `${id} keeps SubagentStop`);
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
    ['connector-ipc', 'net.createConnection();'],
    ['registry-or-dedupe', 'const key = causation_id;'],
    ['collaboration-state', 'const scope = { sender_scope: [] };'],
    ['service-manager', 'spawn("systemctl", ["--user", "start", "loam"]);'],
  ];
  for (const [expected, line] of planted) {
    assert.deepEqual(scanSource(`${clean}\n${line}\n`), [expected], `planted ${expected} must be caught`);
  }
  assert.deepEqual(scanSource(clean), []);
});

test('the in-process ingestion subtree reaches no broker, IPC, or service manager either', async () => {
  const files = await ingestionSubtreeFiles();
  // The enumeration is part of the claim: an empty or wrong file list would
  // read exactly like a clean subtree.
  for (const expected of [
    'integration/ingest.mjs',
    'integration/loam.mjs',
    'adapters/ingest-worker.mjs',
    'adapters/opencode.mjs',
  ]) {
    assert.ok(files.includes(expected), `${expected} must be inside the scanned subtree`);
  }

  for (const relative of files) {
    const source = await readFile(join(packageRoot, relative), 'utf8');
    assert.deepEqual(scanSubtreeSource(source), [], `${relative} runs inside the plugin and must reach none of these`);
  }
});

test('the extended scan catches a violation planted in the ingestion subtree', async () => {
  // The subtree is reached through computed dynamic imports, so the risk is not
  // a bad pattern — it is a file that never gets handed to the scanner at all.
  const clean = await readFile(join(packageRoot, 'integration', 'ingest.mjs'), 'utf8');
  assert.deepEqual(scanSubtreeSource(clean), []);
  for (const [expected, line] of [
    ['broker', 'const url = "mqtts://broker.example";'],
    ['connector-ipc', 'net.createConnection();'],
    ['service-manager', 'spawn("systemctl", ["--user", "start", "loam"]);'],
    ['registry-or-dedupe', 'const key = causation_id;'],
    ['collaboration-state', 'const scope = { sender_scope: [] };'],
  ]) {
    assert.deepEqual(
      scanSubtreeSource(`${clean}\n${line}\n`),
      [expected],
      `planted ${expected} in the ingestion subtree must be caught`,
    );
  }
});

test('the shared Node session integration is gone from the tree', async () => {
  for (const relative of [
    'integration/context.mjs',
    'hooks/session-start.mjs',
    'hooks/hooks.json',
    'hooks/hooks-cursor.json',
    'adapters/claude-session-start.mjs',
    'adapters/cursor-session-start.mjs',
  ]) {
    assert.equal(await missing(join(packageRoot, relative)), true, `${relative} must be retired`);
  }

  // The plugin-scoped session hook is NOT retired: the static plugin carries the
  // self-resolving federation shims for the marketplace-only install
  // (harness-native-wake). Guard the contract in the new direction.
  for (const shim of ['session-start.mjs', 'user-prompt-submit.mjs', 'wake.mjs']) {
    assert.equal(
      await missing(join(packageRoot, 'plugins', 'loam-adapter', 'hooks', shim)),
      false,
      `plugins/loam-adapter/hooks/${shim} must ship`,
    );
  }

  const { runIntegration } = await import('../integration/loam.mjs');
  await assert.rejects(() => runIntegration(['hook', '--harness', 'claude', '--workspace', packageRoot]), /usage/);
});
