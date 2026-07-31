import { createHash, randomUUID } from 'node:crypto';
import { mkdir, readFile, realpath, rename, rm, stat, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';

import { assertInside, resolveSkillsRoot } from './paths.mjs';
import { checkReadiness, invokeRuntime, probeFullState } from './runtime.mjs';
import {
  bootIdentity, childIdentity, classifyChild, execFile, processStartIdentity,
  spawnDetached, startTracked, terminateChild,
} from './ingest-process.mjs';
import { FingerprintError, fingerprintActionable } from './ingest-fingerprint.mjs';

const PROMPT = 'Run the existing loam::ingesting-codebase skill for the provided workspace. Do not modify source files, commit, or push. Do not spawn other agents or subagents.';
const DEFAULTS = Object.freeze({ enabled: true, min_interval_seconds: 300, timeout_seconds: 900, lease_ttl_seconds: 1800, visibility: 'silent' });
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

async function sendNotification(notify, configuredVisibility, event) {
  if (configuredVisibility === 'silent' || typeof notify !== 'function') return;
  // Contract: notifiers must pass this signal to every resource they open; the seam cannot cancel resources that ignore it.
  const controller = new AbortController();
  let timer;
  try {
    await Promise.race([
      Promise.resolve().then(() => notify({ ...event, visibility: configuredVisibility, signal: controller.signal })),
      new Promise((resolvePromise) => {
        timer = setTimeout(() => { controller.abort(); resolvePromise(); }, NOTIFICATION_TIMEOUT_MS);
      }),
    ]);
  } catch {}
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
    || payload.stop_hook_active === true || payload.loam_ingest_child === true || payload.child_session === true;
}

export async function gate({ harness, payload = {}, globalRoot, env = process.env, now = Date.now() } = {}) {
  const config = await readIngestConfig(globalRoot, env);
  if (!config.enabled) return { action: 'skip', reason: 'disabled' };
  if (recursion(payload, env)) return { action: 'skip', reason: 'disabled' };
  const workspace = await canonicalWorkspace(payloadWorkspace(payload));
  const root = runRoot(globalRoot, workspace);
  await ensureRoot(root);
  const last = await json(join(root, 'last-run.json'), null);
  if (last && last.schema !== 1) return { action: 'skip', reason: publicReason('schema_unknown'), workspace };
  const previous = last || {};
  if (Number(previous.backoff_until || 0) > now) return { action: 'skip', reason: publicReason('backoff'), workspace };
  if (Number(previous.completed_at || 0) + config.min_interval_seconds * 1000 > now && previous.status === 'ok') {
    return { action: 'skip', reason: publicReason('debounced'), workspace };
  }
  return { action: 'spawn_worker', workspace, config };
}

export function startWorker({
  harness, workspace, globalRoot, skillsRoot, workerPath, hookRunId,
  env = process.env, spawn = spawnDetached,
} = {}) {
  if (!workerPath) throw new Error('installed ingestion worker is unavailable');
  return spawn({
    command: process.execPath,
    args: [
      workerPath, '--harness', harness, '--workspace', resolve(workspace),
      ...(Number.isSafeInteger(hookRunId) && hookRunId > 0 ? ['--hook-run-id', String(hookRunId)] : []),
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

export async function dispatchBoundary(options = {}) {
  const result = await gate(options);
  if (result.action !== 'spawn_worker') return result;
  try {
    startWorker({ ...options, workerPath: await installedWorkerPath(options.globalRoot), workspace: result.workspace });
    return result;
  }
  catch (error) {
    return { action: 'skip', reason: 'unavailable', detail: error.message };
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

async function queryClaude(workspace, lease, env = process.env) {
  const result = await execFile('claude', ['agents', '--json', '--cwd', workspace, '--all'], { cwd: workspace, timeout: 5000, env });
  if (result.code !== 0 || result.category === 'runtime_error') return { state: 'unknown' };
  let records; try { records = JSON.parse(result.stdout); } catch { return { state: 'unknown' }; }
  const list = Array.isArray(records) ? records : records?.agents || [];
  const identity = lease.child_identity || {};
  const plannedName = lease.planned_identity?.name;
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

function sessionState(record) { return record?.type; }

async function inspectIntent(leaseRecord, workspace, openCodeSession, env = process.env) {
  if (!leaseRecord.present) return { state: 'dead', intent: null };
  if (leaseRecord.malformed || !leaseRecord.value || typeof leaseRecord.value !== 'object' || Array.isArray(leaseRecord.value)) {
    return { state: 'unknown', intent: null };
  }
  const lease = leaseRecord.value;
  if (lease.schema !== 1) return { state: 'unknown', intent: lease };
  if (!lease.launch_mode) return { state: 'dead', intent: lease };
  if (lease.launch_mode === 'claude_bg') return { ...(await queryClaude(workspace, lease, env)), intent: lease };
  if (!lease.child_identity) return { state: 'unknown', intent: lease };
  if (lease.launch_mode === 'claude_print' || lease.launch_mode === 'codex_exec') {
    return { state: await classifyChild(lease.child_identity), intent: lease };
  }
  if (lease.launch_mode === 'opencode_child') {
    const sessionId = lease.child_identity?.session_id;
    if (!sessionId || typeof openCodeSession?.status !== 'function') return { state: 'unknown', intent: lease };
    try {
      const record = await openCodeSession.status(sessionId);
      const state = sessionState(record);
      if (['working', 'running', 'pending', 'busy', 'retry'].includes(state)) return { state: 'live', record, intent: lease };
      if (['idle', 'done', 'completed', 'failed', 'stopped'].includes(state)) return { state: 'terminal', record, intent: lease };
    } catch {}
    return { state: 'unknown', intent: lease };
  }
  return { state: 'unknown', intent: lease };
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
      launch_mode: null, launch_state: null, planned_identity: null, child_identity: null,
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

async function launchMode({ harness, workspace, env }) {
  if (harness === 'opencode') return 'opencode_child';
  if (harness === 'codex') return 'codex_exec';
  // ponytail: --help grep is a capability heuristic; replace it if Claude exposes a versioned capability API.
  const help = await execFile('claude', ['--help'], { cwd: workspace, timeout: 5000, env });
  const supportsBg = help.code === 0 && /--bg|--background/.test(help.stdout)
    && env.CLAUDE_CODE_DISABLE_AGENT_VIEW !== '1';
  return supportsBg ? 'claude_bg' : 'claude_print';
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
  const id = record?.id || record?.session_id || record?.sessionID || lease.child_identity?.manager_id;
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

async function launchModel({ launchMode: mode, workspace, env, timeoutMs, lease, openCodeSession, root }) {
  const prompt = PROMPT + ' Workspace: ' + workspace;
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
      await openCodeSession.promptAsync({ sessionId: String(sessionId), parts: [{ type: 'text', text: prompt }] });
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
      if (!(await updateLease(root, lease, { launch_state: 'launched', child_identity: { manager_name: name } }))) {
        await terminateChild(started.child);
        await rm(settingsPath, { force: true });
        return { category: 'orphan_unknown' };
      }
      const completion = started.completion.finally(() => rm(settingsPath, { force: true }));
      const result = await completion;
      if (result.code !== 0) {
        const reset = await updateLease(root, lease, { launch_mode: 'claude_print', launch_state: 'planned', child_identity: null });
        if (!reset) return { category: 'orphan_unknown' };
        return launchModel({ launchMode: 'claude_print', workspace, env, timeoutMs, lease, openCodeSession, root });
      }
      const registered = await queryClaude(workspace, lease, env);
      const managerId = registered.record?.id || registered.record?.session_id || registered.record?.sessionID || null;
      if (!(await updateLease(root, lease, { child_identity: { manager_id: managerId, manager_name: name } }))) {
        if (managerId) await execFile('claude', ['stop', managerId], { cwd: workspace, timeout: 5000, env });
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
    if (!(await updateLease(root, lease, { launch_state: 'launched', child_identity: identity }))) {
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
    });
    return { recorded: true, status };
  } catch {
    return { recorded: false, status: 'failed' };
  }
}

export async function runWorker({
  harness, workspace, globalRoot, skillsRoot, env = process.env, platform = process.platform,
  runtimeRunner, readiness, modelRunner, openCodeSession, notify,
} = {}) {
  const canonical = await canonicalWorkspace(workspace);
  const root = runRoot(globalRoot, canonical);
  const config = await readIngestConfig(globalRoot, env);
  await ensureRoot(root);
  const leaseResult = await acquireLease(root, canonical, harness, config, openCodeSession, env);
  if (['held', 'orphan_live', 'orphan_unknown'].includes(leaseResult.status)) return { reason: 'busy' };
  if (leaseResult.status !== 'acquired') return { reason: 'unavailable' };
  const lease = leaseResult.lease;
  const skip = async (reason, fields = {}) => {
    await writeSkip(root, reason, fields, lease.lease_id);
    return { reason: publicReason(reason) };
  };
  let retainLease = false;
  let launchNotification = Promise.resolve();
  try {
    if (!config.enabled) return skip('disabled');
    const localOutcome = await json(join(root, 'last-run.json'));
    if (localOutcome && localOutcome.schema !== 1) return skip('schema_unknown');
    if (Number(localOutcome?.backoff_until || 0) > Date.now()) {
      return skip('backoff');
    }
    if (Number(localOutcome?.completed_at || 0) + config.min_interval_seconds * 1000 > Date.now()
      && localOutcome?.status === 'ok') {
      return skip('debounced');
    }
    const ready = readiness || await checkReadiness({ globalRoot, skillsRoot, env, platform });
    if (!ready.ready) return skip(ready.category || 'runtime_unavailable');
    const stateResult = await probeFullState({ readiness: ready, workspace: canonical, timeoutMs: 20000, runner: runtimeRunner });
    if (!stateResult.ready) {
      const reason = stateResult.category === 'timeout' ? 'probe_timeout' : stateResult.category === 'malformed_state' ? 'malformed_state' : stateResult.category === 'runtime_failed' ? 'probe_failed' : 'runtime_unavailable';
      return skip(reason);
    }
    const state = stateResult.state;
    if (!validState(state)) return skip('schema_unknown');
    if (!state.wiki_root) return skip('wiki_missing');
    if (!(await hasExistingWiki(state.wiki_root))) return skip('wiki_missing');
    if (!(await hasExistingCodegraph(state.wiki_root))) return skip('codegraph_missing');
    const pending = pendingHint(state);
    if (!pending || pendingCount(pending) === 0) return skip('no_pending');
    let exclusionsPath;
    try { exclusionsPath = await resolveExclusions(skillsRoot || env.LOAM_INGEST_SKILLS_ROOT); }
    catch { return skip('exclusions_unavailable'); }
    const diffResult = await diff({ readiness: ready, workspace: canonical, wikiRoot: state.wiki_root, exclusionsPath, runner: runtimeRunner });
    if (diffResult.error) return skip(diffResult.error);
    let fingerprint;
    try { fingerprint = await fingerprintActionable({ workspace: canonical, entries: diffResult.entries, exclusionsPath, deadlineMs: 20000 }); }
    catch (error) { return skip(error.reason || 'fingerprint_unavailable'); }
    if (fingerprint.count === 0) return skip('no_actionable_work', { actionable_count: 0, actionable_fingerprint: fingerprint.fingerprint });
    if (!fingerprint.complete) return skip('fingerprint_unavailable', { actionable_count: fingerprint.count, actionable_fingerprint: fingerprint.fingerprint });
    const previousRecord = await json(join(root, 'last-run.json'));
    if (previousRecord && previousRecord.schema !== 1) return skip('schema_unknown');
    const selectedLaunchMode = await launchMode({ harness, workspace: canonical, env });
    const plannedIdentity = selectedLaunchMode === 'claude_bg'
      ? {
          name: claudeSessionName(canonical),
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
    if (!(await updateLease(root, lease, {
      actionable_fingerprint: fingerprint.fingerprint,
      launch_mode: selectedLaunchMode,
      launch_state: 'planned',
      planned_identity: plannedIdentity,
      child_identity: null,
      hard_deadline: new Date(Date.now() + config.timeout_seconds * 1000).toISOString(),
    }))) return skip('orphan_unknown');
    let launch;
    try {
      launch = modelRunner
        ? await modelRunner({ harness, workspace: canonical, lease, root })
        : await launchModel({ launchMode: lease.launch_mode, workspace: canonical, env: { ...env, LOAM_INGEST_GLOBAL_ROOT: globalRoot }, timeoutMs: config.timeout_seconds * 1000, lease, openCodeSession, root });
    } catch (error) {
      if (lease.child_identity) retainLease = true;
      return skip('runtime_unavailable', { detail: error instanceof Error ? error.message.slice(0, 256) : String(error).slice(0, 256) });
    }
    if (launch.category) {
      if (['orphan_live', 'orphan_unknown'].includes(launch.category)) retainLease = true;
      return skip(launch.category);
    }
    launchNotification = sendNotification(notify, config.visibility, {
      phase: 'launch', harness, workspace: canonical, launchMode: lease.launch_mode,
      identity: lease.child_identity || lease.planned_identity,
    });
    const result = await (launch.completion || Promise.resolve({ code: 0 }));
    if (!launch.background && result.category === 'timeout' && lease.child_identity) {
      const childState = await classifyChild(lease.child_identity, { platform });
      if (childState === 'live' || childState === 'unknown') {
        retainLease = true;
        const reason = childState === 'live' ? 'orphan_live' : 'orphan_unknown';
        return skip(reason);
      }
    }
    if (launch.background) {
      let manager = lease.launch_mode === 'claude_bg'
        ? await waitForClaude(canonical, lease, Date.parse(lease.hard_deadline), env)
        : await waitForOpenCode(openCodeSession, launch.sessionId, Date.parse(lease.hard_deadline));
      if (manager.state === 'live') {
        if (lease.launch_mode === 'claude_bg') {
          await stopClaude(canonical, lease, env);
          manager = await waitForClaude(canonical, lease, Date.now() + 5000, env);
        } else if (typeof openCodeSession?.abort === 'function') {
          try { await openCodeSession.abort(launch.sessionId); } catch {}
          manager = await waitForOpenCode(openCodeSession, launch.sessionId, Date.now() + 5000);
        }
      }
      if (manager.state === 'live' || manager.state === 'unknown') {
        retainLease = true;
        const reason = manager.state === 'live' ? 'orphan_live' : 'orphan_unknown';
        return skip(reason);
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
    const recorded = await recordProgress(root, fingerprint, post, fingerprint.count, lease);
    await launchNotification;
    await sendNotification(notify, config.visibility, {
      phase: 'terminal', harness, workspace: canonical, launchMode: lease.launch_mode,
      status: recorded.status,
    });
    return { reason: result?.category === 'timeout' ? 'unavailable' : 'ok' };
  } finally {
    if (!retainLease && await liveOwnedChild(root, canonical, openCodeSession, lease.lease_id, env)) retainLease = true;
    if (!retainLease) await releaseLease(root, lease.lease_id);
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

export { inspectIntent, resolveExclusions };
