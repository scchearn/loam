import { mkdir, readdir, readFile, rename, rm } from 'node:fs/promises';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { randomUUID } from 'node:crypto';

import { cleanupStaging, createStagingDirectory, writeAtomicFile, publishJson } from './atomic.mjs';
import { confirmSetup, renderDiscovery, stage } from './wizard.mjs';
import { ensureGlobalSkills, verifyGlobalSkills } from './skills.mjs';
import { installRuntime } from './runtime.mjs';
import { installHarnesses } from './harnesses.mjs';
import { migrateLegacyProject } from './migration.mjs';
import { verifyInstallation } from './verify.mjs';
import { withSetupLock } from './lock.mjs';

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

async function verifyIntegrationCandidate({ discovery, install, integrationPath, runner, timeoutMs }) {
  const integration = await import(`${pathToFileURL(integrationPath).href}?candidate=${randomUUID()}`);
  if (typeof integration.runIntegration !== 'function') throw new Error('candidate integration is missing runIntegration');
  const options = {
    globalRoot: discovery.globalRoot,
    skillsRoot: discovery.skillsRoot,
    integrationPath,
    target: discovery.target,
    platform: discovery.platform,
    arch: discovery.arch,
    workspace: discovery.workspace,
    install,
    runner,
    timeoutMs,
    output: { write: () => {} },
  };
  if (await integration.runIntegration(['status'], options) !== 0) {
    throw new Error('candidate integration status verification failed');
  }
  if (await integration.runIntegration(['hook', '--harness', 'opencode', '--workspace', discovery.workspace], options) !== 0) {
    throw new Error('candidate integration hook verification failed');
  }
}

export async function executeSetup(parsed, discovery, options = {}) {
  const output = options.output || process.stdout;
  const errorOutput = options.errorOutput || process.stderr;
  renderDiscovery(discovery, output, { dryRun: parsed.dryRun });
  if (parsed.dryRun) {
    stage(output, 'Dry run', 'no files, configuration, downloads, or mutating Skills CLI commands will run');
    return 0;
  }
  if (!(await confirmSetup({ yes: parsed.yes, confirm: options.confirm, input: options.input, output }))) {
    stage(output, 'Setup cancelled');
    return 130;
  }

  return withSetupLock({ globalRoot: discovery.globalRoot, ...(options.lockOptions || {}) }, async () => {
    const alreadyReady = await verifyInstallation({
      discovery,
      packageRoot: discovery.packageRoot,
      runner: options.runner,
      runtimeRunner: options.smokeRunner,
    });
    if (alreadyReady.ready) {
      stage(output, 'Loam is ready', 'already ready; no replacement or network operation required');
      return 0;
    }

    stage(output, 'Environment checked');
    const metadataPath = join(discovery.globalRoot, 'install.json');
    let candidateIntegration;
    let harnessInstall;
    let activated = false;
    try {
      const skills = await ensureGlobalSkills({
        packageRoot: discovery.packageRoot,
        skillsRoot: discovery.skillsRoot,
        cwd: discovery.workspace,
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
        lock: false,
      });
      stage(output, runtime.reused ? 'Runtime reused' : 'Runtime downloaded and verified');

      candidateIntegration = await stageIntegration({
        packageRoot: discovery.packageRoot,
        globalRoot: discovery.globalRoot,
        pluginVersion: discovery.packageVersion,
      });
      const integrationPath = candidateIntegration.path;
      stage(output, 'Shared integration staged');

      harnessInstall = await installHarnesses({
        home: discovery.home,
        globalRoot: discovery.globalRoot,
        pluginVersion: discovery.packageVersion,
        integrationPath,
        detected: discovery.harnesses,
      });
      const harnesses = harnessInstall;
      if (Object.values(harnesses).some((harness) => harness.state === 'partial')) {
        errorOutput.write('Harness integration is incomplete.\n');
        return 1;
      }
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
          yes: parsed.yes,
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
      await verifyIntegrationCandidate({
        discovery,
        install,
        integrationPath,
        runner: options.smokeRunner,
        timeoutMs: options.runtimeTimeoutMs,
      });
      const final = await (options.finalVerify || verifyInstallation)({
        discovery,
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
      await options.beforeActivate?.({ install, metadataPath, integrationPath });
      await publishJson({ filePath: metadataPath, value: install });
      activated = true;
      stage(output, 'Loam is ready');
      return 0;
    } finally {
      if (!activated) {
        try {
          await harnessInstall?.rollback?.();
        } finally {
          if (candidateIntegration) await rm(candidateIntegration.root, { recursive: true, force: true });
        }
      }
    }
  });
}
