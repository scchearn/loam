import { readFile } from 'node:fs/promises';
import { loadIngestModules } from './ingest-modules.mjs';

const { resolveGlobalRoot, resolveSkillsRoot, dispatchBoundary } = await loadIngestModules().catch(() => ({}));

async function input() {
  try { return JSON.parse(await readFile(0, 'utf8')); } catch { return {}; }
}

export async function main({ env = process.env, payload = null } = {}) {
  const body = payload || await input();
  if (!dispatchBoundary) return { action: 'skip', reason: 'runtime_unavailable' };
  return dispatchBoundary({
    harness: 'claude',
    payload: body,
    globalRoot: env.LOAM_INGEST_GLOBAL_ROOT || resolveGlobalRoot({ env }),
    skillsRoot: env.LOAM_INGEST_SKILLS_ROOT || resolveSkillsRoot({ env }),
    env,
  });
}

if (process.argv[1] && process.argv[1].endsWith('claude-stop.mjs')) {
  try { await main(); } catch (error) { process.stderr.write('loam ingest: ' + String(error?.message || error) + '\n'); }
}
