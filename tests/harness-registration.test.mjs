import assert from 'node:assert/strict';
import { cp, mkdir, mkdtemp, readFile, readdir, stat, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { test } from 'node:test';

import { detectHarnesses, installHarnesses } from '../setup/harnesses.mjs';

const packageRoot = fileURLToPath(new URL('..', import.meta.url));

// The files a harness loads or executes directly at session time.
const RUNTIME_PATH_FILES = [
  '.opencode/plugins/loam.js',
  'adapters/opencode.mjs',
  'plugins/loam-adapter/adapter.mjs',
  'plugins/loam-adapter/hooks/stop.mjs',
];

// ...and the subtree those four reach *in the same process*: they resolve an
// integration root and then `import(new URL('<sibling>.mjs', root).href)`, and
// `integration/ingest.mjs` spawns `adapters/ingest-worker.mjs`. Scanned as whole
// directories rather than a hand-written closure — the closure is built from
// computed URLs, so a maintained list falls silently behind a new sibling and
// the prohibitions stop covering code that runs inside the plugin.
const INGESTION_SUBTREE_DIRS = ['integration', 'adapters'];

// One class is asserted on the four session-time files only: Stop/ingestion is
// the Node boundary this slice deliberately *kept*, and `integration/loam.mjs`
// is its entry point. Everything the plugin could otherwise reach — a broker,
// our IPC, the OS service manager, the registry/dedupe ledger, collaboration
// state — is asserted over the whole subtree.
const SUBTREE_EXEMPT = new Set(['retired-node-integration']);

// Each pattern is one structural claim the slice makes about the runtime path.
// The scanner is only worth trusting because the positive-control tests below
// plant each of these deliberately and prove the scan catches it.
//
// `retired-node-integration` matches how this codebase actually loads a module
// — `import(new URL('context.mjs', root).href)`, with no `integration/` prefix —
// as well as the static spelling. Naming `loam.mjs` as a *path anchor*
// (resolving the integration root) is not a violation; loading it is.
// ponytail: a literal-filename grep, so `import(new URL(NAME, root))` with a
// computed name would evade it — tighten to an AST walk only if that shape appears.
const FORBIDDEN = [
  [
    'retired-node-integration',
    /(?:new URL|import|from)\s*\(?\s*['"`][^'"`]*(?:loam|context)\.mjs|\bformatContext\b|\brunIntegration\s*\(|--harness\b/,
  ],
  ['broker', /rumqttc|MqttTransport|MqttSession|DeliveryProcessor|\bmqtts?:\/\//],
  ['connector-ipc', /node:net\b|UnixStream|createConnection|federation\.snapshot/],
  ['registry-or-dedupe', /record_response|causation_id|ChannelRegistry|SnapshotStore/],
  ['collaboration-state', /sender_scope|handler-allowlist|publication:\s*verified/],
  ['service-manager', /systemctl|launchctl|\bsc\.exe|LaunchAgents|systemd/],
];

function scanSource(source, { exempt = new Set() } = {}) {
  return FORBIDDEN
    .filter(([name, pattern]) => !exempt.has(name) && pattern.test(source))
    .map(([name]) => name);
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

// The runtime is version- and target-qualified, which is the whole reason the
// command has to be written at stage time rather than resolved at session time.
function stagedRuntime(globalRoot, runtimeVersion) {
  return join(globalRoot, 'bin', runtimeVersion, 'x86_64-unknown-linux-gnu', 'loam');
}

async function missing(path) {
  try {
    await stat(path);
    return false;
  } catch (error) {
    return error?.code === 'ENOENT';
  }
}

async function harnessFixture(prefix, { pluginVersion = '0.9.10', runtimeVersion = '0.9.10' } = {}) {
  const home = await mkdtemp(join(tmpdir(), prefix));
  const globalRoot = join(home, '.agents', 'loam');
  const runtimePath = stagedRuntime(globalRoot, runtimeVersion);
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

test('the retired-integration scan catches the idiom this codebase actually uses', async () => {
  // A control that only plants a spelling the repo never writes proves nothing.
  // Every runtime-path file reaches a sibling module through
  // `import(new URL('<name>.mjs', root).href)`, so that is the shape a
  // re-introduced context read path would take.
  const clean = await readFile(join(packageRoot, 'adapters', 'opencode.mjs'), 'utf8');
  for (const line of [
    "const m = await import(new URL('context.mjs', root).href); await m.render();",
    'const m = await import(new URL("loam.mjs", root).href);',
    "import { formatContext } from '../integration/context.mjs';",
    "const body = formatContext({ workspace });",
    "await runIntegration(['hook', 'claude', workspace]);",
    'spawn(process.execPath, [loamEntry, "hook", "--harness", "claude"]);',
  ]) {
    assert.deepEqual(
      scanSource(`${clean}\n${line}\n`),
      ['retired-node-integration'],
      `planted retired spelling must be caught: ${line}`,
    );
  }
  // ...and resolving the integration *root* by naming `loam.mjs` as a path is
  // still legal, which is what the two adapters do today. If this ever fires,
  // the pattern got broad rather than the tree getting dirty.
  assert.deepEqual(
    scanSource(`${clean}\nconst anchor = join(globalRoot, 'integration', 'loam.mjs');\n`),
    [],
  );
});

test('the in-process ingestion subtree reaches no broker, IPC, or service manager either', async () => {
  const files = await ingestionSubtreeFiles();
  // The enumeration is part of the claim: an empty or wrong file list would
  // read exactly like a clean subtree.
  for (const expected of [
    'integration/ingest.mjs',
    'integration/hooks.mjs',
    'integration/paths.mjs',
    'integration/runtime.mjs',
    'integration/metadata.mjs',
    'integration/shadow.mjs',
    'integration/ingest-process.mjs',
    'integration/ingest-fingerprint.mjs',
    'integration/loam.mjs',
    'adapters/ingest-worker.mjs',
    'adapters/ingest-modules.mjs',
  ]) {
    assert.ok(files.includes(expected), `${expected} must be inside the scanned subtree`);
  }

  for (const relative of files) {
    const source = await readFile(join(packageRoot, relative), 'utf8');
    assert.deepEqual(
      scanSource(source, { exempt: SUBTREE_EXEMPT }),
      [],
      `${relative} runs inside the plugin and must reach none of these`,
    );
  }
});

test('the extended scan catches a violation planted in the ingestion subtree', async () => {
  // The subtree is reached through computed dynamic imports, so the risk is not
  // a bad pattern — it is a file that never gets handed to the scanner at all.
  const clean = await readFile(join(packageRoot, 'integration', 'ingest.mjs'), 'utf8');
  assert.deepEqual(scanSource(clean, { exempt: SUBTREE_EXEMPT }), []);
  for (const [expected, line] of [
    ['broker', 'const url = "mqtts://broker.example";'],
    ['connector-ipc', "import net from 'node:net'; net.createConnection();"],
    ['service-manager', 'spawn("systemctl", ["--user", "start", "loam"]);'],
    ['registry-or-dedupe', 'const key = causation_id;'],
    ['collaboration-state', 'const scope = { sender_scope: [] };'],
  ]) {
    assert.deepEqual(
      scanSource(`${clean}\n${line}\n`, { exempt: SUBTREE_EXEMPT }),
      [expected],
      `planted ${expected} in the ingestion subtree must be caught`,
    );
  }

  // The one exemption is real and scoped to exactly one class: the same planted
  // line is legal in the subtree and illegal on a session-time file.
  const retired = `${clean}\nconst m = await import(new URL('context.mjs', root).href);\n`;
  assert.deepEqual(scanSource(retired, { exempt: SUBTREE_EXEMPT }), []);
  assert.deepEqual(scanSource(retired), ['retired-node-integration']);
});

test('a plugin update that resets the shipped hooks file is re-staged onto the new runtime', async () => {
  // Positive control for the "tracks the staged runtime" claim on the *update*
  // path: two fresh fixtures only prove the renderer reads its argument, not
  // that an already-installed slot is rewritten when the runtime moves.
  const { home, globalRoot, claudeCache, codexCache, runtimePath } =
    await harnessFixture('loam-native-registration-update-');
  const startCommand = async (cache) => {
    const hooks = JSON.parse(await readFile(join(cache, 'hooks', 'hooks.json'), 'utf8'));
    return sessionEntries(hooks, 'SessionStart')[0]?.command;
  };
  const openCodeSource = () => readFile(join(home, '.config', 'opencode', 'plugins', 'loam.js'), 'utf8');
  assert.equal(await startCommand(claudeCache), runtimePath);
  assert.ok((await openCodeSource()).includes(JSON.stringify(runtimePath)));

  // `plugin update` replaces the installed hooks file with the shipped one,
  // which carries Stop only — and the new runtime lands in a new version dir.
  const shipped = join(packageRoot, 'plugins', 'loam-adapter', 'hooks', 'hooks.json');
  for (const cache of [claudeCache, codexCache]) await cp(shipped, join(cache, 'hooks', 'hooks.json'));
  assert.equal(await startCommand(claudeCache), undefined, 'the update reset must really have happened');

  const next = stagedRuntime(globalRoot, '0.9.11');
  assert.notEqual(next, runtimePath);
  const detected = await detectHarnesses({ home, pluginVersion: '0.9.10' });
  const result = await installHarnesses({ home, globalRoot, pluginVersion: '0.9.10', runtimePath: next, detected });

  for (const [id, cache] of [['claude', claudeCache], ['codex', codexCache]]) {
    assert.equal(result[id].state, 'ready');
    assert.equal(await startCommand(cache), next, `${id} SessionStart must flip to the new runtime`);
  }
  assert.ok((await openCodeSource()).includes(JSON.stringify(next)));
  assert.ok(!(await openCodeSource()).includes(JSON.stringify(runtimePath)), 'no stale runtime may survive');
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
