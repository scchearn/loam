import { isAbsolute, resolve } from 'node:path';

import { readInstallMetadata } from './metadata.mjs';
import { invokeRuntime } from './runtime.mjs';

const IDENTIFIER = /^[a-z][a-z0-9_-]{0,31}$/;
const SESSION_CONTROLS = /[\u0000-\u001F\u007F]/;

function validSessionId(value) {
  return value === undefined
    || (typeof value === 'string' && value.length > 0 && [...value].length <= 256 && !SESSION_CONTROLS.test(value));
}

function failureDetail(value) {
  const cleaned = String(value ?? 'hook failed').replace(/[\u0000-\u001F\u007F]/g, '�');
  return [...(cleaned || 'hook failed')].slice(0, 1024).join('');
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
  detail,
  timeoutMs = 300,
  runner,
} = {}) {
  try {
    if (!run || !Number.isSafeInteger(run.id) || run.id < 1) return false;
    if (!isAbsolute(run.globalRoot) || !isAbsolute(run.runtimePath) || !isAbsolute(run.workspace)) return false;
    if (status !== 'succeeded' && status !== 'failed') return false;
    const args = [
      'hooks', 'finish', run.globalRoot,
      '--id', String(run.id),
      '--status', status,
    ];
    if (status === 'failed') args.push('--detail', failureDetail(detail));
    const result = await invokeRuntime({
      runtimePath: run.runtimePath,
      args,
      cwd: run.workspace,
      timeoutMs,
      runner,
    });
    return result.code === 0;
  } catch {
    return false;
  }
}
