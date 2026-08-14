import { stat } from 'node:fs/promises';
import { join } from 'node:path';

import { invokeRuntime, safeDetail } from '../integration/runtime.mjs';

// Mirror of cli/src/service.rs `definition_path` / SERVICE_LABEL. The runtime
// owns rendering; Node only needs to know WHERE the definition lives so it can
// detect an existing definition (the #100 refresh gate) and confirm absence
// after a disable. Windows keeps its definition inside Task Scheduler, not a
// file, so there is no file-based definition to stat there.
const SERVICE_LABEL = 'io.loam.connector';

export function federationDefinitionPath({ globalRoot, platform = process.platform } = {}) {
  if (!globalRoot) return null;
  if (platform === 'linux') return join(globalRoot, 'systemd', 'loam-connector.service');
  if (platform === 'darwin') return join(globalRoot, 'launchagents', `${SERVICE_LABEL}.plist`);
  return null; // win32 (Task Scheduler) and any platform without a file-based unit.
}

// True only when a file-based definition is present. On win32 (no file) this is
// always false — callers must treat `fileBased === false` as "cannot tell from a
// file" rather than "definitely absent".
export async function federationDefinitionExists({ globalRoot, platform = process.platform } = {}) {
  const path = federationDefinitionPath({ globalRoot, platform });
  if (!path) return { exists: false, fileBased: false, path: null };
  try {
    return { exists: (await stat(path)).isFile(), fileBased: true, path };
  } catch {
    return { exists: false, fileBased: true, path };
  }
}

// Bounded delegation to the private runtime's hidden federation service
// lifecycle commands. Node NEVER renders a service definition,
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

// Stage the dormant native definition through the runtime, preserving prior
// active/inert desired state across a runtime-path update. A fresh install
// stays dormant (status non-zero => inert); an already-active service (status
// exit 0) is re-enabled on the new runtime after the dormant re-install.
// Returns a report plus a bounded rollback for a later setup failure. No
// identity is minted here: the instance id is the certificate's SAN suffix at
// connect time, and a dormant definition carries a deterministic root-derived
// scheduler label (`federation-enrollment-simplification.md`).
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

// setup configurator — enable federation: install the definition through the
// runtime, then enable-start it so the connector is active. Idempotent (install
// re-renders; enable is a no-op if already active). Returns a bounded rollback
// that returns the machine to its prior state: a definition we newly created is
// removed; a service that was already active is left enabled. Never mints
// identity or contacts a broker (the runtime owns enrollment).
export async function enableFederationService({ runtimePath, globalRoot, runner, timeoutMs } = {}) {
  const opts = { runtimePath, globalRoot, runner, timeoutMs };
  const prior = await runFederationService('status', opts);
  const wasActive = prior.ok;

  const installed = await runFederationService('install', opts);
  if (!installed.ok) {
    return { ready: false, category: 'federation_install_failed', detail: installed.stderr, wasActive, rollback: async () => {} };
  }
  const enabled = await runFederationService('enable', opts);
  if (!enabled.ok) {
    return {
      ready: false,
      category: 'federation_enable_failed',
      detail: enabled.stderr,
      wasActive,
      rollback: async () => { if (!wasActive) await runFederationService('uninstall', opts); },
    };
  }
  return {
    ready: true,
    wasActive,
    rollback: async () => {
      if (!wasActive) {
        await runFederationService('disable', opts);
        await runFederationService('uninstall', opts);
      }
    },
  };
}

// setup configurator — verify that a disable was COMPLETE (symmetric-disable
// contract): nothing loam-owned remains for federation. Checks the file-based
// definition is gone AND the manager reports the service not active. Names the
// exact leftovers so a partial disable never reports success. On win32 there is
// no file-based definition, so absence rests on the manager status alone.
export async function verifyFederationAbsent({ runtimePath, globalRoot, runner, timeoutMs, platform = process.platform } = {}) {
  const leftovers = [];
  const { exists, fileBased, path } = await federationDefinitionExists({ globalRoot, platform });
  if (fileBased && exists) leftovers.push({ kind: 'definition_file', path });

  const status = await runFederationService('status', { runtimePath, globalRoot, runner, timeoutMs });
  // A hard invocation error (missing runtime / crash) means we could not confirm
  // absence — surface it rather than claiming a clean disable.
  if (status.category === 'timeout' || status.category === 'process_error') {
    leftovers.push({ kind: 'status_unverifiable', detail: status.stderr || status.category });
  } else if (status.ok) {
    // Exit 0 == still active/enabled.
    leftovers.push({ kind: 'service_active' });
  }
  return { ready: leftovers.length === 0, leftovers, active: status.ok, fileBased };
}

export { runFederationService, FEDERATION_TIMEOUT_MS };
