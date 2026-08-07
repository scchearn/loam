import { isAbsolute, resolve } from 'node:path';

import { readInstallMetadata } from './metadata.mjs';
import { invokeRuntime } from './runtime.mjs';

const IDENTIFIER = /^[a-z][a-z0-9_-]{0,31}$/;
const SESSION_CONTROLS = /[\u0000-\u001F\u007F]/;
const WORKER_STATUS = Object.freeze({
  ok: 'succeeded',
  disabled: 'skipped',
  too_soon: 'skipped',
  busy: 'skipped',
  nothing_to_do: 'skipped',
  unavailable: 'failed',
});

const EVENT_BATCH_MAX_EVENTS = 16;
const EVENT_BATCH_MAX_BYTES = 16 * 1024;

// Serialize an already-typed event batch into the bounded schema-1 envelope for
// --events-stdin, or return null when there is nothing valid to send. An invalid
// batch (empty, over-count, oversized, or a non-object member) is dropped
// fail-open so the lifecycle transition still proceeds without it. The DTOs are
// forwarded verbatim; producers own their content and privacy projection.
function eventsEnvelope(events) {
  if (!Array.isArray(events) || events.length === 0) return null;
  if (events.length > EVENT_BATCH_MAX_EVENTS) return null;
  if (!events.every((event) => event && typeof event === 'object' && !Array.isArray(event))) {
    return null;
  }
  const input = JSON.stringify({ schema: 1, events });
  if (Buffer.byteLength(input, 'utf8') > EVENT_BATCH_MAX_BYTES) return null;
  return input;
}

function validSessionId(value) {
  return value === undefined
    || (typeof value === 'string' && value.length > 0 && [...value].length <= 256 && !SESSION_CONTROLS.test(value));
}

function boundedDetail(value, fallback = '') {
  const cleaned = String(value ?? fallback).replace(/[\u0000-\u001F\u007F]/g, '�');
  return [...(cleaned || fallback)].slice(0, 1024).join('');
}

async function preparedRun(run) {
  if (!run || !Number.isSafeInteger(run.id) || run.id < 1) return null;
  if (!isAbsolute(run.globalRoot) || !isAbsolute(run.workspace)) return null;
  const prepared = {
    ...run,
    globalRoot: resolve(run.globalRoot),
    workspace: resolve(run.workspace),
  };
  if (!isAbsolute(prepared.runtimePath || '')) {
    prepared.runtimePath = (await readInstallMetadata(prepared.globalRoot)).runtime_path;
  }
  return isAbsolute(prepared.runtimePath) ? prepared : null;
}

export async function beginHookRun({
  globalRoot,
  harness,
  hook,
  workspace,
  sessionId,
  timeoutMs = 300,
  runner,
} = {}) {
  try {
    if (!isAbsolute(globalRoot) || !isAbsolute(workspace)) return null;
    if (!IDENTIFIER.test(harness) || !IDENTIFIER.test(hook) || !validSessionId(sessionId)) return null;
    const root = resolve(globalRoot);
    const cwd = resolve(workspace);
    const install = await readInstallMetadata(root);
    const args = [
      'hooks', 'begin', root,
      '--harness', harness,
      '--hook', hook,
      '--workspace', cwd,
      '--plugin-version', install.plugin_version,
    ];
    if (sessionId !== undefined) args.push('--session-id', sessionId);
    const result = await invokeRuntime({
      runtimePath: install.runtime_path,
      args,
      cwd,
      timeoutMs,
      runner,
    });
    if (result.code !== 0 || !/^[1-9]\d*\r?\n?$/.test(result.stdout)) return null;
    const id = Number(result.stdout.trim());
    if (!Number.isSafeInteger(id)) return null;
    return {
      id,
      globalRoot: root,
      runtimePath: install.runtime_path,
      workspace: cwd,
    };
  } catch {
    return null;
  }
}

export async function finishHookRun({
  run,
  status,
  action,
  reason,
  detail,
  events,
  timeoutMs = 300,
  runner,
} = {}) {
  try {
    run = await preparedRun(run);
    if (!run) return false;
    if (status !== 'succeeded' && status !== 'failed') return false;
    if (status === 'succeeded') {
      if (action !== 'spawn_worker' && action !== 'skip') return false;
      if (action === 'skip' && !IDENTIFIER.test(reason)) return false;
      if (reason !== undefined && !IDENTIFIER.test(reason)) return false;
    }
    const args = [
      'hooks', 'finish', run.globalRoot,
      '--id', String(run.id),
      '--status', status,
    ];
    if (status === 'failed') args.push('--detail', boundedDetail(detail, 'hook failed'));
    else {
      args.push('--action', action);
      if (reason !== undefined) args.push('--reason', reason);
      if (detail !== undefined) args.push('--detail', boundedDetail(detail));
    }
    const input = eventsEnvelope(events);
    if (input !== null) args.push('--events-stdin');
    const result = await invokeRuntime({
      runtimePath: run.runtimePath,
      args,
      cwd: run.workspace,
      timeoutMs,
      input: input ?? undefined,
      runner,
    });
    return result.code === 0;
  } catch {
    return false;
  }
}

export async function startHookWorker({
  run,
  sessionId,
  events,
  timeoutMs = 300,
  runner,
} = {}) {
  try {
    run = await preparedRun(run);
    if (!run || !validSessionId(sessionId)) return false;
    const args = ['hooks', 'worker-start', run.globalRoot, '--id', String(run.id)];
    if (sessionId !== undefined) args.push('--session-id', sessionId);
    const input = eventsEnvelope(events);
    if (input !== null) args.push('--events-stdin');
    const result = await invokeRuntime({
      runtimePath: run.runtimePath,
      args,
      cwd: run.workspace,
      timeoutMs,
      input: input ?? undefined,
      runner,
    });
    return result.code === 0;
  } catch {
    return false;
  }
}

export async function finishHookWorker({
  run,
  reason,
  detail,
  events,
  timeoutMs = 300,
  runner,
} = {}) {
  try {
    run = await preparedRun(run);
    const status = WORKER_STATUS[reason];
    if (!run || !status) return false;
    const args = [
      'hooks', 'worker-finish', run.globalRoot,
      '--id', String(run.id),
      '--status', status,
      '--reason', reason,
    ];
    if (detail !== undefined) args.push('--detail', boundedDetail(detail));
    const input = eventsEnvelope(events);
    if (input !== null) args.push('--events-stdin');
    const result = await invokeRuntime({
      runtimePath: run.runtimePath,
      args,
      cwd: run.workspace,
      timeoutMs,
      input: input ?? undefined,
      runner,
    });
    return result.code === 0;
  } catch {
    return false;
  }
}
