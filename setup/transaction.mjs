import { mkdir, open, readdir, readFile, rename, rm, unlink } from 'node:fs/promises';
import { join } from 'node:path';
import { randomUUID } from 'node:crypto';

import { cleanupStaging, createStagingDirectory, writeAtomicFile, publishJson } from './atomic.mjs';
import { confirmSetup, finish, renderDiscovery, selectMarketplaceHarnesses, stage } from './wizard.mjs';
import { ensureGlobalSkills, verifyGlobalSkills } from './skills.mjs';
import { installRuntime } from './runtime.mjs';
import { detectHarnesses, installHarnesses } from './harnesses.mjs';
import { installMarketplacePlugins } from './marketplace.mjs';
import { migrateLegacyProject } from './migration.mjs';
import { verifyInstallation } from './verify.mjs';
import { stageFederationService } from './federation.mjs';

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
  await renderDiscovery(discovery, output, { action: refresh ? 'Update' : 'Setup', dryRun: parsed.dryRun });
  if (parsed.dryRun) {
    finish(output, 'Dry run', 'no files, configuration, downloads, or mutating Skills CLI commands will run');
    return 0;
  }
  if (!(await confirmSetup({ yes, confirm: options.confirm, input: options.input, output }))) {
    finish(output, 'Setup cancelled');
    return 130;
  }
  const selectedMarketplaceHarnesses = await selectMarketplaceHarnesses({
    yes,
    harnesses: discovery.harnesses,
    select: options.marketplaceSelect,
  });
  if (selectedMarketplaceHarnesses === null) {
    finish(output, 'Setup cancelled');
    return 130;
  }

  const selected = new Set(selectedMarketplaceHarnesses);
  const requestedHarnesses = Object.fromEntries(Object.entries(discovery.harnesses).map(([id, harness]) => [
    id,
    (id === 'claude' || id === 'codex') && !selected.has(id) && !harness.marketplaceReady
      ? { ...harness, state: 'skipped' }
      : harness,
  ]));
  const requestedDiscovery = { ...discovery, harnesses: requestedHarnesses };

  return withSetupLock({ globalRoot: discovery.globalRoot, ...(options.lockOptions || {}) }, async () => {
    const alreadyReady = await verifyInstallation({
      discovery: requestedDiscovery,
      packageRoot: discovery.packageRoot,
      runner: options.runner,
      runtimeRunner: options.smokeRunner,
    });
    if (alreadyReady.ready && !refresh) {
      finish(output, 'Loam is ready', 'already ready; no replacement or network operation required');
      return 0;
    }

    stage(output, 'Environment checked');
    const metadataPath = join(discovery.globalRoot, 'install.json');
    let candidateIntegration;
    let harnessInstall;
    let federationRollback;
    let activated = false;
    try {
      const skills = await ensureGlobalSkills({
        packageRoot: discovery.packageRoot,
        skillsRoot: discovery.skillsRoot,
        cwd: discovery.workspace,
        refresh,
        runner: options.runner,
      });
      if (!skills.ready) {
        errorOutput.write(`Skills CLI: ${skills.detail || skills.category}\n`);
        return 1;
      }
      stage(output, 'Global Loam skills installed by Skills CLI');

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
      stage(output, runtime.reused ? 'Runtime reused' : 'Runtime downloaded and verified');

      candidateIntegration = await stageIntegration({
        packageRoot: discovery.packageRoot,
        globalRoot: discovery.globalRoot,
        pluginVersion: discovery.packageVersion,
      });
      const integrationPath = candidateIntegration.path;
      stage(output, 'Shared integration staged');

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
          marketplace[id] = { ...marketplace[id], state: 'partial', category: 'verification_failed' };
        }
      }
      const effectiveHarnesses = Object.fromEntries(Object.entries(requestedHarnesses).map(([id, harness]) => [
        id,
        marketplace[id]?.state === 'partial' ? marketplace[id] : marketplace[id] ? refreshedHarnesses[id] : harness,
      ]));

      harnessInstall = await installHarnesses({
        home: discovery.home,
        globalRoot: discovery.globalRoot,
        pluginVersion: discovery.packageVersion,
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
      if (marketplaceFailed) errorOutput.write('Marketplace plugin installation is incomplete.\n');
      stage(output, 'Supported integrations configured');

      const globalSkills = await verifyGlobalSkills({
        packageRoot: discovery.packageRoot,
        skillsRoot: discovery.skillsRoot,
        cwd: discovery.workspace,
        runner: options.runner,
      });
      if (!globalSkills.ready) {
        errorOutput.write(`Skills verification: ${globalSkills.detail || globalSkills.category}\n`);
        return 1;
      }

      let migration = discovery.legacy;
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
        stage(output, 'Legacy project Loam migrated');
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
        legacy: { ...migration, ready: true },
      });
      if (!final.ready) {
        errorOutput.write('Final readiness verification failed.\n');
        return 1;
      }

      // Stage the dormant native connector definition + stable identity through
      // the just-verified private runtime, preserving prior active/inert desired
      // state across a runtime-path update. Node delegates entirely to the
      // runtime here — it never renders a definition or calls a manager. Runs
      // after the runtime is committed and before metadata activation, so its
      // rollback participates in the same transaction.
      const federation = await stageFederationService({
        runtimePath: runtime.path,
        globalRoot: discovery.globalRoot,
        runner: options.federationRunner,
      });
      if (!federation.ready) {
        errorOutput.write(`Federation service staging failed: ${federation.detail || federation.category}\n`);
        return 1;
      }
      federationRollback = federation.rollback;

      await options.beforeActivate?.({ install, metadataPath, integrationPath });
      await publishJson({ filePath: metadataPath, value: install });
      activated = true;
      finish(output, marketplaceFailed ? 'Loam core is ready' : 'Loam is ready');
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
