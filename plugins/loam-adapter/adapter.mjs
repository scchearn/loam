import { homedir } from 'node:os';
import { readFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

async function defaultIntegrationPath() {
  if (process.env.LOAM_INTEGRATION_PATH) return process.env.LOAM_INTEGRATION_PATH;
  const fallback = join(homedir(), '.agents', 'loam', 'integration', 'loam.mjs');
  try {
    const metadata = JSON.parse(await readFile(join(homedir(), '.agents', 'loam', 'install.json'), 'utf8'));
    return typeof metadata.integration_path === 'string' ? metadata.integration_path : fallback;
  } catch {
    return fallback;
  }
}

async function defaultIngestModules({ integrationPath } = {}) {
  integrationPath ||= await defaultIntegrationPath();
  const root = new URL('./', pathToFileURL(integrationPath));
  const [paths, ingest] = await Promise.all([
    import(new URL('paths.mjs', root).href),
    import(new URL('ingest.mjs', root).href),
  ]);
  return { ...paths, ...ingest };
}

async function defaultHookModules({ integrationPath } = {}) {
  integrationPath ||= await defaultIntegrationPath();
  const root = new URL('./', pathToFileURL(integrationPath));
  const [paths, hooks] = await Promise.all([
    import(new URL('paths.mjs', root).href),
    import(new URL('hooks.mjs', root).href),
  ]);
  return { ...paths, ...hooks };
}

export function workspaceFromPayload(payload = {}, fallback = process.cwd()) {
  const value = payload.cwd || payload.workspaceRoot || payload.workspace?.root || payload.session?.cwd || fallback;
  return resolve(value);
}

export async function handleMarketplaceStop(payload = {}, {
  harness = 'claude',
  env = process.env,
  loadHooks = defaultHookModules,
  loadIngest = defaultIngestModules,
} = {}) {
  const workspace = workspaceFromPayload(payload);
  let hookRun = null;
  let finishHookRun;
  try {
    const hooks = await loadHooks();
    finishHookRun = hooks.finishHookRun;
    hookRun = await hooks.beginHookRun?.({
      globalRoot: hooks.resolveGlobalRoot({ env }),
      harness,
      hook: 'stop',
      workspace,
      sessionId: typeof payload?.session_id === 'string' ? payload.session_id : undefined,
    });
  } catch {}

  let failure;
  let outcome;
  try {
    const { resolveGlobalRoot, resolveSkillsRoot, dispatchBoundary } = await loadIngest();
    if (!dispatchBoundary) throw new Error('Loam ingestion integration is unavailable');
    outcome = await dispatchBoundary({
      harness,
      payload: {
        session_id: typeof payload?.session_id === 'string' ? payload.session_id : undefined,
        cwd: typeof payload?.cwd === 'string' ? payload.cwd : undefined,
        stop_hook_active: payload?.stop_hook_active === true,
      },
      globalRoot: env.LOAM_INGEST_GLOBAL_ROOT || resolveGlobalRoot({ env }),
      skillsRoot: env.LOAM_INGEST_SKILLS_ROOT || resolveSkillsRoot({ env }),
      hookRunId: hookRun?.id,
      env,
    });
  } catch (error) {
    failure = error;
  }

  if (hookRun && finishHookRun) {
    try {
      await finishHookRun({
        run: hookRun,
        status: failure ? 'failed' : 'succeeded',
        ...(failure
          ? { detail: failure instanceof Error ? failure.message : String(failure) }
          : {
              action: outcome?.action,
              ...(outcome?.reason !== undefined ? { reason: outcome.reason } : {}),
              ...(outcome?.detail !== undefined ? { detail: outcome.detail } : {}),
            }),
      });
    } catch {}
  }
  return {};
}
