import { createHash } from 'node:crypto';
import { spawn } from 'node:child_process';
import { lstat, readFile, realpath, stat } from 'node:fs/promises';
import { join, resolve } from 'node:path';

import { readInstallMetadata, readRequiredVersion, readSkillContent, validateInstallMetadata } from './metadata.mjs';
import { assertInside, detectTarget, resolveSkillsRoot, runtimePath } from './paths.mjs';

export const MAX_DETAIL = 4096;
const MAX_RUNTIME_BYTES = 64 * 1024 * 1024;
const SHA256 = /^[a-f0-9]{64}$/i;

export function safeDetail(value, limit = MAX_DETAIL) {
  const cleaned = String(value ?? '').replace(/[\u0000-\u0008\u000B\u000C\u000E-\u001F\u007F]/g, '�');
  return cleaned.length > limit ? `${cleaned.slice(0, limit - 1)}…` : cleaned;
}

function withTimeout(task, timeoutMs) {
  return new Promise((resolvePromise) => {
    let settled = false;
    const timer = setTimeout(() => {
      if (settled) return;
      settled = true;
      resolvePromise({
        code: null,
        signal: 'SIGTERM',
        stdout: '',
        stderr: `runtime timed out after ${timeoutMs}ms`,
        category: 'timeout',
      });
    }, timeoutMs);

    Promise.resolve()
      .then(task)
      .then((result) => {
        if (settled) return;
        settled = true;
        clearTimeout(timer);
        resolvePromise({
          code: result?.code ?? 1,
          signal: result?.signal ?? null,
          stdout: String(result?.stdout ?? ''),
          stderr: safeDetail(result?.stderr),
          ...(result?.category ? { category: result.category } : {}),
        });
      })
      .catch((error) => {
        if (settled) return;
        settled = true;
        clearTimeout(timer);
        resolvePromise({
          code: null,
          signal: null,
          stdout: '',
          stderr: safeDetail(error instanceof Error ? error.message : error),
          category: 'runtime_error',
        });
      });
  });
}

function spawnRuntime({ runtimePath, args, cwd, timeoutMs }) {
  return new Promise((resolvePromise) => {
    const child = spawn(runtimePath, args, { cwd, shell: false, windowsHide: true });
    let stdout = '';
    let stderr = '';
    let settled = false;
    const finish = (result) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      resolvePromise({
        code: result.code,
        signal: result.signal,
        stdout,
        stderr: safeDetail(stderr),
        ...(result.category ? { category: result.category } : {}),
      });
    };
    const timer = setTimeout(() => {
      child.kill();
      finish({ code: null, signal: 'SIGTERM', category: 'timeout' });
    }, timeoutMs);

    child.stdout?.on('data', (chunk) => {
      if (stdout.length < 1_048_576) stdout += chunk.toString();
    });
    child.stderr?.on('data', (chunk) => {
      if (stderr.length < 1_048_576) stderr += chunk.toString();
    });
    child.once('error', (error) => {
      stderr += error.message;
      finish({ code: null, signal: null, category: 'runtime_error' });
    });
    child.once('close', (code, signal) => finish({ code, signal }));
  });
}

export function invokeRuntime({ runtimePath: executable, args = [], cwd, timeoutMs = 5000, runner } = {}) {
  return withTimeout(
    () => (runner ? runner({ runtimePath: executable, args, cwd, timeoutMs }) : spawnRuntime({ runtimePath: executable, args, cwd, timeoutMs })),
    timeoutMs,
  );
}

function unavailable(category, message, fields = {}) {
  return { ready: false, category, message, ...fields };
}

export async function verifyRuntimeFile({ runtimePath, globalRoot, expectedSha256 } = {}) {
  if (typeof expectedSha256 !== 'string' || !SHA256.test(expectedSha256)) {
    return unavailable('runtime_untrusted', 'install metadata does not contain a valid runtime digest');
  }

  const root = resolve(globalRoot);
  let candidate;
  try {
    candidate = assertInside(root, runtimePath, 'runtime path');
    const info = await lstat(candidate);
    if (!info.isFile() || info.isSymbolicLink()) {
      return unavailable('runtime_untrusted', 'runtime path is not a regular file');
    }
    assertInside(root, await realpath(candidate), 'runtime physical path');
    if (info.size > MAX_RUNTIME_BYTES) {
      return unavailable('runtime_untrusted', 'runtime file exceeds the supported size limit');
    }
    const actual = createHash('sha256').update(await readFile(candidate)).digest('hex');
    if (actual !== expectedSha256.toLowerCase()) {
      return unavailable('runtime_untrusted', 'runtime digest does not match install metadata', {
        expected: expectedSha256.toLowerCase(),
        actual,
      });
    }
    return { ready: true, sha256: actual };
  } catch (error) {
    return unavailable(
      error?.code === 'ENOENT' ? 'runtime_missing' : 'runtime_untrusted',
      safeDetail(error instanceof Error ? error.message : error),
    );
  }
}

export async function checkReadiness({
  globalRoot,
  skillsRoot,
  target,
  platform = process.platform,
  arch = process.arch,
  home,
  env = process.env,
  install: suppliedInstall,
} = {}) {
  const root = resolve(globalRoot);
  const skillRoot = resolve(skillsRoot || resolveSkillsRoot({ home, env }));
  let requiredVersion;
  let skillContent;
  let install;
  let actualTarget;
  try {
    actualTarget = target || detectTarget({ platform, arch, override: env.LOAM_TARGET });
    install = suppliedInstall ? validateInstallMetadata(root, suppliedInstall) : await readInstallMetadata(root);
    requiredVersion = await readRequiredVersion({ skillsRoot: skillRoot });
    skillContent = await readSkillContent({ skillsRoot: skillRoot });
  } catch (error) {
    return unavailable('metadata_invalid', safeDetail(error instanceof Error ? error.message : error));
  }

  if (install.runtime_version !== requiredVersion) {
    return unavailable('runtime_version_mismatch', 'installed runtime does not match CLI_VERSION', {
      expected: requiredVersion,
      actual: install.runtime_version,
      install,
      skillContent,
      globalRoot: root,
      skillsRoot: skillRoot,
    });
  }
  if (install.target !== actualTarget) {
    return unavailable('runtime_target_mismatch', 'installed runtime target does not match this host', {
      expected: actualTarget,
      actual: install.target,
      install,
      skillContent,
      globalRoot: root,
      skillsRoot: skillRoot,
    });
  }

  const expectedRuntime = runtimePath(root, requiredVersion, actualTarget, { platform });
  try {
    assertInside(root, expectedRuntime, 'runtime path');
    if (resolve(install.runtime_path) !== resolve(expectedRuntime)) {
      return unavailable('runtime_path_mismatch', 'install metadata points to an unexpected runtime path', {
        expected: expectedRuntime,
        actual: install.runtime_path,
        install,
        skillContent,
        globalRoot: root,
        skillsRoot: skillRoot,
      });
    }
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    return unavailable('runtime_missing', safeDetail(message), {
      install,
      skillContent,
      globalRoot: root,
      skillsRoot: skillRoot,
      requiredVersion,
      target: actualTarget,
    });
  }
  const integrity = await verifyRuntimeFile({
    runtimePath: install.runtime_path,
    globalRoot: root,
    expectedSha256: install.runtime_sha256,
  });
  if (!integrity.ready) {
    return unavailable(integrity.category, integrity.message, {
      install,
      skillContent,
      globalRoot: root,
      skillsRoot: skillRoot,
      requiredVersion,
      target: actualTarget,
    });
  }
  try {
    await stat(install.integration_path);
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    return unavailable('integration_missing', safeDetail(message), {
      install,
      skillContent,
      globalRoot: root,
      skillsRoot: skillRoot,
      requiredVersion,
      target: actualTarget,
    });
  }
  try {
    if (!(await stat(install.adapter_root)).isDirectory()) throw new Error('adapter root is not a directory');
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    return unavailable('adapter_missing', safeDetail(message), {
      install,
      skillContent,
      globalRoot: root,
      skillsRoot: skillRoot,
      requiredVersion,
      target: actualTarget,
    });
  }

  return {
    ready: true,
    globalRoot: root,
    skillsRoot: skillRoot,
    install,
    requiredVersion,
    target: actualTarget,
    runtimePath: install.runtime_path,
    integrationPath: install.integration_path,
    skillContent,
  };
}

export async function probeState({
  workspace,
  runner,
  timeoutMs = 5000,
  readiness,
  ...readinessOptions
} = {}) {
  const status = readiness || (await checkReadiness(readinessOptions));
  if (!status.ready) return status;

  const result = await invokeRuntime({
    runtimePath: status.runtimePath,
    args: ['state', '--fast', resolve(workspace)],
    cwd: resolve(workspace),
    timeoutMs,
    runner,
  });
  if (result.category === 'timeout') {
    return { ...status, ready: false, category: 'timeout', detail: result.stderr };
  }
  if (result.code !== 0) {
    return {
      ...status,
      ready: false,
      category: 'runtime_failed',
      detail: safeDetail(result.stderr || result.stdout || `exit ${result.code}`),
    };
  }

  try {
    const parsed = JSON.parse(result.stdout);
    if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) throw new Error('state output must be a JSON object');
    return { ...status, state: parsed };
  } catch (error) {
    return {
      ...status,
      ready: false,
      category: 'malformed_state',
      detail: safeDetail(error instanceof Error ? `invalid JSON: ${error.message}` : error),
    };
  }
}
