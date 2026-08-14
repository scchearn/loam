import { readFile } from 'node:fs/promises';
import { join } from 'node:path';

import { discover } from './discovery.mjs';
import {
  enableFederationService,
  removeFederationService,
  verifyFederationAbsent,
  verifyFederationService,
} from './federation.mjs';
import { catalogEntry, CATALOG } from './integrations/catalog.mjs';
import { announce, finish, stepStart, stepDone, confirmAction } from './wizard.mjs';

// `setup` is the configurator for an EXISTING install: it toggles federation and
// optional integrations, and selects harnesses. It never installs or updates
// core loam and never touches versions. Federation is its headline job, so the
// federation enable/disable/verify path is first-class here; harness selection
// reconciles through the idempotent install transaction (a same-version install
// is a no-op on skills/runtime, so no version moves); the integrations catalog
// is iterated generically over the (Unit A: empty) registry seam.

async function readInstall(globalRoot) {
  try {
    return JSON.parse(await readFile(join(globalRoot, 'install.json'), 'utf8'));
  } catch {
    return null;
  }
}

// Grace for the retired first-time `setup` (one release train): a bare `setup`
// on a machine with no install guides to `install` rather than silently doing
// nothing. Interactive TTY: offer to run install now; otherwise print the hint.
async function graceNoInstall(parsed, discovery, options) {
  const output = options.output || process.stdout;
  const input = options.input || process.stdin;
  await announce(output, '🌱 Loam setup', [
    `No Loam installation found at ${discovery.globalRoot}.`,
    '`setup` configures an existing install; it no longer performs first-time installation.',
  ], { level: 'warn' });

  if (parsed.dryRun) {
    finish(output, 'Nothing to configure', 'run `npx @scchearn/loam install` first');
    return 1;
  }
  const wantsInstall = await confirmAction({
    yes: false,
    confirm: options.confirm,
    input,
    output,
    promptText: 'Run `npx @scchearn/loam install` now? [y/N] ',
    nonInteractiveMessage: 'Run `npx @scchearn/loam install` first, then `setup` to configure it.',
  });
  if (!wantsInstall) {
    finish(output, 'Nothing configured', 'run `npx @scchearn/loam install` first');
    return 1;
  }
  const { runSetup } = await import('./main.mjs');
  return runSetup({ command: 'install', yes: parsed.yes, dryRun: false }, options);
}

// The runtime spawner federation delegation uses. Tests inject options.runner
// (a recording/stub runner); production leaves it undefined so invokeRuntime
// spawns the installed private runtime binary.
function federationRunner(options) {
  return options.federationRunner;
}

async function configureFederation(action, { discovery, install, parsed, options }) {
  const output = options.output || process.stdout;
  const errorOutput = options.errorOutput || process.stderr;
  const runner = federationRunner(options);
  const runtimePath = install.runtime_path;
  const platform = discovery.platform;
  const base = { runtimePath, globalRoot: discovery.globalRoot, runner, timeoutMs: options.federationTimeoutMs, platform };

  if (!runtimePath) {
    errorOutput.write('Cannot manage federation: the install has no recorded runtime path. Run `update` first.\n');
    return { ok: false };
  }

  if (parsed.dryRun) {
    stepStart(output, `Federation ${action} (dry-run)`);
    stepDone(output, action === 'enable'
      ? 'Would install and enable the connector service definition through the runtime'
      : 'Would stop, disable, and remove the connector service definition (identity/enrollment preserved)');
    return { ok: true };
  }

  if (action === 'enable') {
    stepStart(output, 'Enabling federation');
    const result = await enableFederationService(base);
    if (!result.ready) {
      errorOutput.write(`Federation enable failed: ${result.detail || result.category}\n`);
      await result.rollback?.();
      return { ok: false };
    }
    // Verify the definition is actually present/inspectable through the runtime.
    const verified = await verifyFederationService(base);
    if (!verified.ready) {
      errorOutput.write(`Federation enable could not be verified: ${verified.detail || verified.category}\n`);
      return { ok: false };
    }
    stepDone(output, 'Federation enabled — connector service installed and active');
    return { ok: true };
  }

  // disable — symmetric, complete, and verified. Identity/enrollment/rosters are
  // NOT destroyed (disable ≠ disenroll); only the service + staged definition go.
  stepStart(output, 'Disabling federation');
  const removed = await removeFederationService(base);
  if (!removed.ok) {
    errorOutput.write(`Federation disable did not complete cleanly: ${removed.stderr || removed.category}\n`);
  }
  const absence = await verifyFederationAbsent(base);
  if (!absence.ready) {
    // Never claim "disabled" with leftovers — name exactly what remains.
    const detail = absence.leftovers.map((l) => l.path || l.detail || l.kind).join('; ');
    errorOutput.write(`Federation disable incomplete — still present: ${detail}\n`);
    return { ok: false };
  }
  stepDone(output, 'Federation disabled — service and definition removed; identity and enrollment preserved');
  return { ok: true };
}

async function configureIntegration(id, mode, { discovery, install, parsed, options }) {
  const output = options.output || process.stdout;
  const errorOutput = options.errorOutput || process.stderr;
  const entry = catalogEntry(id);
  if (!entry) {
    const known = CATALOG.map((e) => e.id).join(', ') || 'none available in this release';
    errorOutput.write(`Unknown integration: ${id} (available: ${known})\n`);
    return { ok: false };
  }
  const ctx = {
    discovery,
    install,
    dryRun: parsed.dryRun,
    purge: parsed.purge,
    harnesses: discovery.harnesses,
    runner: options.runner,
    output,
  };
  const result = mode === 'enable' ? await entry.enable(ctx) : await entry.disable(ctx);
  if (!result?.ready) {
    const leftovers = result?.leftovers?.length ? ` — leftovers: ${result.leftovers.join(', ')}` : '';
    errorOutput.write(`Integration ${id} ${mode} failed: ${result?.detail || result?.category || 'unknown'}${leftovers}\n`);
    return { ok: false };
  }
  stepDone(output, `Integration ${id} ${mode === 'enable' ? 'enabled' : 'disabled'}`);
  return { ok: true };
}

// Harness selection reconciles WHICH harnesses are wired. The adapter + native
// marketplace-plugin pipeline that does this lives in the install transaction;
// re-implementing it here would duplicate the marketplace/adapter/verify/rollback
// machinery. Instead we drive that same transaction with a FIXED selection and
// the install's current package version — so skills and runtime are a no-op (no
// version moves, the configurator's contract), and only the harness wiring is
// reconciled to `desired`. Returns { ok }.
async function configureHarnesses(desired, { parsed, options }) {
  const output = options.output || process.stdout;
  stepStart(output, 'Reconciling harness selection');
  const { runSetup } = await import('./main.mjs');
  const select = async () => desired; // stands in for the clack multiselect widget
  const code = await runSetup(
    { command: 'install', yes: false, dryRun: parsed.dryRun },
    { ...options, marketplaceSelect: select, confirm: async () => true, migrationConfirm: async () => true },
  );
  if (code !== 0) return { ok: false };
  stepDone(output, 'Harness selection reconciled');
  return { ok: true };
}

export async function runConfigure(parsed, options = {}) {
  const output = options.output || process.stdout;
  const errorOutput = options.errorOutput || process.stderr;
  try {
    const discovery = await discover({
      home: options.home,
      workspace: options.workspace,
      packageRoot: options.packageRoot,
      target: options.target,
      platform: options.platform,
      arch: options.arch,
      runner: options.runner,
    });

    const install = await readInstall(discovery.globalRoot);
    if (!install) return graceNoInstall(parsed, discovery, options);

    // Decide what to do. Flag-driven when any component flag is present;
    // otherwise interactive. `--yes` alone (no component flags) is a no-op.
    const hasComponentFlags = parsed.federation !== null || (parsed.integrations?.length > 0);
    let harnessSelection = null;
    if (!hasComponentFlags) {
      if (options.select) {
        // Injected interactive selection (tests / future clack menu). Shape:
        // { federation?: 'enable'|'disable', integrations?: [{id, mode}], harnesses?: string[] }.
        const chosen = await options.select({ install, discovery });
        parsed = {
          ...parsed,
          federation: chosen?.federation ?? null,
          integrations: (chosen?.integrations || []).map((i) => i.id),
          integrationModes: Object.fromEntries((chosen?.integrations || []).map((i) => [i.id, i.mode])),
        };
        harnessSelection = Array.isArray(chosen?.harnesses) ? chosen.harnesses : null;
      } else {
        finish(output, 'Nothing to configure',
          'pass --federation enable|disable or --integration <id> (or run interactively)');
        return 0;
      }
    }

    await announce(output, `🌱 Loam setup${parsed.dryRun ? ' (dry-run)' : ''}`, [
      `Global root: ${discovery.globalRoot}`,
      `Plugin v${install.plugin_version} · runtime v${install.runtime_version}`,
    ]);

    let ok = true;
    if (parsed.federation) {
      const result = await configureFederation(parsed.federation, { discovery, install, parsed, options });
      ok = ok && result.ok;
    }
    for (const id of parsed.integrations || []) {
      const mode = parsed.integrationModes?.[id] || 'enable';
      const result = await configureIntegration(id, mode, { discovery, install, parsed, options });
      ok = ok && result.ok;
    }
    if (harnessSelection) {
      const result = await configureHarnesses(harnessSelection, { parsed, options });
      ok = ok && result.ok;
    }

    if (!ok) return 1;
    finish(output, parsed.dryRun ? 'Dry run complete' : '🌱 Loam configured');
    return 0;
  } catch (error) {
    errorOutput.write(`Setup failed: ${error instanceof Error ? error.message : String(error)}\n`);
    return 1;
  }
}
