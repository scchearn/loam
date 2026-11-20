import { invokeRuntime, safeDetail } from '../integration/runtime.mjs';

// Bounded delegation to the private runtime's `seal trust init`. Node NEVER
// generates or writes key material: the runtime owns the ed25519 agent key, the
// quoted `allowed_signers` line and the default policy, all under
// `<configRoot>/seal/`. The step is non-interactive and idempotent (a second run
// reports nothing created) and never fails setup: any failure, an older runtime
// that lacks `seal`, or a missing ssh-keygen becomes a typed skip.

const SEAL_TIMEOUT_MS = 30_000;

function skip(detail) {
  return { ready: false, created: [], category: 'seal_unavailable', detail: safeDetail(detail) };
}

export async function provisionSealTrust({ runtimePath, cwd, runner, timeoutMs = SEAL_TIMEOUT_MS } = {}) {
  if (!runtimePath) return skip('no runtime path');
  let result;
  try {
    result = await invokeRuntime({
      runtimePath,
      args: ['seal', 'trust', 'init', '--json'],
      cwd,
      timeoutMs,
      runner,
    });
  } catch (error) {
    return skip(error instanceof Error ? error.message : error);
  }
  if (result?.code !== 0) {
    return skip(result?.stderr || result?.category || `exit ${result?.code ?? 'unknown'}`);
  }
  let report;
  try {
    report = JSON.parse(String(result.stdout ?? '').trim());
  } catch {
    return skip('seal trust init returned unparseable output');
  }
  if (!report || typeof report.principal !== 'string' || !report.principal) {
    return skip('seal trust init returned no principal');
  }
  const created = Array.isArray(report.created) ? report.created.map(String) : [];
  return { ready: true, principal: report.principal, created, category: undefined, detail: '' };
}
