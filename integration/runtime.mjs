import { createHash } from 'node:crypto';
import { spawn } from 'node:child_process';
import { lstat, readFile, stat } from 'node:fs/promises';
import { join, resolve } from 'node:path';

import { readInstallMetadata, readSkillContent, validateInstallMetadata } from './metadata.mjs';
import { assertInside, assertPhysicalInside, detectTarget, resolveSkillsRoot } from './paths.mjs';
import { readLedger } from './ledger.mjs';
// Config-dir resolver from the integration tree — see integration/config-store.mjs;
// the staged integration must not import ../setup/*.
import { configRoot } from './config-store.mjs';

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

function spawnRuntime({ runtimePath, args, cwd, timeoutMs, input }) {
  return new Promise((resolvePromise) => {
    const child = spawn(runtimePath, args, {
      cwd,
      shell: false,
      windowsHide: true,
      // Open stdin only when there is a bounded payload to write; the no-input
      // path keeps stdin closed exactly as before.
      stdio: [input === undefined ? 'ignore' : 'pipe', 'pipe', 'pipe'],
    });
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
    if (input !== undefined) {
      // Fail-open: a child that exits before reading its stdin raises EPIPE on
      // the write; swallow it so the bounded result still resolves normally.
      child.stdin?.on('error', () => {});
      child.stdin?.end(String(input));
    }
  });
}

export function invokeRuntime({ runtimePath: executable, args = [], cwd, timeoutMs = 5000, input, runner } = {}) {
  return withTimeout(
    () => (runner
      ? runner({ runtimePath: executable, args, cwd, timeoutMs, input })
      : spawnRuntime({ runtimePath: executable, args, cwd, timeoutMs, input })),
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
    await assertPhysicalInside(root, candidate, 'runtime physical path');
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

// Readiness is authoritative on the config-dir ledger + the runtime's own
// self-report, never the skills-tree CLI_VERSION (a stale skills copy provably
// cannot change the outcome). This function verifies the ledger's store binary
// (existence + integrity); the self-report version diff (state.version ===
// ledger.target) happens at the smoke in probeStateWithMode, so a hung/failed
// spawn stays a distinct availability category, not runtime_stale. install.json
// remains readable for its non-version fields (target, integration/adapter
// paths) and the T6 migration only. See plans/runtime-channel-ledger.md.
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
  const config = configRoot({ env, home, platform });
  let skillContent;
  let install;
  let actualTarget;
  let ledger;
  try {
    actualTarget = target || detectTarget({ platform, arch, override: env.LOAM_TARGET });
    install = suppliedInstall ? validateInstallMetadata(root, suppliedInstall) : await readInstallMetadata(root);
    skillContent = await readSkillContent({ skillsRoot: skillRoot });
    ledger = await readLedger({ root: config });
  } catch (error) {
    return unavailable('metadata_invalid', safeDetail(error instanceof Error ? error.message : error));
  }

  const base = { install, skillContent, globalRoot: root, skillsRoot: skillRoot, target: actualTarget };

  if (!ledger) {
    return unavailable('runtime_missing', 'no runtime ledger found; run install', { ...base, hint: 'install' });
  }
  if (install.target !== actualTarget) {
    return unavailable('runtime_target_mismatch', 'installed runtime target does not match this host', {
      expected: actualTarget,
      actual: install.target,
      ...base,
      ledger,
    });
  }

  const integrity = await verifyRuntimeFile({
    runtimePath: ledger.store_path,
    globalRoot: config || root,
    expectedSha256: ledger.sha256,
  });
  if (!integrity.ready) {
    // Missing store binary → install. A byte/sha mismatch (integrity carries
    // expected/actual) means the store binary is not the ledger's target →
    // stale, converge with update. Other integrity failures (symlink, size,
    // physical escape) stay untrusted — a distinct security signal.
    const category = integrity.category === 'runtime_missing'
      ? 'runtime_missing'
      : (integrity.expected || integrity.actual) ? 'runtime_stale' : integrity.category;
    const hint = category === 'runtime_missing' ? 'install' : category === 'runtime_stale' ? 'update' : undefined;
    return unavailable(category, integrity.message, { ...base, ledger, ...(hint ? { hint } : {}) });
  }
  try {
    await stat(install.integration_path);
  } catch (error) {
    return unavailable('integration_missing', safeDetail(error instanceof Error ? error.message : error), { ...base, ledger });
  }
  try {
    if (!(await stat(install.adapter_root)).isDirectory()) throw new Error('adapter root is not a directory');
  } catch (error) {
    return unavailable('adapter_missing', safeDetail(error instanceof Error ? error.message : error), { ...base, ledger });
  }

  return {
    ready: true,
    globalRoot: root,
    skillsRoot: skillRoot,
    install,
    ledger,
    expectedVersion: ledger.target,
    target: actualTarget,
    runtimePath: ledger.store_path,
    integrationPath: install.integration_path,
    skillContent,
  };
}

async function probeStateWithMode({
  workspace,
  runner,
  timeoutMs = 5000,
  fast = true,
  readiness,
  ...readinessOptions
} = {}) {
  const status = readiness || (await checkReadiness(readinessOptions));
  if (!status.ready) return status;

  const result = await invokeRuntime({
    runtimePath: status.runtimePath,
    args: fast ? ['state', '--fast', resolve(workspace)] : ['state', resolve(workspace)],
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

  let parsed;
  try {
    parsed = JSON.parse(result.stdout);
    if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) throw new Error('state output must be a JSON object');
  } catch (error) {
    return {
      ...status,
      ready: false,
      category: 'malformed_state',
      detail: safeDetail(error instanceof Error ? `invalid JSON: ${error.message}` : error),
    };
  }
  // The self-report diff: the runtime's own compiled version must string-equal
  // the ledger target. String equality only — inert across the 1.0.0 rollover.
  if (status.expectedVersion !== undefined) {
    const reported = parsed.version;
    if (typeof reported !== 'string') {
      // No self-report at all → a pre-T1 runtime that predates the compiled
      // `version` field. checkReadiness has ALREADY verified the store binary's
      // sha against the ledger, so this IS the recorded target binary; it just
      // cannot self-report. Transitional PASS (the sha is the proof) with a
      // distinct note — never runtime_stale/`update`, which would loop because
      // the target constant still names this same pre-self-report build. Self-
      // limiting: env!(CARGO_PKG_VERSION) is never empty, so only a pre-T1
      // binary reaches here; a post-T1 mis-build reports a (mismatched) version
      // and fails below. Dissolves the moment a T1+ runtime ships.
      return {
        ...status,
        // Explicit, though `status.ready` is already true here (the `!status.ready`
        // guard above returned) — refactor-proof if that guard ever moves.
        ready: true,
        state: parsed,
        note: 'runtime_predates_self_report',
        detail: `runtime ${status.expectedVersion} predates the self-report field; verified by ledger sha256`,
      };
    }
    if (reported !== status.expectedVersion) {
      return {
        ...status,
        ready: false,
        category: 'runtime_stale',
        hint: 'update',
        expected: status.expectedVersion,
        actual: reported,
        detail: `runtime self-reports ${reported}, ledger target is ${status.expectedVersion}`,
        state: parsed,
      };
    }
  }
  return { ...status, state: parsed };
}

export async function probeState(options = {}) {
  return probeStateWithMode({ ...options, fast: true });
}

export async function probeFullState(options = {}) {
  return probeStateWithMode({ ...options, fast: false });
}
