import { invokeRuntime, safeDetail } from '../integration/runtime.mjs';

// Bounded delegation to the private runtime's hidden federation service
// lifecycle commands (Slice C T8/T12). Node NEVER renders a service definition,
// calls a manager (systemctl/launchctl/schtasks) directly, or starts the
// connector — the Rust CLI owns all of that. No credential is resolved and no
// broker is contacted here: these commands only manage the dormant per-user
// definition and the stable instance identity.

const FEDERATION_TIMEOUT_MS = 30_000;

async function runFederationService(
  action,
  { runtimePath, globalRoot, runner, timeoutMs = FEDERATION_TIMEOUT_MS } = {},
) {
  const result = await invokeRuntime({
    runtimePath,
    args: ['federation', 'service', action, '--global-root', globalRoot],
    cwd: globalRoot,
    timeoutMs,
    runner,
  });
  const code = result?.code ?? 1;
  return {
    ok: code === 0,
    action,
    code,
    category: result?.category,
    stderr: safeDetail(result?.stderr),
  };
}

// Stage the dormant native definition + stable identity through the runtime,
// preserving prior active/inert desired state across a runtime-path update.
// A fresh install stays dormant (status non-zero => inert); an already-active
// service (status exit 0) is re-enabled on the new runtime after the dormant
// re-install. Returns a report plus a bounded rollback for a later setup
// failure. The stable instance identity is never replaced (the runtime's
// `ensure_instance_id` only ever generates it once).
export async function stageFederationService({ runtimePath, globalRoot, runner, timeoutMs } = {}) {
  const opts = { runtimePath, globalRoot, runner, timeoutMs };

  const prior = await runFederationService('status', opts);
  const wasActive = prior.ok;

  const installed = await runFederationService('install', opts);
  if (!installed.ok) {
    return {
      ready: false,
      category: 'federation_install_failed',
      detail: installed.stderr,
      wasActive,
      rollback: async () => {},
    };
  }

  if (wasActive) {
    const enabled = await runFederationService('enable', opts);
    if (!enabled.ok) {
      return {
        ready: false,
        category: 'federation_enable_failed',
        detail: enabled.stderr,
        wasActive,
        // Leave it disabled rather than half-enabled against a runtime that the
        // failing setup may roll back; the identity and definition file remain.
        rollback: async () => {
          await runFederationService('disable', opts);
        },
      };
    }
  }

  return {
    ready: true,
    wasActive,
    // On a later setup failure: a definition we *newly* created (there was no
    // prior active service) is removed; an update that re-pointed an existing
    // active service at the committed new runtime is left as found. We never
    // delete the instance identity or contact a broker.
    rollback: async () => {
      if (!wasActive) await runFederationService('uninstall', opts);
    },
  };
}

// Verification helper: the native definition references the trusted current
// runtime and the instance identity is valid/preserved. Read-only (status),
// never starts the connector.
export async function verifyFederationService({ runtimePath, globalRoot, runner, timeoutMs } = {}) {
  const result = await runFederationService('status', {
    runtimePath,
    globalRoot,
    runner,
    timeoutMs,
  });
  // Either enabled (0) or a clean "disabled" report proves the definition is
  // present and inspectable; only a hard invocation error (no runtime / crash)
  // is a verification failure.
  const ready = result.category !== 'timeout' && result.category !== 'process_error';
  return { ready, active: result.ok, category: result.category, detail: result.stderr };
}

// Remove the Loam-owned native definition through the runtime during uninstall:
// disable/stop first (best-effort), then remove the definition. Never touches
// credentials or a broker.
export async function removeFederationService({ runtimePath, globalRoot, runner, timeoutMs } = {}) {
  const opts = { runtimePath, globalRoot, runner, timeoutMs };
  await runFederationService('disable', opts);
  return runFederationService('uninstall', opts);
}

export { runFederationService, FEDERATION_TIMEOUT_MS };
