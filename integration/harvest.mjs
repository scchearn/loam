import { resolve } from 'node:path';
import { join } from 'node:path';
import { createHash } from 'node:crypto';
import { mkdir, readFile, stat as fsStat } from 'node:fs/promises';

import { canonicalWorkspace } from './ingest.mjs';
import {
  readHarvestConfig, readHarvestState, writeHarvestState,
  harvestLastRunPath, pruneHarvestSessions,
} from './harvest-state.mjs';
import { getHarvestBackend, measureStore as defaultMeasureStore } from './harvest-store.mjs';
import { openCodeMeasure } from './harvest-opencode.mjs';
import { spawnDetached } from './ingest-process.mjs';
import { groupExchanges, renderWindow, writeWindow } from './harvest-window.mjs';
import { readdir as fsReaddir } from 'node:fs/promises';
import { stat as fsStatDir } from 'node:fs/promises';
import './harvest-claude.mjs';
import './harvest-codex.mjs';
import './harvest-opencode.mjs';

function hash(value) { return createHash('sha256').update(String(value)).digest('hex'); }
async function jsonRead(path, fallback = null) {
  try { return JSON.parse(await readFile(path, 'utf8')); } catch { return fallback; }
}

const HARVEST_AGENT_TYPES = new Set(['loam:ingestor', 'loam_ingestor', 'loam:harvester', 'loam_harvester']);

export function harvestRecursion(payload, env) {
  return env.LOAM_HARVEST_WORKER === '1' || env.LOAM_HARVEST_CHILD === '1'
    || env.LOAM_INGEST_WORKER === '1' || env.LOAM_INGEST_CHILD === '1'
    || payload.stop_hook_active === true
    || payload.loam_ingest_child === true || payload.loam_harvest_child === true
    || payload.child_session === true
    || HARVEST_AGENT_TYPES.has(payload.agent_type);
}

export function startHarvestWorker({
  harness, workspace, globalRoot, workerPath, sessionId,
  env = process.env, spawn = spawnDetached, startWorker,
} = {}) {
  if (startWorker) return startWorker({ harness, workspace, globalRoot, sessionId, env });
  if (!workerPath) throw new Error('installed harvest worker is unavailable');
  return spawn({
    command: process.execPath,
    args: [
      workerPath, '--harness', harness, '--workspace', resolve(workspace),
      '--session-id', sessionId, '--global-root', resolve(globalRoot),
    ],
    cwd: resolve(workspace),
    env: { ...env, LOAM_HARVEST_WORKER: '1', LOAM_HARVEST_GLOBAL_ROOT: resolve(globalRoot) },
  });
}

async function resolveStorePath({ harness, workspace, sessionId, state, env, storePath }) {
  if (storePath) return storePath;
  if (state?.store?.path) return state.store.path;
  const backend = getHarvestBackend(harness);
  if (!backend) return null;
  try {
    const located = await backend.locateStore({ workspace, sessionId, env });
    return located?.path || null;
  } catch {
    return null;
  }
}

export async function harvestTick({
  harness, payload = {}, globalRoot, env = process.env, now = Date.now(),
  startWorker, measureStore: injectedMeasure, storePath,
} = {}) {
  const config = await readHarvestConfig(globalRoot, env);
  if (!config.enabled || harvestRecursion(payload, env)) return { action: 'skip', reason: 'disabled' };
  const workspace = await canonicalWorkspace(payload.cwd || payload.directory || payload.workspace?.root || process.cwd());
  const sessionId = typeof payload.session_id === 'string' && payload.session_id ? payload.session_id : null;
  if (!sessionId) return { action: 'skip', reason: 'session_unknown' };
  const state = await readHarvestState(globalRoot, workspace, sessionId);
  if (state && state.schema !== undefined && state.schema !== 1) return { action: 'skip', reason: 'schema_unknown' };
  const wiki = state?.wiki;
  if (wiki && wiki.present === false && now - Number(wiki.checked_at || 0) < config.wiki_recheck_seconds * 1000) {
    return { action: 'skip', reason: 'wiki_missing' };
  }
  const backend = getHarvestBackend(harness);
  const measure = injectedMeasure || backend?.measure || (harness === 'opencode' ? openCodeMeasure : defaultMeasureStore);
  const storePathResolved = await resolveStorePath({ harness, workspace, sessionId, state, env, storePath });
  let measured = { present: false, size: 0, mtime_ms: 0 };
  if (storePathResolved) {
    try { measured = await measure({ store: { path: storePathResolved } }); }
    catch { measured = { present: false, size: 0, mtime_ms: 0 }; }
  }
  const previousSize = Number(state?.observed?.store_size || 0);
  const delta = measured.present ? Math.max(0, measured.size - previousSize) : 0;
  await writeHarvestState(globalRoot, workspace, sessionId, {
    ...(state || {}),
    session_id: sessionId, harness, workspace,
    observed: { store_size: measured.size, checked_at: now },
  });
  if (delta < config.min_store_delta_bytes) {
    return { action: 'skip', reason: 'below_threshold', workspace };
  }
  const lastAttempt = Number(state?.last_attempt_at || 0);
  if (lastAttempt && lastAttempt + config.min_interval_seconds * 1000 > now) {
    return { action: 'skip', reason: 'debounced', workspace };
  }
  const pending = { ...(state || {}), session_id: sessionId, harness, workspace, last_attempt_at: now };
  try {
    await writeHarvestState(globalRoot, workspace, sessionId, pending);
    const workerPath = await installedHarvestWorkerPath(globalRoot);
    startHarvestWorker({ harness, workspace, globalRoot, sessionId, env, startWorker, workerPath });
  } catch (error) {
    const cleared = { ...pending, last_attempt_at: null };
    await writeHarvestState(globalRoot, workspace, sessionId, cleared).catch(() => {});
    return { action: 'skip', reason: 'unavailable', detail: String(error?.message || error), workspace };
  }
  return { action: 'spawn_worker', workspace, config };
}

async function atomicJson(path, value) {
  const { writeFile, rename, rm } = await import('node:fs/promises');
  const { randomUUID } = await import('node:crypto');
  const temporary = `${path}.${randomUUID()}.tmp`;
  try {
    await writeFile(temporary, `${JSON.stringify(value)}\n`, { encoding: 'utf8', mode: 0o600 });
    await rename(temporary, path);
  } catch (error) {
    await rm(temporary, { force: true }).catch(() => {});
    throw error;
  }
}

export function harvestPrompt({ workspace, windowPath }) {
  return [
    'This is a background session-learning harvest. Your live conversation context does not contain the session under review; the conversation to review is in the window file at the absolute path below.',
    '',
    `Workspace: ${workspace}`,
    `Window file: ${windowPath}`,
    '',
    'Read the window file in full. Treat it as the conversation of the session to review, in place of live session context. Then invoke the loam::learning-from-session skill exactly once, using this session as the focus, and follow its routing report format.',
    '',
    'The window contains only records from the current session after the last harvested cursor; it is already normalised and truncated, and it may contain nothing admissible. Do not re-parse, re-classify, or pre-judge the material — the skill owns all judgment.',
    '',
    'Rules:',
    '- Do not spawn or delegate to another agent, subagent, or process.',
    '- Do not modify source files, commit, or push.',
    '- Do not read the harness store; the window is the only conversation content you may use.',
    '- If the window yields nothing admissible, report that outcome and stop.',
  ].join('\n');
}

export async function launchHarvestAgent({
  harness, workspace, lease, config, env, root, windowPath, sessionId, openCodeSession,
} = {}) {
  const { launchModel } = await import('./ingest.mjs');
  const prompt = harvestPrompt({ workspace, windowPath });
  const options = {
    launchMode: lease.launch_mode,
    workspace,
    env: { ...env, LOAM_HARVEST_GLOBAL_ROOT: root },
    timeoutMs: config.timeout_seconds * 1000,
    lease,
    openCodeSession,
    root,
    prompt,
    agentId: 'loam:harvester',
  };
  return launchModel(options);
}

async function installedHarvestWorkerPath(globalRoot) {
  const { readFile } = await import('node:fs/promises');
  const { assertInside } = await import('./paths.mjs');
  try {
    const install = JSON.parse(await readFile(join(resolve(globalRoot), 'install.json'), 'utf8'));
    if (typeof install?.adapter_root !== 'string') return null;
    return assertInside(resolve(globalRoot), join(resolve(install.adapter_root), 'harvest-worker.mjs'), 'worker path');
  } catch {
    return null;
  }
}

async function pruneWindowFiles(root, retainWindows) {
  const { readdir, rm } = await import('node:fs/promises');
  const directory = join(root, 'harvest');
  let entries;
  try { entries = await readdir(directory, { withFileTypes: true }); } catch { return; }
  const windows = [];
  for (const entry of entries) {
    if (!entry.isFile() || !entry.name.endsWith('.window.md')) continue;
    const path = join(directory, entry.name);
    try { windows.push({ path, mtime: (await fsStat(path)).mtimeMs }); } catch {}
  }
  windows.sort((a, b) => b.mtime - a.mtime);
  for (const file of windows.slice(retainWindows)) {
    await rm(file.path, { force: true }).catch(() => {});
  }
}

export async function runHarvest({
  harness, workspace, sessionId, globalRoot, skillsRoot, env = process.env,
  probeFullState, backend, readWindow, launch, acquireLease: acquire, releaseLease: release,
  writeSkip, launchModel: modelLauncher, openCodeSession,
} = {}) {
  const config = await readHarvestConfig(globalRoot, env);
  const canonical = await canonicalWorkspace(workspace);
  const root = join(resolve(globalRoot), 'run', hash(canonical).slice(0, 16));
  await mkdir(root, { recursive: true, mode: 0o700 });
  await mkdir(join(root, 'harvest'), { recursive: true, mode: 0o700 });
  const acquireFn = acquire || (await import('./ingest.mjs')).acquireLease;
  const releaseFn = release || (await import('./ingest.mjs')).releaseLease;
  const leaseResult = await acquireFn(root, canonical, harness, config, undefined, env);
  if (['held', 'orphan_live', 'orphan_unknown'].includes(leaseResult.status)) {
    return { reason: 'busy', detail: leaseResult.status, cursorChanged: false };
  }
  if (leaseResult.status !== 'acquired') {
    return { reason: 'unavailable', detail: leaseResult.status, cursorChanged: false };
  }
  const lease = leaseResult.lease;
  let leaseHandled = false;
  try {
    const state = (await readHarvestState(globalRoot, canonical, sessionId)) || {};
    const probe = probeFullState || (await import('./runtime.mjs')).probeFullState;
    const wikiProbe = probe
      ? await probe({ workspace: canonical, globalRoot, env })
      : { ready: false };
    const wiki = wikiProbe?.state
      ? { present: Boolean(wikiProbe.state.wiki_root), root: wikiProbe.state.wiki_root || null, checked_at: Date.now() }
      : { present: false, root: null, checked_at: Date.now() };
    await writeHarvestState(globalRoot, canonical, sessionId, {
      ...state, session_id: sessionId, harness, workspace: canonical, wiki,
    });
    if (!wiki.present) {
      const lastRunPath = harvestLastRunPath(globalRoot, canonical);
      await atomicJson(lastRunPath, {
        schema: 1, completed_at: Date.now(), status: 'skipped', reason: 'wiki_missing',
        session_id: sessionId, turns: 0, conversation_bytes: 0,
      });
      return { reason: 'wiki_missing', cursorChanged: false };
    }

    let store = state.store || {};
    const resolvedBackend = backend || getHarvestBackend(harness);
    const readFn = resolvedBackend?.readWindow || readWindow;
    if (!readFn) throw new Error('no window reader available');
    if (!store.path && resolvedBackend?.locateStore) {
      try {
        const located = await resolvedBackend.locateStore({ workspace: canonical, sessionId, env });
        if (located?.path) store = { path: located.path, kind: located.kind || 'jsonl', size: 0, mtime_ms: 0 };
      } catch {}
    }
    if (!store.path) {
      const lastRunPath = harvestLastRunPath(globalRoot, canonical);
      await atomicJson(lastRunPath, {
        schema: 1, completed_at: Date.now(), status: 'failed', reason: 'store_missing',
        session_id: sessionId,
      });
      return { reason: 'store_missing', cursorChanged: false };
    }
    let rotationReset = false;
    if (Number(state.cursor?.value || 0) > 0) {
      let size = 0;
      try { size = (await fsStat(store.path)).size; } catch {}
      if (size < Number(state.cursor.value)) {
        await writeHarvestState(globalRoot, canonical, sessionId, {
          ...state, session_id: sessionId, harness, workspace: canonical, wiki,
          cursor: { kind: state.cursor?.kind || 'bytes', value: 0, updated_at: Date.now() },
          rotations: Number(state.rotations || 0) + 1,
        });
        rotationReset = true;
      }
    }
    const effectiveState = rotationReset
      ? (await readHarvestState(globalRoot, canonical, sessionId)) || state
      : state;
    let window;
    try {
      window = await readFn({ store, state: effectiveState, workspace: canonical, config, sessionId });
    } catch (error) {
      const reason = error?.reason || 'store_unreadable';
      const lastRunPath = harvestLastRunPath(globalRoot, canonical);
      await atomicJson(lastRunPath, {
        schema: 1, completed_at: Date.now(), status: 'failed', reason,
        session_id: sessionId, detail: String(error?.message || error).slice(0, 256),
      });
      return { reason, cursorChanged: false };
    }
    if (!window.exchanges && Array.isArray(window.records)) {
      window = {
        ...window,
        ...groupExchanges(window.records, {
          maxTurns: config.max_window_turns,
          maxBytes: config.max_window_bytes,
          toolOutputBytes: config.tool_output_bytes,
        }),
      };
    }
    const exchanges = window.exchanges || [];
    const conversationBytes = exchanges.reduce((sum, exchange) => {
      let total = Buffer.byteLength(exchange.user || '', 'utf8');
      for (const text of exchange.assistant || []) total += Buffer.byteLength(text, 'utf8');
      for (const tool of exchange.tools || []) {
        total += Buffer.byteLength(tool.output || '', 'utf8');
      }
      return sum + total;
    }, 0);
    if (exchanges.length < config.threshold_turns && conversationBytes < config.threshold_conversation_bytes) {
      await writeHarvestState(globalRoot, canonical, sessionId, {
        ...state, session_id: sessionId, harness, workspace: canonical, wiki,
        pending: { turns: exchanges.length, conversation_bytes: conversationBytes, measured_at: Date.now() },
        last_run_at: Date.now(), last_status: 'skipped', last_reason: 'below_threshold',
      });
      return { reason: 'below_threshold', cursorChanged: false, turns: exchanges.length, conversation_bytes: conversationBytes };
    }

    const rendered = renderWindow({
      harness, sessionId, workspace: canonical, exchanges,
      windowStart: exchanges[0]?.timestamp || '', windowEnd: exchanges[exchanges.length - 1]?.timestamp || '',
    });
    const windowPath = join(root, 'harvest', `${hash(sessionId).slice(0, 16)}.window.md`);
    await mkdir(join(root, 'harvest'), { recursive: true, mode: 0o700 });
    await writeWindow(windowPath, rendered, atomicJson);

    const usingDefaultLauncher = !launch && !modelLauncher;
    const launchFn = launch || modelLauncher || launchHarvestAgent;
    if (!launchFn) throw new Error('no launcher available');
    if (usingDefaultLauncher && !lease.launch_mode) {
      const { launchPlan, updateLease } = await import('./ingest.mjs');
      const planned = await launchPlan({ harness, workspace: canonical, env });
      const plannedIdentity = planned.mode === 'claude_bg'
        ? { name: `loam-harvest-${lease.lease_id.slice(0, 8)}` }
        : planned.mode === 'opencode_child'
          ? { parent_session_id: openCodeSession?.parentSessionId || null, title: 'Loam background session harvest' }
          : { boot_id: lease.boot_id, launch_at: new Date().toISOString() };
      await updateLease(root, lease, {
        launch_mode: planned.mode,
        launch_state: 'planned',
        planned_identity: plannedIdentity,
        hard_deadline: new Date(Date.now() + config.timeout_seconds * 1000).toISOString(),
      });
    }
    const launched = await launchFn({
      harness, workspace: canonical, sessionId, globalRoot, skillsRoot,
      lease, windowPath, config, env, openCodeSession, root,
    });
    if (launched?.category) {
      return { reason: launched.category === 'orphan_live' || launched.category === 'orphan_unknown' ? 'busy' : 'unavailable', cursorChanged: false };
    }
    await (launched?.completion || Promise.resolve({ code: 0 }));
    if (launched?.orphaned === true) {
      return { reason: 'busy', cursorChanged: false };
    }

    const boundaryCursor = window.boundaryCursor ?? 0;
    const previousCursor = Number(effectiveState.cursor?.value || 0);
    const nextCursor = Math.max(previousCursor, boundaryCursor);
    await writeHarvestState(globalRoot, canonical, sessionId, {
      ...effectiveState, session_id: sessionId, harness, workspace: canonical, wiki,
      cursor: { kind: effectiveState.cursor?.kind || 'bytes', value: nextCursor, updated_at: Date.now() },
      store: { path: store.path || '', kind: 'jsonl', size: 0, mtime_ms: 0 },
      pending: { turns: exchanges.length, conversation_bytes: conversationBytes, measured_at: Date.now() },
      last_run_at: Date.now(), last_status: 'ok', last_reason: 'ok',
    });
    const lastRunPath = harvestLastRunPath(globalRoot, canonical);
    await atomicJson(lastRunPath, {
      schema: 1, completed_at: Date.now(), status: 'ok', reason: 'ok',
      session_id: sessionId, turns: exchanges.length, conversation_bytes: conversationBytes,
      zero_admission: exchanges.length === 0,
    });
    await pruneHarvestSessions(globalRoot, canonical, config);
    await pruneWindowFiles(root, config.retain_sessions);
    return { reason: 'ok', sessionId, cursorChanged: true, boundaryCursor: nextCursor, turns: exchanges.length, conversation_bytes: conversationBytes };
  } finally {
    if (!leaseHandled) await releaseFn(root, lease.lease_id);
  }
}

export async function harvestStatus({ globalRoot, workspace, env = process.env } = {}) {
  const canonical = await canonicalWorkspace(workspace);
  const config = await readHarvestConfig(globalRoot, env);
  const root = join(resolve(globalRoot), 'run', hash(canonical).slice(0, 16));
  const stateDir = join(root, 'harvest');
  const sessions = [];
  let entries = [];
  try { entries = await fsReaddir(stateDir, { withFileTypes: true }); } catch {}
  for (const entry of entries) {
    if (!entry.isFile() || !entry.name.endsWith('.json') || entry.name.endsWith('.window.md')) continue;
    const state = await jsonRead(join(stateDir, entry.name), null);
    if (!state || state.schema !== 1) continue;
    sessions.push({
      session_id: state.session_id,
      harness: state.harness,
      cursor: state.cursor,
      observed: state.observed,
      pending: state.pending,
      wiki: state.wiki,
      last_run_at: state.last_run_at,
      last_status: state.last_status,
      last_reason: state.last_reason,
      zero_admission_streak: state.zero_admission_streak,
      rotations: state.rotations,
    });
  }
  let lastRun = null;
  try { lastRun = await jsonRead(harvestLastRunPath(globalRoot, canonical), null); } catch {}
  let lease = null;
  let leaseState = 'dead';
  try {
    lease = await jsonRead(join(root, 'lease.json'), null);
    if (lease) {
      const { classifyChild } = await import('./ingest-process.mjs');
      const state = await classifyChild({ pid: lease.owner_pid, boot_id: lease.boot_id, process_start: lease.process_start });
      leaseState = state;
    }
  } catch {}
  return {
    schema: 1,
    workspace: canonical,
    enabled: config.enabled,
    sessions,
    last_run: lastRun,
    lease: lease ? { lease_id: lease.lease_id, launch_mode: lease.launch_mode, launch_state: lease.launch_state, started_at: lease.started_at, hard_deadline: lease.hard_deadline } : null,
    lease_state: leaseState,
    queue: { root: join(root, 'harvest') },
  };
}
