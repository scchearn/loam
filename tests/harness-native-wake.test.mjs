import assert from 'node:assert/strict';
import net from 'node:net';
import { readFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { join } from 'node:path';
import { test } from 'node:test';

import {
  handleMarketplaceSessionStart,
  handleMarketplaceUserPromptSubmit,
  pollWake,
  startWakeListener,
  buildRenderArgs,
  buildInjectArgs,
} from '../plugins/loam-adapter/adapter.mjs';

const fixtures = fileURLToPath(new URL('./fixtures/codex/', import.meta.url));

// The rendered wake body the runtime's `hook <harness> --event Wake --body`
// produces: terse elements closed by exactly one [tip]. The adapter never builds
// this — it only wraps it — so the tests treat it as an opaque runtime string.
const WAKE_BODY = '<io.loam.work.state key="task-c" state="blocked" trust="claimed">\n'
  + '[Carol <carol@example.com>] Task C is blocked on the API key.\n'
  + '</io.loam.work.state>\n\n'
  + '[tip] federation: status from a teammate\'s machine — informational, no reply or action expected.';

// --- SessionStart / UserPromptSubmit render (seam A) -------------------------

test('buildRenderArgs: mailbox events carry --session-id; the Wake drain adds --body', () => {
  assert.deepEqual(
    buildRenderArgs({ harness: 'claude', event: 'SessionStart', workspace: '/w' }),
    ['hook', 'claude', '--event', 'SessionStart', '--workspace', '/w'],
    'SessionStart renders the full snapshot — no session id, no --body',
  );
  assert.deepEqual(
    buildRenderArgs({ harness: 'claude', event: 'UserPromptSubmit', workspace: '/w', sessionId: 's1' }),
    ['hook', 'claude', '--event', 'UserPromptSubmit', '--workspace', '/w', '--session-id', 's1'],
    'per-turn drains with the session id',
  );
  const wake = buildRenderArgs({ harness: 'codex', event: 'Wake', workspace: '/w', sessionId: 's1', bodyOnly: true });
  assert.deepEqual(wake, ['hook', 'codex', '--event', 'Wake', '--workspace', '/w', '--session-id', 's1', '--body']);
  assert.ok(wake.includes('--session-id'), 'the Wake drain must carry the session id or the runtime refuses it');
  assert.ok(wake.includes('--body'), 'the Wake drain takes the bare body, not the envelope');
});

test('buildInjectArgs: workspace is positional; only register carries the wake-ref', () => {
  const reg = buildInjectArgs({ action: 'register', workspace: '/w', globalRoot: '/g', sessionId: 's', wakeRef: 'notify-tcp://127.0.0.1:9' });
  assert.deepEqual(reg.slice(0, 4), ['federation', 'inject', 'register', '/w']);
  assert.ok(!reg.includes('--workspace'), 'workspace as a flag is rejected (exit 64)');
  assert.equal(reg[reg.indexOf('--global-root') + 1], '/g');
  assert.equal(reg[reg.indexOf('--session-id') + 1], 's');
  assert.equal(reg[reg.indexOf('--wake-ref') + 1], 'notify-tcp://127.0.0.1:9');
  const drop = buildInjectArgs({ action: 'drop', workspace: '/w', globalRoot: '/g', sessionId: 's' });
  assert.deepEqual(drop.slice(0, 4), ['federation', 'inject', 'drop', '/w']);
  assert.ok(!drop.includes('--wake-ref'), 'drop carries no wake-ref');
});

test('SessionStart forwards the runtime envelope verbatim and passes the boundary', async () => {
  const calls = [];
  const envelope = JSON.stringify({ hookSpecificOutput: { hookEventName: 'SessionStart', additionalContext: '<LOAM_IMPORTANT>\nbaseline\n</LOAM_IMPORTANT>' } });
  const out = await handleMarketplaceSessionStart(
    { cwd: '/workspace', session_id: 'sess-1' },
    { harness: 'claude', render: async (opts) => { calls.push(opts); return { ok: true, stdout: envelope, unavailable: false }; } },
  );
  assert.equal(out, envelope, 'the runtime already built the harness envelope — forward it, never re-wrap');
  assert.equal(calls[0].event, 'SessionStart');
  assert.equal(calls[0].workspace, '/workspace');
  assert.equal(calls[0].sessionId, 'sess-1');
});

test('SessionStart degrades to the repair hint on no runtime (claude envelope, codex bare body)', async () => {
  const unavailable = async () => ({ unavailable: true, ok: false, stdout: '' });
  const claude = await handleMarketplaceSessionStart({ cwd: '/w' }, { harness: 'claude', render: unavailable });
  const parsed = JSON.parse(claude);
  assert.match(parsed.hookSpecificOutput.additionalContext, /npx @scchearn\/loam install/);
  assert.equal(parsed.hookSpecificOutput.hookEventName, 'SessionStart');
  const codex = await handleMarketplaceSessionStart({ cwd: '/w' }, { harness: 'codex', render: unavailable });
  assert.match(codex, /npx @scchearn\/loam install/);
  assert.ok(!codex.startsWith('{'), 'codex consumes the bare body on stdout, not an envelope');
});

test('UserPromptSubmit forwards the drain envelope but degrades SILENTLY, never the repair hint', async () => {
  const envelope = JSON.stringify({ hookSpecificOutput: { hookEventName: 'UserPromptSubmit', additionalContext: '<LOAM_IMPORTANT>\nrefresh\n</LOAM_IMPORTANT>' } });
  const ok = await handleMarketplaceUserPromptSubmit({ cwd: '/w', session_id: 's' }, { harness: 'claude', render: async () => ({ ok: true, stdout: envelope, unavailable: false }) });
  assert.equal(ok, envelope);
  // A broken install must not spam the repair hint on every prompt — the per-turn
  // surface stays empty.
  const degraded = await handleMarketplaceUserPromptSubmit({ cwd: '/w' }, { harness: 'claude', render: async () => ({ unavailable: true, ok: false, stdout: '' }) });
  assert.equal(JSON.parse(degraded).hookSpecificOutput.additionalContext, '', 'per-turn degrade is silent');
});

// --- Stop-hook long-poll wake (seam B) ---------------------------------------

// A scripted listener: `next()` consumes booleans from `fired` and advances the
// injected clock by the wait it was granted, so pollWake's `while (now < deadline)`
// terminates deterministically without real timers or sockets.
function scriptedListener(fired, clock) {
  let i = 0;
  const listener = {
    wakeRef: 'notify-tcp://127.0.0.1:55555',
    port: 55555,
    closed: false,
    next: async (waitMs) => { clock.t += waitMs; return i < fired.length ? fired[i++] : false; },
    close: async () => { listener.closed = true; },
  };
  return listener;
}

const FALLBACK = { fallback: true };
// A resolvable runtime so the poller proceeds past its no-runtime guard; the
// register/drain/listen are all injected, so the actual paths are never spawned.
const RESOLVED = async () => ({ runtimePath: '/x', globalRoot: '/g' });

test('a fired wake with a non-empty drain returns the block-decision carrying the runtime body', async () => {
  const clock = { t: 0 };
  const actions = [];
  const listener = scriptedListener([true], clock);
  const result = await pollWake(
    { cwd: '/w', session_id: 's', stop_hook_active: false },
    {
      harness: 'claude',
      fallback: FALLBACK, resolvePaths: RESOLVED,
      listen: async () => listener,
      inject: async (action) => { actions.push(action); return { ok: true, code: 0 }; },
      render: async (opts) => { assert.equal(opts.event, 'Wake'); assert.equal(opts.bodyOnly, true); return { ok: true, stdout: WAKE_BODY, unavailable: false }; },
      now: () => clock.t,
      budgetMs: 10000,
      renewMs: 1000,
    },
  );
  assert.deepEqual(result, { decision: 'block', reason: WAKE_BODY }, 'block only with a newly admitted frame in hand');
  assert.equal(actions[0], 'register', 'the wake_ref is armed before the wait');
  assert.equal(actions[actions.length - 1], 'drop', 'the ref is always dropped in the finally');
  assert.equal(listener.closed, true, 'the listener is always closed');
});

test('the block-decision is identical for codex (verified against the codex output schema)', async () => {
  const clock = { t: 0 };
  const result = await pollWake(
    { cwd: '/w', session_id: 's' },
    {
      harness: 'codex', fallback: FALLBACK, resolvePaths: RESOLVED,
      listen: async () => scriptedListener([true], clock),
      inject: async () => ({ ok: true, code: 0 }),
      render: async () => ({ ok: true, stdout: WAKE_BODY, unavailable: false }),
      now: () => clock.t, budgetMs: 10000, renewMs: 1000,
    },
  );
  assert.deepEqual(result, { decision: 'block', reason: WAKE_BODY });
});

test('an empty drain is not a delivery: keep waiting, then allow-stop on budget expiry', async () => {
  const clock = { t: 0 };
  const actions = [];
  const listener = scriptedListener([true, false, false, false, false], clock);
  const result = await pollWake(
    { cwd: '/w', session_id: 's' },
    {
      harness: 'claude', fallback: FALLBACK, resolvePaths: RESOLVED,
      listen: async () => listener,
      inject: async (action) => { actions.push(action); return { ok: true, code: 0 }; },
      // An empty stdout is the mailbox-already-drained race — must not block.
      render: async () => ({ ok: true, stdout: '', unavailable: false }),
      now: () => clock.t, budgetMs: 3000, renewMs: 1000,
    },
  );
  assert.deepEqual(result, FALLBACK, 'budget expiry degrades to the ingestion fallback (allow-stop)');
  assert.ok(actions.filter((a) => a === 'register').length >= 2, 're-arms across renew windows');
  assert.equal(actions[actions.length - 1], 'drop');
  assert.equal(listener.closed, true);
});

test('no session id degrades immediately with no listener or registration', async () => {
  let listened = false;
  const result = await pollWake(
    { cwd: '/w' },
    { harness: 'claude', fallback: FALLBACK, resolvePaths: RESOLVED, listen: async () => { listened = true; return scriptedListener([], { t: 0 }); } },
  );
  assert.deepEqual(result, FALLBACK);
  assert.equal(listened, false, 'a wake needs a registered session — never open a socket without one');
});

test('a connector-down first arm degrades to allow-stop but still drops and closes', async () => {
  const clock = { t: 0 };
  const actions = [];
  const listener = scriptedListener([true], clock);
  const result = await pollWake(
    { cwd: '/w', session_id: 's' },
    {
      harness: 'claude', fallback: FALLBACK, resolvePaths: RESOLVED,
      listen: async () => listener,
      inject: async (action) => { actions.push(action); return { ok: action !== 'register', code: action === 'register' ? 7 : 0 }; },
      render: async () => { throw new Error('must not drain when arming failed'); },
      now: () => clock.t, budgetMs: 10000, renewMs: 1000,
    },
  );
  assert.deepEqual(result, FALLBACK);
  assert.deepEqual(actions, ['register', 'drop'], 'a failed arm still deregisters in the finally');
  assert.equal(listener.closed, true);
});

test('a re-arm failure mid-poll (connector died) degrades to allow-stop', async () => {
  const clock = { t: 0 };
  let arms = 0;
  const result = await pollWake(
    { cwd: '/w', session_id: 's' },
    {
      harness: 'claude', fallback: FALLBACK, resolvePaths: RESOLVED,
      listen: async () => scriptedListener([false, false], clock),
      inject: async (action) => {
        if (action === 'drop') return { ok: true, code: 0 };
        arms += 1;
        return { ok: arms === 1, code: arms === 1 ? 0 : 7 }; // first arm ok, re-arm fails
      },
      render: async () => ({ ok: true, stdout: WAKE_BODY, unavailable: false }),
      now: () => clock.t, budgetMs: 10000, renewMs: 1000,
    },
  );
  assert.deepEqual(result, FALLBACK, 'a dead connector mid-poll never hangs the session');
  assert.equal(arms, 2, 'it tried to re-arm before giving up');
});

test('a runtime-unavailable drain does not block (treated as no delivery)', async () => {
  const clock = { t: 0 };
  const result = await pollWake(
    { cwd: '/w', session_id: 's' },
    {
      harness: 'claude', fallback: FALLBACK, resolvePaths: RESOLVED,
      listen: async () => scriptedListener([true, false], clock),
      inject: async () => ({ ok: true, code: 0 }),
      render: async () => ({ unavailable: true, ok: false, stdout: '' }),
      now: () => clock.t, budgetMs: 2000, renewMs: 1000,
    },
  );
  assert.deepEqual(result, FALLBACK);
});

test('startWakeListener fires next() on a loam-wake frame only, and closes cleanly', async () => {
  const hints = [];
  const listener = await startWakeListener({ log: (message, extra) => { if (message === 'wake frame') hints.push(extra.hint); } });
  assert.match(listener.wakeRef, /^notify-tcp:\/\/127\.0\.0\.1:\d+$/);
  const port = listener.port;

  const send = (payload) => new Promise((resolve, reject) => {
    const socket = net.connect({ port, host: '127.0.0.1' });
    socket.once('connect', () => { socket.write(payload); socket.end(); });
    socket.once('close', resolve);
    socket.once('error', reject);
  });

  // A non-wake frame must NOT satisfy a wait.
  const pendingBefore = listener.next(150);
  await send('{"kind":"other"}');
  assert.equal(await pendingBefore, false, 'a non-wake frame is ignored');

  // A loam-wake frame satisfies the next wait and surfaces only the hint.
  const pending = listener.next(1000);
  await send('{"kind":"loam-wake","project":"loam","hint":"01ABC"}');
  assert.equal(await pending, true, 'a loam-wake frame fires the waiter');
  assert.deepEqual(hints, ['01ABC'], 'only the topic-derived hint is logged, never content');

  await listener.close();
  await assert.rejects(send('{"kind":"loam-wake"}'), 'the socket is closed after teardown');
});

// --- Codex contract fixtures -------------------------------------------------

test('codex Stop input schema: the fields the poller reads are the published contract', async () => {
  const schema = JSON.parse(await readFile(join(fixtures, 'stop.command.input.schema.json'), 'utf8'));
  // We read session_id, cwd, and stop_hook_active off the Codex Stop payload —
  // pin them against openai/codex codex-rs/hooks/schema/generated so a contract
  // change breaks the test, not a live session.
  assert.equal(schema.properties.session_id.type, 'string');
  assert.equal(schema.properties.cwd.type, 'string');
  assert.equal(schema.properties.stop_hook_active.type, 'boolean');
  for (const field of ['session_id', 'cwd', 'stop_hook_active']) {
    assert.ok(schema.required.includes(field), `${field} is required by the codex Stop contract`);
  }
});

test('codex Stop output schema: our block-decision conforms to the published shape', async () => {
  const schema = JSON.parse(await readFile(join(fixtures, 'stop.command.output.schema.json'), 'utf8'));
  // pollWake returns { decision: 'block', reason } for BOTH harnesses; the codex
  // output schema is the proof it is a valid codex Stop response, not just Claude's.
  assert.deepEqual(schema.definitions.BlockDecisionWire.enum, ['block'], 'decision is the block wire enum');
  assert.equal(schema.properties.reason.type, 'string');
  assert.equal(schema.additionalProperties, false, 'no stray keys allowed — {decision, reason} is the whole response');
  // Sanity: our literal response validates against the two constrained fields.
  const response = { decision: 'block', reason: WAKE_BODY };
  assert.ok(schema.definitions.BlockDecisionWire.enum.includes(response.decision));
  assert.equal(typeof response.reason, 'string');
});
