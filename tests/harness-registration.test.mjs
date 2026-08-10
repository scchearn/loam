import assert from 'node:assert/strict';
import { readFile, readdir } from 'node:fs/promises';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { test } from 'node:test';

const packageRoot = fileURLToPath(new URL('..', import.meta.url));

// The files a harness loads or executes directly at session time. Under the
// additive federation wiring these legitimately call the native runtime
// (`<runtime> hook <harness> --body`) as a subprocess, and fall back to the Node
// integration — neither of which is a raw broker/IPC/service-manager surface.
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
// plugin/adapter/ingestion code must NEVER reach: a raw broker connection, raw
// IPC, the registry/dedupe ledger, collaboration state, or the OS service
// manager. Calling the trusted runtime binary as a subprocess (the additive
// federation wiring) is explicitly NOT a violation — the runtime owns the
// broker/IPC/service lifecycle behind its own authenticated boundary and the
// plugin only ever consumes its already-sanitized output. The scanner is only
// worth trusting because the positive-control tests below plant each pattern
// and prove the scan catches it.
const FORBIDDEN = [
  ['broker', /rumqttc|MqttTransport|MqttSession|DeliveryProcessor|\bmqtts?:\/\//],
  ['connector-ipc', /node:net\b|UnixStream|createConnection|federation\.snapshot/],
  ['registry-or-dedupe', /record_response|causation_id|ChannelRegistry|SnapshotStore/],
  ['collaboration-state', /sender_scope|handler-allowlist|publication:\s*verified/],
  ['service-manager', /systemctl|launchctl|\bsc\.exe|LaunchAgents|systemd/],
];

function scanSource(source) {
  return FORBIDDEN.filter(([, pattern]) => pattern.test(source)).map(([name]) => name);
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

test('no session-time runtime-path file opens a raw broker, IPC, or service manager', async () => {
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

test('calling the runtime hook subprocess and the Node integration are permitted, not violations', async () => {
  // The additive federation wiring: the adapter runs `<runtime> hook <harness>
  // --body` and, as a fallback, the Node integration. Neither of those shapes —
  // nor resolving/loading a sibling integration module — may trip the guard;
  // only a *raw* broker/IPC/service-manager reach may. This is what keeps the
  // restored Node integration from being mistaken for a backdoor while still
  // letting main's session-start path run.
  const clean = await readFile(join(packageRoot, 'adapters', 'opencode.mjs'), 'utf8');
  for (const line of [
    "const child = spawn(runtimePath, ['hook', harness, '--workspace', workspace, '--body']);",
    "await runIntegration(['hook', '--harness', candidate, '--workspace', workspace], opts);",
    "const m = await import(new URL('context.mjs', root).href);",
    "import { formatContext } from '../integration/context.mjs';",
  ]) {
    assert.deepEqual(scanSource(`${clean}\n${line}\n`), [], `permitted call must not trip the guard: ${line}`);
  }
});

test('the in-process ingestion subtree reaches no broker, IPC, or service manager either', async () => {
  const files = await ingestionSubtreeFiles();
  // The enumeration is part of the claim: an empty or wrong file list would
  // read exactly like a clean subtree.
  for (const expected of [
    'integration/ingest.mjs',
    'integration/loam.mjs',
    'integration/context.mjs',
    'adapters/ingest-worker.mjs',
    'adapters/opencode.mjs',
  ]) {
    assert.ok(files.includes(expected), `${expected} must be inside the scanned subtree`);
  }

  for (const relative of files) {
    const source = await readFile(join(packageRoot, relative), 'utf8');
    assert.deepEqual(scanSource(source), [], `${relative} runs inside the plugin and must reach none of these`);
  }
});

test('the extended scan catches a violation planted in the ingestion subtree', async () => {
  // The subtree is reached through computed dynamic imports, so the risk is not
  // a bad pattern — it is a file that never gets handed to the scanner at all.
  const clean = await readFile(join(packageRoot, 'integration', 'ingest.mjs'), 'utf8');
  assert.deepEqual(scanSource(clean), []);
  for (const [expected, line] of [
    ['broker', 'const url = "mqtts://broker.example";'],
    ['connector-ipc', "import net from 'node:net'; net.createConnection();"],
    ['service-manager', 'spawn("systemctl", ["--user", "start", "loam"]);'],
    ['registry-or-dedupe', 'const key = causation_id;'],
    ['collaboration-state', 'const scope = { sender_scope: [] };'],
  ]) {
    assert.deepEqual(
      scanSource(`${clean}\n${line}\n`),
      [expected],
      `planted ${expected} in the ingestion subtree must be caught`,
    );
  }
});
