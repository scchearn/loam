import { loadIngestModules } from './ingest-modules.mjs';

const { resolveGlobalRoot, resolveSkillsRoot, runWorker } = await loadIngestModules().catch(() => ({}));

function args(argv) {
  const result = {};
  for (let index = 0; index < argv.length; index += 1) {
    if (argv[index] === '--harness') result.harness = argv[++index];
    else if (argv[index] === '--workspace') result.workspace = argv[++index];
  }
  return result;
}

export async function main(options = {}) {
  const parsed = options.harness ? options : args(process.argv.slice(2));
  if (!parsed.harness || !parsed.workspace) throw new Error('worker requires --harness and --workspace');
  if (!runWorker) throw new Error('loam ingestion integration is unavailable');
  const env = options.env || process.env;
  return runWorker({
    harness: parsed.harness,
    workspace: parsed.workspace,
    globalRoot: options.globalRoot || env.LOAM_INGEST_GLOBAL_ROOT || resolveGlobalRoot({ env }),
    skillsRoot: options.skillsRoot || env.LOAM_INGEST_SKILLS_ROOT || resolveSkillsRoot({ env }),
    env,
  });
}

if (process.argv[1] && process.argv[1].endsWith('ingest-worker.mjs')) {
  try { await main(); } catch (error) { process.stderr.write('loam ingest worker: ' + String(error?.message || error) + '\n'); process.exitCode = 1; }
}
