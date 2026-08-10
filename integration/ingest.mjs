import { createHash, randomUUID } from 'node:crypto';
import { mkdir, readFile, realpath, rename, rm, stat, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';

import { assertInside, resolveSkillsRoot } from './paths.mjs';
import { readInstallMetadata } from './metadata.mjs';
import { checkReadiness, invokeRuntime, probeFullState } from './runtime.mjs';
import {
  bootIdentity, childIdentity, classifyChild, execFile, processStartIdentity,
  spawnDetached, startTracked, terminateChild,
} from './ingest-process.mjs';
import { FingerprintError, fingerprintActionable } from './ingest-fingerprint.mjs';

export const PROMPT = 'Run the existing loam::ingesting-codebase skill for the provided workspace. Do not modify source files, commit, or push. Do not spawn other agents or subagents.';
const DEFAULTS = Object.freeze({ enabled: true, min_interval_seconds: 300, timeout_seconds: 900, lease_ttl_seconds: 1800, visibility: 'native', require_visible_worker: false });
const CODEX_NATIVE_REASON = 'Call spawn_agent exactly once using the loam_ingestor agent profile, fork_turns set to "none", and task_name set to "loam_ingest_stop_<N>" where <N> is the numeric component of this hook_prompt\'s hook_run_id. Run the pending Loam code-memory ingestion, then finish this continuation immediately without doing any other work or spawning any additional agents.';
const NATIVE_AGENT_ID = /^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/;
// Notification surfaces are local IPC; 250 ms caps terminal teardown without making a hung surface part of ingestion latency.
const NOTIFICATION_TIMEOUT_MS = 250;
function hash(value) { return createHash('sha256').update(String(value)).digest('hex'); }
export function claudeSessionName(workspace) { return `loam-ingest-${hash(workspace).slice(0, 10)}`; }
export function runRoot(globalRoot, workspace) { return join(resolve(globalRoot), 'run', hash(workspace).slice(0, 16)); }
async function json(path, fallback = null) { try { return JSON.parse(await readFile(path, 'utf8')); } catch { return fallback; } }
async function jsonRecord(path) {
  try { return { present: true, value: JSON.parse(await readFile(path, 'utf8')) }; }
  catch (error) { return error?.code === 'ENOENT' ? { present: false } : { present: true, malformed: true }; }
}
async function ensureRoot(root) { await mkdir(root, { recursive: true, mode: 0o700 }); }
async function delay(ms) { await new Promise((resolvePromise) => setTimeout(resolvePromise, ms)); }
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
function numeric(value, fallback) { const parsed = Number(value); return Number.isFinite(parsed) && parsed >= 0 ? parsed : fallback; }
function visibility(value) { return ['silent', 'toast', 'native'].includes(value) ? value : DEFAULTS.visibility; }

// Returns the delivery outcome so callers can emit a matching visibility_delivery
// event: 'emitted' (fulfilled), 'failed' (threw/rejected), 'aborted' (250 ms
// deadline won), or null when no attempt was made (silent, or no notifier).
async function sendNotification(notify, configuredVisibility, event) {
  if (configuredVisibility === 'silent' || typeof notify !== 'function') return null;
  // Contract: notifiers must pass this signal to every resource they open; the seam cannot cancel resources that ignore it.
  const controller = new AbortController();
  let timer;
  try {
    return await Promise.race([
      Promise.resolve()
        .then(() => notify({ ...event, visibility: configuredVisibility, signal: controller.signal }))
        .then(() => 'emitted', () => 'failed'),
      new Promise((resolvePromise) => {
        timer = setTimeout(() => { controller.abort(); resolvePromise('aborted'); }, NOTIFICATION_TIMEOUT_MS);
      }),
    ]);
  } catch { return 'failed'; }
  finally { clearTimeout(timer); }
}

export async function readIngestConfig(globalRoot, env = process.env) {
  const file = await json(join(resolve(globalRoot), 'config.json'), {});
  const section = file?.background_ingest || {};
  const enabled = env.LOAM_INGEST_BACKGROUND === '0'
    ? false : env.LOAM_INGEST_BACKGROUND === '1' ? true : section.enabled !== false;
  return {
    enabled,
    min_interval_seconds: numeric(env.LOAM_INGEST_MIN_INTERVAL, numeric(section.min_interval_seconds, DEFAULTS.min_interval_seconds)),
    timeout_seconds: numeric(env.LOAM_INGEST_TIMEOUT, numeric(section.timeout_seconds, DEFAULTS.timeout_seconds)),
    lease_ttl_seconds: numeric(env.LOAM_INGEST_LEASE_TTL, numeric(section.lease_ttl_seconds, DEFAULTS.lease_ttl_seconds)),
    visibility: visibility(section.visibility),
    require_visible_worker: section.require_visible_worker === true,
  };
}

export async function canonicalWorkspace(value = process.cwd()) {
  const path = resolve(String(value));
  try { return await realpath(path); } catch { return path; }
}

function payloadWorkspace(payload = {}) { return payload.cwd || payload.directory || payload.workspace?.root || process.cwd(); }

function publicReason(reason) {
  if (reason === 'disabled' || reason === 'recursion') return 'disabled';
  if (['lease_held', 'orphan_live', 'orphan_unknown', 'duplicate_intent'].includes(reason)) return 'busy';
  if (reason === 'debounced' || reason === 'backoff') return 'too_soon';
  if (['wiki_missing', 'codegraph_missing', 'no_pending', 'no_actionable_work'].includes(reason)) return 'nothing_to_do';
  return reason === 'ok' ? 'ok' : 'unavailable';
}

function recursion(payload, env) {
  return env.LOAM_INGEST_WORKER === '1' || env.LOAM_INGEST_CHILD === '1'
    || payload.stop_hook_active === true || payload.loam_ingest_child === true || payload.child_session === true
    || payload.agent_type === 'loam:ingestor';
}

export async function gate({ harness, payload = {}, globalRoot, env = process.env, now = Date.now() } = {}) {
  const config = await readIngestConfig(globalRoot, env);
  if (!config.enabled) return { action: 'skip', reason: 'disabled' };
  // Mark the recursion refusal distinctly from a config-disabled skip so the
  // hook-finish producer can emit claude_recursion_guard only for the former.
  if (recursion(payload, env)) return { action: 'skip', reason: 'disabled', recursion: true };
  const workspace = await canonicalWorkspace(payloadWorkspace(payload));
  const root = runRoot(globalRoot, workspace);
  await ensureRoot(root);
  const last = await json(join(root, 'last-run.json'), null);
  if (last && last.schema !== 1) return { action: 'skip', reason: publicReason('schema_unknown'), workspace };
  const previous = last || {};
  if (Number(previous.backoff_until || 0) > now) return { action: 'skip', reason: publicReason('backoff'), workspace };
  if (Number(previous.completed_at || 0) + config.min_interval_seconds * 1000 > now
    && (previous.status === 'ok' || (previous.status === 'skipped' && previous.reason === 'nothing_to_do'))) {
    return { action: 'skip', reason: publicReason('debounced'), workspace };
  }
  return { action: 'spawn_worker', workspace, config };
}

export function startWorker({
  harness, workspace, globalRoot, skillsRoot, workerPath, hookRunId, workerOrigin,
  env = process.env, spawn = spawnDetached,
} = {}) {
  if (!workerPath) throw new Error('installed ingestion worker is unavailable');
  return spawn({
    command: process.execPath,
    args: [
      workerPath, '--harness', harness, '--workspace', resolve(workspace),
      ...(Number.isSafeInteger(hookRunId) && hookRunId > 0 ? ['--hook-run-id', String(hookRunId)] : []),
      ...(workerOrigin === 'fallback' ? ['--worker-origin', 'fallback'] : []),
    ],
    cwd: resolve(workspace),
    env: { ...env, LOAM_INGEST_WORKER: '1', LOAM_INGEST_GLOBAL_ROOT: resolve(globalRoot), ...(skillsRoot ? { LOAM_INGEST_SKILLS_ROOT: resolve(skillsRoot) } : {}) },
  });
}

async function installedWorkerPath(globalRoot) {
  const install = await json(join(resolve(globalRoot), 'install.json'));
  if (typeof install?.adapter_root !== 'string') return null;
  return assertInside(resolve(globalRoot), join(resolve(install.adapter_root), 'ingest-worker.mjs'), 'worker path');
}

function nativeIntentPaths(globalRoot, workspace) {
  const root = runRoot(globalRoot, workspace);
  return {
    root,
    intentPath: join(root, 'native-intent.json'),
    claimPath: join(root, 'native-claim.json'),
    lockPath: join(root, 'native-intent.lock'),
  };
}

function nativeAgentPath(root, agentId) {
  return join(root, `native-agent-${hash(agentId).slice(0, 16)}.json`);
}

function nativeSessionId(value) {
  return typeof value === 'string' && value.length > 0 && [...value].length <= 256 && !/[\u0000-\u001F\u007F]/u.test(value)
    ? value : null;
}

function validNativeIntent(value, workspace, now) {
  return value && typeof value === 'object' && !Array.isArray(value)
    && value.schema === 1 && typeof value.intent_id === 'string' && value.intent_id.length <= 64
    && value.workspace === workspace && value.harness === 'codex'
    && ['pending', 'fallback', 'agent'].includes(value.claim)
    && Number.isFinite(value.created_at) && Number.isFinite(value.expires_at) && value.expires_at > now
    && (value.session_id === null || nativeSessionId(value.session_id) === value.session_id)
    && (value.claim !== 'agent' || NATIVE_AGENT_ID.test(value.agent_id));
}

function validNativeAgent(value, workspace, agentId, now) {
  return value && typeof value === 'object' && !Array.isArray(value)
    && value.schema === 1 && value.workspace === workspace && value.harness === 'codex'
    && value.agent_id === agentId && NATIVE_AGENT_ID.test(value.agent_id)
    && typeof value.intent_id === 'string' && value.intent_id.length <= 64
    && Number.isFinite(value.expires_at) && (now === undefined || value.expires_at > now);
}

async function installedNativePaths(globalRoot) {
  const root = resolve(globalRoot);
  const install = await readInstallMetadata(root);
  const adapterPath = install.adapter_root;
  const integrationPath = install.integration_path;
  const workerPath = assertInside(root, join(adapterPath, 'ingest-worker.mjs'), 'worker path');
  const [adapter, integration, worker] = await Promise.all([stat(adapterPath), stat(integrationPath), stat(workerPath)]);
  if (!adapter.isDirectory() || !integration.isFile() || !worker.isFile()) throw new Error('installed native paths are unavailable');
  return { adapter_path: adapterPath, integration_path: integrationPath, worker_path: workerPath };
}

async function withNativeIntentLock(paths, callback) {
  await ensureRoot(paths.root);
  for (let attempt = 0; attempt < 2; attempt += 1) {
    const lock = { schema: 1, lock_id: randomUUID(), expires_at: Date.now() + 1000 };
    try {
      await writeFile(paths.lockPath, JSON.stringify(lock) + '\n', { flag: 'wx', mode: 0o600 });
    } catch (error) {
      if (error?.code !== 'EEXIST') return { status: 'unavailable' };
      const existing = await json(paths.lockPath);
      if (Number(existing?.expires_at || 0) > Date.now()) return { status: 'busy' };
      await rm(paths.lockPath, { force: true }).catch(() => {});
      continue;
    }
    try { return await callback(); }
    finally {
      const current = await json(paths.lockPath);
      if (current?.lock_id === lock.lock_id) await rm(paths.lockPath, { force: true }).catch(() => {});
    }
  }
  return { status: 'busy' };
}

function createNativeIntent({ workspace, payload, config, hookRunId, now, claim }) {
  return {
    schema: 1,
    intent_id: randomUUID(),
    workspace,
    harness: 'codex',
    session_id: nativeSessionId(payload?.session_id),
    hook_run_id: Number.isSafeInteger(hookRunId) && hookRunId > 0 ? hookRunId : null,
    claim,
    created_at: now,
    expires_at: now + config.timeout_seconds * 1000,
  };
}

async function recordNativeIntent({ globalRoot, workspace, payload, config, hookRunId, now }) {
  const paths = nativeIntentPaths(globalRoot, workspace);
  return withNativeIntentLock(paths, async () => {
    const [intentRecord, claimRecord] = await Promise.all([
      jsonRecord(paths.intentPath),
      jsonRecord(paths.claimPath),
    ]);
    const active = [claimRecord.value, intentRecord.value].find((value) => validNativeIntent(value, workspace, now));
    if (active) return { status: 'duplicate', intent: active };
    await Promise.all([rm(paths.intentPath, { force: true }), rm(paths.claimPath, { force: true })]);
    const intent = createNativeIntent({ workspace, payload, config, hookRunId, now, claim: 'pending' });
    try {
      await writeFile(paths.intentPath, JSON.stringify(intent) + '\n', { flag: 'wx', mode: 0o600 });
      return { status: 'recorded', intent };
    } catch (error) {
      return { status: error?.code === 'EEXIST' ? 'duplicate' : 'unavailable' };
    }
  });
}

async function claimNativeFallback({ globalRoot, workspace, payload, config, hookRunId, now }) {
  const paths = nativeIntentPaths(globalRoot, workspace);
  return withNativeIntentLock(paths, async () => {
    const [intentRecord, claimRecord] = await Promise.all([
      jsonRecord(paths.intentPath),
      jsonRecord(paths.claimPath),
    ]);
    if (validNativeIntent(claimRecord.value, workspace, now)) {
      return { status: claimRecord.value.claim === 'agent' ? 'bound' : 'duplicate', intent: claimRecord.value };
    }
    let intent = validNativeIntent(intentRecord.value, workspace, now) ? intentRecord.value : null;
    const sessionId = nativeSessionId(payload?.session_id);
    if (intent && intent.session_id && sessionId && intent.session_id !== sessionId) return { status: 'duplicate', intent };
    if (!intent) intent = createNativeIntent({ workspace, payload, config, hookRunId, now, claim: 'fallback' });
    else intent = { ...intent, claim: 'fallback', claimed_at: now, hook_run_id: Number.isSafeInteger(hookRunId) && hookRunId > 0 ? hookRunId : intent.hook_run_id };
    await rm(paths.claimPath, { force: true });
    try {
      await writeFile(paths.claimPath, JSON.stringify(intent) + '\n', { flag: 'wx', mode: 0o600 });
      await rm(paths.intentPath, { force: true });
      return { status: 'claimed', intent };
    } catch (error) {
      return { status: error?.code === 'EEXIST' ? 'duplicate' : 'unavailable' };
    }
  });
}

async function clearNativeFallback(globalRoot, workspace, intentId) {
  const paths = nativeIntentPaths(globalRoot, workspace);
  await withNativeIntentLock(paths, async () => {
    const claim = await json(paths.claimPath);
    if (claim?.claim === 'fallback' && claim.intent_id === intentId) await rm(paths.claimPath, { force: true });
    return { status: 'cleared' };
  });
}

export async function bindNativeAgent({ globalRoot, workspace, agentId, now = Date.now() } = {}) {
  if (!NATIVE_AGENT_ID.test(agentId || '')) return { status: 'invalid' };
  const canonical = await canonicalWorkspace(workspace);
  let installed;
  try { installed = await installedNativePaths(globalRoot); }
  catch { return { status: 'unavailable' }; }
  const paths = nativeIntentPaths(globalRoot, canonical);
  return withNativeIntentLock(paths, async () => {
    const agentPath = nativeAgentPath(paths.root, agentId);
    const existing = await jsonRecord(agentPath);
    if (validNativeAgent(existing.value, canonical, agentId, now)
      && ['bound', 'preparing', 'prepared'].includes(existing.value.state)) {
      return {
        status: existing.value.owns_claim ? 'bound' : 'late',
        ...installed,
        ...existing.value,
      };
    }
    const [intentRecord, claimRecord] = await Promise.all([
      jsonRecord(paths.intentPath),
      jsonRecord(paths.claimPath),
    ]);
    if (intentRecord.malformed || claimRecord.malformed) return { status: 'malformed' };
    const intent = validNativeIntent(intentRecord.value, canonical, now) ? intentRecord.value : null;
    const claim = validNativeIntent(claimRecord.value, canonical, now) ? claimRecord.value : null;
    const source = claim || intent;
    if (!source) return { status: 'missing' };
    const ownsClaim = source.claim === 'pending' || (source.claim === 'agent' && source.agent_id === agentId);
    if (source.claim === 'pending') {
      const bound = { ...source, claim: 'agent', agent_id: agentId, claimed_at: now };
      await atomicJson(paths.claimPath, bound);
      await rm(paths.intentPath, { force: true });
    }
    const record = {
      ...source,
      ...installed,
      schema: 1,
      claim: ownsClaim ? 'agent' : source.claim,
      agent_id: agentId,
      owns_claim: ownsClaim,
      state: 'bound',
      bound_at: now,
    };
    await atomicJson(agentPath, record);
    return { status: ownsClaim ? 'bound' : 'late', ...record };
  });
}

function persistedPreparation(prepared) {
  const {
    action, harness, workspace, globalRoot, skillsRoot, platform, root,
    config, lease, readiness, exclusionsPath, fingerprint, events,
  } = prepared;
  return {
    action, harness, workspace, globalRoot, skillsRoot, platform, root,
    config, lease, readiness, exclusionsPath, fingerprint,
    // Carry the buffered preparation event across the native prepare/stop
    // process boundary so finalization can find its causal parent.
    events: Array.isArray(events) ? events : [],
  };
}

async function updateNativeAgent(paths, agentId, prepareId, update) {
  return withNativeIntentLock(paths, async () => {
    const path = nativeAgentPath(paths.root, agentId);
    const record = await json(path);
    if (!record || (prepareId && record.prepare_id !== prepareId)) return { status: 'missing' };
    await atomicJson(path, { ...record, ...update, updated_at: Date.now() });
    return { status: 'updated' };
  });
}

export async function prepareNativeAgentRun({
  globalRoot, workspace, agentId, skillsRoot, env = process.env, platform = process.platform,
  runtimeRunner, readiness,
} = {}) {
  if (!NATIVE_AGENT_ID.test(agentId || '')) return { action: 'skip', reason: 'unavailable' };
  const canonical = await canonicalWorkspace(workspace);
  const paths = nativeIntentPaths(globalRoot, canonical);
  const prepareId = randomUUID();
  const admitted = await withNativeIntentLock(paths, async () => {
    const path = nativeAgentPath(paths.root, agentId);
    const record = await json(path);
    if (!validNativeAgent(record, canonical, agentId, Date.now()) || record.state !== 'bound') return { status: 'busy' };
    await atomicJson(path, { ...record, state: 'preparing', prepare_id: prepareId, updated_at: Date.now() });
    return { status: 'admitted' };
  });
  if (admitted.status !== 'admitted') return { action: 'skip', reason: admitted.status === 'busy' ? 'busy' : 'unavailable' };
  try {
    const prepared = await prepareWorkerRun({
      harness: 'codex', workspace: canonical, globalRoot, skillsRoot, env, platform,
      runtimeRunner, readiness, nativeAgentId: agentId,
    });
    if (prepared.action !== 'run') {
      const result = prepared.result || { reason: 'unavailable' };
      await updateNativeAgent(paths, agentId, prepareId, { state: 'skipped', result });
      return { action: 'skip', reason: result.reason };
    }
    if (!(await updateLease(prepared.root, prepared.lease, { launch_state: 'launched' }))) {
      await finalizeWorkerRun(prepared, { launch: { background: false }, skipReason: 'orphan_unknown' });
      await updateNativeAgent(paths, agentId, prepareId, { state: 'failed', result: { reason: 'unavailable' } });
      return { action: 'skip', reason: 'unavailable' };
    }
    const updated = await updateNativeAgent(paths, agentId, prepareId, {
      state: 'prepared',
      prepared: persistedPreparation(prepared),
    });
    if (updated.status !== 'updated') {
      await finalizeWorkerRun(prepared, { launch: { background: false }, skipReason: 'orphan_unknown' });
      return { action: 'skip', reason: 'unavailable' };
    }
    return { action: 'run' };
  } catch {
    await updateNativeAgent(paths, agentId, prepareId, { state: 'failed', result: { reason: 'unavailable' } });
    return { action: 'skip', reason: 'unavailable' };
  }
}

async function finishNativeRecord(paths, record, update) {
  await withNativeIntentLock(paths, async () => {
    const path = nativeAgentPath(paths.root, record.agent_id);
    const current = await json(path);
    if (!current || current.intent_id !== record.intent_id) return { status: 'missing' };
    await atomicJson(path, { ...current, ...update, updated_at: Date.now() });
    if (current.owns_claim) {
      const claim = await json(paths.claimPath);
      if (claim?.intent_id === current.intent_id && claim.agent_id === current.agent_id) {
        await rm(paths.claimPath, { force: true });
      }
    }
    return { status: 'finished' };
  });
}

export async function finalizeNativeAgentRun({
  globalRoot, workspace, agentId, env = process.env, runtimeRunner,
} = {}) {
  if (!NATIVE_AGENT_ID.test(agentId || '')) return { reason: 'unavailable' };
  const canonical = await canonicalWorkspace(workspace);
  const paths = nativeIntentPaths(globalRoot, canonical);
  const claimed = await withNativeIntentLock(paths, async () => {
    const path = nativeAgentPath(paths.root, agentId);
    const record = await json(path);
    if (!validNativeAgent(record, canonical, agentId)) return { status: 'missing' };
    if (record.state === 'finished' || record.state === 'finalizing') return { status: 'busy' };
    if (record.state === 'skipped' || record.state === 'failed') return { status: 'terminal', record };
    await atomicJson(path, { ...record, state: 'finalizing', updated_at: Date.now() });
    return { status: record.state === 'prepared' && record.prepared ? 'prepared' : 'abort', record };
  });
  if (claimed.status === 'missing' || claimed.status === 'busy') return { reason: 'busy' };
  const record = claimed.record;
  const resultBase = {
    owns_claim: record.owns_claim,
    hook_run_id: record.hook_run_id,
    workspace: canonical,
  };
  if (claimed.status === 'terminal') {
    const reason = record.result?.reason || 'unavailable';
    await finishNativeRecord(paths, record, { state: 'finished', result: { reason } });
    return { reason, ...resultBase };
  }
  if (claimed.status === 'abort') {
    const lease = await json(join(paths.root, 'lease.json'));
    if (lease?.launch_mode === 'codex_native' && lease.child_identity?.agent_id === agentId) {
      await updateLease(paths.root, lease, { launch_state: 'terminal' });
      await writeSkip(paths.root, 'runtime_unavailable', { detail: 'native agent stopped before preparation completed' }, lease.lease_id);
      await releaseLease(paths.root, lease.lease_id);
    }
    await finishNativeRecord(paths, record, { state: 'finished', result: { reason: 'unavailable' } });
    return { reason: 'unavailable', ...resultBase };
  }
  const prepared = { ...record.prepared, env, runtimeRunner };
  await updateLease(paths.root, prepared.lease, { launch_state: 'terminal' });
  let outcome;
  try {
    outcome = await finalizeWorkerRun(prepared, { launch: { background: false }, result: { code: 0 } });
  } catch {
    outcome = { reason: 'unavailable' };
  }
  await finishNativeRecord(paths, record, { state: 'finished', prepared: null, result: outcome });
  return { reason: outcome.reason || 'unavailable', events: prepared.events, ...resultBase };
}

async function startBoundaryWorker(options, result, { nativeFallback = false, intentId } = {}) {
  try {
    const workerPath = await installedWorkerPath(options.globalRoot);
    const launch = () => startWorker({
      ...options, workerPath, workspace: result.workspace,
      ...(nativeFallback ? { workerOrigin: 'fallback' } : {}),
    });
    const outcome = nativeFallback ? { ...result, native_fallback: true } : result;
    if (options.deferSpawn) {
      // S1: hand the physical spawn back so the caller runs it only after
      // hook-finish returns, letting the worker's worker-start reliably see the
      // finished parent. The spawn stays best-effort and fail-open.
      return { ...outcome, spawn: async () => { try { launch(); } catch {} } };
    }
    launch();
    return outcome;
  } catch (error) {
    if (intentId) await clearNativeFallback(options.globalRoot, result.workspace, intentId).catch(() => {});
    return { action: 'skip', reason: 'unavailable', detail: error.message };
  }
}

async function preflightNativeBoundary(options, result) {
  const root = runRoot(options.globalRoot, result.workspace);
  const leaseResult = await acquireLease(root, result.workspace, 'codex', result.config, undefined, options.env);
  if (['held', 'orphan_live', 'orphan_unknown'].includes(leaseResult.status)) {
    return { action: 'skip', reason: 'busy', workspace: result.workspace };
  }
  if (leaseResult.status !== 'acquired') return { action: 'skip', reason: 'unavailable', workspace: result.workspace };
  try {
    const checked = await actionablePreflight({ ...options, workspace: result.workspace });
    if (checked.action === 'run') return result;
    await writeSkip(root, checked.reason, checked.fields, leaseResult.lease.lease_id);
    return { action: 'skip', reason: publicReason(checked.reason), workspace: result.workspace };
  } finally {
    await releaseLease(root, leaseResult.lease.lease_id);
  }
}

export async function dispatchBoundary(options = {}) {
  const now = Number.isFinite(options.now) ? options.now : Date.now();
  if (options.harness === 'codex' && options.payload?.stop_hook_active === true) {
    const config = await readIngestConfig(options.globalRoot, options.env);
    if (config.visibility === 'native') {
      const payload = { ...options.payload, stop_hook_active: false };
      const result = await gate({ ...options, payload, now });
      if (result.action !== 'spawn_worker') return result;
      const claimed = await claimNativeFallback({ ...options, payload, config, workspace: result.workspace, now })
        .catch(() => ({ status: 'unavailable' }));
      if (claimed.status === 'bound' || claimed.status === 'duplicate') {
        return { action: 'skip', reason: 'busy', workspace: result.workspace };
      }
      return startBoundaryWorker(options, result, {
        nativeFallback: true,
        intentId: claimed.intent?.intent_id,
      });
    }
  }
  const result = await gate({ ...options, now });
  if (result.action !== 'spawn_worker') return result;
  if (options.harness === 'codex' && result.config.visibility === 'native') {
    const preflight = await preflightNativeBoundary(options, result)
      .catch(() => ({ action: 'skip', reason: 'unavailable', workspace: result.workspace }));
    if (preflight.action !== 'spawn_worker') return preflight;
    const recorded = await recordNativeIntent({ ...options, config: result.config, workspace: result.workspace, now })
      .catch(() => ({ status: 'unavailable' }));
    if (recorded.status === 'recorded') {
      return {
        ...result,
        native_continuation: { decision: 'block', reason: CODEX_NATIVE_REASON },
      };
    }
    if (recorded.status === 'duplicate') return { action: 'skip', reason: 'busy', workspace: result.workspace };
    const claimed = await claimNativeFallback({ ...options, config: result.config, workspace: result.workspace, now })
      .catch(() => ({ status: 'unavailable' }));
    return startBoundaryWorker(options, result, {
      nativeFallback: true,
      intentId: claimed.intent?.intent_id,
    });
  }
  return startBoundaryWorker(options, result);
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

async function actionablePreflight({
  workspace, globalRoot, skillsRoot, env = process.env, platform = process.platform,
  runtimeRunner, readiness,
} = {}) {
  const ready = readiness || await checkReadiness({ globalRoot, skillsRoot, env, platform });
  if (!ready.ready) return { action: 'skip', reason: ready.category || 'runtime_unavailable' };
  const stateResult = await probeFullState({ readiness: ready, workspace, timeoutMs: 20000, runner: runtimeRunner });
  if (!stateResult.ready) {
    const reason = stateResult.category === 'timeout' ? 'probe_timeout'
      : stateResult.category === 'malformed_state' ? 'malformed_state'
      : stateResult.category === 'runtime_failed' ? 'probe_failed' : 'runtime_unavailable';
    return { action: 'skip', reason };
  }
  const state = stateResult.state;
  if (!validState(state)) return { action: 'skip', reason: 'schema_unknown' };
  if (!state.wiki_root || !(await hasExistingWiki(state.wiki_root))) return { action: 'skip', reason: 'wiki_missing' };
  if (!(await hasExistingCodegraph(state.wiki_root))) return { action: 'skip', reason: 'codegraph_missing' };
  const pending = pendingHint(state);
  if (!pending || pendingCount(pending) === 0) return { action: 'skip', reason: 'no_pending' };
  let exclusionsPath;
  try { exclusionsPath = await resolveExclusions(skillsRoot || env.LOAM_INGEST_SKILLS_ROOT); }
  catch { return { action: 'skip', reason: 'exclusions_unavailable' }; }
  const diffResult = await diff({ readiness: ready, workspace, wikiRoot: state.wiki_root, exclusionsPath, runner: runtimeRunner });
  if (diffResult.error) return { action: 'skip', reason: diffResult.error };
  let fingerprint;
  try { fingerprint = await fingerprintActionable({ workspace, entries: diffResult.entries, exclusionsPath, deadlineMs: 20000 }); }
  catch (error) { return { action: 'skip', reason: error.reason || 'fingerprint_unavailable' }; }
  const fields = { actionable_count: fingerprint.count, actionable_fingerprint: fingerprint.fingerprint };
  if (fingerprint.count === 0) return { action: 'skip', reason: 'no_actionable_work', fields };
  if (!fingerprint.complete) return { action: 'skip', reason: 'fingerprint_unavailable', fields };
  return { action: 'run', readiness: ready, exclusionsPath, fingerprint };
}

async function leaseOwner() {
  return { pid: process.pid, boot_id: await bootIdentity(), process_start: await processStartIdentity(process.pid) };
}

const CLAUDE_TERMINAL_STATES = ['done', 'failed', 'stopped', 'idle', 'completed', 'error'];

function claudeState(record) { return record?.state?.status || record?.state || record?.status; }
function claudeId(record) { return record?.id || record?.session_id || record?.sessionID || null; }

async function claudeAgentList(workspace, env = process.env) {
  const result = await execFile('claude', ['agents', '--json', '--cwd', workspace, '--all'], { cwd: workspace, timeout: 5000, env });
  if (result.code !== 0 || result.category === 'runtime_error') return null;
  let records; try { records = JSON.parse(result.stdout); } catch { return null; }
  return Array.isArray(records) ? records : records?.agents || [];
}

async function queryClaude(workspace, lease, env = process.env) {
  const list = await claudeAgentList(workspace, env);
  if (!list) return { state: 'unknown' };
  const identity = lease.child_identity || {};
  const plannedName = lease.planned_identity?.name;
  const match = plannedName
    ? list.find((item) => item.name === plannedName || item.title === plannedName)
      || list.find((item) => identity.manager_id && (item.id === identity.manager_id || item.session_id === identity.manager_id))
    : list.find((item) => identity.manager_id && (item.id === identity.manager_id || item.session_id === identity.manager_id));
  if (!match) return { state: 'dead' };
  const state = claudeState(match);
  if (['working', 'blocked', 'running', 'pending'].includes(state)) return { state: 'live', record: match };
  if (CLAUDE_TERMINAL_STATES.includes(state)) return { state: 'terminal', record: match };
  return { state: 'unknown', record: match };
}

// Every `claude --bg` ingestion leaves a permanent Agent View row and Claude never collects them, so the
// one surface Loam gives the user fills with its own dead records. Retaining the newest few keeps a
// failed run inspectable without letting the list grow without bound.
// ponytail: fixed retention count; make it configurable if anyone actually wants a different depth.
const CLAUDE_SESSION_RETENTION = 5;

async function pruneClaudeSessions(workspace, lease, env = process.env) {
  const list = await claudeAgentList(workspace, env);
  if (!list) return;
  const prefix = claudeSessionName(workspace);
  const current = lease.child_identity?.manager_id || null;
  const stale = list
    .filter((item) => typeof item?.name === 'string' && item.name.startsWith(prefix))
    .filter((item) => CLAUDE_TERMINAL_STATES.includes(claudeState(item)))
    .sort((a, b) => (Number(b?.startedAt) || 0) - (Number(a?.startedAt) || 0))
    .slice(CLAUDE_SESSION_RETENTION)
    .filter((item) => claudeId(item) && claudeId(item) !== current);
  for (const item of stale) {
    await execFile('claude', ['rm', String(claudeId(item))], { cwd: workspace, timeout: 5000, env });
  }
}

function sessionState(record) { return record?.type; }

function boundedIntent(state, lease, fields = {}) {
  const hardDeadline = Date.parse(lease.hard_deadline);
  return {
    state: state === 'unknown' && Number.isFinite(hardDeadline) && hardDeadline <= Date.now() ? 'terminal' : state,
    ...fields,
    intent: lease,
  };
}

async function inspectIntent(leaseRecord, workspace, openCodeSession, env = process.env) {
  if (!leaseRecord.present) return { state: 'dead', intent: null };
  if (leaseRecord.malformed || !leaseRecord.value || typeof leaseRecord.value !== 'object' || Array.isArray(leaseRecord.value)) {
    return { state: 'unknown', intent: null };
  }
  const lease = leaseRecord.value;
  if (lease.schema !== 1) return { state: 'unknown', intent: lease };
  if (!lease.launch_mode) return { state: 'dead', intent: lease };
  if (lease.launch_mode === 'claude_bg') {
    const { state, ...fields } = await queryClaude(workspace, lease, env);
    return boundedIntent(state, lease, fields);
  }
  if (lease.launch_mode === 'codex_native') {
    if (lease.launch_state === 'terminal') return { state: 'terminal', intent: lease };
    if (!NATIVE_AGENT_ID.test(lease.child_identity?.agent_id || '')) return { state: 'unknown', intent: lease };
    return { state: Date.parse(lease.hard_deadline) > Date.now() ? 'live' : 'terminal', intent: lease };
  }
  if (!lease.child_identity) return boundedIntent('unknown', lease);
  if (lease.launch_mode === 'claude_print' || lease.launch_mode === 'codex_exec') {
    return boundedIntent(await classifyChild(lease.child_identity), lease);
  }
  if (lease.launch_mode === 'opencode_child') {
    const sessionId = lease.child_identity?.session_id;
    if (!sessionId || typeof openCodeSession?.status !== 'function') return boundedIntent('unknown', lease);
    try {
      const record = await openCodeSession.status(sessionId);
      const state = sessionState(record);
      if (['working', 'running', 'pending', 'busy', 'retry'].includes(state)) return { state: 'live', record, intent: lease };
      if (['idle', 'done', 'completed', 'failed', 'stopped'].includes(state)) return { state: 'terminal', record, intent: lease };
    } catch {}
    return boundedIntent('unknown', lease);
  }
  return boundedIntent('unknown', lease);
}

async function acquireLease(root, workspace, harness, config, openCodeSession, env = process.env) {
  await ensureRoot(root);
  const path = join(root, 'lease.json');
  // ponytail: stale reclaim gets one retry; add OS-level locking only if concurrent reclaim becomes observable.
  for (let attempt = 0; attempt < 2; attempt += 1) {
    const leaseRecord = await jsonRecord(path);
    if (leaseRecord.malformed || (leaseRecord.present && (!leaseRecord.value || typeof leaseRecord.value !== 'object' || Array.isArray(leaseRecord.value)))) return { status: 'orphan_unknown' };
    const existing = leaseRecord.value || null;
    if (existing) {
      if (existing.schema !== 1) return { status: 'orphan_unknown' };
      const current = await classifyChild({ pid: existing.owner_pid, boot_id: existing.boot_id, process_start: existing.process_start });
      if (current === 'live') return { status: 'held' };
      if (current !== 'dead') return { status: 'orphan_unknown' };
      const orphan = await inspectIntent(leaseRecord, workspace, openCodeSession, env);
      if (orphan.state === 'live') return { status: 'orphan_live' };
      if (orphan.state === 'unknown') return { status: 'orphan_unknown' };
      try { await rm(path); } catch (error) { if (error?.code !== 'ENOENT') return { status: 'held' }; }
      continue;
    }
    const owner = await leaseOwner();
    const lease = {
      schema: 1, lease_id: randomUUID(), workspace, harness, owner_pid: owner.pid,
      boot_id: owner.boot_id, process_start: owner.process_start, started_at: Date.now(),
      hard_deadline: new Date(Date.now() + config.lease_ttl_seconds * 1000).toISOString(),
      launch_mode: null, launch_state: null, planned_identity: null, child_identity: null, downgrade_reason: null,
    };
    try {
      await writeFile(path, JSON.stringify(lease) + '\n', { flag: 'wx', mode: 0o600 });
      return { status: 'acquired', path, lease };
    } catch (error) {
      if (error?.code !== 'EEXIST') return { status: 'error', detail: error?.message || 'lease creation failed' };
    }
  }
  return { status: 'held' };
}

async function writeSkip(root, reason, fields = {}, leaseId) {
  const category = publicReason(reason);
  try {
    if (leaseId) {
      const lease = await json(join(root, 'lease.json'));
      if (lease?.lease_id !== leaseId) return false;
    }
    const previous = await json(join(root, 'last-run.json'));
    if (previous && previous.schema !== 1) return false;
    const { no_progress_count, suppressed_fingerprint, ...kept } = previous || {};
    await atomicJson(join(root, 'last-run.json'), {
      ...kept, schema: 1, completed_at: Date.now(), status: 'skipped', reason: category,
      ...(category === reason ? {} : { detail: reason }), ...fields,
    });
    return true;
  } catch {
    return false;
  }
}

async function releaseLease(root, leaseId) {
  try {
    const path = join(root, 'lease.json');
    const current = await json(path);
    if (current?.lease_id === leaseId) await rm(path, { force: true });
  } catch {}
}

async function liveOwnedChild(root, workspace, openCodeSession, leaseId, env = process.env) {
  const leaseRecord = await jsonRecord(join(root, 'lease.json'));
  const current = leaseRecord.value;
  if (!current || current.lease_id !== leaseId || current.launch_state !== 'launched') return false;
  const observed = await inspectIntent(leaseRecord, workspace, openCodeSession, env).catch(() => ({ state: 'unknown' }));
  return observed.state === 'live' || observed.state === 'unknown';
}

async function updateLease(root, lease, update) {
  try {
    const path = join(root, 'lease.json');
    const current = await json(path);
    if (!current || current.lease_id !== lease.lease_id) return false;
    const next = { ...current, ...update };
    await atomicJson(path, next);
    Object.assign(lease, next);
    return true;
  } catch {
    return false;
  }
}

async function launchPlan({ harness, workspace, env }) {
  if (harness === 'opencode') return { mode: 'opencode_child' };
  if (harness === 'codex') return { mode: 'codex_exec' };
  if (env.CLAUDE_CODE_DISABLE_AGENT_VIEW === '1') {
    return { mode: 'claude_print', downgradeReason: 'agent_view_disabled' };
  }
  // ponytail: --help grep is a capability heuristic; replace it if Claude exposes a versioned capability API.
  const help = await execFile('claude', ['--help'], { cwd: workspace, timeout: 5000, env });
  return help.code === 0 && /--bg|--background/.test(help.stdout)
    ? { mode: 'claude_bg' }
    : { mode: 'claude_print', downgradeReason: 'agent_view_unavailable' };
}

async function waitForClaude(workspace, lease, deadline, env = process.env) {
  let result = { state: 'unknown' };
  let observed = false;
  const registrationDeadline = Math.min(deadline, Date.now() + 5000);
  while (Date.now() < deadline) {
    result = await queryClaude(workspace, lease, env);
    if (result.state === 'live') observed = true;
    if (result.state === 'terminal' || (result.state === 'dead' && observed)) return result;
    if (!observed && Date.now() >= registrationDeadline) return { ...result, state: 'unknown' };
    await new Promise((resolvePromise) => setTimeout(resolvePromise, Math.min(1000, Math.max(1, deadline - Date.now()))));
  }
  return result;
}

async function stopClaude(workspace, lease, env = process.env) {
  const queried = await queryClaude(workspace, lease, env);
  const record = queried.record;
  const id = claudeId(record) || lease.child_identity?.manager_id;
  if (!id) return { state: 'unknown' };
  const result = await execFile('claude', ['stop', id], { cwd: workspace, timeout: 5000, env });
  return result.code === 0 ? { state: 'stopping' } : { state: 'unknown' };
}

async function waitForOpenCode(openCodeSession, sessionId, deadline) {
  if (typeof openCodeSession?.status !== 'function') return { state: 'unknown' };
  let last = { state: 'unknown' };
  while (Date.now() < deadline) {
    try {
      const record = await openCodeSession.status(sessionId);
      const state = sessionState(record);
      if (['working', 'running', 'pending', 'busy', 'retry'].includes(state)) last = { state: 'live', record };
      else if (['idle', 'done', 'completed', 'failed', 'stopped', 'error'].includes(state)) return { state: 'terminal', record };
      else last = { state: 'unknown', record };
    } catch { last = { state: 'unknown' }; }
    if (last.state === 'terminal') return last;
    await delay(Math.min(1000, Math.max(1, deadline - Date.now())));
  }
  return last;
}

async function launchModel({ launchMode: mode, workspace, env, timeoutMs, lease, openCodeSession, root, requireVisibleWorker = false, prompt = PROMPT, agentId = 'loam:ingestor' }) {
  const resolvedPrompt = prompt + ' Workspace: ' + workspace;
  if (mode === 'opencode_child') {
    if (!openCodeSession?.createChild || !openCodeSession?.promptAsync || !openCodeSession.parentSessionId) return { category: 'runtime_unavailable' };
    const child = await openCodeSession.createChild({
      parentId: openCodeSession.parentSessionId,
      title: lease.planned_identity.title,
    });
    const sessionId = child?.id || child?.session_id || child?.sessionID;
    if (!sessionId) return { category: 'runtime_unavailable' };
    const identity = {
      session_id: String(sessionId),
      parent_session_id: String(openCodeSession.parentSessionId),
      host_identity: lease.planned_identity?.owner_identity || null,
    };
    if (!(await updateLease(root, lease, { launch_state: 'launched', child_identity: identity }))) {
      if (typeof openCodeSession.abort === 'function') await openCodeSession.abort(String(sessionId)).catch(() => {});
      return { category: 'orphan_unknown' };
    }
    try {
      await openCodeSession.promptAsync({ sessionId: String(sessionId), parts: [{ type: 'text', text: resolvedPrompt }] });
    } catch {
      try {
        const record = await openCodeSession.status(String(sessionId));
        const state = sessionState(record);
        if (['working', 'running', 'pending', 'busy', 'retry'].includes(state)) return { category: 'orphan_live' };
        if (!['idle', 'done', 'completed', 'failed', 'stopped', 'error'].includes(state)) return { category: 'orphan_unknown' };
      } catch {}
      return { category: 'runtime_unavailable' };
    }
    return { category: null, completion: Promise.resolve({ code: 0 }), background: true, sessionId: String(sessionId) };
  }
  if (mode === 'claude_bg') {
      const name = lease.planned_identity.name;
      const started = startTracked({
          command: 'claude',
          args: ['--bg', '--agent', agentId, '--name', name, '--settings', JSON.stringify({ worktree: { bgIsolation: 'none' } }), '--setting-sources', 'user', '--strict-mcp-config', '--permission-mode', 'dontAsk', '--allowedTools', 'Read Glob Grep Write Edit Bash Skill', '--', resolvedPrompt],
          cwd: workspace, env, timeoutMs,
          detached: true, captureOutput: false,
      });
      if (!(await updateLease(root, lease, { launch_state: 'launched', child_identity: { manager_name: name } }))) {
        await terminateChild(started.child);
        return { category: 'orphan_unknown' };
      }
      const result = await started.completion;
      if (result.code !== 0) {
        const reset = await updateLease(root, lease, {
          launch_mode: 'claude_print', launch_state: 'planned', child_identity: null,
          downgrade_reason: 'agent_view_launch_failed',
        });
        if (!reset) return { category: 'orphan_unknown' };
        if (requireVisibleWorker) return { category: 'agent_view_launch_failed' };
        return launchModel({ launchMode: 'claude_print', workspace, env, timeoutMs, lease, openCodeSession, root, requireVisibleWorker });
      }
      const registered = await queryClaude(workspace, lease, env);
      const managerId = claudeId(registered.record);
      if (!(await updateLease(root, lease, { child_identity: { manager_id: managerId, manager_name: name } }))) {
        if (managerId) await execFile('claude', ['stop', managerId], { cwd: workspace, timeout: 5000, env });
        return { category: 'orphan_unknown' };
      }
      return { category: null, completion: Promise.resolve(result), background: true };
  }
  if (mode === 'claude_print') {
    const started = startTracked({
      command: 'claude',
      args: ['-p', resolvedPrompt, '--permission-mode', 'dontAsk', '--allowedTools', 'Read Glob Grep Write Edit Bash'],
      cwd: workspace, env, timeoutMs,
        detached: true, captureOutput: false,
      });
    const identity = await childIdentity(started.child.pid);
    if (!(await updateLease(root, lease, { launch_state: 'launched', child_identity: identity }))) {
      await terminateChild(started.child);
      return { category: 'orphan_unknown' };
    }
    return { category: null, completion: started.completion };
  }
  const started = startTracked({
    command: 'codex',
    args: ['-a', 'never', 'exec', '--ephemeral', '-s', 'workspace-write', '-C', workspace, '--color', 'never', '-'],
    cwd: workspace, env, input: resolvedPrompt, timeoutMs,
    detached: true, captureOutput: false,
  });
  const identity = await childIdentity(started.child.pid);
  if (!(await updateLease(root, lease, { launch_state: 'launched', child_identity: identity }))) {
    await terminateChild(started.child);
    return { category: 'orphan_unknown' };
  }
  return { category: null, completion: started.completion };
}

async function recordProgress(root, pre, post, count, lease) {
  try {
    const current = await json(join(root, 'lease.json'));
    if (current?.lease_id !== lease.lease_id) return { recorded: false, status: 'failed' };
    const loaded = await json(join(root, 'last-run.json'));
    if (loaded && loaded.schema !== 1) return { recorded: false, status: 'failed' };
    const previous = loaded || {};
    const same = pre.complete && post.complete && pre.fingerprint === post.fingerprint;
    const progress = post.complete && (post.count === 0 || post.fingerprint !== pre.fingerprint);
    const failureCount = progress ? 0 : Number(previous.failure_count || 0) + 1;
    const status = post.complete && post.count === 0 ? 'ok' : post.complete && !same ? 'partial' : 'failed';
    const backoff = progress ? null : Date.now() + Math.min(3600000, 300000 * Math.max(1, failureCount));
    await atomicJson(join(root, 'last-run.json'), {
      schema: 1, completed_at: Date.now(), lease_id: lease.lease_id, status,
      pre_fingerprint: pre.fingerprint, post_fingerprint: post.fingerprint,
      fingerprint_complete: post.complete, actionable_count: count,
      failure_count: failureCount, backoff_until: backoff,
      ...(lease.downgrade_reason ? { downgrade_reason: lease.downgrade_reason } : {}),
    });
    return { recorded: true, status, failureCount, backoff };
  } catch {
    return { recorded: false, status: 'failed' };
  }
}

export async function prepareWorkerRun({
  harness, workspace, globalRoot, skillsRoot, env = process.env, platform = process.platform,
  runtimeRunner, readiness, openCodeSession, notify, nativeAgentId,
} = {}) {
  const canonical = await canonicalWorkspace(workspace);
  const root = runRoot(globalRoot, canonical);
  const config = await readIngestConfig(globalRoot, env);
  await ensureRoot(root);
  const leaseResult = await acquireLease(root, canonical, harness, config, openCodeSession, env);
  if (['held', 'orphan_live', 'orphan_unknown'].includes(leaseResult.status)) return { action: 'skip', result: { reason: 'busy' } };
  if (leaseResult.status !== 'acquired') return { action: 'skip', result: { reason: 'unavailable' } };
  const lease = leaseResult.lease;
  let leaseHandled = false;
  // Typed events buffered here and flushed at worker-finish (T5). Only the
  // reusable preparation/finalization telemetry is projected at its point.
  const events = [];
  const skip = async (reason, fields = {}) => {
    await writeSkip(root, reason, {
      ...(lease.downgrade_reason ? { downgrade_reason: lease.downgrade_reason } : {}),
      ...fields,
    }, lease.lease_id);
    await releaseLease(root, lease.lease_id);
    leaseHandled = true;
    events.push({ event: 'ingest_preparation', outcome: 'skipped', reason: publicReason(reason) });
    return { action: 'skip', result: { reason: publicReason(reason), events } };
  };
  try {
    if (!config.enabled) return await skip('disabled');
    const localOutcome = await json(join(root, 'last-run.json'));
    if (localOutcome && localOutcome.schema !== 1) return await skip('schema_unknown');
    if (Number(localOutcome?.backoff_until || 0) > Date.now()) {
      return await skip('backoff');
    }
    if (Number(localOutcome?.completed_at || 0) + config.min_interval_seconds * 1000 > Date.now()
      && localOutcome?.status === 'ok') {
      return await skip('debounced');
    }
    const checked = await actionablePreflight({
      workspace: canonical, globalRoot, skillsRoot, env, platform, runtimeRunner, readiness,
    });
    if (checked.action !== 'run') return await skip(checked.reason, checked.fields);
    const { readiness: ready, exclusionsPath, fingerprint } = checked;
    const previousRecord = await json(join(root, 'last-run.json'));
    if (previousRecord && previousRecord.schema !== 1) return await skip('schema_unknown');
    const selectedLaunch = nativeAgentId ? { mode: 'codex_native' } : await launchPlan({ harness, workspace: canonical, env });
    const selectedLaunchMode = selectedLaunch.mode;
    const plannedIdentity = selectedLaunchMode === 'codex_native'
      ? { agent_id: nativeAgentId }
      : selectedLaunchMode === 'claude_bg'
      ? {
          name: `${claudeSessionName(canonical)}-${lease.lease_id.slice(0, 8)}`,
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
            owner_identity: { pid: lease.owner_pid, boot_id: lease.boot_id, process_start: lease.process_start },
          };
    const hardDeadlineMs = Date.now() + config.timeout_seconds * 1000;
    if (!(await updateLease(root, lease, {
      actionable_fingerprint: fingerprint.fingerprint,
      actionable_count: fingerprint.count,
      launch_mode: selectedLaunchMode,
      launch_state: 'planned',
      planned_identity: plannedIdentity,
      child_identity: selectedLaunchMode === 'codex_native' ? { agent_id: nativeAgentId } : null,
      downgrade_reason: selectedLaunch.downgradeReason || null,
      hard_deadline: new Date(hardDeadlineMs).toISOString(),
    }))) return await skip('orphan_unknown');
    if (harness === 'claude' && selectedLaunch.downgradeReason) {
      // Agent View downgraded to the invisible claude_print path: record whether
      // the fallback was taken or refused by policy (S: claude_agent_view).
      events.push({
        event: 'claude_agent_view',
        outcome: config.require_visible_worker ? 'refused' : 'fallback',
        reason: selectedLaunch.downgradeReason,
        launch_mode: 'claude_bg', fallback_launch_mode: 'claude_print',
        visibility: config.visibility, lease_id: lease.lease_id,
        require_visible_worker: config.require_visible_worker,
      });
    }
    if (selectedLaunch.downgradeReason && config.require_visible_worker) return await skip(selectedLaunch.downgradeReason);
    leaseHandled = true;
    events.push({
      event: 'ingest_preparation', outcome: 'admitted',
      launch_mode: selectedLaunchMode, lease_id: lease.lease_id,
      actionable_digest: fingerprint.fingerprint, actionable_count: fingerprint.count,
      deadline_ms: hardDeadlineMs,
    });
    return {
      action: 'run', harness, workspace: canonical, globalRoot, skillsRoot, env, platform,
      runtimeRunner, openCodeSession, notify, root, config, lease, readiness: ready,
      exclusionsPath, fingerprint, events,
      intent: {
        schema: 1, lease_id: lease.lease_id, workspace: canonical, harness,
        actionable_fingerprint: fingerprint.fingerprint, actionable_count: fingerprint.count,
        launch_mode: lease.launch_mode, hard_deadline: lease.hard_deadline,
      },
    };
  } finally {
    if (!leaseHandled) await releaseLease(root, lease.lease_id);
  }
}

export async function finalizeWorkerRun(prepared, {
  launch,
  result,
  launchNotification = Promise.resolve(),
  skipReason,
  skipFields = {},
  retainLease = false,
} = {}) {
  if (prepared?.action !== 'run' || !prepared.lease?.lease_id) throw new Error('prepared worker run is required');
  const {
    harness, workspace, env, platform, runtimeRunner, openCodeSession, notify,
    root, config, lease, readiness, exclusionsPath, fingerprint,
  } = prepared;
  const skip = async (reason, fields = {}) => {
    await writeSkip(root, reason, {
      ...(lease.downgrade_reason ? { downgrade_reason: lease.downgrade_reason } : {}),
      ...fields,
    }, lease.lease_id);
    return { reason: publicReason(reason) };
  };
  try {
    if (skipReason) return await skip(skipReason, skipFields);
    if (!launch.background && result.category === 'timeout' && lease.child_identity) {
      const childState = await classifyChild(lease.child_identity, { platform });
      if (childState === 'live' || childState === 'unknown') {
        retainLease = true;
        const reason = childState === 'live' ? 'orphan_live' : 'orphan_unknown';
        return await skip(reason);
      }
    }
    if (launch.background) {
      let manager = lease.launch_mode === 'claude_bg'
        ? await waitForClaude(workspace, lease, Date.parse(lease.hard_deadline), env)
        : await waitForOpenCode(openCodeSession, launch.sessionId, Date.parse(lease.hard_deadline));
      if (manager.state === 'live') {
        if (lease.launch_mode === 'claude_bg') {
          await stopClaude(workspace, lease, env);
          manager = await waitForClaude(workspace, lease, Date.now() + 5000, env);
        } else if (typeof openCodeSession?.abort === 'function') {
          try { await openCodeSession.abort(launch.sessionId); } catch {}
          manager = await waitForOpenCode(openCodeSession, launch.sessionId, Date.now() + 5000);
        }
      }
      if (manager.state === 'live' || manager.state === 'unknown') {
        retainLease = true;
        const reason = manager.state === 'live' ? 'orphan_live' : 'orphan_unknown';
        return await skip(reason);
      }
    }
    const postState = await probeFullState({ readiness, workspace, timeoutMs: 20000, runner: runtimeRunner });
    let post = { fingerprint: '', complete: false, count: 0 };
    try {
      if (postState.ready && postState.state?.wiki_root && await hasExistingWiki(postState.state.wiki_root)
        && await hasExistingCodegraph(postState.state.wiki_root)) {
        const postDiff = await diff({ readiness, workspace, wikiRoot: postState.state.wiki_root, exclusionsPath, runner: runtimeRunner });
        if (!postDiff.error) post = await fingerprintActionable({ workspace, entries: postDiff.entries, exclusionsPath, deadlineMs: 20000 });
      }
    } catch {}
    if (result.category || (typeof result.code === 'number' && result.code !== 0)) post.complete = false;
    const recorded = await recordProgress(root, fingerprint, post, fingerprint.count, lease);
    // Finalization telemetry: omit the event entirely when either digest is
    // unavailable rather than storing a sentinel or invalid digest (S6).
    if (recorded.recorded && /^[a-f0-9]{64}$/i.test(fingerprint.fingerprint) && /^[a-f0-9]{64}$/i.test(post.fingerprint)) {
      (prepared.events ||= []).push({
        event: 'ingest_finalization', outcome: recorded.status,
        lease_id: lease.lease_id, pre_digest: fingerprint.fingerprint, post_digest: post.fingerprint,
        actionable_count: fingerprint.count, failure_count: recorded.failureCount,
        ...(recorded.backoff ? { backoff_until_ms: recorded.backoff } : {}),
      });
    }
    if (harness === 'claude' && lease.launch_mode === 'claude_bg') {
      // Agent View registered: record the selected profile and its manager
      // identity from the freshly persisted lease (S: claude_agent_profile).
      const finalLease = await json(join(root, 'lease.json'));
      const managerId = finalLease?.child_identity?.manager_id;
      const managerName = finalLease?.child_identity?.manager_name;
      if (managerId && managerName) {
        (prepared.events ||= []).push({
          event: 'claude_agent_profile', outcome: 'selected',
          launch_mode: 'claude_bg', agent_type: 'loam:ingestor',
          manager_name: managerName, manager_id: managerId, lease_id: lease.lease_id,
        });
      }
    }
    if (lease.launch_mode === 'claude_bg') await pruneClaudeSessions(workspace, lease, env);
    const launchDelivery = await launchNotification;
    const terminalDelivery = await sendNotification(notify, config.visibility, {
      phase: 'terminal', harness, workspace, launchMode: lease.launch_mode,
      status: recorded.status,
    });
    if (config.visibility !== 'silent' && typeof notify === 'function') {
      (prepared.events ||= []).push({
        event: 'ingest_visibility', phase: 'terminal', outcome: recorded.status,
        visibility: config.visibility, launch_mode: lease.launch_mode,
      });
      if (launchDelivery) {
        prepared.events.push({
          event: 'visibility_delivery', phase: 'launch', outcome: launchDelivery,
          visibility: config.visibility, launch_mode: lease.launch_mode,
        });
      }
      if (terminalDelivery) {
        prepared.events.push({
          event: 'visibility_delivery', phase: 'terminal', outcome: terminalDelivery,
          visibility: config.visibility, launch_mode: lease.launch_mode,
        });
      }
    }
    return { reason: result?.category === 'timeout' ? 'unavailable' : 'ok' };
  } finally {
    if (!retainLease && await liveOwnedChild(root, workspace, openCodeSession, lease.lease_id, env)) retainLease = true;
    if (!retainLease) await releaseLease(root, lease.lease_id);
  }
}

export async function runWorker(options = {}) {
  const prepared = await prepareWorkerRun(options);
  if (prepared.action !== 'run') return prepared.result || prepared;
  const { harness, workspace, globalRoot, env, root, config, lease, openCodeSession, notify } = prepared;
  // Attach the worker's buffered event batch to every terminal result so the
  // detached worker can flush it at worker-finish.
  const withEvents = (result) => ({ ...result, events: prepared.events });
  try {
    const launch = options.modelRunner
      ? await options.modelRunner({ harness, workspace, lease, root })
      : await launchModel({
          launchMode: lease.launch_mode, workspace,
          env: { ...env, LOAM_INGEST_GLOBAL_ROOT: globalRoot },
          timeoutMs: config.timeout_seconds * 1000, lease, openCodeSession, root,
          requireVisibleWorker: config.require_visible_worker,
        });
    if (launch.category) {
      return withEvents(await finalizeWorkerRun(prepared, {
        skipReason: launch.category,
        retainLease: ['orphan_live', 'orphan_unknown'].includes(launch.category),
      }));
    }
    if (config.visibility !== 'silent' && typeof notify === 'function') {
      prepared.events.push({
        event: 'ingest_visibility', phase: 'launch', outcome: 'started',
        visibility: config.visibility, launch_mode: lease.launch_mode,
      });
    }
    const launchNotification = sendNotification(notify, config.visibility, {
      phase: 'launch', harness, workspace, launchMode: lease.launch_mode,
      identity: lease.child_identity || lease.planned_identity,
    });
    const result = await (launch.completion || Promise.resolve({ code: 0 }));
    return withEvents(await finalizeWorkerRun(prepared, { launch, result, launchNotification }));
  } catch (error) {
    return withEvents(await finalizeWorkerRun(prepared, {
      skipReason: 'runtime_unavailable',
      skipFields: { detail: error instanceof Error ? error.message.slice(0, 256) : String(error).slice(0, 256) },
      retainLease: Boolean(lease.child_identity),
    }));
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
  const lastRun = await json(join(root, 'last-run.json'));
  const intentState = leaseRecord.present
    ? await inspectIntent(leaseRecord, canonical, undefined, env)
    : { state: 'dead' };
  let exclusions;
  try { exclusions = { ready: true, path: await resolveExclusions(env.LOAM_INGEST_SKILLS_ROOT) }; }
  catch (error) { exclusions = { ready: false, reason: error.reason || 'exclusions_unavailable' }; }
  return {
    schema: 1, workspace: canonical, enabled: (await readIngestConfig(globalRoot, env)).enabled,
    lease, lease_state: leaseState, intent: lease?.launch_mode ? lease : null,
    intent_state: intentState.state,
    orphan: intentState.state === 'live' || intentState.state === 'unknown',
    exclusions,
    last_run: lastRun,
    diagnostics: {
      count: lastRun?.actionable_count ?? null,
      fingerprint: lastRun?.actionable_fingerprint || lastRun?.post_fingerprint || null,
    },
    queue: { root },
  };
}

export { inspectIntent, resolveExclusions, launchModel, acquireLease, releaseLease, updateLease, writeSkip, liveOwnedChild, launchPlan };
