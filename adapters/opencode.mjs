import { homedir } from 'node:os';
import { readFile } from 'node:fs/promises';
import { isAbsolute, join } from 'node:path';
import { pathToFileURL } from 'node:url';

const OWN_MARKER = 'You have loam';

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
      const [paths, ingest] = await Promise.all([
        import(new URL('paths.mjs', root).href),
        import(new URL('ingest.mjs', root).href),
      ]);
      let hooks = {};
      try { hooks = await import(new URL('hooks.mjs', root).href); } catch {}
      return { ...paths, ...ingest, ...hooks };
    } catch {}
  }
  throw new Error('loam ingestion integration is unavailable');
}

const {
  resolveGlobalRoot, resolveSkillsRoot, gate, runWorker, beginHookRun, finishHookRun,
} = await loadIngestModules().catch(() => ({}));

async function defaultContext({ integrationPath, workspace }) {
  try {
    integrationPath ||= await defaultIntegrationPath();
    const integration = await import(pathToFileURL(integrationPath).href);
    const chunks = [];
    await integration.runIntegration(
      ['hook', '--harness', 'opencode', '--workspace', workspace],
      { integrationPath, output: { write: (chunk) => chunks.push(String(chunk)) } },
    );
    return chunks.join('');
  } catch {
    return '<LOAM_IMPORTANT>\nYou have loam.\nLoam is unavailable. Run: npx @scchearn/loam setup\n</LOAM_IMPORTANT>';
  }
}

function responseData(response) { return response?.data ?? response; }

export function createOpenCodeAdapter({
  client,
  integrationPath,
  getContext = defaultContext,
  ingestion = {},
  hookRuns = {},
} = {}) {
  const childSessions = new Set();
  const completedChildSessions = new Set();
  const completedOrder = [];
  const COMPLETED_CHILD_MAX = 64;
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
  const hookGlobalRoot = hookRuns.resolveGlobalRoot || resolveGlobalRoot;
  return async ({ directory, client: invocationClient } = {}) => {
    const sdk = client || invocationClient;
    return {
    'experimental.chat.messages.transform': async (_input, output) => {
      if (!output?.messages?.length) return;
      const firstUser = output.messages.find((message) => message.info?.role === 'user');
      if (!firstUser?.parts?.length) return;
      if (firstUser.parts.some((part) => part.type === 'text' && part.text.includes(OWN_MARKER))) return;
      const context = await getContext({ harness: 'opencode', workspace: directory || process.cwd(), integrationPath });
      if (!context) return;
      const reference = firstUser.parts[0];
      firstUser.parts.unshift({ ...reference, type: 'text', text: context });
    },
    event: async ({ event } = {}) => {
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
      try {
        if (!ingestGate || !ingestWorker || !ingestGlobalRoot || !ingestSkillsRoot) {
          throw new Error('Loam ingestion integration is unavailable');
        }
        const globalRoot = ingestGlobalRoot({ env, integrationPath });
        const skillsRoot = ingestSkillsRoot({ env });
        const gated = await ingestGate({
          harness: 'opencode',
          payload: { cwd: workspace, event_id: event.id, session_id: childId },
          globalRoot,
          env,
        });
        if (gated.action === 'spawn_worker' && sdk?.session) {
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
          void Promise.resolve(ingestWorker({
            harness: 'opencode',
            workspace: gated.workspace,
            globalRoot,
            skillsRoot,
            env,
            openCodeSession: childSession,
          })).catch(() => {}).finally(() => {
            if (childSession.lastChildId) rememberCompleted(childSession.lastChildId);
          });
        }
      } catch (error) {
        failure = error;
      }
      if (hookRun && hookFinish) {
        try {
          await hookFinish({
            run: hookRun,
            status: failure ? 'failed' : 'succeeded',
            ...(failure ? { detail: failure instanceof Error ? failure.message : String(failure) } : {}),
          });
        } catch {}
      }
    },
    };
  };
}

export const LoamPlugin = async ({ client, directory } = {}) => createOpenCodeAdapter({ client })({ directory });
