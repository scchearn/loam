import { homedir } from 'node:os';
import { spawn } from 'node:child_process';
import { readFile } from 'node:fs/promises';
import { isAbsolute, join } from 'node:path';
import { pathToFileURL } from 'node:url';
import net from 'node:net';

const UNAVAILABLE = '<LOAM_IMPORTANT>\nYou have loam.\nLoam is unavailable. Run: npx @scchearn/loam setup\n</LOAM_IMPORTANT>';

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
// (UserPromptSubmit) renders the federation refresh only.
async function defaultContext({ workspace, event = 'SessionStart' }) {
  return new Promise((settle) => {
    let child;
    try {
      child = spawn(RUNTIME_PATH, ['hook', 'opencode', '--workspace', workspace, '--event', event], {
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
      settle({ ok: false, body: '' });
      return;
    }
    let body = '';
    child.stdout.setEncoding('utf8');
    child.stdout.on('data', (chunk) => { body += chunk; });
    child.once('error', () => settle({ ok: false, body: '' }));
    child.once('close', () => settle({ ok: true, body: body.trim() }));
    child.stdin.on('error', () => {});
    child.stdin.end('{}');
  });
}

/**
 * Start the notify listener and register the wake_ref with the connector.
 * `onWake` receives the rendered body and must inject it into the session.
 * Returns a teardown that stops the listener and deregisters the session.
 */
export async function startLoamNotifyServer({
  workspace,
  sessionId,
  globalRoot,
  onWake,
  register = null,
}) {
  const server = net.createServer((socket) => {
    let frame = '';
    socket.setEncoding('utf8');
    socket.on('data', (chunk) => {
      frame += chunk;
      if (frame.includes('"kind":"loam-wake"')) {
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
  const runDir = join(globalRoot, 'run');
  const reg = register || (async (action, ref) => {
    const args = [
      'federation', 'inject', action,
      '--workspace', workspace,
      '--global-root', globalRoot,
      '--session-id', sessionId,
    ];
    if (ref) args.push('--wake-ref', ref);
    return spawnRuntime(args);
  });
  const registered = await reg('register', wakeRef).catch(() => null);
  return {
    wakeRef,
    registered: Boolean(registered?.ok),
    close: async () => {
      await new Promise((resolve) => server.close(resolve));
      await reg('drop', null).catch(() => {});
    },
  };
}

export function createOpenCodeAdapter({
  client,
  integrationPath,
  getContext = defaultContext,
  ingestion = {},
  hookRuns = {},
  wakeServer = startLoamNotifyServer,
} = {}) {
  const childSessions = new Set();
  const completedChildSessions = new Set();
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

  // Wake injection: render the federation refresh through the same read path
  // the per-turn boundary uses, then push it into the live session with
  // promptAsync. The guard prevents two overlapping injections from queuing
  // the same wake twice; the renderer collapses duplicate items by key anyway.
  const injectWake = async (workspace) => {
    if (loamWake.pending || !loamWake.sessionId || !sdk?.session?.promptAsync) return;
    loamWake.pending = true;
    try {
      const context = await getContext({ harness: 'opencode', workspace, integrationPath, event: 'UserPromptSubmit' });
      if (context && context !== UNAVAILABLE) {
        await sdk.session.promptAsync({
          sessionID: loamWake.sessionId,
          parts: [{ type: 'text', text: context }],
        });
      }
    } catch {
      // Wake is best-effort: a failed injection degrades to the next natural
      // turn boundary, which still drains the mailbox.
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
    // T4: the first transform fire is the session start (full block); every
    // later fire is a per-turn refresh (federation only). The native hook
    // renders the right shape for the event, so the adapter only tracks which
    // boundary it is on.
    let sessionStarted = false;
    return {
    'experimental.chat.messages.transform': async (_input, output) => {
      if (!output?.messages?.length) return;
      const firstUser = output.messages.find((message) => message.info?.role === 'user');
      if (!firstUser?.parts?.length) return;
      const event = sessionStarted ? 'UserPromptSubmit' : 'SessionStart';
      const context = await getContext({ harness: 'opencode', workspace: directory || process.cwd(), integrationPath, event });
      if (!context) return;
      if (!sessionStarted) {
        // First fire of this adapter instance is the SessionStart boundary.
        // OpenCode does not emit session.created for the main session, so the
        // notify listener opens here: once per plugin instance, registered
        // against the session id carried on the first user message.
        sessionStarted = true;
        const childId = firstUser.info?.sessionID || firstUser.info?.session_id;
        const workspace = directory || process.cwd();
        loamWake.sessionId = typeof childId === 'string' ? childId : null;
        void (async () => {
          try {
            loamWake.server = await wakeServer({
              workspace,
              sessionId: loamWake.sessionId || 'unknown',
              globalRoot: resolveLoamRoot(),
              onWake: () => injectWake(workspace),
            });
          } catch {
            // No listener, no wake: the per-turn boundary still delivers.
          }
        })();
      }
      const reference = firstUser.parts[0];
      firstUser.parts.unshift({ ...reference, type: 'text', text: context });
    },
    event: async ({ event } = {}) => {
      if ((event?.type === 'session.deleted' || event?.type === 'session.ended') && loamWake.server) {
        const teardown = loamWake.server;
        loamWake.server = null;
        loamWake.sessionId = null;
        void teardown.close().catch(() => {});
        return;
      }
      if (event?.type !== 'session.idle') return;
      const childId = event.sessionID || event.session_id || event.properties?.sessionID || event.properties?.session_id;
      if (childId && (childSessions.has(childId) || completedChildSessions.has(childId))) return;
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

export const LoamPlugin = async ({ client, directory } = {}) => createOpenCodeAdapter({ client })({ directory });
