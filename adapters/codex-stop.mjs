import { readFile } from 'node:fs/promises';
import { loadIngestModules } from './ingest-modules.mjs';

const { resolveGlobalRoot, resolveSkillsRoot, dispatchBoundary } = await loadIngestModules().catch(() => ({}));

export async function main({ env = process.env, input = null } = {}) {
  let payload = input;
  if (!payload) {
    try { payload = JSON.parse(await readFile(0, 'utf8')); } catch { payload = {}; }
  }
  payload = {
    session_id: typeof payload?.session_id === 'string' ? payload.session_id : undefined,
    cwd: typeof payload?.cwd === 'string' ? payload.cwd : undefined,
    stop_hook_active: payload?.stop_hook_active === true,
  };
  try {
    if (!dispatchBoundary) return {};
    await dispatchBoundary({
      harness: 'codex',
      payload,
      globalRoot: env.LOAM_INGEST_GLOBAL_ROOT || resolveGlobalRoot({ env }),
      skillsRoot: env.LOAM_INGEST_SKILLS_ROOT || resolveSkillsRoot({ env }),
      env,
    });
  } catch (error) {
    process.stderr.write('loam ingest: ' + String(error?.message || error) + '\n');
  }
  return {};
}

if (process.argv[1] && process.argv[1].endsWith('codex-stop.mjs')) {
  const result = await main();
  process.stdout.write(JSON.stringify(result));
}
