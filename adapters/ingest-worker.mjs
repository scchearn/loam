import { loadIngestModules } from './ingest-modules.mjs';

const {
  resolveGlobalRoot, resolveSkillsRoot, runWorker, startHookWorker, finishHookWorker,
} = await loadIngestModules().catch(() => ({}));

function args(argv) {
  const result = {};
  for (let index = 0; index < argv.length; index += 1) {
    if (argv[index] === '--harness') result.harness = argv[++index];
    else if (argv[index] === '--workspace') result.workspace = argv[++index];
    else if (argv[index] === '--hook-run-id') result.hookRunId = argv[++index];
  }
  return result;
}

export async function main(options = {}) {
  const parsed = options.harness ? options : args(process.argv.slice(2));
  if (!parsed.harness || !parsed.workspace) throw new Error('worker requires --harness and --workspace');
  const env = options.env || process.env;
  const globalRoot = options.globalRoot || env.LOAM_INGEST_GLOBAL_ROOT || resolveGlobalRoot?.({ env });
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
      try { await workerFinish({ run: hookRun, reason: result?.reason, detail: result?.detail }); } catch {}
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
  try { await main(); } catch (error) { process.stderr.write('loam ingest worker: ' + String(error?.message || error) + '\n'); process.exitCode = 1; }
}
