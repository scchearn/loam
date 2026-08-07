import { createHash, randomUUID } from 'node:crypto';
import { mkdir, readFile, readdir, rename, rm, stat, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';

const STATE_SCHEMA = 1;
const DEFAULTS = Object.freeze({
  enabled: true,
  min_interval_seconds: 900,
  timeout_seconds: 900,
  lease_ttl_seconds: 1800,
  min_store_delta_bytes: 32768,
  threshold_turns: 8,
  threshold_conversation_bytes: 16384,
  max_window_turns: 40,
  max_window_bytes: 262144,
  tool_output_bytes: 1000,
  wiki_recheck_seconds: 3600,
  retain_sessions: 32,
  retain_session_days: 14,
});

function hash(value) { return createHash('sha256').update(String(value)).digest('hex'); }
function runRoot(globalRoot, workspace) { return join(resolve(globalRoot), 'run', hash(resolve(workspace)).slice(0, 16)); }

async function json(path, fallback = null) {
  try { return JSON.parse(await readFile(path, 'utf8')); } catch { return fallback; }
}

async function writeAtomicFile(path, contents) {
  const temporary = `${path}.${randomUUID()}.tmp`;
  await mkdir(resolve(path, '..'), { recursive: true, mode: 0o700 });
  try {
    await writeFile(temporary, contents, { encoding: 'utf8', mode: 0o600 });
    await rename(temporary, path);
  } catch (error) {
    await rm(temporary, { force: true }).catch(() => {});
    throw error;
  }
}

function numeric(value, fallback) {
  const parsed = Number(value);
  return Number.isFinite(parsed) && parsed >= 0 ? parsed : fallback;
}

export async function readHarvestConfig(globalRoot, env = process.env) {
  const file = await json(join(resolve(globalRoot), 'config.json'), {});
  const section = file?.background_harvest || {};
  const enabled = env.LOAM_HARVEST_BACKGROUND === '0'
    ? false : env.LOAM_HARVEST_BACKGROUND === '1' ? true : section.enabled !== false;
  return {
    enabled,
    min_interval_seconds: numeric(env.LOAM_HARVEST_MIN_INTERVAL, numeric(section.min_interval_seconds, DEFAULTS.min_interval_seconds)),
    timeout_seconds: numeric(env.LOAM_HARVEST_TIMEOUT, numeric(section.timeout_seconds, DEFAULTS.timeout_seconds)),
    lease_ttl_seconds: numeric(section.lease_ttl_seconds, DEFAULTS.lease_ttl_seconds),
    min_store_delta_bytes: numeric(section.min_store_delta_bytes, DEFAULTS.min_store_delta_bytes),
    threshold_turns: numeric(env.LOAM_HARVEST_THRESHOLD_TURNS, numeric(section.threshold_turns, DEFAULTS.threshold_turns)),
    threshold_conversation_bytes: numeric(env.LOAM_HARVEST_THRESHOLD_BYTES, numeric(section.threshold_conversation_bytes, DEFAULTS.threshold_conversation_bytes)),
    max_window_turns: numeric(section.max_window_turns, DEFAULTS.max_window_turns),
    max_window_bytes: numeric(section.max_window_bytes, DEFAULTS.max_window_bytes),
    tool_output_bytes: numeric(section.tool_output_bytes, DEFAULTS.tool_output_bytes),
    wiki_recheck_seconds: numeric(section.wiki_recheck_seconds, DEFAULTS.wiki_recheck_seconds),
    retain_sessions: numeric(section.retain_sessions, DEFAULTS.retain_sessions),
    retain_session_days: numeric(section.retain_session_days, DEFAULTS.retain_session_days),
  };
}

export function harvestStatePath(globalRoot, workspace, sessionId) {
  return join(runRoot(globalRoot, workspace), 'harvest', `${hash(sessionId).slice(0, 16)}.json`);
}

export function harvestLastRunPath(globalRoot, workspace) {
  return join(runRoot(globalRoot, workspace), 'harvest-last-run.json');
}

export async function readHarvestState(globalRoot, workspace, sessionId) {
  return json(harvestStatePath(globalRoot, workspace, sessionId), null);
}

export async function writeHarvestState(globalRoot, workspace, sessionId, state) {
  const path = harvestStatePath(globalRoot, workspace, sessionId);
  const existing = await json(path, null);
  if (existing && existing.schema !== STATE_SCHEMA) {
    throw new Error(`harvest state schema ${existing.schema} is unknown; refusing to write over it`);
  }
  await writeAtomicFile(path, `${JSON.stringify({ ...state, schema: STATE_SCHEMA })}\n`);
  return path;
}

export async function pruneHarvestSessions(globalRoot, workspace, config) {
  const directory = join(runRoot(globalRoot, workspace), 'harvest');
  let entries;
  try { entries = await readdir(directory, { withFileTypes: true }); } catch { return; }
  const files = [];
  for (const entry of entries) {
    if (!entry.isFile() || !entry.name.endsWith('.json')) continue;
    const path = join(directory, entry.name);
    let info;
    try { info = await stat(path); } catch { continue; }
    files.push({ path, mtime: info.mtimeMs });
  }
  files.sort((a, b) => b.mtime - a.mtime);
  const ageCutoff = Date.now() - config.retain_session_days * 24 * 3600 * 1000;
  for (const file of files.slice(config.retain_sessions)) {
    if (file.mtime < ageCutoff) await rm(file.path, { force: true }).catch(() => {});
  }
}

export function harvestPublicReason(reason) {
  if (reason === 'disabled' || reason === 'recursion') return 'disabled';
  if (['lease_held', 'orphan_live', 'orphan_unknown'].includes(reason)) return 'busy';
  if (reason === 'debounced' || reason === 'backoff') return 'too_soon';
  if (['wiki_missing', 'below_threshold', 'nothing_durable', 'foreign_workspace'].includes(reason)) return 'nothing_to_do';
  if (['store_missing', 'store_unreadable', 'session_unknown', 'sqlite_unavailable', 'schema_unknown'].includes(reason)) return 'unavailable';
  return reason === 'ok' ? 'ok' : 'unavailable';
}
