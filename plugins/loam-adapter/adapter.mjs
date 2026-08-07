import { homedir } from 'node:os';
import { readFile } from 'node:fs/promises';
import { basename, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const CODEX_START_FAILURE_MESSAGE = 'Loam background ingestion could not start. Run npx @scchearn/loam setup to repair the installation.';

function stopResponse({ harness, visibility, outcome, failure }) {
  if (harness === 'codex' && visibility === 'native' && outcome?.native_continuation) return outcome.native_continuation;
  return harness === 'codex' && visibility === 'toast' && (failure || outcome?.reason === 'unavailable')
    ? { systemMessage: CODEX_START_FAILURE_MESSAGE }
    : {};
}

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

async function defaultContext({ harness, integrationPath, workspace }) {
  try {
    integrationPath ||= await defaultIntegrationPath();
    const integration = await import(pathToFileURL(integrationPath).href);
    const candidates = harness === 'codex' ? ['codex', 'claude'] : [harness];
    for (const [index, candidate] of candidates.entries()) {
      const chunks = [];
      try {
        await integration.runIntegration(
          ['hook', '--harness', candidate, '--workspace', workspace],
          { integrationPath, output: { write: (chunk) => chunks.push(String(chunk)) } },
        );
        return chunks.join('');
      } catch (error) {
        if (index === candidates.length - 1) throw error;
      }
    }
  } catch {
    return '<LOAM_IMPORTANT>\nYou have loam.\nLoam is unavailable. Run: npx @scchearn/loam setup\n</LOAM_IMPORTANT>';
  }
}

export function createMarketplaceAdapter({ harness = 'claude', integrationPath, getContext = defaultContext } = {}) {
  return async (payload = {}) => ({
    hookSpecificOutput: {
      hookEventName: 'SessionStart',
      additionalContext: await getContext({
        harness,
        workspace: workspaceFromPayload(payload),
        integrationPath,
      }),
    },
  });
}

export function createClaudeAdapter(options = {}) {
  return createMarketplaceAdapter({ ...options, harness: 'claude' });
}

export async function handleMarketplaceHook(payload, options = {}) {
  return createMarketplaceAdapter(options)(typeof payload === 'string' ? JSON.parse(payload) : payload);
}

export async function handleClaudeHook(payload, options = {}) {
  return handleMarketplaceHook(payload, { ...options, harness: 'claude' });
}

async function defaultHarvestModules({ integrationPath } = {}) {
  integrationPath ||= await defaultIntegrationPath();
  const root = new URL('./', pathToFileURL(integrationPath));
  try {
    const [paths, harvest] = await Promise.all([
      import(new URL('paths.mjs', root).href),
      import(new URL('harvest.mjs', root).href),
    ]);
    return { ...paths, ...harvest };
  } catch {
    return null;
  }
}

export async function handleMarketplaceStop(payload = {}, {
  harness = 'claude',
  env = process.env,
  loadHooks = defaultHookModules,
  loadIngest = defaultIngestModules,
  loadHarvest = defaultHarvestModules,
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
  let harvestOutcome;
  let visibility = 'silent';
  try {
    const { resolveGlobalRoot, resolveSkillsRoot, readIngestConfig, dispatchBoundary } = await loadIngest();
    if (!dispatchBoundary) throw new Error('Loam ingestion integration is unavailable');
    const globalRoot = env.LOAM_INGEST_GLOBAL_ROOT || resolveGlobalRoot({ env });
    if (harness === 'codex') visibility = (await readIngestConfig?.(globalRoot, env))?.visibility || 'silent';
    const dispatchPayload = {
      session_id: typeof payload?.session_id === 'string' ? payload.session_id : undefined,
      cwd: typeof payload?.cwd === 'string' ? payload.cwd : undefined,
      stop_hook_active: payload?.stop_hook_active === true,
      agent_type: typeof payload?.agent_type === 'string' ? payload.agent_type : undefined,
    };
    outcome = await dispatchBoundary({
      harness,
      payload: dispatchPayload,
      globalRoot,
      skillsRoot: env.LOAM_INGEST_SKILLS_ROOT || resolveSkillsRoot({ env }),
      hookRunId: hookRun?.id,
      env,
    });
    const harvest = await loadHarvest();
    if (harvest?.harvestTick) {
      try {
        harvestOutcome = await harvest.harvestTick({
          harness,
          payload: dispatchPayload,
          globalRoot,
          env,
          hookRunId: hookRun?.id,
        });
      } catch {}
    }
  } catch (error) {
    failure = error;
  }

  if (hookRun && finishHookRun) {
    try {
      const recorded = harvestOutcome?.action === 'spawn_worker'
        ? { action: harvestOutcome.action, reason: 'harvest_dispatched' }
        : null;
      await finishHookRun({
        run: hookRun,
        status: failure ? 'failed' : 'succeeded',
        ...(failure
          ? { detail: failure instanceof Error ? failure.message : String(failure) }
          : recorded || {
              action: outcome?.action,
              ...(outcome?.reason !== undefined ? { reason: outcome.reason } : {}),
              ...(outcome?.detail !== undefined ? { detail: outcome.detail } : {}),
            }),
      });
    } catch {}
  }
  return stopResponse({ harness, visibility, outcome, failure });
}

function shellArg(value, platform = process.platform) {
  const text = String(value);
  return platform === 'win32'
    ? `'${text.replaceAll("'", "''")}'`
    : `'${text.replaceAll("'", `'"'"'`)}'`;
}

function nativeAgentContext(bound, { globalRoot, platform = process.platform } = {}) {
  const command = [
    process.execPath,
    bound.worker_path,
    '--native-prepare',
    '--global-root', globalRoot,
    '--workspace', bound.workspace,
    '--agent-id', bound.agent_id,
  ].map((value) => shellArg(value, platform)).join(' ');
  return [
    'You are the Codex loam_ingestor subagent for this pending Loam run.',
    `Workspace: ${bound.workspace}`,
    `Agent identity: ${bound.agent_id}`,
    `Installed integration: ${bound.integration_path}`,
    `Installed adapter: ${bound.adapter_path}`,
    'Your first action must be exactly this preparation command:',
    command,
    'Parse its one-line JSON output. If action is "skip", stop immediately without invoking any ingestion skill.',
    `Only if action is "run", invoke loam::ingesting-codebase exactly once for ${bound.workspace}.`,
    'Do not spawn or delegate to any other agent.',
  ].join('\n');
}

function nativeHookRun(result, globalRoot) {
  return Number.isSafeInteger(result?.hook_run_id) && result.hook_run_id > 0
    ? { id: result.hook_run_id, globalRoot, workspace: result.workspace }
    : null;
}

export async function handleMarketplaceSubagentStart(payload = {}, {
  harness = 'claude',
  env = process.env,
  loadHooks = defaultHookModules,
  loadIngest = defaultIngestModules,
  platform = process.platform,
} = {}) {
  if (harness !== 'codex' || payload?.agent_type !== 'loam_ingestor') return {};
  try {
    const ingest = await loadIngest();
    const globalRoot = env.LOAM_INGEST_GLOBAL_ROOT || ingest.resolveGlobalRoot({ env });
    const bound = await ingest.bindNativeAgent({
      globalRoot,
      workspace: workspaceFromPayload(payload),
      agentId: payload.agent_id,
    });
    if (bound?.status !== 'bound' && bound?.status !== 'late') return {};
    const run = bound.owns_claim ? nativeHookRun(bound, globalRoot) : null;
    if (run) {
      try { await (await loadHooks()).startHookWorker?.({ run, sessionId: bound.agent_id }); } catch {}
    }
    return {
      hookSpecificOutput: {
        hookEventName: 'SubagentStart',
        additionalContext: nativeAgentContext(bound, { globalRoot, platform }),
      },
    };
  } catch {
    return {};
  }
}

export async function handleMarketplaceSubagentStop(payload = {}, {
  harness = 'claude',
  env = process.env,
  loadHooks = defaultHookModules,
  loadIngest = defaultIngestModules,
} = {}) {
  if (harness !== 'codex' || payload?.agent_type !== 'loam_ingestor') return {};
  try {
    const ingest = await loadIngest();
    const globalRoot = env.LOAM_INGEST_GLOBAL_ROOT || ingest.resolveGlobalRoot({ env });
    const result = await ingest.finalizeNativeAgentRun({
      globalRoot,
      workspace: workspaceFromPayload(payload),
      agentId: payload.agent_id,
      env,
    });
    const run = result?.owns_claim ? nativeHookRun(result, globalRoot) : null;
    if (run) {
      try { await (await loadHooks()).finishHookWorker?.({ run, reason: result.reason }); } catch {}
    }
  } catch {}
  return {};
}

if (process.argv[1] && resolve(process.argv[1]) === resolve(fileURLToPath(import.meta.url))) {
  let input = '';
  process.stdin.setEncoding('utf8');
  for await (const chunk of process.stdin) input += chunk;
  const harness = basename(process.argv[1]).startsWith('codex-') ? 'codex' : 'claude';
  process.stdout.write(`${JSON.stringify(await handleMarketplaceHook(input || '{}', { harness }))}\n`);
}
