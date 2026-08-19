import { readFile } from 'node:fs/promises';
import { loadIngestModules } from './ingest-modules.mjs';

const START_FAILURE_MESSAGE = 'Loam background ingestion could not start. Run npx @scchearn/loam install to repair the installation.';

function response({ visibility, outcome, failure }) {
  if (visibility === 'native' && outcome?.native_continuation) return outcome.native_continuation;
  return visibility === 'toast' && (failure || outcome?.reason === 'unavailable')
    ? { systemMessage: START_FAILURE_MESSAGE }
    : {};
}

export async function main({
  env = process.env,
  input = null,
  loadIngest = loadIngestModules,
  errorOutput = process.stderr,
} = {}) {
  let payload = input;
  if (!payload) {
    try { payload = JSON.parse(await readFile(0, 'utf8')); } catch { payload = {}; }
  }
  payload = {
    session_id: typeof payload?.session_id === 'string' ? payload.session_id : undefined,
    cwd: typeof payload?.cwd === 'string' ? payload.cwd : undefined,
    stop_hook_active: payload?.stop_hook_active === true,
  };
  let failure;
  let outcome;
  let configuredVisibility = 'silent';
  try {
    const { resolveGlobalRoot, resolveSkillsRoot, readIngestConfig, dispatchBoundary } = await loadIngest();
    if (!dispatchBoundary) return {};
    const globalRoot = env.LOAM_INGEST_GLOBAL_ROOT || resolveGlobalRoot({ env });
    configuredVisibility = (await readIngestConfig?.(globalRoot, env))?.visibility || 'silent';
    outcome = await dispatchBoundary({
      harness: 'codex',
      payload,
      globalRoot,
      skillsRoot: env.LOAM_INGEST_SKILLS_ROOT || resolveSkillsRoot({ env }),
      env,
    });
    const { resolveGlobalRoot: harvestRoot } = await loadIngest();
    const harvest = await import('./harvest-modules.mjs').catch(() => null);
    if (harvest?.harvestTick) {
      try {
        await harvest.harvestTick({
          harness: 'codex',
          payload,
          globalRoot: harvestRoot({ env }),
          env,
        });
      } catch {}
    }
  } catch (error) {
    failure = error;
    errorOutput.write('loam ingest: ' + String(error?.message || error) + '\n');
  }
  return response({ visibility: configuredVisibility, outcome, failure });
}

if (process.argv[1] && process.argv[1].endsWith('codex-stop.mjs')) {
  const result = await main();
  process.stdout.write(JSON.stringify(result));
}
