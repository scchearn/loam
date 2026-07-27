import { createHash, randomBytes, randomUUID } from 'node:crypto';
import { mkdir, readFile, readdir, realpath, rename, rm, stat, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';

import { assertInside, resolveSkillsRoot } from './paths.mjs';
import { checkReadiness, invokeRuntime, probeFullState } from './runtime.mjs';
import {
  bootIdentity, childIdentity, classifyChild, execFile, processStartIdentity,
  spawnDetached, startTracked, terminateChild,
} from './ingest-process.mjs';
import { FingerprintError, fingerprintActionable } from './ingest-fingerprint.mjs';

const PROMPT = 'Run the existing loam::ingesting-codebase skill for the provided workspace. Do not modify source files, commit, or push.';
const DEFAULTS = Object.freeze({ enabled: false, min_interval_seconds: 300, timeout_seconds: 900, lease_ttl_seconds: 1800 });
const LOG_MAX_RECORD = 2048;
const LOG_MAX_BYTES = 256 * 1024;
const OWNERSHIP_STALE_MS = 30000;
const OWNERSHIP_FILE = '.ownership.lock';
const ARCHIVE_MAX = 8;
function hash(value) { return createHash('sha256').update(String(value)).digest('hex'); }
export function runRoot(globalRoot, workspace) { return join(resolve(globalRoot), 'run', hash(workspace).slice(0, 16)); }
async function json(path, fallback = null) { try { return JSON.parse(await readFile(path, 'utf8')); } catch { return fallback; } }
async function jsonRecord(path) {
  try { return { present: true, value: JSON.parse(await readFile(path, 'utf8')) }; }
  catch (error) { return error?.code === 'ENOENT' ? { present: false } : { present: true, malformed: true }; }
}
async function ensureRoot(root) { await mkdir(root, { recursive: true, mode: 0o700 }); }
async function delay(ms) { await new Promise((resolvePromise) => setTimeout(resolvePromise, ms)); }
async function ownershipRecord(path) {
  const info = await stat(path).catch(() => null);
  if (!info) return null;
  const raw = await readFile(path, 'utf8').catch(() => '');
  try {
    const parsed = JSON.parse(raw);
    if (parsed && typeof parsed === 'object') return { ...parsed, age: Date.now() - info.mtimeMs };
  } catch {}
  const pid = Number.parseInt(raw.trim(), 10);
  return Number.isInteger(pid) && pid > 0 ? { pid, age: Date.now() - info.mtimeMs } : { age: Date.now() - info.mtimeMs };
}
async function ownershipDead(owner) {
  if (!owner?.pid) return owner?.age > OWNERSHIP_STALE_MS;
  if (owner.boot_id && owner.process_start) {
    return (await classifyChild(owner, { platform: process.platform })) === 'dead';
  }
  try { process.kill(Number(owner.pid), 0); return false; } catch (error) { return error?.code === 'ESRCH'; }
}
async function claimWorkspace(root) {
  const path = join(root, OWNERSHIP_FILE);
  await ensureRoot(root);
  let generation = 1;
  for (let attempt = 0; attempt < 40; attempt += 1) {
    const owner = {
      schema: 1, token: randomUUID(), generation, pid: process.pid,
      boot_id: await bootIdentity(), process_start: await processStartIdentity(process.pid),
      acquired_at: Date.now(),
    };
    try {
      await writeFile(path, JSON.stringify(owner) + '\n', { flag: 'wx', mode: 0o600 });
      return owner;
    } catch (error) {
      if (error?.code !== 'EEXIST') throw error;
      const current = await ownershipRecord(path);
      if (!current) continue;
      generation = Math.max(generation, (Number(current.generation) || 0) + 1);
      if (!(await ownershipDead(current))) {
        await delay(10);
        continue;
      }
      const takeoverPath = `${path}.takeover-${hash(String(current.token || 'legacy'))}-${current.generation || 0}`;
      const takeover = {
        schema: 1, token: randomUUID(), expected_token: current.token,
        expected_generation: current.generation || 0, pid: process.pid,
        boot_id: owner.boot_id, process_start: owner.process_start, acquired_at: Date.now(),
      };
      try {
        await writeFile(takeoverPath, JSON.stringify(takeover) + '\n', { flag: 'wx', mode: 0o600 });
      } catch (takeoverError) {
        if (takeoverError?.code !== 'EEXIST') throw takeoverError;
        const activeTakeover = await ownershipRecord(takeoverPath);
        if (await ownershipDead(activeTakeover)) {
          const staleTakeover = `${takeoverPath}.${randomUUID()}.stale`;
          try { await rename(takeoverPath, staleTakeover); await rm(staleTakeover, { force: true }); } catch {}
        } else await delay(10);
        continue;
      }
      try {
        const latest = await ownershipRecord(path);
        if (latest?.token !== takeover.expected_token
          || (latest.generation || 0) !== takeover.expected_generation) continue;
        const stale = `${path}.${randomUUID()}.stale`;
        await rename(path, stale);
        await rm(stale, { force: true });
      } catch (takeoverError) {
        if (takeoverError?.code !== 'ENOENT') await delay(10);
      } finally {
        const activeTakeover = await json(takeoverPath);
        if (activeTakeover?.token === takeover.token) await rm(takeoverPath, { force: true });
      }
    }
  }
  throw new Error('workspace ownership unavailable');
}
async function releaseWorkspace(root, owner) {
  const path = join(root, OWNERSHIP_FILE);
  const current = await json(path);
  if (current?.token === owner?.token && current?.generation === owner?.generation) await rm(path, { force: true });
}
async function withWorkspaceOwnership(root, callback) {
  const owner = await claimWorkspace(root);
  try { return await callback(owner); } finally { await releaseWorkspace(root, owner); }
}
async function atomicJson(path, value) { await writeAtomicFile(path, JSON.stringify(value) + '\n'); }
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
async function appendLog(root, event, fields = {}) {
  await withWorkspaceOwnership(root, async () => {
    const record = JSON.stringify({ schema: 1, at: new Date().toISOString(), event, ...fields }) + '\n';
    if (Buffer.byteLength(record) > LOG_MAX_RECORD) return;
    const path = join(root, 'log.jsonl');
    let current = await readFile(path, 'utf8').catch(() => '');
    if (Buffer.byteLength(current) + Buffer.byteLength(record) > LOG_MAX_BYTES) {
      await rm(join(root, 'log.1.jsonl'), { force: true });
      await rename(path, join(root, 'log.1.jsonl')).catch(() => {});
      current = '';
    }
    await writeAtomicFile(path, current + record);
  }).catch(() => {});
}
function numeric(value, fallback) { const parsed = Number(value); return Number.isFinite(parsed) && parsed >= 0 ? parsed : fallback; }

export async function readIngestConfig(globalRoot, env = process.env) {
  const file = await json(join(resolve(globalRoot), 'config.json'), {});
  const section = file?.background_ingest || {};
  const enabled = env.LOAM_INGEST_BACKGROUND === '0'
    ? false : env.LOAM_INGEST_BACKGROUND === '1' ? true : section.enabled === true;
  return {
    enabled,
    min_interval_seconds: numeric(env.LOAM_INGEST_MIN_INTERVAL, numeric(section.min_interval_seconds, DEFAULTS.min_interval_seconds)),
    timeout_seconds: numeric(env.LOAM_INGEST_TIMEOUT, numeric(section.timeout_seconds, DEFAULTS.timeout_seconds)),
    lease_ttl_seconds: numeric(env.LOAM_INGEST_LEASE_TTL, numeric(section.lease_ttl_seconds, DEFAULTS.lease_ttl_seconds)),
  };
}

export async function canonicalWorkspace(value = process.cwd()) {
  const path = resolve(String(value));
  try { return await realpath(path); } catch { return path; }
}

function payloadWorkspace(payload = {}) { return payload.cwd || payload.directory || payload.workspace?.root || process.cwd(); }
function eventKey({ workspace, harness, payload = {}, now = Date.now() }) {
  const stable = payload.event_id || payload.turn_id || payload.stop_hook_id;
  return workspace + ':' + harness + ':' + (stable || Math.floor(now / 2000));
}

async function eventRecord(root, keyHash, now = Date.now()) {
  return withWorkspaceOwnership(root, async () => {
    const path = join(root, 'events.json');
    const current = await json(path, null);
    if (current && current.schema !== 1) return { unknown: true };
    const record = current || { schema: 1, entries: [] };
    const entries = Array.isArray(record.entries) ? record.entries : [];
    const fresh = entries.filter((entry) => Number(entry.at) > now - 120000).slice(-31);
    if (fresh.some((entry) => entry.key_hash === keyHash)) return { duplicate: true };
    fresh.push({ key_hash: keyHash, at: now });
    await atomicJson(path, { schema: 1, entries: fresh });
    return { duplicate: false };
  });
}

function recursion(payload, env) {
  return env.LOAM_INGEST_WORKER === '1' || env.LOAM_INGEST_CHILD === '1'
    || payload.stop_hook_active === true || payload.loam_ingest_child === true || payload.child_session === true;
}

export async function gate({ harness, payload = {}, globalRoot, env = process.env, now = Date.now() } = {}) {
  const config = await readIngestConfig(globalRoot, env);
  const logEarlySkip = async (reason) => {
    try {
      const workspace = await canonicalWorkspace(payloadWorkspace(payload));
      await appendLog(runRoot(globalRoot, workspace), 'gate_skip', { reason });
    } catch {}
    return { action: 'skip', reason };
  };
  if (!config.enabled) return logEarlySkip('disabled');
  if (recursion(payload, env)) return logEarlySkip('recursion');
  const workspace = await canonicalWorkspace(payloadWorkspace(payload));
  const root = runRoot(globalRoot, workspace);
  await ensureRoot(root);
  const skip = async (reason, fields = {}) => {
    await appendLog(root, 'gate_skip', { reason, ...fields });
    return { action: 'skip', reason, workspace, ...fields };
  };
  const last = await json(join(root, 'last-run.json'), null);
  if (last && last.schema !== 1) return skip('schema_unknown');
  const previous = last || {};
  if (Number(previous.backoff_until || 0) > now) return skip('backoff');
  if (Number(previous.completed_at || 0) + config.min_interval_seconds * 1000 > now && previous.status === 'ok') {
    return skip('debounced');
  }
  const keyHash = hash(eventKey({ workspace, harness, payload, now }));
  const event = await eventRecord(root, keyHash, now);
  if (event.unknown) return skip('schema_unknown');
  if (event.duplicate) return skip('duplicate_event', { key_hash: keyHash });
  return { action: 'spawn_worker', workspace, key_hash: keyHash, config };
}

export function startWorker({ harness, workspace, globalRoot, skillsRoot, workerPath, env = process.env } = {}) {
  if (!workerPath) throw new Error('installed ingestion worker is unavailable');
  return spawnDetached({
    command: process.execPath,
    args: [workerPath, '--harness', harness, '--workspace', resolve(workspace)],
    cwd: resolve(workspace),
    env: { ...env, LOAM_INGEST_WORKER: '1', LOAM_INGEST_GLOBAL_ROOT: resolve(globalRoot), ...(skillsRoot ? { LOAM_INGEST_SKILLS_ROOT: resolve(skillsRoot) } : {}) },
  });
}

async function installedWorkerPath(globalRoot) {
  const install = await json(join(resolve(globalRoot), 'install.json'));
  if (typeof install?.adapter_root !== 'string') return null;
  return assertInside(resolve(globalRoot), join(resolve(install.adapter_root), 'ingest-worker.mjs'), 'worker path');
}

async function forgetEvent(root, keyHash) {
  await withWorkspaceOwnership(root, async () => {
    const path = join(root, 'events.json');
    const events = await json(path, { schema: 1, entries: [] });
    await atomicJson(path, { schema: 1, entries: (events.entries || []).filter((entry) => entry.key_hash !== keyHash) });
  }).catch(() => {});
}

export async function dispatchBoundary(options = {}) {
  const result = await gate(options);
  if (result.action !== 'spawn_worker') return result;
  try {
    const started = startWorker({ ...options, workerPath: await installedWorkerPath(options.globalRoot), workspace: result.workspace });
    started.child.once('error', () => { forgetEvent(runRoot(options.globalRoot, result.workspace), result.key_hash).catch(() => {}); });
    return result;
  }
  catch (error) {
    const root = runRoot(options.globalRoot, result.workspace);
    await forgetEvent(root, result.key_hash);
    return { action: 'skip', reason: 'runtime_unavailable', detail: error.message };
  }
}

async function resolveExclusions(skillsRoot) {
  const root = resolve(skillsRoot || resolveSkillsRoot());
  let physicalRoot;
  try { physicalRoot = await realpath(root); } catch { throw new FingerprintError('exclusions_unavailable', 'installed skills root is unavailable'); }
  const candidates = [
    join(physicalRoot, 'loam-ingesting-codebase', 'references', 'ingestion-exclusions.md'),
    join(physicalRoot, 'loam-memory', 'loam-ingesting-codebase', 'references', 'ingestion-exclusions.md'),
  ];
  for (const candidate of candidates) {
    try {
      assertInside(physicalRoot, candidate, 'exclusions path');
      const physicalCandidate = await realpath(candidate);
      assertInside(physicalRoot, physicalCandidate, 'exclusions path');
      if ((await stat(physicalCandidate)).isFile()) { await readFile(physicalCandidate); return physicalCandidate; }
    } catch {}
  }
  throw new FingerprintError('exclusions_unavailable', 'installed ingestion exclusions are unavailable');
}

function pendingHint(state) { return Array.isArray(state?.hints) ? state.hints.find((item) => item?.kind === 'code_ingest_pending') : null; }
function pendingCount(item) { const value = item?.evidence?.pending_count; return Number.isInteger(value) && value > 0 ? value : 0; }
async function hasExistingWiki(wikiRoot) {
  try { return (await stat(resolve(wikiRoot))).isDirectory(); } catch { return false; }
}
async function hasExistingCodegraph(wikiRoot) {
  try { return (await stat(join(resolve(wikiRoot), 'code'))).isDirectory(); } catch { return false; }
}

async function diff({ readiness, workspace, wikiRoot, exclusionsPath, runner }) {
  const result = await invokeRuntime({
    runtimePath: readiness.runtimePath,
    args: ['codegraph', 'diff', workspace, wikiRoot, '--exclusions', exclusionsPath],
    cwd: workspace, timeoutMs: 20000, runner,
  });
  if (result.category === 'timeout') return { error: 'probe_timeout', detail: result.stderr };
  if (result.code !== 0) return { error: 'probe_failed', detail: result.stderr || result.stdout };
  try {
    const parsed = JSON.parse(result.stdout);
    if (!Array.isArray(parsed)) throw new Error('diff output must be an array');
    return { entries: parsed };
  } catch (error) { return { error: 'malformed_state', detail: error.message }; }
}

function validState(state) {
  return state && typeof state === 'object' && !Array.isArray(state)
    && (!('schema' in state) || state.schema === 1)
    && Array.isArray(state.hints);
}

async function leaseOwner() {
  return { pid: process.pid, boot_id: await bootIdentity(), process_start: await processStartIdentity(process.pid) };
}

async function queryClaude(workspace, intent) {
  const result = await execFile('claude', ['agents', '--json', '--cwd', workspace, '--all'], { cwd: workspace, timeout: 5000 });
  if (result.code !== 0 || result.category === 'runtime_error') return { state: 'unknown' };
  let records; try { records = JSON.parse(result.stdout); } catch { return { state: 'unknown' }; }
  const list = Array.isArray(records) ? records : records?.agents || [];
  const identity = intent.child_identity || {};
  const plannedName = intent.planned_identity?.name;
  const match = plannedName
    ? list.find((item) => item.name === plannedName || item.title === plannedName)
      || list.find((item) => identity.manager_id && (item.id === identity.manager_id || item.session_id === identity.manager_id))
    : list.find((item) => identity.manager_id && (item.id === identity.manager_id || item.session_id === identity.manager_id));
  if (!match) return { state: 'dead' };
  const state = match.state?.status || match.state || match.status;
  if (['working', 'blocked', 'running', 'pending'].includes(state)) return { state: 'live', record: match };
  if (['done', 'failed', 'stopped', 'idle', 'completed', 'error'].includes(state)) return { state: 'terminal', record: match };
  return { state: 'unknown', record: match };
}

function sessionRecord(value, id) {
  const data = value?.data ?? value;
  return data?.[id] || data?.sessions?.[id] || data;
}

function sessionState(value, id) {
  const record = sessionRecord(value, id);
  return record?.type || record?.state || record?.status || record?.data?.type || record?.data?.state || record?.data?.status;
}

async function inspectIntent(path, workspace, openCodeSession) {
  const record = await jsonRecord(path);
  if (!record.present) return { state: 'dead', intent: null };
  if (record.malformed || !record.value || typeof record.value !== 'object' || Array.isArray(record.value)) {
    return { state: 'unknown', intent: null };
  }
  const intent = record.value;
  if (intent.schema !== 1 || intent.state !== 'active') return { state: 'unknown', intent };
  if (intent.launch_mode === 'claude_bg') return { ...(await queryClaude(workspace, intent)), intent };
  if (intent.launch_state === 'planned' && !intent.child_identity) {
    return { state: 'unknown', intent };
  }
  if (intent.launch_mode === 'claude_print' || intent.launch_mode === 'codex_exec') {
    if (!intent.child_identity) return { state: 'unknown', intent };
    return { state: await classifyChild(intent.child_identity), intent };
  }
  if (intent.launch_mode === 'opencode_child') {
    const sessionId = intent.child_identity?.session_id;
    if (!sessionId || typeof openCodeSession?.status !== 'function') return { state: 'unknown', intent };
    try {
      const record = await openCodeSession.status(sessionId);
      const state = sessionState(record, sessionId);
      if (['working', 'running', 'pending', 'busy', 'retry'].includes(state)) return { state: 'live', record, intent };
      if (['idle', 'done', 'completed', 'failed', 'stopped'].includes(state)) return { state: 'terminal', record, intent };
    } catch {}
    return { state: 'unknown', intent };
  }
  return { state: 'unknown', intent };
}

async function archiveIntentFile(root, expected) {
  const current = await json(join(root, 'intent.json'));
  if (!current || current.schema !== 1 || current.state !== 'active') return true;
  if (expected && (current.attempt_id !== expected.attempt_id || current.lease_id !== expected.lease_id
    || current.launch_token !== expected.launch_token)) return false;
  try {
    await rename(join(root, 'intent.json'), join(root, `intent.${current.attempt_id}.stale`));
    await pruneArchives(root, 'intent.', '.stale');
    return true;
  } catch (error) {
    return error?.code === 'ENOENT';
  }
}
async function pruneArchives(root, prefix, suffix) {
  const entries = (await readdir(root).catch(() => []))
    .filter((name) => name.startsWith(prefix) && name.endsWith(suffix))
    .sort()
    .reverse();
  for (const entry of entries.slice(ARCHIVE_MAX)) await rm(join(root, entry), { force: true });
}

function recordSignature(record) {
  if (!record?.present) return 'absent';
  if (record.malformed) return 'malformed';
  return 'value:' + JSON.stringify(record.value);
}
function sameRecord(left, right) { return recordSignature(left) === recordSignature(right); }
async function workspaceMetadataSnapshot(root) {
  return withWorkspaceOwnership(root, async () => ({
    lease: await jsonRecord(join(root, 'lease.json')),
    intent: await jsonRecord(join(root, 'intent.json')),
  }));
}

async function acquireLease(root, workspace, harness, config, openCodeSession) {
  await ensureRoot(root);
  const path = join(root, 'lease.json');
  const intentPath = join(root, 'intent.json');
  for (let attempt = 0; attempt < 40; attempt += 1) {
    const snapshot = await workspaceMetadataSnapshot(root);
    const leaseRecord = snapshot.lease;
    if (leaseRecord.malformed || (leaseRecord.present && (!leaseRecord.value || typeof leaseRecord.value !== 'object' || Array.isArray(leaseRecord.value)))) return { status: 'orphan_unknown' };
    const existing = leaseRecord.value || null;
    if (existing) {
      if (existing.schema !== 1) return { status: 'orphan_unknown' };
      const current = await classifyChild({ pid: existing.owner_pid, boot_id: existing.boot_id, process_start: existing.process_start });
      if (current === 'live') return { status: 'held' };
      if (current !== 'dead') return { status: 'orphan_unknown' };
      const orphan = await inspectIntent(intentPath, workspace, openCodeSession);
      if (orphan.state === 'live') return { status: 'orphan_live' };
      if (orphan.state === 'unknown') return { status: 'orphan_unknown' };
      const result = await withWorkspaceOwnership(root, async () => {
        const latestLease = await jsonRecord(path);
        const latestIntent = await jsonRecord(intentPath);
        if (!sameRecord(latestLease, snapshot.lease) || !sameRecord(latestIntent, snapshot.intent)) return { status: 'retry' };
        if (latestLease.value?.lease_id !== existing.lease_id) return { status: 'retry' };
        try {
          const stale = `${path}.${randomUUID()}.stale`;
          await rename(path, stale);
          await rm(stale, { force: true });
        } catch (error) {
          return error?.code === 'ENOENT' ? { status: 'retry' } : { status: 'held' };
        }
        if (orphan.intent && !(await archiveIntentFile(root, orphan.intent))) return { status: 'orphan_unknown' };
        return { status: 'retry' };
      });
      if (result.status !== 'retry') return result;
      continue;
    }
    const orphan = await inspectIntent(intentPath, workspace, openCodeSession);
    if (orphan.state === 'live') return { status: 'orphan_live' };
    if (orphan.state === 'unknown') return { status: 'orphan_unknown' };
    const owner = await leaseOwner();
    const lease = {
      schema: 1, lease_id: randomUUID(), workspace, harness, owner_pid: owner.pid,
      boot_id: owner.boot_id, process_start: owner.process_start, started_at: Date.now(),
      hard_deadline: new Date(Date.now() + config.lease_ttl_seconds * 1000).toISOString(),
    };
    const result = await withWorkspaceOwnership(root, async () => {
      const latestLease = await jsonRecord(path);
      const latestIntent = await jsonRecord(intentPath);
      if (!sameRecord(latestLease, snapshot.lease) || !sameRecord(latestIntent, snapshot.intent)) return { status: 'retry' };
      if (latestLease.present) return { status: 'retry' };
      if (orphan.intent && !(await archiveIntentFile(root, orphan.intent))) return { status: 'orphan_unknown' };
      try {
        await writeFile(path, JSON.stringify(lease) + '\n', { flag: 'wx', mode: 0o600 });
        return { status: 'acquired', path, lease };
      } catch (error) {
        return error?.code === 'EEXIST' ? { status: 'retry' } : { status: 'error', detail: error?.message || 'lease creation failed' };
      }
    });
    if (result.status !== 'retry') return result;
  }
  return { status: 'error', detail: 'workspace lease unavailable' };
}

async function writeSkip(root, reason, extra = {}) {
  const result = await withWorkspaceOwnership(root, async () => {
    const leaseId = extra.__lease_id;
    const intent = extra.__intent;
    const fields = { ...extra };
    delete fields.__lease_id;
    delete fields.__intent;
    if (leaseId) {
      const lease = await json(join(root, 'lease.json'));
      if (lease?.lease_id !== leaseId) return false;
    }
    if (intent) {
      const current = await json(join(root, 'intent.json'));
      if (!current || current.attempt_id !== intent.attempt_id || current.lease_id !== intent.lease_id || current.launch_token !== intent.launch_token) return false;
    }
    const previous = await json(join(root, 'last-run.json'));
    if (previous && previous.schema !== 1) return false;
    await atomicJson(join(root, 'last-run.json'), {
      ...(previous || {}), schema: 1, completed_at: Date.now(), status: 'skipped', reason, ...fields,
    });
    return true;
  }).catch(() => false);
  if (result) await appendLog(root, 'skip', { reason });
  return result;
}

async function retireIntent(root, tokens) {
  return withWorkspaceOwnership(root, async () => {
    const path = join(root, 'intent.json');
    const lease = await json(join(root, 'lease.json'));
    if (lease?.lease_id !== tokens.lease_id) return false;
    const current = await json(path);
    if (!current || current.attempt_id !== tokens.attempt_id || current.lease_id !== tokens.lease_id || current.launch_token !== tokens.launch_token) return false;
    try { await rename(path, join(root, 'intent.' + tokens.attempt_id + '.done')); } catch (error) { if (error?.code !== 'ENOENT') return false; }
    const entries = await readdir(root).catch(() => []);
    for (const entry of entries.filter((name) => name.startsWith('intent.') && name.endsWith('.done')).slice(0, -8)) await rm(join(root, entry), { force: true });
    return true;
  }).catch(() => false);
}

async function releaseLease(root, leaseId) {
  await withWorkspaceOwnership(root, async () => {
    const path = join(root, 'lease.json');
    const current = await json(path);
    if (current?.lease_id === leaseId) await rm(path, { force: true });
  }).catch(() => {});
}

async function publishIntent(root, leaseId, intent) {
  return withWorkspaceOwnership(root, async () => {
    const lease = await json(join(root, 'lease.json'));
    if (lease?.lease_id !== leaseId) return { status: 'orphan_unknown' };
    const current = await jsonRecord(join(root, 'intent.json'));
    if (current.present) return { status: 'duplicate_intent' };
    try {
      await writeFile(join(root, 'intent.json'), JSON.stringify(intent) + '\n', { flag: 'wx', mode: 0o600 });
      return { status: 'published' };
    } catch (error) {
      return error?.code === 'EEXIST' ? { status: 'duplicate_intent' } : { status: 'runtime_unavailable' };
    }
  }).catch(() => ({ status: 'runtime_unavailable' }));
}

async function liveOwnedIntent(root, workspace, openCodeSession, tokens) {
  const current = await json(join(root, 'intent.json'));
  if (!current || current.attempt_id !== tokens.attempt_id || current.lease_id !== tokens.lease_id
    || current.launch_token !== tokens.launch_token || current.launch_state !== 'launched') return false;
  const observed = await inspectIntent(join(root, 'intent.json'), workspace, openCodeSession).catch(() => ({ state: 'unknown' }));
  return observed.state === 'live' || observed.state === 'unknown';
}

async function updateIntent(root, tokens, update) {
  return withWorkspaceOwnership(root, async () => {
    const path = join(root, 'intent.json');
    const lease = await json(join(root, 'lease.json'));
    if (lease?.lease_id !== tokens.lease_id) return false;
    const current = await json(path);
    if (!current || current.attempt_id !== tokens.attempt_id || current.lease_id !== tokens.lease_id || current.launch_token !== tokens.launch_token) return false;
    const next = { ...current, ...update };
    await atomicJson(path, next);
    Object.assign(tokens, next);
    return true;
  }).catch(() => false);
}

async function launchMode({ harness, workspace, env, root }) {
  if (harness === 'opencode') return 'opencode_child';
  if (harness === 'codex') return 'codex_exec';
  const help = await execFile('claude', ['--help'], { cwd: workspace, timeout: 5000, env });
  if (help.code !== 0 || !/--bg|--background/.test(help.stdout) || env.CLAUDE_CODE_DISABLE_AGENT_VIEW === '1') return 'claude_print';
  const settingsPath = join(root, 'claude-settings.json');
  await atomicJson(settingsPath, { worktree: { bgIsolation: 'none' } });
  const name = 'loam-ingest-capability-probe-' + randomUUID().slice(0, 8);
  try {
    const probe = startTracked({
      command: 'claude',
      args: ['--bg', '--name', name, '--settings', settingsPath, '--permission-mode', 'dontAsk', '--allowedTools', 'Read', 'Reply only OK.'],
      cwd: workspace, env, timeoutMs: 5000, detached: true, captureOutput: false,
    });
    const result = await probe.completion;
    const probeIntent = { planned_identity: { name }, child_identity: null };
    let manager = await waitForClaude(workspace, probeIntent, Date.now() + 5000);
    if (manager.state === 'live') {
      await stopClaude(workspace, probeIntent);
      manager = await waitForClaude(workspace, probeIntent, Date.now() + 5000);
    }
    if (result.code !== 0 || !['dead', 'terminal'].includes(manager.state)) return 'claude_print';
    const id = manager.record?.id || manager.record?.session_id || manager.record?.sessionID;
    if (!id) return 'claude_print';
    const removed = await execFile('claude', ['rm', id], { cwd: workspace, timeout: 5000, env });
    if (removed.code !== 0 || (await queryClaude(workspace, probeIntent)).state !== 'dead') return 'claude_print';
    return 'claude_bg';
  } finally {
    await rm(settingsPath, { force: true });
  }
}

async function waitForClaude(workspace, intent, deadline) {
  let result = { state: 'unknown' };
  let observed = false;
  const registrationDeadline = Math.min(deadline, Date.now() + 5000);
  while (Date.now() < deadline) {
    result = await queryClaude(workspace, intent);
    if (result.state === 'live') observed = true;
    if (result.state === 'terminal' || (result.state === 'dead' && observed)) return result;
    if (!observed && Date.now() >= registrationDeadline) return { ...result, state: 'unknown' };
    await new Promise((resolvePromise) => setTimeout(resolvePromise, Math.min(1000, Math.max(1, deadline - Date.now()))));
  }
  return result;
}

async function stopClaude(workspace, intent) {
  const queried = await queryClaude(workspace, intent);
  const record = queried.record;
  const id = record?.id || record?.session_id || record?.sessionID || intent.child_identity?.manager_id;
  if (!id) return { state: 'unknown' };
  const result = await execFile('claude', ['stop', id], { cwd: workspace, timeout: 5000 });
  return result.code === 0 ? { state: 'stopping' } : { state: 'unknown' };
}

async function waitForOpenCode(openCodeSession, sessionId, deadline) {
  if (typeof openCodeSession?.status !== 'function') return { state: 'unknown' };
  let last = { state: 'unknown' };
  while (Date.now() < deadline) {
    try {
      const record = await openCodeSession.status(sessionId);
      const state = sessionState(record, sessionId);
      if (['working', 'running', 'pending', 'busy', 'retry'].includes(state)) last = { state: 'live', record };
      else if (['idle', 'done', 'completed', 'failed', 'stopped', 'error'].includes(state)) return { state: 'terminal', record };
      else last = { state: 'unknown', record };
    } catch { last = { state: 'unknown' }; }
    if (last.state === 'terminal') return last;
    await delay(Math.min(1000, Math.max(1, deadline - Date.now())));
  }
  return last;
}

async function launchModel({ launchMode: mode, workspace, env, timeoutMs, intent, openCodeSession, root }) {
  const prompt = PROMPT + ' Workspace: ' + workspace;
  if (mode === 'opencode_child') {
    if (!openCodeSession?.createChild || !openCodeSession?.promptAsync || !openCodeSession.parentSessionId) return { category: 'runtime_unavailable' };
    const child = await openCodeSession.createChild({
      parentId: openCodeSession.parentSessionId,
      title: intent.planned_identity.title,
    });
    const sessionId = child?.id || child?.session_id || child?.sessionID;
    if (!sessionId) return { category: 'runtime_unavailable' };
    const identity = {
      session_id: String(sessionId),
      parent_session_id: String(openCodeSession.parentSessionId),
      host_identity: intent.planned_identity?.owner_identity || null,
      launch_token: intent.launch_token,
    };
    if (!(await updateIntent(root, intent, { launch_state: 'launched', child_identity: identity }))) {
      if (typeof openCodeSession.abort === 'function') await openCodeSession.abort(String(sessionId)).catch(() => {});
      return { category: 'orphan_unknown' };
    }
    try {
      await openCodeSession.promptAsync({ sessionId: String(sessionId), parts: [{ type: 'text', text: prompt }] });
    } catch {
      try {
        const record = await openCodeSession.status(String(sessionId));
        const state = sessionState(record, String(sessionId));
        if (['working', 'running', 'pending', 'busy', 'retry'].includes(state)) return { category: 'orphan_live' };
        if (!['idle', 'done', 'completed', 'failed', 'stopped', 'error'].includes(state)) return { category: 'orphan_unknown' };
      } catch {}
      return { category: 'runtime_unavailable' };
    }
    return { category: null, completion: Promise.resolve({ code: 0 }), background: true, sessionId: String(sessionId) };
  }
  if (mode === 'claude_bg') {
      const name = intent.planned_identity.name;
      const settingsPath = join(root, 'claude-settings.json');
      await atomicJson(settingsPath, { worktree: { bgIsolation: 'none' } });
      let started;
      try {
        started = startTracked({
          command: 'claude',
          args: ['--bg', '--name', name, '--settings', settingsPath, '--permission-mode', 'dontAsk', '--allowedTools', 'Read Glob Grep Write Edit Bash', prompt],
          cwd: workspace, env, timeoutMs,
          detached: true, captureOutput: false,
        });
      } catch (error) {
        await rm(settingsPath, { force: true });
        throw error;
      }
      if (!(await updateIntent(root, intent, { launch_state: 'launched', child_identity: { manager_name: name } }))) {
        await terminateChild(started.child);
        await rm(settingsPath, { force: true });
        return { category: 'orphan_unknown' };
      }
      const completion = started.completion.finally(() => rm(settingsPath, { force: true }));
      const result = await completion;
      const match = result.stdout.match(/(?:backgrounded|session|id)[^A-Za-z0-9_-]+([A-Za-z0-9_-]{4,})/i);
      if (!(await updateIntent(root, intent, { child_identity: { manager_id: match?.[1] || null, manager_name: name } }))) {
        if (match?.[1]) await execFile('claude', ['stop', match[1]], { cwd: workspace, timeout: 5000 });
        return { category: 'orphan_unknown' };
      }
      return { category: null, completion: Promise.resolve(result), background: true };
  }
  if (mode === 'claude_print') {
    const started = startTracked({
      command: 'claude',
      args: ['-p', prompt, '--permission-mode', 'dontAsk', '--allowedTools', 'Read Glob Grep Write Edit Bash'],
      cwd: workspace, env, timeoutMs,
        detached: true, captureOutput: false,
      });
    const identity = await childIdentity(started.child.pid);
    if (!(await updateIntent(root, intent, { launch_state: 'launched', child_identity: identity }))) {
      await terminateChild(started.child);
      return { category: 'orphan_unknown' };
    }
    return { category: null, completion: started.completion };
  }
  const started = startTracked({
    command: 'codex',
    args: ['-a', 'never', 'exec', '--ephemeral', '-s', 'workspace-write', '-C', workspace, '--color', 'never', '-'],
    cwd: workspace, env, input: prompt, timeoutMs,
    detached: true, captureOutput: false,
  });
  const identity = await childIdentity(started.child.pid);
  if (!(await updateIntent(root, intent, { launch_state: 'launched', child_identity: identity }))) {
    await terminateChild(started.child);
    return { category: 'orphan_unknown' };
  }
  return { category: null, completion: started.completion };
}

async function recordProgress(root, pre, post, count, intent) {
  const result = await withWorkspaceOwnership(root, async () => {
    const lease = await json(join(root, 'lease.json'));
    const current = await json(join(root, 'intent.json'));
    if (lease?.lease_id !== intent.lease_id || !current
      || current.attempt_id !== intent.attempt_id
      || current.lease_id !== intent.lease_id
      || current.launch_token !== intent.launch_token) return false;
    const loaded = await json(join(root, 'last-run.json'));
    if (loaded && loaded.schema !== 1) return false;
    const previous = loaded || {};
    const same = pre.complete && post.complete && pre.fingerprint === post.fingerprint;
    const progress = post.complete && (post.count === 0 || post.fingerprint !== pre.fingerprint);
    const noProgressCount = same ? Number(previous.no_progress_count || 0) + 1 : 0;
    const failureCount = progress ? 0 : Number(previous.failure_count || 0) + 1;
    const status = post.complete && post.count === 0 ? 'ok' : post.complete && !same ? 'partial' : 'failed';
    const suppressed = noProgressCount >= 3 ? pre.fingerprint : progress ? null : previous.suppressed_fingerprint || null;
    const backoff = progress || same ? null : Date.now() + Math.min(3600000, 300000 * Math.max(1, failureCount));
    await atomicJson(join(root, 'last-run.json'), {
      schema: 1, completed_at: Date.now(), attempt_id: intent.attempt_id, status,
      pre_fingerprint: pre.fingerprint, post_fingerprint: post.fingerprint,
      fingerprint_complete: post.complete, actionable_count: count,
      failure_count: failureCount, no_progress_count: noProgressCount,
      suppressed_fingerprint: suppressed, backoff_until: backoff,
    });
    return { status, complete: post.complete };
  }).catch(() => false);
  if (result) await appendLog(root, 'outcome', { status: result.status, fingerprint_complete: result.complete, actionable_count: count });
  return Boolean(result);
}

export async function runWorker({
  harness, workspace, globalRoot, skillsRoot, env = process.env, platform = process.platform,
  runtimeRunner, readiness, modelRunner, openCodeSession,
} = {}) {
  const canonical = await canonicalWorkspace(workspace);
  const root = runRoot(globalRoot, canonical);
  const config = await readIngestConfig(globalRoot, env);
  await ensureRoot(root);
  const leaseResult = await acquireLease(root, canonical, harness, config, openCodeSession);
  if (leaseResult.status === 'held') { await writeSkip(root, 'lease_held'); return { reason: 'lease_held' }; }
  if (leaseResult.status === 'orphan_live') { await writeSkip(root, 'orphan_live'); return { reason: 'orphan_live' }; }
  if (leaseResult.status === 'orphan_unknown') { await writeSkip(root, 'orphan_unknown'); return { reason: 'orphan_unknown' }; }
  if (leaseResult.status !== 'acquired') { await writeSkip(root, 'runtime_unavailable'); return { reason: 'runtime_unavailable' }; }
  const lease = leaseResult.lease;
  const skip = (reason, fields = {}) => writeSkip(root, reason, { ...fields, __lease_id: lease.lease_id });
  let intent = null;
  let keepIntent = false;
  let retainOwnership = false;
  try {
    if (!config.enabled) { await skip('disabled'); return { reason: 'disabled' }; }
    const localOutcome = await json(join(root, 'last-run.json'));
    if (localOutcome && localOutcome.schema !== 1) { await skip('schema_unknown'); return { reason: 'schema_unknown' }; }
    if (Number(localOutcome?.backoff_until || 0) > Date.now()) {
      await skip('backoff'); return { reason: 'backoff' };
    }
    if (Number(localOutcome?.completed_at || 0) + config.min_interval_seconds * 1000 > Date.now()
      && localOutcome?.status === 'ok') {
      await skip('debounced'); return { reason: 'debounced' };
    }
    const ready = readiness || await checkReadiness({ globalRoot, skillsRoot, env, platform });
    if (!ready.ready) { await skip(ready.category || 'runtime_unavailable'); return { reason: ready.category || 'runtime_unavailable' }; }
    const stateResult = await probeFullState({ readiness: ready, workspace: canonical, timeoutMs: 20000, runner: runtimeRunner });
    if (!stateResult.ready) {
      const reason = stateResult.category === 'timeout' ? 'probe_timeout' : stateResult.category === 'malformed_state' ? 'malformed_state' : stateResult.category === 'runtime_failed' ? 'probe_failed' : 'runtime_unavailable';
      await skip(reason); return { reason };
    }
    const state = stateResult.state;
    if (!validState(state)) { await skip('schema_unknown'); return { reason: 'schema_unknown' }; }
    if (!state.wiki_root) { await skip('wiki_missing'); return { reason: 'wiki_missing' }; }
    if (!(await hasExistingWiki(state.wiki_root))) { await skip('wiki_missing'); return { reason: 'wiki_missing' }; }
    if (!(await hasExistingCodegraph(state.wiki_root))) { await skip('codegraph_missing'); return { reason: 'codegraph_missing' }; }
    const pending = pendingHint(state);
    if (!pending || pendingCount(pending) === 0) { await skip('no_pending'); return { reason: 'no_pending' }; }
    let exclusionsPath;
    try { exclusionsPath = await resolveExclusions(skillsRoot || env.LOAM_INGEST_SKILLS_ROOT); }
    catch { await skip('exclusions_unavailable'); return { reason: 'exclusions_unavailable' }; }
    const diffResult = await diff({ readiness: ready, workspace: canonical, wikiRoot: state.wiki_root, exclusionsPath, runner: runtimeRunner });
    if (diffResult.error) { await skip(diffResult.error); return { reason: diffResult.error }; }
    let fingerprint;
    try { fingerprint = await fingerprintActionable({ workspace: canonical, entries: diffResult.entries, exclusionsPath, deadlineMs: 20000 }); }
    catch (error) { const reason = error.reason || 'fingerprint_unavailable'; await skip(reason); return { reason }; }
    if (fingerprint.count === 0) { await skip('no_actionable_work', { actionable_count: 0, actionable_fingerprint: fingerprint.fingerprint }); return { reason: 'no_actionable_work' }; }
    if (!fingerprint.complete) { await skip('fingerprint_unavailable', { actionable_count: fingerprint.count, actionable_fingerprint: fingerprint.fingerprint }); return { reason: 'fingerprint_unavailable' }; }
    const previousRecord = await json(join(root, 'last-run.json'));
    if (previousRecord && previousRecord.schema !== 1) { await skip('schema_unknown'); return { reason: 'schema_unknown' }; }
    const previous = previousRecord || {};
    if (previous.suppressed_fingerprint === fingerprint.fingerprint) { await skip('no_progress_suppressed', { actionable_count: fingerprint.count, actionable_fingerprint: fingerprint.fingerprint }); return { reason: 'no_progress_suppressed' }; }
    const existingIntent = await json(join(root, 'intent.json'));
    if (existingIntent && existingIntent.schema !== 1) { await skip('schema_unknown'); return { reason: 'schema_unknown' }; }
    if (existingIntent?.lease_id === lease.lease_id && existingIntent.actionable_fingerprint === fingerprint.fingerprint) {
      await skip('duplicate_intent'); return { reason: 'duplicate_intent' };
    }
    const selectedLaunchMode = await launchMode({ harness, workspace: canonical, env, root });
    intent = {
      schema: 1, state: 'active', attempt_id: randomBytes(16).toString('hex'), lease_id: lease.lease_id,
      actionable_fingerprint: fingerprint.fingerprint, launch_token: randomBytes(16).toString('hex'),
      launch_mode: selectedLaunchMode,
      launch_state: 'planned',
      planned_identity: selectedLaunchMode === 'claude_bg'
        ? {
            name: 'loam-ingest-' + hash(canonical).slice(0, 10) + '-' + randomUUID().slice(0, 8),
            owner_identity: { pid: lease.owner_pid, boot_id: lease.boot_id, process_start: lease.process_start },
          }
        : selectedLaunchMode === 'opencode_child'
          ? {
              parent_session_id: openCodeSession?.parentSessionId || null,
              title: 'Loam background code ingestion',
              owner_identity: { pid: lease.owner_pid, boot_id: lease.boot_id, process_start: lease.process_start },
            }
        : {
            boot_id: lease.boot_id,
            launch_at: new Date().toISOString(),
            token: randomBytes(16).toString('hex'),
            owner_identity: { pid: lease.owner_pid, boot_id: lease.boot_id, process_start: lease.process_start },
          },
      child_identity: null,
      hard_deadline: new Date(Date.now() + config.timeout_seconds * 1000).toISOString(),
      created_at: new Date().toISOString(),
    };
    const published = await publishIntent(root, lease.lease_id, intent);
    if (published.status !== 'published') {
      if (published.status === 'orphan_unknown') return { reason: 'orphan_unknown' };
      await skip(published.status);
      return { reason: published.status };
    }
    let launch;
    try {
      launch = modelRunner
        ? await modelRunner({ harness, workspace: canonical, intent, root })
        : await launchModel({ launchMode: intent.launch_mode, workspace: canonical, env: { ...env, LOAM_INGEST_GLOBAL_ROOT: globalRoot }, timeoutMs: config.timeout_seconds * 1000, intent, openCodeSession, root });
    } catch (error) {
      if (intent.child_identity) { keepIntent = true; retainOwnership = true; }
      await skip('runtime_unavailable', { detail: error instanceof Error ? error.message.slice(0, 256) : String(error).slice(0, 256), __intent: intent });
      return { reason: 'runtime_unavailable' };
    }
    if (launch.category) {
      if (['orphan_live', 'orphan_unknown'].includes(launch.category)) { keepIntent = true; retainOwnership = true; }
      await skip(launch.category, { __intent: intent }); return { reason: launch.category };
    }
    const result = await (launch.completion || Promise.resolve({ code: 0 }));
    if (!launch.background && result.category === 'timeout' && intent.child_identity) {
      const childState = await classifyChild(intent.child_identity, { platform });
      if (childState === 'live' || childState === 'unknown') {
        keepIntent = true;
        retainOwnership = true;
        const reason = childState === 'live' ? 'orphan_live' : 'orphan_unknown';
        await skip(reason, { __intent: intent });
        return { reason };
      }
    }
    if (launch.background) {
      let manager = intent.launch_mode === 'claude_bg'
        ? await waitForClaude(canonical, intent, Date.parse(intent.hard_deadline))
        : await waitForOpenCode(openCodeSession, launch.sessionId, Date.parse(intent.hard_deadline));
      if (manager.state === 'live') {
        if (intent.launch_mode === 'claude_bg') {
          await stopClaude(canonical, intent);
          manager = await waitForClaude(canonical, intent, Date.now() + 5000);
        } else if (typeof openCodeSession?.abort === 'function') {
          try { await openCodeSession.abort(launch.sessionId); } catch {}
          manager = await waitForOpenCode(openCodeSession, launch.sessionId, Date.now() + 5000);
        }
      }
      if (manager.state === 'live' || manager.state === 'unknown') {
        keepIntent = true;
        retainOwnership = true;
        const reason = manager.state === 'live' ? 'orphan_live' : 'orphan_unknown';
        await skip(reason, { __intent: intent });
        return { reason };
      }
    }
    const postState = await probeFullState({ readiness: ready, workspace: canonical, timeoutMs: 20000, runner: runtimeRunner });
    let post = { fingerprint: '', complete: false, count: 0 };
    try {
      if (postState.ready && postState.state?.wiki_root && await hasExistingWiki(postState.state.wiki_root)
        && await hasExistingCodegraph(postState.state.wiki_root)) {
        const postDiff = await diff({ readiness: ready, workspace: canonical, wikiRoot: postState.state.wiki_root, exclusionsPath, runner: runtimeRunner });
        if (!postDiff.error) post = await fingerprintActionable({ workspace: canonical, entries: postDiff.entries, exclusionsPath, deadlineMs: 20000 });
      }
    } catch {}
    if (result.category || (typeof result.code === 'number' && result.code !== 0)) post.complete = false;
    await recordProgress(root, fingerprint, post, fingerprint.count, intent);
    return { reason: result?.category === 'timeout' ? 'probe_timeout' : 'ok' };
  } finally {
    if (intent && !keepIntent && await liveOwnedIntent(root, canonical, openCodeSession, intent)) {
      keepIntent = true;
      retainOwnership = true;
    }
    if (intent && !keepIntent) await retireIntent(root, intent);
    if (!retainOwnership) await releaseLease(root, lease.lease_id);
  }
}

export async function ingestStatus({ globalRoot, workspace, env = process.env } = {}) {
  const canonical = await canonicalWorkspace(workspace);
  const root = runRoot(globalRoot, canonical);
  const leaseRecord = await jsonRecord(join(root, 'lease.json'));
  const lease = leaseRecord.value || null;
  const leaseState = leaseRecord.malformed || (leaseRecord.present && (!lease || typeof lease !== 'object' || Array.isArray(lease)))
    ? 'unknown'
    : lease
    ? await classifyChild({ pid: lease.owner_pid, boot_id: lease.boot_id, process_start: lease.process_start })
    : 'dead';
  const intentRecord = await jsonRecord(join(root, 'intent.json'));
  const intent = intentRecord.value || null;
  const lastRun = await json(join(root, 'last-run.json'));
  const intentState = intentRecord.present
    ? await inspectIntent(join(root, 'intent.json'), canonical)
    : { state: 'dead' };
  let exclusions;
  try { exclusions = { ready: true, path: await resolveExclusions(env.LOAM_INGEST_SKILLS_ROOT) }; }
  catch (error) { exclusions = { ready: false, reason: error.reason || 'exclusions_unavailable' }; }
  const records = (await readFile(join(root, 'log.jsonl'), 'utf8').catch(() => ''))
    .split('\n').filter(Boolean).slice(-16).map((line) => jsonLine(line)).filter(Boolean);
  return {
    schema: 1, workspace: canonical, enabled: (await readIngestConfig(globalRoot, env)).enabled,
    lease, lease_state: leaseState, intent,
    intent_state: intentState.state,
    orphan: intentState.state === 'live' || intentState.state === 'unknown',
    exclusions,
    last_run: lastRun,
    diagnostics: {
      count: lastRun?.actionable_count ?? null,
      fingerprint: lastRun?.actionable_fingerprint || lastRun?.post_fingerprint || null,
    },
    events: await json(join(root, 'events.json'), { schema: 1, entries: [] }),
    queue: { root }, records,
  };
}

function jsonLine(line) { try { return JSON.parse(line); } catch { return null; } }

export { inspectIntent, resolveExclusions };
