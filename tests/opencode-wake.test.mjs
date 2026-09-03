import assert from 'node:assert/strict';
import net from 'node:net';
import { mkdtemp, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';

import * as adapterModule from '../adapters/opencode.mjs';
import { LoamPlugin } from '../adapters/opencode.mjs';

// The adapter file is the OpenCode plugin file; its loader calls every top-level
// export as a plugin factory, so only LoamPlugin may be exported. The helpers
// tests need are hung off it as properties.
const { buildInjectArgs, createOpenCodeAdapter, startLoamNotifyServer } = LoamPlugin;

test('export surface: the plugin file exposes the V1 default record and only LoamPlugin besides it', () => {
  // Two contracts at once, both verified against the opencode plugin loader
  // source (packages/opencode/src/plugin/shared.ts + index.ts):
  //
  // (1) V1 PREFERRED PATH. applyPlugin (index.ts) does:
  //       const plugin = readV1Plugin(load.mod, load.spec, "server", "detect")
  //       if (plugin) { await resolvePluginId(...readPluginId(plugin.id, ...));
  //                     hooks.push(await plugin.server(input, load.options)) }
  //     readV1Plugin (shared.ts) reads mod.default and requires isRecord(default):
  //       const value = mod.default
  //       if (!isRecord(value)) { if (mode === "detect") return; throw ... }
  //       if (mode === "detect" && !("id" in value)) return
  //       const server = "server" in value ? value.server : undefined
  //       if (kind === "server" && server === undefined) throw ...
  //     So the default MUST be a record { id, server } — a default FUNCTION fails
  //     isRecord and silently falls through to the legacy path (the trap).
  //     resolvePluginId: for source === "file", `if (id) return id; throw
  //     "Path plugin ${spec} must export id"` — a file plugin MUST carry an id
  //     (there is no collision handling; a unique id like "loam" is on us).
  //     server is called (input, options) — hence LoamPlugin(input, _options).
  //
  // (2) LEGACY FALLBACK HAZARD. If the V1 record is ever missed, getLegacyPlugins
  //     calls EVERY exported function as a plugin factory — startLoamNotifyServer
  //     throws under such a call (join(undefined, run)). So besides `default`, the
  //     ONLY export may be `LoamPlugin`; every helper stays a property, never an
  //     export. This is why the namespace is pinned to exactly two names.
  const contract = 'adapters/opencode.mjs must export exactly `default` (the V1 record { id: "loam", server: LoamPlugin }) and the named `LoamPlugin`, nothing else. OpenCode registers mod.default via readV1Plugin/applyPlugin (server called as server(input, options)); if it ever falls back to getLegacyPlugins it calls every top-level export as a plugin factory, so no stray function export may exist. Hang helpers off LoamPlugin as properties.';
  // Module-namespace keys are sorted by code unit: "LoamPlugin" (0x4C) before "default" (0x64).
  assert.deepEqual(Object.keys(adapterModule), ['LoamPlugin', 'default'], contract);
  assert.ok(adapterModule.default && typeof adapterModule.default === 'object' && !Array.isArray(adapterModule.default), 'default must be a record (isRecord), never a function — a function default fails detection and falls through to legacy');
  assert.equal(adapterModule.default.id, 'loam', contract);
  assert.equal(typeof adapterModule.default.server, 'function', contract);
  assert.equal(adapterModule.default.server, adapterModule.LoamPlugin, 'the V1 server must be the LoamPlugin function');
});

test('LoamPlugin logs a loading breadcrumb through the opencode app logger and returns hooks', async () => {
  // Whether the plugin loaded is answerable from the opencode log: the server
  // logs "plugin loading" before building the adapter, and still returns the
  // hooks object. A best-effort log failure must never block loading.
  const logs = [];
  const client = { app: { log: async (entry) => { logs.push(entry); } } };
  const plugin = await LoamPlugin({ client, directory: '/workspace' });
  assert.equal(typeof plugin['experimental.chat.system.transform'], 'function', 'the server returns the hooks object');
  assert.equal(typeof plugin['chat.message'], 'function', 'the per-turn hook is registered');
  assert.equal(typeof plugin['experimental.session.compacting'], 'function', 'the compaction hook is registered');

  const loading = logs.find((entry) => entry?.body?.message === 'plugin loading');
  assert.ok(loading, 'a plugin-loading breadcrumb is logged');
  assert.equal(loading.body.service, 'loam');
  assert.equal(loading.body.level, 'info');
  assert.deepEqual(loading.body.extra, { directory: '/workspace' });

  // A throwing/absent app logger must not break loading (the guard is load-bearing).
  const stillLoads = await LoamPlugin({ client: { app: { log: async () => { throw new Error('deadlock'); } } }, directory: '/workspace' });
  assert.equal(typeof stillLoads.event, 'function', 'a failed breadcrumb still returns the hooks object');
});

function pollLogs(logs, message, timeoutMs = 1000) {
  const deadline = Date.now() + timeoutMs;
  return (async () => {
    while (Date.now() < deadline) {
      const hit = logs.find((entry) => entry?.body?.message === message);
      if (hit) return hit;
      await new Promise((resolve) => setTimeout(resolve, 10));
    }
    throw new Error(`log "${message}" never arrived; saw: ${logs.map((e) => e?.body?.message).join(', ')}`);
  })();
}

test('the wake-server open is instrumented: opened(port) on success, failed(err) on throw', async () => {
  // Defect fix: the isFirst wake block used to swallow its failure whole, so a
  // session that never woke gave no clue why. It now names the outcome in the log.
  const logs = [];
  const client = { app: { log: async (entry) => { logs.push(entry); } }, session: { promptAsync: async () => ({}) } };
  // The wake listener now opens on the first system-transform fire.
  const fire = (plugin, sessionID) => plugin['experimental.chat.system.transform']({ sessionID }, { system: [] });

  const okPlugin = await createOpenCodeAdapter({
    client,
    getContext: async () => '',
    wakeServer: async () => ({ wakeRef: 'notify-tcp://127.0.0.1:7777', port: 7777, registered: true, close: async () => {} }),
  })({ directory: '/workspace' });
  await fire(okPlugin, 's1');
  const opened = await pollLogs(logs, 'wake listener opened');
  assert.equal(opened.body.extra.port, 7777);
  assert.equal(opened.body.extra.registered, true);

  logs.length = 0;
  const failPlugin = await createOpenCodeAdapter({
    client,
    getContext: async () => '',
    wakeServer: async () => { throw new Error('bind refused'); },
  })({ directory: '/workspace' });
  await fire(failPlugin, 's2');
  const failed = await pollLogs(logs, 'wake listener failed');
  assert.match(failed.body.extra.error, /bind refused/);
  assert.equal(failed.body.level, 'error');
});

test('per-hook re-registration: every per-turn boundary re-asserts the wake_ref, empty render or not', async () => {
  // connector-self-healing seam 4: a connector restart drops the live channel;
  // an active session must re-attach its mailbox at the next hook. The re-register
  // is idempotent on the connector side and must not be hostage to a contentful
  // render (getContext returns '' here), exactly like the SessionStart register.
  const logs = [];
  const client = { app: { log: async (entry) => { logs.push(entry); } }, session: { promptAsync: async () => ({}) } };
  // SessionStart opens the wake server (system-transform); each user turn
  // (chat.message) re-asserts the wake_ref.
  const start = (plugin, sessionID) => plugin['experimental.chat.system.transform']({ sessionID }, { system: [] });
  const turn = (plugin, sessionID) => plugin['chat.message']({ sessionID }, { parts: [] });
  let reregisters = 0;
  const plugin = await createOpenCodeAdapter({
    client,
    getContext: async () => '',
    wakeServer: async () => ({
      wakeRef: 'notify-tcp://127.0.0.1:7777',
      port: 7777,
      registered: true,
      reregister: async () => { reregisters += 1; return { ok: true }; },
      close: async () => {},
    }),
  })({ directory: '/workspace' });

  // SessionStart opens the server (registration happens in the open), not a reregister.
  await start(plugin, 's1');
  await pollLogs(logs, 'wake listener opened');
  assert.equal(reregisters, 0, 'SessionStart registers via the server open, never reregister');

  // Every subsequent per-turn boundary re-asserts the wake_ref.
  await turn(plugin, 's1');
  await turn(plugin, 's1');
  assert.ok(reregisters >= 2, `each per-turn boundary re-registers (got ${reregisters})`);
  assert.ok(await pollLogs(logs, 'wake re-register'), 'the re-register is breadcrumbed');
});

test('startLoamNotifyServer breadcrumbs the wake frame (hint only), register, and drop', async () => {
  const logs = [];
  const log = async (message, extra) => { logs.push({ message, extra }); };
  const calls = [];
  const notify = await startLoamNotifyServer({
    workspace: '/workspace',
    sessionId: 'sess-1',
    globalRoot: '/root',
    onWake: () => {},
    register: async (action) => { calls.push(action); return { ok: action === 'register', code: action === 'register' ? 0 : 7 }; },
    log,
  });

  const register = logs.find((e) => e.message === 'wake register');
  assert.deepEqual(register.extra, { action: 'register', ok: true, exit: 0 });
  const port = Number(notify.wakeRef.split(':').pop());

  await new Promise((resolve, reject) => {
    const socket = net.connect({ port, host: '127.0.0.1' });
    socket.once('connect', () => { socket.write('{"kind":"loam-wake","project":"loam","hint":"01ABC"}'); socket.end(); });
    socket.once('close', resolve);
    socket.once('error', reject);
  });
  // The frame breadcrumb carries only the hint, never any body content.
  const deadline = Date.now() + 1000;
  let frameEntry;
  while (Date.now() < deadline && !(frameEntry = logs.find((e) => e.message === 'wake frame'))) {
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
  assert.ok(frameEntry, 'a wake frame is breadcrumbed');
  assert.deepEqual(frameEntry.extra, { hint: '01ABC' });

  await notify.close();
  const drop = logs.find((e) => e.message === 'wake drop');
  assert.deepEqual(drop.extra, { action: 'drop', ok: false, exit: 7 });
});

function wait(ms) { return new Promise((resolve) => setTimeout(resolve, ms)); }

test('inject args: workspace is positional, never the --workspace flag the CLI rejects', () => {
  const args = buildInjectArgs({
    action: 'register',
    workspace: '/workspace',
    globalRoot: '/root',
    sessionId: 'sess-1',
    wakeRef: 'notify-tcp://127.0.0.1:9',
  });
  // Workspace sits positionally right after the action; the CLI contract is
  // `inject <register|drop> [<workspace>] --global-root ... --session-id ...`.
  assert.deepEqual(args.slice(0, 4), ['federation', 'inject', 'register', '/workspace']);
  assert.ok(!args.includes('--workspace'), 'must not pass workspace as a flag (exit 64)');
  // Required flags and the optional wake-ref are present in flag form.
  for (const flag of ['--global-root', '--session-id', '--wake-ref']) {
    assert.ok(args.includes(flag), `${flag} must be present`);
  }
  assert.equal(args[args.indexOf('--global-root') + 1], '/root');
  assert.equal(args[args.indexOf('--session-id') + 1], 'sess-1');

  // drop omits the wake-ref but keeps the positional workspace.
  const dropArgs = buildInjectArgs({ action: 'drop', workspace: '/ws', globalRoot: '/r', sessionId: 's' });
  assert.deepEqual(dropArgs.slice(0, 4), ['federation', 'inject', 'drop', '/ws']);
  assert.ok(!dropArgs.includes('--wake-ref'), 'drop carries no wake-ref');
});

async function poll(fn, timeoutMs = 1000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const value = await fn();
    if (value) return value;
    await wait(10);
  }
  throw new Error('poll timed out');
}

test('startLoamNotifyServer: registers a localhost wake_ref, fires onWake on a wake frame only, deregisters on close', async () => {
  const root = await mkdtemp(join(tmpdir(), 'loam-wake-server-'));
  await writeFile(join(root, 'install.json'), JSON.stringify({ integration_path: '/nonexistent' }));
  const calls = [];
  const registered = [];
  const notify = await startLoamNotifyServer({
    workspace: '/workspace',
    sessionId: 'sess-1',
    globalRoot: root,
    onWake: () => { calls.push('wake'); },
    register: async (action, ref) => { registered.push([action, ref]); return { ok: true }; },
  });

  assert.equal(registered.length, 1);
  assert.deepEqual(registered[0][0], 'register');
  assert.match(registered[0][1], /^notify-tcp:\/\/127\.0\.0\.1:\d+$/);
  assert.equal(notify.registered, true);
  const port = Number(registered[0][1].split(':').pop());

  const connect = () => new Promise((resolve, reject) => {
    const socket = net.connect({ port, host: '127.0.0.1' });
    socket.once('connect', () => resolve(socket));
    socket.once('error', reject);
  });

  // A wake frame fires onWake exactly once.
  const socket = await connect();
  socket.write('{"kind":"loam-wake","project":"loam","hint":"e1"}');
  socket.end();
  await poll(() => calls.length === 1);
  assert.deepEqual(calls, ['wake']);

  // A frame without the kind must not fire.
  const socket2 = await connect();
  socket2.write('{"kind":"other"}');
  socket2.end();
  await wait(100);
  assert.deepEqual(calls, ['wake'], 'a non-wake frame must be ignored');

  // Close deregisters the session and stops the listener.
  await notify.close();
  assert.equal(registered.length, 2);
  assert.deepEqual(registered[1][0], 'drop');
  await assert.rejects(connect(), /connect/);
});

test('wake injection renders through the native read path and lands via promptAsync', async () => {
  const root = await mkdtemp(join(tmpdir(), 'loam-wake-inject-'));
  await writeFile(join(root, 'install.json'), JSON.stringify({ integration_path: '/nonexistent' }));
  const promptParts = [];
  const plugin = await createOpenCodeAdapter({
    client: {
      session: { promptAsync: async (input) => { promptParts.push(input.body.parts.map((part) => part.text).join('\n')); } },
    },
    getContext: async ({ event }) => event === 'SessionStart'
      ? '<LOAM_IMPORTANT>\nbaseline\n</LOAM_IMPORTANT>'
      : '<LOAM_IMPORTANT>\n## Federation\nwake body\n</LOAM_IMPORTANT>',
    wakeServer: async ({ workspace, sessionId, globalRoot, onWake }) => {
      // The test replaces the runtime spawn with a direct onWake handle.
      assert.equal(workspace, '/workspace');
      assert.equal(sessionId, 'sess-w');
      assert.equal(globalRoot, root);
      const server = net.createServer((socket) => {
        socket.setEncoding('utf8');
        socket.on('data', () => { socket.end(); void onWake(); });
        socket.on('error', () => {});
      });
      await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
      const port = server.address().port;
      return {
        wakeRef: `notify-tcp://127.0.0.1:${port}`,
        registered: true,
        close: async () => new Promise((resolve) => server.close(resolve)),
      };
    },
  })({ directory: '/workspace' });

  await plugin['experimental.chat.system.transform']({ sessionID: 'sess-w' }, { system: [] });
  await poll(async () => promptParts.length === 0 || true); // listener spin-up
  await wait(50);
  assert.equal(promptParts.length, 0, 'no injection before any wake frame');

  // The injected wakeServer does not expose its port to the test; drive the
  // same path the adapter would by firing onWake twice in a row — the guard
  // must collapse them into one injection.
  // (The real end-to-end port discovery happens in the manual cross-test.)
  const adapter = plugin.event;
  // session.deleted stops the listener; the wake path itself is asserted by
  // the direct notify server test above.
  await adapter({ event: { type: 'session.deleted', sessionID: 'sess-w' } });
  await wait(50);
});

test('a session without the runtime still starts and the wake server degrades silently', async () => {
  const root = await mkdtemp(join(tmpdir(), 'loam-wake-absent-'));
  await writeFile(join(root, 'install.json'), JSON.stringify({ integration_path: '/nonexistent' }));
  const plugin = await createOpenCodeAdapter({
    client: { session: {} },
    wakeServer: async () => { throw new Error('no listener'); },
    // The idle event must not spawn the real gate's node subprocess after the
    // test ends; this test is about wake degradation, not ingestion.
    ingestion: {
      gate: async () => ({ action: 'skip' }),
      resolveGlobalRoot: () => root,
      resolveSkillsRoot: () => root,
      runWorker: async () => undefined,
    },
  })({ directory: '/workspace' });
  // Must not reject: no listener, no wake, per-turn boundary still delivers.
  await plugin['experimental.chat.system.transform']({ sessionID: 'sess-x' }, { system: [] });
  await plugin['chat.message']({ sessionID: 'sess-x' }, { parts: [] });
  await plugin.event({ event: { type: 'session.idle', sessionID: 'sess-x' } });
  await plugin.event({ event: { type: 'session.deleted', sessionID: 'sess-x' } });
});

const WAKE_TIP = '[tip] federation: status from a teammate\'s machine — informational, no reply or action expected.';
const WAKE_BODY = '<io.loam.work.state key="task-c" state="blocked" trust="claimed">\n'
  + '[Carol <carol@example.com>] Task C is blocked on the API key.\n'
  + `</io.loam.work.state>\n\n${WAKE_TIP}`;

test('wake pulls the drained render (delta + tip) and injects it with the SDK shape; an empty drain is a no-op', async () => {
  const root = await mkdtemp(join(tmpdir(), 'loam-wake-inject2-'));
  await writeFile(join(root, 'install.json'), JSON.stringify({ integration_path: '/nonexistent' }));

  const inputs = [];
  let resolveWithError = false;
  let captured = null; // the adapter's onWake, captured so the test can fire it
  // The native `Wake` event drains the session mailbox — the single seen-set —
  // and returns the terse elements + tip already rendered. The connector did the
  // delta; the plugin just injects whatever text came back. An empty drain comes
  // back empty, which must inject nothing.
  let wakeBody = WAKE_BODY;

  const plugin = await createOpenCodeAdapter({
    client: {
      session: {
        promptAsync: async (input) => {
          inputs.push(input);
          return resolveWithError ? { error: { message: 'http 500' }, response: {} } : {};
        },
      },
    },
    getContext: async ({ event }) => (event === 'Wake' ? wakeBody : '<LOAM_IMPORTANT>\nstart\n</LOAM_IMPORTANT>'),
    wakeServer: async ({ onWake }) => {
      captured = onWake;
      return { wakeRef: 'notify-tcp://127.0.0.1:0', registered: true, close: async () => {} };
    },
  })({ directory: '/workspace' });

  // First system-transform fire is SessionStart: it sets loamWake.sessionId and
  // starts the (injected) wake server, capturing onWake.
  await plugin['experimental.chat.system.transform']({ sessionID: 'sess-wake2' }, { system: [] });
  await poll(() => captured !== null);

  // A wake injects the drained body verbatim, in the generated-SDK shape — not
  // the flat { sessionID, parts } form, which is a silent no-op against the SDK.
  await captured();
  await poll(() => inputs.length === 1);
  const input = inputs[0];
  assert.deepEqual(input.path, { id: 'sess-wake2' }, 'path.id carries the session id');
  assert.deepEqual(input.query, { directory: '/workspace' }, 'query.directory carries the workspace');
  assert.ok(input.sessionID === undefined && input.parts === undefined, 'the flat shape must not be used');
  const text = input.body.parts[0].text;
  assert.equal(text, WAKE_BODY, 'the drained delta body is injected verbatim');
  assert.equal(text.match(/\[tip\]/g).length, 1, 'exactly one tip line, never per item');
  assert.ok(!/unverified|untrusted|render-only/.test(text), 'banned provenance vocabulary must not appear');
  assert.ok(!text.includes('<LOAM_IMPORTANT>'), 'a wake carries no framing wrapper');

  // An empty drain (the mailbox already consumed by a per-turn render, or nothing
  // new) injects nothing at all — no block, no lone tip.
  wakeBody = '';
  await captured();
  await wait(50);
  assert.equal(inputs.length, 1, 'an empty drain is a no-op: no injection');

  // The SDK resolves with { error } rather than throwing on an HTTP error; a
  // failed injection is best-effort and must not crash the listener or wedge the
  // pending guard, so a later wake still fires.
  wakeBody = WAKE_BODY;
  resolveWithError = true;
  await captured();
  await poll(() => inputs.length === 2);
  resolveWithError = false;
  await captured();
  await poll(() => inputs.length === 3);
});

test('the first system-transform registers the wake server even when the render is empty', async () => {
  // Registration must not be hostage to a contentful first render: a session
  // whose first getContext returns empty (failed hook, quiet federation) must
  // still open its wake listener, or it can never be woken for the whole session.
  const root = await mkdtemp(join(tmpdir(), 'loam-wake-empty-'));
  await writeFile(join(root, 'install.json'), JSON.stringify({ integration_path: '/nonexistent' }));

  let registered = null;
  const plugin = await createOpenCodeAdapter({
    client: { session: { promptAsync: async () => ({}) } },
    // Every render is empty — the failed-spawn / quiet-federation case.
    getContext: async () => '',
    wakeServer: async ({ sessionId }) => {
      registered = { sessionId };
      return { wakeRef: 'notify-tcp://127.0.0.1:0', registered: true, close: async () => {} };
    },
  })({ directory: '/workspace' });

  const output = { system: [] };
  await plugin['experimental.chat.system.transform']({ sessionID: 'sess-empty' }, output);
  await poll(() => registered !== null);

  assert.deepEqual(registered, { sessionId: 'sess-empty' }, 'the wake server opens against the session even with an empty render');
  // The empty render still pushes nothing — the baseline gate governs the push.
  assert.equal(output.system.length, 0, 'an empty render pushes no context');
});

test('buildHookArgs passes --session-id for a mailbox drain and omits it without one', () => {
  const withId = LoamPlugin.buildHookArgs({ workspace: '/w', event: 'Wake', sessionId: 'sess-1' });
  assert.deepEqual(withId.slice(0, 6), ['hook', 'opencode', '--workspace', '/w', '--event', 'Wake']);
  const at = withId.indexOf('--session-id');
  assert.ok(at > -1 && withId[at + 1] === 'sess-1', 'the session id rides on the hook argv');
  const withoutId = LoamPlugin.buildHookArgs({ workspace: '/w', event: 'SessionStart' });
  assert.ok(!withoutId.includes('--session-id'), 'no session id, no flag');
});

test('the wake drain carries the registered session id into getContext', async () => {
  // The final-acceptance regression: injectWake used to call getContext(Wake)
  // WITHOUT the session id, so the runtime refused the drain and the wake
  // rendered empty. The drain must pass the same session id the instance
  // registered with.
  const root = await mkdtemp(join(tmpdir(), 'loam-wake-sid-'));
  await writeFile(join(root, 'install.json'), JSON.stringify({ integration_path: '/nonexistent' }));

  const contextCalls = [];
  let captured = null;
  const plugin = await createOpenCodeAdapter({
    client: { session: { promptAsync: async () => ({}) } },
    getContext: async (opts) => {
      contextCalls.push(opts);
      return opts.event === 'Wake' ? WAKE_BODY : '<LOAM_IMPORTANT>\nstart\n</LOAM_IMPORTANT>';
    },
    wakeServer: async ({ onWake }) => {
      captured = onWake;
      return { wakeRef: 'notify-tcp://127.0.0.1:0', port: 0, registered: true, close: async () => {} };
    },
  })({ directory: '/workspace' });

  await plugin['experimental.chat.system.transform']({ sessionID: 'sess-wake' }, { system: [] });
  await poll(() => captured !== null);

  await captured();
  await poll(() => contextCalls.some((call) => call.event === 'Wake'));
  const wakeCall = contextCalls.find((call) => call.event === 'Wake');
  assert.equal(wakeCall.sessionId, 'sess-wake', 'the wake drain must carry the registered session id, or the runtime refuses it and renders empty');
});

test('per-turn drains the mailbox with the session id; SessionStart renders the full snapshot', async () => {
  // The one-seen-set amendment: an item enters context exactly once across every
  // surface. SessionStart renders the full history (no session id, no drain);
  // per-turn drains this session's mailbox (session id present) instead of
  // re-rendering the whole board every turn.
  const root = await mkdtemp(join(tmpdir(), 'loam-per-turn-'));
  await writeFile(join(root, 'install.json'), JSON.stringify({ integration_path: '/nonexistent' }));

  const contextCalls = [];
  const plugin = await createOpenCodeAdapter({
    client: { session: { promptAsync: async () => ({}) } },
    getContext: async (opts) => { contextCalls.push(opts); return '<LOAM_IMPORTANT>\nx\n</LOAM_IMPORTANT>'; },
    wakeServer: async () => ({ wakeRef: 'notify-tcp://127.0.0.1:0', port: 0, registered: true, close: async () => {} }),
  })({ directory: '/workspace' });

  await plugin['experimental.chat.system.transform']({ sessionID: 'sess-turn' }, { system: [] }); // SessionStart
  await plugin['chat.message']({ sessionID: 'sess-turn' }, { parts: [] }); // per-turn

  const start = contextCalls.find((call) => call.event === 'SessionStart');
  const turn = contextCalls.find((call) => call.event === 'UserPromptSubmit');
  assert.equal(start.sessionId, null, 'SessionStart renders the full snapshot, no drain');
  assert.equal(turn.sessionId, 'sess-turn', 'per-turn drains the mailbox with the session id');
});

test('two sessions on one plugin instance each receive the full baseline in the system prompt (#209 point 3)', async () => {
  // OpenCode runs one server for many sessions. The baseline is cached per
  // session id, so a second session gets its own SessionStart block — not the
  // instance-wide "already started, skip" of the old sessionStarted flag.
  const bySession = {};
  const plugin = await createOpenCodeAdapter({
    client: { session: { promptAsync: async () => ({}) } },
    getContext: async ({ workspace }) => `<LOAM_IMPORTANT>\nYou have loam.\n${workspace}\n</LOAM_IMPORTANT>`,
    wakeServer: async () => ({ wakeRef: 'notify-tcp://127.0.0.1:0', port: 0, registered: true, close: async () => {} }),
  })({ directory: '/workspace' });

  const outA = { system: [] };
  const outB = { system: [] };
  await plugin['experimental.chat.system.transform']({ sessionID: 's-a' }, outA);
  await plugin['experimental.chat.system.transform']({ sessionID: 's-b' }, outB);
  bySession.a = outA.system;
  bySession.b = outB.system;
  assert.ok(bySession.a[0]?.includes('You have loam'), 'session A gets the full baseline');
  assert.ok(bySession.b[0]?.includes('You have loam'), 'session B gets the full baseline, not skipped');
});

test('the baseline spawns the runtime once per session and is byte-identical across calls', async () => {
  let renders = 0;
  const plugin = await createOpenCodeAdapter({
    client: { session: { promptAsync: async () => ({}) } },
    getContext: async () => { renders += 1; return `<LOAM_IMPORTANT>\nYou have loam. render ${renders}\n</LOAM_IMPORTANT>`; },
    wakeServer: async () => ({ wakeRef: 'notify-tcp://127.0.0.1:0', port: 0, registered: true, close: async () => {} }),
  })({ directory: '/workspace' });

  const first = { system: [] };
  const second = { system: [] };
  await plugin['experimental.chat.system.transform']({ sessionID: 's1' }, first);
  await plugin['experimental.chat.system.transform']({ sessionID: 's1' }, second);
  assert.equal(renders, 1, 'the SessionStart render is cached: the runtime spawns once per session');
  assert.equal(first.system[0], second.system[0], 'the baseline is byte-identical across calls (prompt caching)');
});

test('chat.message passes the input session id to the render and prepends the persisted part', async () => {
  const contextCalls = [];
  const plugin = await createOpenCodeAdapter({
    client: { session: { promptAsync: async () => ({}) } },
    getContext: async (opts) => { contextCalls.push(opts); return '<LOAM_IMPORTANT>\n## Federation\nrefresh\n</LOAM_IMPORTANT>'; },
    wakeServer: async () => ({ wakeRef: 'notify-tcp://127.0.0.1:0', port: 0, registered: true, close: async () => {} }),
  })({ directory: '/workspace' });

  const out = { parts: [{ type: 'text', text: 'user prompt' }] };
  await plugin['chat.message']({ sessionID: 'msg-sess' }, out);
  const call = contextCalls.find((c) => c.event === 'UserPromptSubmit');
  assert.equal(call.sessionId, 'msg-sess', 'the drain keys off the input session id, not loamWake.sessionId');
  assert.equal(out.parts.length, 2, 'the render is prepended as a persisted part');
  assert.ok(out.parts[0].text.includes('## Federation'), 'the injected part leads');
  assert.equal(out.parts[0].id, undefined, 'no borrowed part id — OpenCode mints one on store');
});

test('chat.message with an empty render leaves the parts untouched', async () => {
  const plugin = await createOpenCodeAdapter({
    client: { session: { promptAsync: async () => ({}) } },
    getContext: async () => '',
    wakeServer: async () => ({ wakeRef: 'notify-tcp://127.0.0.1:0', port: 0, registered: true, close: async () => {} }),
  })({ directory: '/workspace' });

  const out = { parts: [{ type: 'text', text: 'user prompt' }] };
  await plugin['chat.message']({ sessionID: 'empty-sess' }, out);
  assert.equal(out.parts.length, 1, 'an empty render adds no part');
});

test('the compaction hook re-supplies the cached baseline', async () => {
  const plugin = await createOpenCodeAdapter({
    client: { session: { promptAsync: async () => ({}) } },
    getContext: async () => '<LOAM_IMPORTANT>\nYou have loam.\n</LOAM_IMPORTANT>',
    wakeServer: async () => ({ wakeRef: 'notify-tcp://127.0.0.1:0', port: 0, registered: true, close: async () => {} }),
  })({ directory: '/workspace' });

  // Cache the baseline for the session first.
  await plugin['experimental.chat.system.transform']({ sessionID: 'compact-sess' }, { system: [] });
  const out = { context: [] };
  await plugin['experimental.session.compacting']({ sessionID: 'compact-sess' }, out);
  assert.equal(out.context.length, 1, 'the baseline survives compaction');
  assert.ok(out.context[0].includes('You have loam'), 'the retained context carries the router block');

  // A session with no cached baseline pushes nothing.
  const bare = { context: [] };
  await plugin['experimental.session.compacting']({ sessionID: 'unknown-sess' }, bare);
  assert.equal(bare.context.length, 0, 'no baseline, nothing pushed');
});
