import { readFile } from 'node:fs/promises';
import { homedir } from 'node:os';
import { isAbsolute, join } from 'node:path';
import { pathToFileURL } from 'node:url';

async function loadHarvestModules() {
  const roots = [];
  const globalRoot = process.env.LOAM_HOME && isAbsolute(process.env.LOAM_HOME)
    ? process.env.LOAM_HOME : null;
  try {
    const metadata = JSON.parse(await readFile(join(globalRoot || join(homedir(), '.agents', 'loam'), 'install.json'), 'utf8'));
    if (typeof metadata.integration_path === 'string') {
      roots.unshift(new URL('./', pathToFileURL(metadata.integration_path)));
    }
  } catch {}
  if (process.env.LOAM_INTEGRATION_PATH) {
    roots.unshift(new URL('./', pathToFileURL(process.env.LOAM_INTEGRATION_PATH)));
  }
  for (const root of roots) {
    try {
      const [paths, harvest] = await Promise.all([
        import(new URL('paths.mjs', root).href),
        import(new URL('harvest.mjs', root).href),
      ]);
      let hooks = {};
      try { hooks = await import(new URL('hooks.mjs', root).href); } catch {}
      return { ...paths, ...harvest, ...hooks };
    } catch {}
  }
  throw new Error('loam harvest integration is unavailable');
}

const {
  resolveGlobalRoot, resolveSkillsRoot, runHarvest,
  startHookWorker, finishHookWorker,
} = await loadHarvestModules().catch(() => ({}));

function args(argv) {
  const result = {};
  for (let index = 0; index < argv.length; index += 1) {
    if (argv[index] === '--harness') result.harness = argv[++index];
    else if (argv[index] === '--workspace') result.workspace = argv[++index];
    else if (argv[index] === '--session-id') result.sessionId = argv[++index];
    else if (argv[index] === '--hook-run-id') result.hookRunId = argv[++index];
    else if (argv[index] === '--global-root') result.globalRoot = argv[++index];
  }
  return result;
}

export async function main(options = {}) {
  const parsed = options.harness ? options : args(process.argv.slice(2));
  if (!parsed.workspace) throw new Error('worker requires --workspace');
  if (!parsed.harness) throw new Error('worker requires --harness');
  if (!parsed.sessionId) throw new Error('worker requires --session-id');
  const env = options.env || process.env;
  const globalRoot = parsed.globalRoot || env.LOAM_HARVEST_GLOBAL_ROOT || resolveGlobalRoot?.({ env });
  const worker = options.runHarvest || runHarvest;
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
    if (!worker) throw new Error('loam harvest integration is unavailable');
    const result = await worker({
      harness: parsed.harness,
      workspace: parsed.workspace,
      sessionId: parsed.sessionId,
      globalRoot,
      skillsRoot: options.skillsRoot || env.LOAM_HARVEST_SKILLS_ROOT || resolveSkillsRoot?.({ env }),
      env,
    });
    if (hookRun && workerFinish) {
      try {
        await workerFinish({
          run: hookRun,
          reason: result?.reason,
          ...(result?.detail !== undefined ? { detail: result.detail } : {}),
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

if (process.argv[1] && process.argv[1].endsWith('harvest-worker.mjs')) {
  try {
    const result = await main();
    if (process.argv.includes('--json')) process.stdout.write(`${JSON.stringify(result)}\n`);
  } catch (error) {
    process.stderr.write('loam harvest worker: ' + String(error?.message || error) + '\n');
    process.exitCode = 1;
  }
}
