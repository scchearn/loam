import { mkdir, open, readdir, readFile, rename, rm, unlink } from 'node:fs/promises';
import { join } from 'node:path';
import { randomUUID } from 'node:crypto';

import { cleanupStaging, createStagingDirectory, writeAtomicFile, publishJson } from './atomic.mjs';
import { confirmSetup, finish, harnessLabel, renderDiscovery, selectHarnesses, stepDetail, stepDone, stepSkip, stepStart, summaryNote } from './wizard.mjs';
import { ensureGlobalSkills, verifyGlobalSkills, skillsAgentsFor } from './skills.mjs';
import { installRuntime } from './runtime.mjs';
import { detectHarnesses, installHarnesses } from './harnesses.mjs';
import { installMarketplacePlugins } from './marketplace.mjs';
import { migrateLegacyProject } from './migration.mjs';
import { removeHarnesses } from './uninstall.mjs';
import { verifyInstallation } from './verify.mjs';
import { stageFederationService, federationDefinitionExists } from './federation.mjs';

// #97 fix 1 — a destructive rollback decision must name its reason. Turn the
// per-check breakdown the verifier already computes into one operator-readable
// line instead of discarding it behind "Final readiness verification failed".
function verifyFailureDetail(result, discovery) {
  const failed = [];
  if (result?.install?.plugin_version !== discovery.packageVersion) {
    failed.push(`plugin version (${result?.install?.plugin_version ?? 'none'} != ${discovery.packageVersion})`);
  }
  if (result?.skills && !result.skills.ready) failed.push(`skills (${result.skills.category || result.skills.detail || 'not ready'})`);
  if (result?.runtime && !result.runtime.ready) failed.push(`runtime (${result.runtime.category || 'not ready'})`);
  for (const [id, harness] of Object.entries(result?.harnesses || {})) {
    if (!harness.ready) failed.push(`harness ${id} (${harness.category || 'not ready'})`);
  }
  if (result?.migration && !result.migration.ready) failed.push(`legacy migration (${result.migration.category || 'not ready'})`);
  if (result?.ingestExclusions && !result.ingestExclusions.ready) failed.push(`ingest exclusions (${result.ingestExclusions.category || 'not ready'})`);
  if (result?.federation && result.federation.checked && !result.federation.ready) failed.push(`federation (${result.federation.category || 'not ready'})`);
  return failed.length ? failed.join('; ') : (result?.category || 'unknown check');
}

// ponytail: trivial lockfile — no polling, no stale-PID detection.
// Two concurrent setups on the same HOME is a near-zero event; the second
// exits 1. Upgrade to bounded waits only if real contention is reported.
async function withSetupLock({ globalRoot }, callback) {
  const lockPath = join(globalRoot, 'setup.lock');
  await mkdir(globalRoot, { recursive: true, mode: 0o700 });
  let handle;
  try {
    handle = await open(lockPath, 'wx', 0o600);
  } catch (error) {
    if (error?.code === 'EEXIST') throw new Error(`setup is already running: ${lockPath}`);
    throw error;
  }
  try {
    return await callback();
  } finally {
    await handle.close().catch(() => {});
    await unlink(lockPath).catch(() => {});
  }
}

async function stageIntegration({ packageRoot, globalRoot, pluginVersion }) {
  const sourceRoot = join(packageRoot, 'integration');
  const staging = await createStagingDirectory(globalRoot, { prefix: 'integration' });
  let candidateRoot;
  try {
    const stagedRoot = join(staging, 'integration');
    await mkdir(stagedRoot, { recursive: true, mode: 0o700 });
    for (const entry of await readdir(sourceRoot, { withFileTypes: true })) {
      if (!entry.isFile() || !entry.name.endsWith('.mjs')) continue;
      await writeAtomicFile(join(stagedRoot, entry.name), await readFile(join(sourceRoot, entry.name), 'utf8'));
    }
    candidateRoot = join(globalRoot, 'integration', `${pluginVersion}-${randomUUID()}`);
    await mkdir(join(globalRoot, 'integration'), { recursive: true, mode: 0o700 });
    await rename(stagedRoot, candidateRoot);
    await cleanupStaging(staging);
  } catch (error) {
    await cleanupStaging(staging);
    throw error;
  }
  return { root: candidateRoot, path: join(candidateRoot, 'loam.mjs') };
}

export async function executeSetup(parsed, discovery, options = {}) {
  const output = options.output || process.stdout;
  const errorOutput = options.errorOutput || process.stderr;
  const refresh = parsed.command === 'update';
  const yes = parsed.yes || refresh;
  const tilde = (p) => (typeof p === 'string' && p.startsWith(discovery.home) ? `~${p.slice(discovery.home.length)}` : p);
  await renderDiscovery(discovery, output, { action: refresh ? 'Update' : 'Install', dryRun: parsed.dryRun });
  if (parsed.dryRun) {
    finish(output, 'Dry run', 'no files, configuration, downloads, or mutating Skills CLI commands will run');
    return 0;
  }
  if (!(await confirmSetup({ yes, confirm: options.confirm, input: options.input, output }))) {
    finish(output, 'Setup cancelled');
    return 130;
  }
  let previouslyConfigured = [];
  try {
    const existing = JSON.parse(await readFile(join(discovery.globalRoot, 'install.json'), 'utf8'));
    if (Array.isArray(existing.configured_harnesses)) previouslyConfigured = existing.configured_harnesses;
  } catch {}

  const selection = await selectHarnesses({
    yes,
    refresh,
    harnesses: discovery.harnesses,
    previouslyConfigured,
    select: options.marketplaceSelect,
    input: options.input,
    output,
  });
  if (selection === null) {
    finish(output, 'Setup cancelled');
    return 130;
  }
  const selectedSet = new Set(selection.selected);
  const toRemove = selection.toRemove;
  const selectedMarketplaceHarnesses = selection.selected.filter((id) => id === 'claude' || id === 'codex');

  const requestedHarnesses = Object.fromEntries(Object.entries(discovery.harnesses).map(([id, harness]) => {
    if (harness.state === 'absent') return [id, harness];
    if (id === 'claude' || id === 'codex') {
      return [id, !selectedSet.has(id) && !harness.marketplaceReady ? { ...harness, state: 'skipped' } : harness];
    }
    // opencode / cursor: adapters gated purely by selection.
    return [id, selectedSet.has(id) ? harness : { ...harness, state: 'absent' }];
  }));
  const requestedDiscovery = { ...discovery, harnesses: requestedHarnesses };

  return withSetupLock({ globalRoot: discovery.globalRoot, ...(options.lockOptions || {}) }, async () => {
    // #97 fix 2 — migrate BEFORE staging and BEFORE any verify. Legacy migration
    // mutates workspace state (removes legacy project skills/markers); running it
    // mid-transaction let it fail the very verification of the transaction that
    // performed it, which then rolled back and wiped a working install. Doing it
    // up front, once, and feeding its post-migration result into every later
    // verify means migration can no longer fail its own transaction. It is
    // workspace cleanup, independent of and outside the staged global install.
    let migration = { ...discovery.legacy, ready: true };
    if (discovery.legacy.needed) {
      migration = await migrateLegacyProject({
        workspace: discovery.workspace,
        packageRoot: discovery.packageRoot,
        yes,
        prompt: options.migrationConfirm || options.confirm,
        runner: options.runner,
      });
      if (!migration.ready) {
        errorOutput.write(`Migration incomplete: ${migration.category || 'legacy project remains'}\n`);
        return 1;
      }
      stepDone(output, 'Legacy project Loam migrated');
    }
    const migratedLegacy = { ...migration, ready: true };

    const alreadyReady = await verifyInstallation({
      discovery: requestedDiscovery,
      packageRoot: discovery.packageRoot,
      runner: options.runner,
      runtimeRunner: options.smokeRunner,
      legacy: migratedLegacy,
    });
    if (alreadyReady.ready && !refresh && toRemove.length === 0) {
      finish(output, '🌱 Loam is ready', 'already ready; no replacement or network operation required');
      return 0;
    }

    stepStart(output, 'Checking environment');
    stepDone(output, refresh ? 'Environment checked — refreshing existing install' : 'Environment checked');
    const metadataPath = join(discovery.globalRoot, 'install.json');
    let candidateIntegration;
    let harnessInstall;
    let federationRollback;
    let activated = false;
    let skillCount;
    try {
      stepStart(output, 'Installing global skills via the Skills CLI');
      const skills = await ensureGlobalSkills({
        packageRoot: discovery.packageRoot,
        skillsRoot: discovery.skillsRoot,
        cwd: discovery.workspace,
        refresh,
        runner: options.runner,
        agents: skillsAgentsFor(discovery.harnesses),
      });
      if (!skills.ready) {
        errorOutput.write(`Skills CLI: ${skills.detail || skills.category}\n`);
        return 1;
      }
      skillCount = skills.inventory?.skills?.length;
      stepDone(output, `Skills ${skills.changed ? 'installed' : 'already current'}${skillCount ? ` — ${skillCount} skills` : ''} · runtime v${skills.requiredVersion}  →  ${tilde(discovery.skillsRoot)}`);

      stepStart(output, `Preparing native runtime v${skills.requiredVersion} (${discovery.target})`);
      const runtime = await installRuntime({
        globalRoot: discovery.globalRoot,
        version: skills.requiredVersion,
        target: discovery.target,
        platform: discovery.platform,
        arch: discovery.arch,
        releaseBaseUrl: options.releaseBaseUrl,
        workspace: discovery.workspace,
        smokeRunner: options.smokeRunner,
        expectedSha256: alreadyReady.install?.runtime_sha256,
      });
      const shortSha = typeof runtime.sha256 === 'string' ? runtime.sha256.slice(0, 12) : '';
      if (runtime.reused) {
        stepDetail(output, `reused verified binary${shortSha ? ` (sha256 ${shortSha}…)` : ''}`);
      } else {
        stepDetail(output, 'downloaded from github.com/scchearn/loam releases');
        if (shortSha) stepDetail(output, `checksum sha256 ${shortSha}…  ✓`);
        stepDetail(output, 'smoke test: state --fast  ✓');
      }
      stepDone(output, `Runtime ready  →  ${tilde(runtime.path)}`);

      stepStart(output, `Staging shared integration (v${discovery.packageVersion})`);
      candidateIntegration = await stageIntegration({
        packageRoot: discovery.packageRoot,
        globalRoot: discovery.globalRoot,
        pluginVersion: discovery.packageVersion,
      });
      const integrationPath = candidateIntegration.path;
      stepDone(output, 'Shared integration staged');

      if (selectedMarketplaceHarnesses.length) stepStart(output, 'Installing marketplace plugins');
      const marketplace = await installMarketplacePlugins({
        selected: selectedMarketplaceHarnesses,
        harnesses: discovery.harnesses,
        refresh,
        cwd: discovery.workspace,
        runner: options.runner,
      });
      const refreshedHarnesses = await detectHarnesses({
        home: discovery.home,
        pluginVersion: discovery.packageVersion,
      });
      for (const id of selectedMarketplaceHarnesses) {
        if (marketplace[id]?.state === 'ready' && !refreshedHarnesses[id]?.marketplaceReady) {
          // Verification failed: fall back to the refreshed detection, whose
          // marketplaceReady/marketplaceRoot reflect what is actually on disk.
          marketplace[id] = { ...refreshedHarnesses[id], state: 'partial', category: 'verification_failed' };
        }
        const st = marketplace[id];
        if (st?.state === 'ready') {
          const verb = st.action === 'existing' ? 'already installed' : st.action === 'updated' ? 'updated' : 'installed';
          stepDone(output, `${harnessLabel(id)} — plugin loam@loam ${verb}`);
        } else if (st?.state === 'partial') {
          stepSkip(output, `${harnessLabel(id)} — plugin verification failed`);
        }
      }
      const effectiveHarnesses = Object.fromEntries(Object.entries(requestedHarnesses).map(([id, harness]) => [
        id,
        marketplace[id]?.state === 'partial' ? marketplace[id] : marketplace[id] ? refreshedHarnesses[id] : harness,
      ]));

      stepStart(output, 'Configuring harnesses');
      harnessInstall = await installHarnesses({
        home: discovery.home,
        globalRoot: discovery.globalRoot,
        pluginVersion: discovery.packageVersion,
        runtimePath: runtime.path,
        integrationPath,
        detected: effectiveHarnesses,
      });
      const harnesses = harnessInstall;
      const integrationFailed = Object.values(harnesses).some((harness) => harness.state === 'partial');
      for (const [id, state] of Object.entries(marketplace)) {
        if (state.state === 'partial' && harnesses[id]?.state !== 'partial') harnesses[id] = state;
      }
      const marketplaceFailed = Object.values(marketplace).some((harness) => harness.state === 'partial');
      if (integrationFailed) {
        errorOutput.write('Harness integration is incomplete.\n');
        return 1;
      }
      for (const id of ['claude', 'codex', 'opencode', 'cursor']) {
        const h = harnesses[id];
        if (h?.state === 'ready') {
          const detail = id === 'opencode' ? `adapter written to ${tilde(h.path)}`
            : id === 'cursor' ? 'session hook registered'
            : 'session hooks ready';
          stepDone(output, `${harnessLabel(id)} — ${detail}`);
        } else if (h?.state === 'skipped') {
          stepSkip(output, `${harnessLabel(id)} — skipped (plugin not selected)`);
        }
      }
      if (marketplaceFailed) errorOutput.write('Marketplace plugin installation is incomplete.\n');

      if (toRemove.length) {
        stepStart(output, 'Removing deselected harnesses');
        await removeHarnesses({
          ids: toRemove,
          home: discovery.home,
          globalRoot: discovery.globalRoot,
          runner: options.runner,
          cwd: discovery.workspace,
        });
        for (const id of toRemove) stepDone(output, `${harnessLabel(id)} — removed`);
      }

      stepStart(output, 'Verifying installation');
      const globalSkills = await verifyGlobalSkills({
        packageRoot: discovery.packageRoot,
        skillsRoot: discovery.skillsRoot,
        cwd: discovery.workspace,
        runner: options.runner,
        agents: skillsAgentsFor(discovery.harnesses),
      });
      if (!globalSkills.ready) {
        errorOutput.write(`Skills verification: ${globalSkills.detail || globalSkills.category}\n`);
        return 1;
      }

      const install = {
        schema_version: 1,
        plugin_version: discovery.packageVersion,
        runtime_version: skills.requiredVersion,
        target: discovery.target,
        runtime_path: runtime.path,
        runtime_sha256: runtime.sha256,
        adapter_root: harnesses.versionRoot,
        integration_path: integrationPath,
        skills_scope: 'global',
        skills_source: 'scchearn/loam',
        configured_harnesses: Object.entries(harnesses)
          .filter(([, harness]) => harness.state === 'ready')
          .map(([id]) => id),
      };
      const verificationHarnesses = Object.fromEntries(Object.entries(harnesses).map(([id, harness]) => [
        id,
        harness.state === 'partial' && marketplace[id]?.state === 'partial'
          ? { ...harness, state: 'skipped' }
          : harness,
      ]));
      const final = await (options.finalVerify || verifyInstallation)({
        discovery: { ...discovery, harnesses: verificationHarnesses },
        packageRoot: discovery.packageRoot,
        install,
        runner: options.runner,
        runtimeRunner: options.smokeRunner,
        legacy: migratedLegacy,
      });
      if (!final.ready) {
        // #97 fix 1 — name the check(s) that failed so the rollback decision is
        // explained, not silent.
        errorOutput.write(`Final readiness verification failed: ${verifyFailureDetail(final, discovery)}\n`);
        return 1;
      }
      stepDone(output, 'All checks passed');

      // #100 — a runtime version bump must refresh the service definition, which
      // embeds the versioned binary path. On `update`, and only when a definition
      // already exists (federation was enabled on this machine — install and a
      // never-enabled machine leave federation alone), re-render it through the
      // just-committed runtime and preserve the prior active/inert state. The
      // runtime owns rendering and the manager calls; its rollback joins this
      // transaction. On win32 there is no file-based definition, so this no-ops.
      // win32 is deliberately excluded: its definition lives in Task Scheduler
      // with only a `windows-task.marker` file (federationDefinitionPath mirrors
      // it for the absence verify). Re-rendering a scheduled task against the new
      // runtime on update is Windows service parity — tracked with #100, out of
      // scope here — so the marker must NOT trip this refresh.
      const definition = refresh && discovery.platform !== 'win32'
        ? await federationDefinitionExists({ globalRoot: discovery.globalRoot, platform: discovery.platform })
        : { exists: false };
      if (refresh && definition.exists && runtime?.path) {
        const federation = await stageFederationService({
          runtimePath: runtime.path,
          globalRoot: discovery.globalRoot,
          runner: options.federationRunner,
          timeoutMs: options.federationTimeoutMs,
        });
        if (!federation.ready) {
          errorOutput.write(`Federation service refresh failed: ${federation.detail || federation.category}\n`);
          return 1;
        }
        stepDone(output, 'Service definition refreshed for the new runtime');
        federationRollback = federation.rollback;
      }

      await options.beforeActivate?.({ install, metadataPath, integrationPath });
      await publishJson({ filePath: metadataPath, value: install });
      activated = true;

      const configuredLabels = install.configured_harnesses.map((id) => harnessLabel(id));
      summaryNote(output, 'Installed', [
        `Plugin     v${discovery.packageVersion}`,
        `Runtime    v${skills.requiredVersion}  (${discovery.target})`,
        `Skills     ${skillCount ?? '?'} · ${tilde(discovery.skillsRoot)}`,
        `Harnesses  ${configuredLabels.length ? configuredLabels.join(', ') : 'none'}`,
        '',
        'Next: open a coding session and say "set up a wiki" or "plan this work".',
      ].join('\n'));
      finish(output, marketplaceFailed ? '🌱 Loam core is ready' : '🌱 Loam is ready');
      return marketplaceFailed ? 1 : 0;
    } finally {
      if (!activated) {
        try {
          await federationRollback?.();
        } finally {
          try {
            await harnessInstall?.rollback?.();
          } finally {
            if (candidateIntegration) await rm(candidateIntegration.root, { recursive: true, force: true });
          }
        }
      }
    }
  });
}
