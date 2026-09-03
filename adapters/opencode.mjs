import { homedir } from 'node:os';
import { spawn } from 'node:child_process';
import { readFile } from 'node:fs/promises';
import { isAbsolute, join } from 'node:path';
import { pathToFileURL } from 'node:url';
import net from 'node:net';

const UNAVAILABLE = '<LOAM_IMPORTANT>\nYou have loam.\nLoam is unavailable. Run: npx @scchearn/loam install\n</LOAM_IMPORTANT>';

// Filled in by setup when this plugin is staged. OpenCode loads the plugin
// in-process, so the absolute private runtime is written here rather than
// resolved at session time; setup rewrites it whenever the runtime moves.
const RUNTIME_PATH = "__LOAM_RUNTIME_PATH__";

async function defaultIntegrationPath() {
  if (process.env.LOAM_INTEGRATION_PATH) return process.env.LOAM_INTEGRATION_PATH;
  const globalRoot = process.env.LOAM_HOME && isAbsolute(process.env.LOAM_HOME)
    ? process.env.LOAM_HOME : join(homedir(), '.agents', 'loam');
  const fallback = join(globalRoot, 'integration', 'loam.mjs');
  try {
    const metadata = JSON.parse(await readFile(join(globalRoot, 'install.json'), 'utf8'));
    return typeof metadata.integration_path === 'string' ? metadata.integration_path : fallback;
  } catch {
    return fallback;
  }
}

async function loadIngestModules() {
  const roots = [];
  const integrationPath = await defaultIntegrationPath();
  if (integrationPath) roots.push(new URL('./', pathToFileURL(integrationPath)));
  for (const root of roots) {
    try {
      const [paths, ingest, harvest] = await Promise.all([
        import(new URL('paths.mjs', root).href),
        import(new URL('ingest.mjs', root).href),
        import(new URL('harvest.mjs', root).href).catch(() => ({})),
      ]);
      let hooks = {};
      try { hooks = await import(new URL('hooks.mjs', root).href); } catch {}
      return { ...paths, ...ingest, ...harvest, ...hooks };
    } catch {}
  }
  throw new Error('loam ingestion integration is unavailable');
}

const {
  resolveGlobalRoot, resolveSkillsRoot, gate, runWorker,
  beginHookRun, finishHookRun, startHookWorker, finishHookWorker,
  harvestTick, runHarvest: harvestRunWorker,
} = await loadIngestModules().catch(() => ({}));

// The whole OpenCode context surface: run the native read path and take its
// stdout. No shared Node integration, no IPC of our own, no broker. The event
// flag splits the injection: SessionStart renders the full block, per-turn
// (UserPromptSubmit) renders the federation refresh only. The Wake event drains
// this session's mailbox and REQUIRES the session id — without it the runtime
// refuses the drain and renders empty — so it is passed on argv as --session-id.
function buildHookArgs({ workspace, event = 'SessionStart', sessionId = null }) {
  const args = ['hook', 'opencode', '--workspace', workspace, '--event', event];
  if (sessionId) args.push('--session-id', sessionId);
  return args;
}

async function defaultContext({ workspace, event = 'SessionStart', sessionId = null }) {
  return new Promise((settle) => {
    let child;
    try {
      child = spawn(RUNTIME_PATH, buildHookArgs({ workspace, event, sessionId }), {
        stdio: ['pipe', 'pipe', 'ignore'],
      });
    } catch {
      settle(UNAVAILABLE);
      return;
    }
    let body = '';
    child.stdout.setEncoding('utf8');
    child.stdout.on('data', (chunk) => { body += chunk; });
    child.once('error', () => settle(UNAVAILABLE));
    child.once('close', () => settle(body.trim() || UNAVAILABLE));
    child.stdin.on('error', () => {});
    child.stdin.end('{}');
  });
}

function responseData(response) { return response?.data ?? response; }

// ---------------------------------------------------------------------------
// Live wake server (live-push T4): a localhost notify listener that receives
// the connector's one-shot wake frame and injects the rendered federation
// refresh into the live session with `client.session.promptAsync()` — the item
// lands before the agent's next user message. The plugin opens no connection
// to the connector; it only listens on 127.0.0.1 and calls the native runtime.
// ---------------------------------------------------------------------------

const WAKE_KIND = 'loam-wake';

async function spawnRuntime(args) {
  return new Promise((settle) => {
    let child;
    try {
      child = spawn(RUNTIME_PATH, args, { stdio: ['pipe', 'pipe', 'ignore'] });
    } catch {
      settle({ ok: false, code: null, body: '' });
      return;
    }
    let body = '';
    child.stdout.setEncoding('utf8');
    child.stdout.on('data', (chunk) => { body += chunk; });
    child.once('error', () => settle({ ok: false, code: null, body: '' }));
    // Honor the exit code: a nonzero runtime exit (e.g. an inject-contract
    // rejection) is a failure, not a success with empty output. Reporting
    // ok:true on any close hid the register bug — the connector never held the
    // session wake_ref and no one saw it fail. `code` is surfaced so the wake
    // breadcrumbs can name the exit when a register/drop fails.
    child.once('close', (code) => settle({ ok: code === 0, code, body: body.trim() }));
    child.stdin.on('error', () => {});
    child.stdin.end('{}');
  });
}

/**
 * Build the argv for `federation inject <register|drop>`. Workspace is a
 * POSITIONAL argument in the CLI contract, never `--workspace` (which the
 * runtime rejects as an unknown flag, exit 64). Module-internal; reached by a
 * contract test via `LoamPlugin.buildInjectArgs` so the shape can't drift back
 * to the flag form. It must NOT be a top-level export — see the note on
 * `LoamPlugin` below.
 */
function buildInjectArgs({ action, workspace, globalRoot, sessionId, wakeRef = null }) {
  const args = [
    'federation', 'inject', action,
    workspace,
    '--global-root', globalRoot,
    '--session-id', sessionId,
  ];
  if (wakeRef) args.push('--wake-ref', wakeRef);
  return args;
}

/**
 * Start the notify listener and register the wake_ref with the connector.
 * `onWake` receives the rendered body and must inject it into the session.
 * `log` (optional) is the guarded lifecycle breadcrumb sink: it is called with
 * (message, extra) for each wake frame (hint only), register, and drop — ids,
 * ports, and exit codes only, never message content. Returns a teardown that
 * stops the listener and deregisters the session.
 */
async function startLoamNotifyServer({
  workspace,
  sessionId,
  globalRoot,
  onWake,
  register = null,
  log = null,
}) {
  const server = net.createServer((socket) => {
    let frame = '';
    socket.setEncoding('utf8');
    socket.on('data', (chunk) => {
      frame += chunk;
      if (frame.includes('"kind":"loam-wake"')) {
        // The hint is a topic-derived event id, never sender content.
        const hint = (frame.match(/"hint":"([^"]*)"/) || [])[1] || '';
        void log?.('wake frame', { hint });
        socket.end();
        void onWake();
      }
    });
    socket.on('error', () => {});
  });
  await new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', resolve);
  });
  const address = server.address();
  const port = typeof address === 'object' && address ? address.port : 0;
  const wakeRef = `notify-tcp://127.0.0.1:${port}`;
  const reg = register || (async (action, ref) =>
    spawnRuntime(buildInjectArgs({ action, workspace, globalRoot, sessionId, wakeRef: ref })));
  const runReg = async (action, ref) => {
    const result = await reg(action, ref).catch(() => ({ ok: false, code: null }));
    await log?.(`wake ${action}`, { action, ok: Boolean(result?.ok), exit: result?.code ?? null });
    return result;
  };
  const registered = await runReg('register', wakeRef);
  return {
    wakeRef,
    port,
    registered: Boolean(registered?.ok),
    // Re-assert this session's wake_ref (connector-self-healing): an idempotent
    // upsert on the connector side, so a connector restart that dropped the live
    // channel re-attaches this active session's mailbox at the next hook — the
    // belt-and-braces beside the connector's own persisted-wake reload.
    reregister: async () => runReg('register', wakeRef),
    close: async () => {
      await new Promise((resolve) => server.close(resolve));
      await runReg('drop', null);
    },
  };
}

function createOpenCodeAdapter({
  client,
  integrationPath,
  getContext = defaultContext,
  ingestion = {},
  hookRuns = {},
  wakeServer = startLoamNotifyServer,
} = {}) {
  const childSessions = new Set();
  const completedChildSessions = new Set();
  const autoContinueCounts = new Map(); // sessionId -> continues this user turn
  const completedOrder = [];
  const COMPLETED_CHILD_MAX = 64;
  // Live wake state (live-push T4), kept per adapter instance so tests that
  // create several adapters in one process cannot cross-contaminate.
  const loamWake = { server: null, pending: false, sessionId: null };
  // sdk is assigned in the inner function below; declared here so injectWake's
  // closure can see it (it's in the outer scope, not the inner return function).
  let sdk = null;

  const resolveLoamRoot = () => {
    if (process.env.LOAM_HOME && isAbsolute(process.env.LOAM_HOME)) return process.env.LOAM_HOME;
    return join(homedir(), '.agents', 'loam');
  };

  // Lifecycle breadcrumbs (all via the deadlock-guarded appLog, service "loam").
  // The rules: only stage names, ids, ports, exit codes, and counts ever reach
  // the log — never message or summary CONTENT; lifecycle transitions log always;
  // per-turn paths log first-fire + failures only (logStageOnce), never per-turn
  // spam; and a logging failure can never break the plugin (appLog swallows it).
  const loggedStages = new Set();
  const logStage = (level, message, extra) => appLog(sdk, level, message, extra);
  const logStageOnce = (key, level, message, extra) => {
    if (loggedStages.has(key)) return Promise.resolve();
    loggedStages.add(key);
    return logStage(level, message, extra);
  };

  // Render the native context for one event and breadcrumb the outcome: byte
  // count, status (ok/empty/unavailable/threw), and whether a session id was
  // supplied — per event on first success and on EVERY non-ok render. Sender text
  // is never logged — only the size, the status class, and the session-present
  // bit. The session bit matters most for Wake: a drain that comes back empty
  // because no session id was passed (session:false) must be loudly
  // distinguishable from a genuine connector refusal (session:true).
  const renderContext = async (event, workspace, sessionId = null) => {
    let context = '';
    try {
      context = await getContext({ harness: 'opencode', workspace, integrationPath, event, sessionId });
    } catch {
      await logStage('error', 'getContext threw', { event, session: Boolean(sessionId) });
      return '';
    }
    const bytes = typeof context === 'string' ? context.length : 0;
    const status = context === UNAVAILABLE ? 'unavailable' : (!context ? 'empty' : 'ok');
    const extra = { event, bytes, status, session: Boolean(sessionId) };
    if (status === 'ok') await logStageOnce(`getContext:${event}`, 'info', 'getContext', extra);
    else await logStage('warn', 'getContext', extra);
    return context;
  };

  // Wake injection (wake-injection-delta): the native `Wake` event drains this
  // session's connector mailbox — the single per-session seen-set authority, the
  // same consume-once mechanism the per-turn boundary uses — and renders the
  // drained items as terse elements closed by one [tip] trailer. The mailbox did
  // the delta selection, so an empty drain renders nothing (the runtime returns
  // its empty sentinel) and this injects nothing: a wake with no new item is a
  // no-op. The pending guard collapses two overlapping wakes; a wake that races a
  // per-turn drain simply finds the mailbox already empty.
  const injectWake = async (workspace) => {
    if (loamWake.pending || !loamWake.sessionId || !sdk?.session?.promptAsync) {
      await logStage('info', 'wake inject skipped', { reason: loamWake.pending ? 'pending' : (!loamWake.sessionId ? 'no-session' : 'no-sdk') });
      return;
    }
    loamWake.pending = true;
    try {
      // The Wake drain keys off the session id this instance registered — pass it
      // through, or the runtime refuses the drain and renders empty.
      const context = await renderContext('Wake', workspace, loamWake.sessionId);
      // The drained item count is the number of terse element tags — a structural
      // type token, never sender content. An empty drain is a no-op.
      const drained = typeof context === 'string' ? (context.match(/<io\.loam/g) || []).length : 0;
      if (!context || context === UNAVAILABLE) {
        await logStage('info', 'wake inject', { outcome: 'empty', drained: 0 });
        return;
      }
      // SessionPromptAsyncData shape: { path:{id}, query:{directory}, body:{parts} } —
      // the same shape the ingest worker below uses. The flat
      // { sessionID, parts } form is silently a no-op against the SDK.
      const result = await sdk.session.promptAsync({
        path: { id: loamWake.sessionId },
        query: { directory: workspace },
        body: { parts: [{ type: 'text', text: context }] },
      });
      // The SDK RESOLVES with { error, response } on an HTTP error rather than
      // throwing, so a rejected prompt would otherwise pass as success. Treat a
      // resolved error as a failed injection — it degrades to the next turn.
      if (result && result.error) {
        await logStage('warn', 'wake inject', { outcome: 'resolved-error', drained });
        return;
      }
      await logStage('info', 'wake inject', { outcome: 'ok', drained });
    } catch {
      // Wake is best-effort: a genuine throw (e.g. promptAsync itself rejecting)
      // degrades to the next natural turn boundary, which still drains the mailbox.
      await logStage('warn', 'wake inject', { outcome: 'threw' });
    } finally {
      loamWake.pending = false;
    }
  };
  const rememberCompleted = (id) => {
    childSessions.delete(id);
    if (completedChildSessions.has(id)) return;
    completedChildSessions.add(id);
    completedOrder.push(id);
    while (completedOrder.length > COMPLETED_CHILD_MAX) completedChildSessions.delete(completedOrder.shift());
  };
  const ingestGate = ingestion.gate || gate;
  const ingestWorker = ingestion.runWorker || runWorker;
  const ingestGlobalRoot = ingestion.resolveGlobalRoot || resolveGlobalRoot;
  const ingestSkillsRoot = ingestion.resolveSkillsRoot || resolveSkillsRoot;
  const hookBegin = hookRuns.beginHookRun || beginHookRun;
  const hookFinish = hookRuns.finishHookRun || finishHookRun;
  const hookWorkerStart = hookRuns.startHookWorker || startHookWorker;
  const hookWorkerFinish = hookRuns.finishHookWorker || finishHookWorker;
  const hookGlobalRoot = hookRuns.resolveGlobalRoot || resolveGlobalRoot;
  return async ({ directory, client: invocationClient } = {}) => {
    sdk = client || invocationClient;
    // #209: the OpenCode transform hook is per-model-call and never persisted, so
    // context injected through it lived for one LLM call and vanished. Each kind
    // of content now rides the hook whose lifetime matches it:
    //   - SessionStart render  -> experimental.chat.system.transform (per call,
    //                             stable prefix) cached per session id.
    //   - UserPromptSubmit render (federation + drained mailbox) -> chat.message
    //                             (once per user turn, mutation persisted).
    //   - post-compaction     -> experimental.session.compacting.
    // Per-session baseline cache. Replaces the instance-wide sessionStarted flag;
    // in a one-server-many-sessions process every session gets its own baseline.
    const sessions = new Map();
    const sessionState = (id) => {
      let s = sessions.get(id);
      if (!s) { s = { baseline: null }; sessions.set(id, s); }
      return s;
    };
    // The wake listener is a single localhost TCP server per adapter instance,
    // injecting into loamWake.sessionId; it can serve only one session, so it is
    // opened once per instance (last-writer-wins across sessions, as today).
    // Keying the wake channel per session is out of scope here (#209 Step 4).
    let wakeListenerOpened = false;
    return {
    'experimental.chat.system.transform': async (input, output) => {
      const sessionId = typeof input?.sessionID === 'string' ? input.sessionID : null;
      if (!sessionId || !Array.isArray(output?.system)) return;
      const workspace = directory || process.cwd();
      const state = sessionState(sessionId);

      if (!wakeListenerOpened) {
        // SessionStart boundary. OpenCode emits no session.created for the main
        // session, so the notify listener opens on the first system-transform
        // fire, registered against this session id. UNCONDITIONAL and before the
        // render gate — registration must not be hostage to a contentful render,
        // or a session whose first render is empty (failed spawn, quiet
        // federation) would never register and never wake.
        wakeListenerOpened = true;
        loamWake.sessionId = sessionId;
        void (async () => {
          try {
            loamWake.server = await wakeServer({
              workspace,
              sessionId,
              globalRoot: resolveLoamRoot(),
              onWake: () => injectWake(workspace),
              log: (message, extra) => logStage('info', message, extra),
            });
            await logStage('info', 'wake listener opened', { port: loamWake.server?.port ?? null, registered: Boolean(loamWake.server?.registered) });
          } catch (error) {
            // No listener, no wake: the per-turn boundary still delivers. Name
            // the failure so the next restart's log says what actually broke.
            await logStage('error', 'wake listener failed', { error: String(error) });
          }
        })();
      }

      // First render for this session: the full SessionStart block. No session
      // id, no drain — the runtime's SessionStart arm registers and
      // drain-and-discards itself, so the per-turn path starts from a clean
      // mailbox. Cached and reused byte-for-byte on every later call so the
      // runtime binary spawns once per session and provider prompt caching pays
      // for the ~22 KB block once.
      if (state.baseline === null) {
        const rendered = await renderContext('SessionStart', workspace, null);
        state.baseline = rendered || '';
        await logStageOnce('hook:system-transform', 'info', 'hook system transform first fire', { boundary: 'SessionStart' });
      }
      // Push, do not unshift: OpenCode's own environment and agent prompt lead,
      // loam's block trails, keeping OpenCode's prefix stable.
      if (state.baseline) output.system.push(state.baseline);
    },
    'chat.message': async (input, output) => {
      const sessionId = typeof input?.sessionID === 'string' ? input.sessionID : null;
      if (!sessionId || !Array.isArray(output?.parts)) return;
      const workspace = directory || process.cwd();
      // New user turn: reset this session's auto-continue budget.
      autoContinueCounts.delete(sessionId);

      if (loamWake.server?.reregister) {
        // Per-turn idempotent re-registration (connector-self-healing): a
        // connector restart drops the live channel; an active session re-attaches
        // its mailbox at the next turn. Best-effort, unconditional.
        void loamWake.server.reregister().catch(() => {});
        await logStageOnce('hook:reregister', 'info', 'wake re-register', {});
      }

      // Per-turn: drain THIS session's mailbox (the input session id, not the
      // instance-wide loamWake.sessionId, is what makes the drain per-session)
      // and render the federation delta. The text is written into the user
      // message before it is stored, so a drained item stays in the transcript
      // for the rest of the session — the one-seen-set contract finally holds on
      // OpenCode. UNAVAILABLE is surfaced once via the system prompt (baseline),
      // never written into a persisted message every turn.
      const context = await renderContext('UserPromptSubmit', workspace, sessionId);
      if (!context || context === UNAVAILABLE) return;
      // OpenCode 1.18.27 validates user parts before save: each part requires
      // id, sessionID, and messageID, and a bare { type, text } fails with a
      // SchemaError ("Missing key at [id],[sessionID],[messageID]"). Mint an id
      // from a sibling part — the same 12-hex time prefix with an all-zero tail,
      // so the injected part sorts first in the message — and borrow the
      // sibling's sessionID/messageID. No sibling part, no safe id: skip rather
      // than emit an invalid part (the system prompt still carries the baseline).
      const ref = output.parts.find((part) => typeof part?.id === 'string' && part.id.startsWith('prt_'));
      if (!ref) return;
      // synthetic:true keeps the part out of the TUI (OpenCode filters
      // `type==='text' && !synthetic` for display and skips synthetic-only
      // messages for title generation) while still sending it to the model.
      output.parts.unshift({
        id: `${ref.id.slice(0, 16)}00000000000000`,
        sessionID: ref.sessionID || sessionId,
        messageID: ref.messageID || output.message?.id,
        type: 'text',
        text: context,
        synthetic: true,
      });
    },
    'experimental.session.compacting': async (input, output) => {
      // Belt-and-braces: compaction summarises the transcript, so re-supply the
      // cached baseline into the retained context. With the system-transform hook
      // pushing it every call this is redundant today; it matters if a future
      // change stops pushing the baseline on every call.
      const sessionId = typeof input?.sessionID === 'string' ? input.sessionID : null;
      if (!sessionId || !Array.isArray(output?.context)) return;
      const state = sessions.get(sessionId);
      if (state?.baseline) output.context.push(state.baseline);
    },
    event: async ({ event } = {}) => {
      await logStageOnce('hook:event', 'info', 'hook event first fire', { type: event?.type });
      if (event?.type === 'session.deleted' || event?.type === 'session.ended') {
        const endedId = event.sessionID || event.session_id || event.properties?.sessionID || event.properties?.session_id;
        if (typeof endedId === 'string') sessions.delete(endedId);
        if (loamWake.server) {
          const teardown = loamWake.server;
          loamWake.server = null;
          loamWake.sessionId = null;
          wakeListenerOpened = false;
          await logStage('info', 'wake listener closed', { type: event?.type });
          void teardown.close().catch(() => {});
        }
        return;
      }
      if (event?.type !== 'session.idle') return;
      const childId = event.sessionID || event.session_id || event.properties?.sessionID || event.properties?.session_id;
      if (childId && (childSessions.has(childId) || completedChildSessions.has(childId))) return;
      // --- auto-continue on truncated / near-cap assistant response ---
      const AUTO_CONTINUE_SOFT_LIMIT = Infinity; // truncation-only: near-cap heuristic off. Set to e.g. 115_000 (~88% of ~131k cap) to also continue clean finishes glued to the cap.
      const AUTO_CONTINUE_MAX = 3;              // per user turn
      const AUTO_CONTINUE_TEXT =
        '[auto-continue] Your previous response was interrupted by the output limit. ' +
        'Continue EXACTLY where you left off — do not repeat anything already said or done.';

      if (childId && sdk?.session?.messages && sdk?.session?.promptAsync &&
          (autoContinueCounts.get(childId) ?? 0) < AUTO_CONTINUE_MAX) {
        const acWorkspace = directory || event.directory || process.cwd();
        try {
          const res = await sdk.session.messages({ path: { id: childId }, query: { directory: acWorkspace } });
          const list = responseData(res) || [];
          let last = null;
          for (let i = list.length - 1; i >= 0; i--) {
            const m = list[i]?.info ?? list[i];
            if (m?.role === 'assistant') { last = m; break; }
          }
          if (last) {
            const truncated = last.error?.name === 'MessageOutputLengthError' || last.finish === 'length';
            const nearCap = (last.tokens?.output ?? 0) >= AUTO_CONTINUE_SOFT_LIMIT;
            if (truncated || nearCap) {
              autoContinueCounts.set(childId, (autoContinueCounts.get(childId) ?? 0) + 1);
              await logStage('info', 'auto-continue', {
                session: childId, n: autoContinueCounts.get(childId), truncated, output: last.tokens?.output ?? null,
              });
              await sdk.session.promptAsync({
                path: { id: childId },
                query: { directory: acWorkspace },
                body: { parts: [{ type: 'text', text: AUTO_CONTINUE_TEXT }] },
              });
              return; // synthetic continue-turn re-idles on its own; don't also harvest now
            }
          }
        } catch { /* best-effort: never disturb the session */ }
      }
      const workspace = directory || event.directory || process.cwd();
      const env = process.env;
      let hookRun = null;
      if (hookBegin && hookGlobalRoot) {
        try {
          hookRun = await hookBegin({
            globalRoot: hookGlobalRoot({ env, integrationPath }),
            harness: 'opencode',
            hook: 'session_idle',
            workspace,
            sessionId: typeof childId === 'string' ? childId : undefined,
          });
        } catch {}
      }
      let failure;
      let gated;
      let harvestGated;
      try {
        if (!ingestGate || !ingestWorker || !ingestGlobalRoot || !ingestSkillsRoot) {
          throw new Error('Loam ingestion integration is unavailable');
        }
        const globalRoot = ingestGlobalRoot({ env, integrationPath });
        const skillsRoot = ingestSkillsRoot({ env });
        gated = await ingestGate({
          harness: 'opencode',
          payload: { cwd: workspace, event_id: event.id, session_id: childId },
          globalRoot,
          env,
        });
        if (gated.action === 'spawn_worker' && sdk?.session) {
          const notify = typeof sdk?.tui?.showToast === 'function'
            ? ({ phase, status, visibility, signal }) => {
                if (visibility !== 'toast') return;
                const failed = phase === 'terminal' && status !== 'ok';
                return sdk.tui.showToast({
                  query: { directory: workspace },
                  body: {
                    title: 'Loam',
                    message: phase === 'launch'
                      ? 'Background code ingestion started.'
                      : failed ? 'Background code ingestion failed.' : 'Background code ingestion completed.',
                    variant: phase === 'launch' ? 'info' : failed ? 'error' : 'success',
                  },
                  signal,
                });
              }
            : undefined;
          const childSession = {
            parentSessionId: childId,
            createChild: async ({ parentId, title }) => {
              const child = responseData(await sdk.session.create({
                query: { directory: workspace },
                body: { parentID: parentId, title },
              }));
              const id = child?.id || child?.session_id || child?.sessionID;
              if (id) { childSession.lastChildId = String(id); childSessions.add(String(id)); }
              return child;
            },
            promptAsync: async ({ sessionId, parts }) => {
              await sdk.session.promptAsync({
                path: { id: sessionId },
                query: { directory: workspace },
                body: { parts },
              });
              childSessions.add(String(sessionId));
            },
            status: async (id) => {
              if (typeof sdk.session.status !== 'function') throw new Error('OpenCode session status is unavailable');
              const response = responseData(await sdk.session.status({ query: { directory: workspace } }));
              return response?.[id] || response?.data?.[id] || response;
            },
            abort: async (id) => {
              if (typeof sdk.session.abort !== 'function') throw new Error('OpenCode session abort is unavailable');
              return responseData(await sdk.session.abort({ path: { id }, query: { directory: workspace } }));
            },
          };
          void (async () => {
            if (hookRun && hookWorkerStart) {
              try { await hookWorkerStart({ run: hookRun }); } catch {}
            }
            try {
              const result = await ingestWorker({
                harness: 'opencode',
                workspace: gated.workspace,
                globalRoot,
                skillsRoot,
                env,
                openCodeSession: childSession,
                hookRun,
                notify,
              });
              if (hookRun && hookWorkerFinish) {
                try {
                  await hookWorkerFinish({
                    run: hookRun,
                    reason: result?.reason,
                    ...(result?.detail !== undefined ? { detail: result.detail } : {}),
                    ...(result?.events?.length ? { events: result.events } : {}),
                  });
                } catch {}
              }
            } catch (error) {
              if (hookRun && hookWorkerFinish) {
                try {
                  await hookWorkerFinish({
                    run: hookRun,
                    reason: 'unavailable',
                    detail: error instanceof Error ? error.message : String(error),
                  });
                } catch {}
              }
            } finally {
              if (childSession.lastChildId) rememberCompleted(childSession.lastChildId);
            }
          })();
        }
        if (harvestTick && harvestRunWorker) {
          try {
            harvestGated = await harvestTick({
              harness: 'opencode',
              payload: { cwd: workspace, session_id: childId },
              globalRoot,
              env,
            });
            if (harvestGated?.action === 'spawn_worker' && sdk?.session) {
              void (async () => {
                const childSession = {
                  parentSessionId: childId,
                  createChild: async ({ parentId, title }) => {
                    const child = responseData(await sdk.session.create({
                      query: { directory: workspace },
                      body: { parentID: parentId, title: title || 'Loam background session harvest' },
                    }));
                    const id = child?.id || child?.session_id || child?.sessionID;
                    if (id) { childSession.lastChildId = String(id); childSessions.add(String(id)); }
                    return child;
                  },
                  promptAsync: async ({ sessionId, parts }) => {
                    await sdk.session.promptAsync({
                      path: { id: sessionId },
                      query: { directory: workspace },
                      body: { parts },
                    });
                    childSessions.add(String(sessionId));
                  },
                  status: async (id) => {
                    if (typeof sdk.session.status !== 'function') throw new Error('OpenCode session status is unavailable');
                    const response = responseData(await sdk.session.status({ query: { directory: workspace } }));
                    return response?.[id] || response?.data?.[id] || response;
                  },
                  abort: async (id) => {
                    if (typeof sdk.session.abort !== 'function') throw new Error('OpenCode session abort is unavailable');
                    return responseData(await sdk.session.abort({ path: { id }, query: { directory: workspace } }));
                  },
                };
                try {
                  await harvestRunWorker({
                    harness: 'opencode',
                    workspace: harvestGated.workspace || workspace,
                    sessionId: childId,
                    globalRoot,
                    skillsRoot,
                    env,
                    openCodeSession: childSession,
                    hookRun,
                  });
                } catch {}
                finally {
                  if (childSession.lastChildId) rememberCompleted(childSession.lastChildId);
                }
              })();
            }
          } catch {}
        }
      } catch (error) {
        failure = error;
      }
      if (hookRun && hookFinish) {
        try {
          const harvestRecorded = harvestGated?.action === 'spawn_worker'
            ? { action: harvestGated.action, reason: 'harvest_dispatched' }
            : null;
          await hookFinish({
            run: hookRun,
            status: failure ? 'failed' : 'succeeded',
            ...(failure
              ? { detail: failure instanceof Error ? failure.message : String(failure) }
              : harvestRecorded || {
                  action: gated?.action,
                  ...(gated?.reason !== undefined ? { reason: gated.reason } : {}),
                  ...(gated?.detail !== undefined ? { detail: gated.detail } : {}),
                }),
          });
        } catch {}
      }
    },
    };
  };
}

/**
 * Best-effort breadcrumb to opencode's own app logger, so whether the plugin
 * loaded (and why not) is answerable from the opencode log in seconds. Wrapped
 * in try/catch because `client.app.log()` during plugin init can deadlock on
 * some opencode versions — it can re-enter a middleware cycle before the app is
 * ready — so the guard is load-bearing, not decorative; stderr is the fallback.
 */
async function appLog(client, level, message, extra) {
  try {
    await client?.app?.log?.({
      body: { service: 'loam', level, message, ...(extra ? { extra } : {}) },
    });
  } catch {
    const line = `[loam] ${level.toUpperCase()}: ${message}`;
    console.error(extra ? `${line} ${JSON.stringify(extra)}` : line);
  }
}

// This file is copied verbatim as the OpenCode plugin file. OpenCode's loader
// prefers the V1 record shape: `readV1Plugin` detects `mod.default` as a record
// with `id` + `server`, registers it under a real plugin identity, and never
// reaches the legacy path. The legacy path (`getLegacyPlugins`) iterates ALL of
// a plugin file's module exports and calls EVERY exported function as a plugin
// factory — e.g. `startLoamNotifyServer` throws under a plugin-shaped call — so
// even with the V1 default present, the only other export stays `LoamPlugin`,
// and stray function exports remain banned as a legacy-path fallback hazard.
// The `server` function is the plugin: (input, options) => plugin handlers.
// Anything tests need is hung off `LoamPlugin` as a property (a property, not an
// export, so no loader path can mistake it for a plugin). An `export-surface`
// contract test pins both contracts. See the local Federation debugging notes.
//
// Factory-local state (childSessions, loamWake, per-session baseline map, sdk) all lives
// inside createOpenCodeAdapter and its returned closure, so an instance-disposal
// re-run of this factory (opencode reruns it on client.config.update) rebuilds
// that state fresh — there is no module-level per-session state to go stale.
export const LoamPlugin = async (input = {}, _options) => {
  const { client, directory } = input || {};
  // First thing: a "plugin loading" breadcrumb (best-effort; see appLog).
  await appLog(client, 'info', 'plugin loading', { directory });
  try {
    const hooks = await createOpenCodeAdapter({ client })({ directory });
    // The plugin is built: the hook-key count confirms the registration is
    // non-empty (silent loaded-but-empty is the class this makes visible).
    await appLog(client, 'info', 'plugin built', { hooks: Object.keys(hooks || {}).length });
    return hooks;
  } catch (error) {
    // A silent loaded-but-empty registration is the failure class that cost a
    // live session: log FATAL through the same path and RETHROW so the loader
    // surfaces the failure instead of registering no hooks.
    await appLog(client, 'error', `plugin factory failed: ${String(error)}`, { directory });
    throw error;
  }
};

LoamPlugin.buildInjectArgs = buildInjectArgs;
LoamPlugin.buildHookArgs = buildHookArgs;
LoamPlugin.startLoamNotifyServer = startLoamNotifyServer;
LoamPlugin.createOpenCodeAdapter = createOpenCodeAdapter;

// The V1 registration record: the preferred loader path. `id` gives the plugin
// a stable identity; `server` is the plugin function above.
export default { id: 'loam', server: LoamPlugin };
