import { loadIngestModules } from './ingest-modules.mjs';

const {
  resolveGlobalRoot, resolveSkillsRoot, runWorker, prepareNativeAgentRun,
  startHookWorker, finishHookWorker,
} = await loadIngestModules().catch(() => ({}));

function args(argv) {
  const result = {};
  for (let index = 0; index < argv.length; index += 1) {
    if (argv[index] === '--harness') result.harness = argv[++index];
    else if (argv[index] === '--workspace') result.workspace = argv[++index];
    else if (argv[index] === '--hook-run-id') result.hookRunId = argv[++index];
    else if (argv[index] === '--global-root') result.globalRoot = argv[++index];
    else if (argv[index] === '--agent-id') result.agentId = argv[++index];
    else if (argv[index] === '--native-prepare') result.nativePrepare = true;
  }
  return result;
}

export async function main(options = {}) {
  const parsed = options.harness || options.nativePrepare ? options : args(process.argv.slice(2));
  if (!parsed.workspace) throw new Error('worker requires --workspace');
  const env = options.env || process.env;
  const globalRoot = parsed.globalRoot || env.LOAM_INGEST_GLOBAL_ROOT || resolveGlobalRoot?.({ env });
  if (parsed.nativePrepare) {
    const prepare = options.prepareNativeAgentRun || prepareNativeAgentRun;
    if (!prepare || !parsed.agentId) throw new Error('native preparation requires --agent-id');
    return prepare({
      globalRoot,
      workspace: parsed.workspace,
      agentId: parsed.agentId,
      skillsRoot: options.skillsRoot || env.LOAM_INGEST_SKILLS_ROOT || resolveSkillsRoot?.({ env }),
      env,
    });
  }
  if (!parsed.harness) throw new Error('worker requires --harness');
  const worker = options.runWorker || runWorker;
  const workerStart = options.startHookWorker || startHookWorker;
  const workerFinish = options.finishHookWorker || finishHookWorker;
  const hookRunId = Number(parsed.hookRunId);
  const hookRun = Number.isSafeInteger(hookRunId) && hookRunId > 0
    ? { id: hookRunId, globalRoot, workspace: parsed.workspace }
    : null;
  if (hookRun && workerStart) {
    try { await workerStart({ run: hookRun }); } catch {}
  }
  try {
    if (!worker) throw new Error('loam ingestion integration is unavailable');
    const result = await worker({
      harness: parsed.harness,
      workspace: parsed.workspace,
      globalRoot,
      skillsRoot: options.skillsRoot || env.LOAM_INGEST_SKILLS_ROOT || resolveSkillsRoot?.({ env }),
      env,
    });
    if (hookRun && workerFinish) {
      try {
        await workerFinish({
          run: hookRun,
          reason: result?.reason,
          detail: result?.detail,
          ...(result?.events?.length ? { events: result.events } : {}),
        });
      } catch {}
    }
    return result;
  } catch (error) {
    if (hookRun && workerFinish) {
      try {
        await workerFinish({
          run: hookRun,
          reason: 'unavailable',
          detail: error instanceof Error ? error.message : String(error),
        });
      } catch {}
    }
    throw error;
  }
}

if (process.argv[1] && process.argv[1].endsWith('ingest-worker.mjs')) {
  try {
    const result = await main();
    if (process.argv.includes('--native-prepare')) process.stdout.write(`${JSON.stringify(result)}\n`);
  } catch (error) { process.stderr.write('loam ingest worker: ' + String(error?.message || error) + '\n'); process.exitCode = 1; }
}
