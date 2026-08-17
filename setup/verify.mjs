import { readFile, stat } from 'node:fs/promises';
import { randomUUID } from 'node:crypto';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';

import { readInstallMetadata, readSkillContent } from '../integration/metadata.mjs';
import { resolveExclusions } from '../integration/ingest.mjs';
import { checkReadiness, probeState } from '../integration/runtime.mjs';
import { resolveRuntimePath } from '../integration/ledger.mjs';
import { verifyGlobalSkills, skillsAgentsFor } from './skills.mjs';
import { isOwnedCommand, renderOpenCodePlugin } from './harnesses.mjs';
import { verifyFederationService } from './federation.mjs';

async function fileExists(path) {
  try {
    return (await stat(path)).isFile();
  } catch {
    return false;
  }
}

async function localSkills(skillsRoot) {
  try {
    // Presence-only: a readable `loam-using/SKILL.md` proves the global skills
    // are installed; the skills tree no longer carries a runtime version.
    await readSkillContent({ skillsRoot });
    return { ready: true };
  } catch (error) {
    return { ready: false, category: 'skills_missing', detail: error instanceof Error ? error.message : String(error) };
  }
}

async function verifyIngestExclusions(skillsRoot) {
  try { return { ready: true, path: await resolveExclusions(skillsRoot) }; }
  catch (error) { return { ready: false, category: error.reason || 'exclusions_unavailable' }; }
}

async function verifyAdapterEnvelope(id, assetPath, workspace, integrationPath, globalRoot) {
  const hadIntegrationPath = Object.hasOwn(process.env, 'LOAM_INTEGRATION_PATH');
  const previousIntegrationPath = process.env.LOAM_INTEGRATION_PATH;
  if (integrationPath) process.env.LOAM_INTEGRATION_PATH = integrationPath;
  let module;
  try {
    module = await import(`${pathToFileURL(assetPath).href}?verify=${randomUUID()}`);
  } finally {
    if (hadIntegrationPath) process.env.LOAM_INTEGRATION_PATH = previousIntegrationPath;
    else delete process.env.LOAM_INTEGRATION_PATH;
  }
  const context = '<LOAM_IMPORTANT>\nverification context\n</LOAM_IMPORTANT>';
  if (id === 'opencode') {
    // The plugin file exports only LoamPlugin (OpenCode's loader calls every
    // top-level export as a plugin factory); the adapter factory rides on it.
    const adapter = await module.LoamPlugin.createOpenCodeAdapter({
      getContext: async () => context,
      // The envelope check fires the transform, which starts the notify
      // listener on its first fire; a real listener would keep the process
      // alive, so verification uses a stub.
      wakeServer: async () => ({ wakeRef: 'notify-tcp://127.0.0.1:0', registered: false, close: async () => {} }),
    })({ directory: workspace });
    const output = { messages: [{ info: { role: 'user' }, parts: [{ type: 'text', text: 'prompt' }] }] };
    await adapter['experimental.chat.messages.transform']({}, output);
    const previous = process.env.LOAM_INGEST_BACKGROUND;
    const previousGlobal = process.env.LOAM_INGEST_GLOBAL_ROOT;
    process.env.LOAM_INGEST_BACKGROUND = '0';
    if (globalRoot) process.env.LOAM_INGEST_GLOBAL_ROOT = globalRoot;
    try {
      await adapter.event({ event: { type: 'session.updated', sessionID: 'verify' } });
      await adapter.event({ event: { type: 'session.idle', sessionID: 'verify' } });
    } finally {
      if (previous === undefined) delete process.env.LOAM_INGEST_BACKGROUND;
      else process.env.LOAM_INGEST_BACKGROUND = previous;
      if (globalRoot) {
        if (previousGlobal === undefined) delete process.env.LOAM_INGEST_GLOBAL_ROOT;
        else process.env.LOAM_INGEST_GLOBAL_ROOT = previousGlobal;
      }
    }
    return output.messages[0].parts.filter((part) => part.type === 'text' && part.text === context).length === 1
      && typeof adapter.event === 'function';
  }
  const result = await module.handleCursorHook({ cwd: workspace }, { getContext: async () => context });
  return result?.additional_context === context;
}

// One registration is correct only when it runs the staged private runtime as a
// `hook <id>` read. Anything else — a Node shim, a stale asset path, a runtime
// from a previous install — is a mismatch, not a variant.
function nativeHookCommands(entries, runtimePath, harnessId) {
  return entries
    .flatMap((entry) => (Array.isArray(entry?.hooks) ? entry.hooks : [entry]))
    .filter((entry) => entry?.type === 'command'
      && entry.command === runtimePath
      && Array.isArray(entry.args)
      && entry.args[0] === 'hook'
      && entry.args[1] === harnessId);
}

async function verifyHarness(id, harness, { packageRoot, globalRoot, install, workspace, home, platform }) {
  if (harness.state === 'absent') return { ...harness, ready: true };
  if (id === 'codex') {
    const profilePath = join(harness.root, 'agents', 'loam_ingestor.toml');
    try {
      const [actual, expected] = await Promise.all([
        readFile(profilePath, 'utf8'),
        readFile(join(packageRoot, 'adapters', 'loam_ingestor.toml'), 'utf8'),
      ]);
      if (actual !== expected) return { ...harness, ready: false, category: 'agent_profile_mismatch' };
    } catch (error) {
      return {
        ...harness,
        ready: false,
        category: error?.code === 'ENOENT' ? 'agent_profile_missing' : 'agent_profile_invalid',
        detail: error instanceof Error ? error.message : String(error),
      };
    }
    if (harness.state === 'skipped') return { ...harness, ready: true, owner: 'setup' };
  } else if (harness.state === 'skipped') return { ...harness, ready: true };
  if (!install) return { ...harness, ready: false, category: 'install_metadata_missing' };
  // The injected hook command runs the config-dir store binary; compare against
  // the ledger's store_path (schema-1 install.json runtime_path as migration
  // fallback), never a dropped schema-2 field.
  const runtimePath = (await resolveRuntimePath({ globalRoot, home, platform })) ?? install.runtime_path;
  try {
    if (id === 'claude' || id === 'codex') {
      if (!harness.marketplaceReady || !harness.marketplaceRoot) return { ...harness, ready: false, category: 'plugin_incomplete' };
      const configPath = id === 'claude' ? join(harness.root, 'settings.json') : join(harness.root, 'hooks.json');
      let config = {};
      try { config = JSON.parse(await readFile(configPath, 'utf8')); }
      catch (error) { if (error?.code !== 'ENOENT') throw error; }
      const session = Array.isArray(config.hooks?.SessionStart) ? config.hooks.SessionStart : [];
      const stop = Array.isArray(config.hooks?.Stop) ? config.hooks.Stop : [];
      if (session.some((entry) => isOwnedCommand(entry, globalRoot, `${id}-session-start.mjs`))
        || stop.some((entry) => isOwnedCommand(entry, globalRoot, `${id}-stop.mjs`))) {
        return { ...harness, ready: false, category: 'registration_duplicate' };
      }
      const plugin = JSON.parse(await readFile(join(harness.marketplaceRoot, 'hooks', 'hooks.json'), 'utf8'));
      const start = nativeHookCommands(Array.isArray(plugin.hooks?.SessionStart) ? plugin.hooks.SessionStart : [], runtimePath, id);
      const refresh = nativeHookCommands(Array.isArray(plugin.hooks?.UserPromptSubmit) ? plugin.hooks.UserPromptSubmit : [], runtimePath, id);
      const preTool = nativeHookCommands(Array.isArray(plugin.hooks?.PreToolUse) ? plugin.hooks.PreToolUse : [], runtimePath, id);
      const postTool = nativeHookCommands(Array.isArray(plugin.hooks?.PostToolUse) ? plugin.hooks.PostToolUse : [], runtimePath, id);
      if (start.length !== 1 || refresh.length !== 1 || preTool.length !== 1 || postTool.length !== 1) {
        return { ...harness, ready: false, category: start.length || refresh.length || preTool.length || postTool.length ? 'registration_duplicate' : 'registration_missing' };
      }
      return { ...harness, ready: true, owner: 'marketplace' };
    }
    if (id === 'opencode') {
      const stablePath = join(harness.root, 'plugins', 'loam.js');
      const [actual, source] = await Promise.all([
        readFile(stablePath, 'utf8'),
        readFile(join(packageRoot, 'adapters', 'opencode.mjs'), 'utf8'),
      ]);
      if (actual !== renderOpenCodePlugin(source, runtimePath)) {
        return { ...harness, ready: false, category: 'registration_mismatch' };
      }
      const assetPath = join(install.adapter_root, 'opencode.mjs');
      if (!(await fileExists(assetPath)) || !(await verifyAdapterEnvelope(id, assetPath, workspace, install.integration_path, globalRoot))) {
        return { ...harness, ready: false, category: 'adapter_envelope_invalid' };
      }
      return { ...harness, ready: true, owner: 'setup' };
    }
    const config = JSON.parse(await readFile(join(harness.root, 'hooks.json'), 'utf8'));
    const owned = nativeHookCommands(
      Array.isArray(config.hooks?.sessionStart) ? config.hooks.sessionStart : [],
      runtimePath,
      id,
    );
    if (owned.length !== 1) {
      return { ...harness, ready: false, category: owned.length ? 'registration_duplicate' : 'registration_missing' };
    }
    return { ...harness, ready: true, owner: 'setup' };
  } catch (error) {
    return { ...harness, ready: false, category: 'adapter_envelope_invalid', detail: error instanceof Error ? error.message : String(error) };
  }
}

async function verifyHarvesterAgent(packageRoot, install, installPluginVersion) {
  const packageAgent = join(packageRoot, 'plugins', 'loam-adapter', 'agents', 'harvester.md');
  try {
    if (!(await fileExists(packageAgent))) return { ready: false, category: 'harvester_agent_missing' };
    if (!install) return { ready: true, source: 'package' };
    const version = installPluginVersion || install.plugin_version;
    const cacheAgent = join(install.adapter_root, '..', '..', 'plugins', 'loam-adapter', 'agents', 'harvester.md');
    try {
      const [expected, actual] = await Promise.all([
        readFile(packageAgent, 'utf8'),
        readFile(cacheAgent, 'utf8'),
      ]);
      return { ready: actual === expected, source: 'installed', category: actual === expected ? null : 'harvester_agent_mismatch' };
    } catch {
      return { ready: true, source: 'package' };
    }
  } catch {
    return { ready: false, category: 'harvester_agent_missing' };
  }
}

export async function verifyInstallation({
  discovery,
  packageRoot,
  runner,
  legacy,
  install: suppliedInstall,
  runtimeRunner,
  runtimeTimeoutMs,
  federationRunner,
} = {}) {
  let install = suppliedInstall;
  if (!install) {
    try {
      install = await readInstallMetadata(discovery.globalRoot);
    } catch {
      install = null;
    }
  }

  const skills = install
    ? await localSkills(discovery.skillsRoot)
    : await verifyGlobalSkills({ packageRoot, skillsRoot: discovery.skillsRoot, runner, agents: skillsAgentsFor(discovery.harnesses) });
  let runtime = install
    ? await checkReadiness({
        globalRoot: discovery.globalRoot,
        skillsRoot: discovery.skillsRoot,
        target: discovery.target,
        platform: discovery.platform,
        arch: discovery.arch,
        home: discovery.home,
        install,
      })
    : { ready: false, category: 'install_metadata_missing' };
  if (runtime.ready) {
    runtime = await probeState({
      readiness: runtime,
      workspace: discovery.workspace,
      runner: runtimeRunner,
      timeoutMs: runtimeTimeoutMs,
    });
  }
  const harnesses = {};
  for (const [id, harness] of Object.entries(discovery.harnesses)) {
    harnesses[id] = await verifyHarness(id, harness, {
      packageRoot,
      globalRoot: discovery.globalRoot,
      install,
      workspace: discovery.workspace,
      home: discovery.home,
      platform: discovery.platform,
    });
  }
  const migration = legacy || discovery.legacy;
  const harnessReady = Object.values(harnesses).every((harness) => harness.ready);
  const pluginVersionReady = install?.plugin_version === discovery.packageVersion;
  const ingestExclusions = await verifyIngestExclusions(discovery.skillsRoot);
  const harvestAgent = await verifyHarvesterAgent(packageRoot, install, discovery.packageVersion);
  // Opt-in native connector lifecycle check: only runs when a runner is supplied
  // (default callers stay unchanged). Read-only status through the runtime proves
  // the definition is present/inspectable and references the trusted runtime
  // without starting the connector or contacting a broker.
  let federation = { ready: true, checked: false };
  const federationRuntimePath = federationRunner !== undefined
    ? (await resolveRuntimePath({ globalRoot: discovery.globalRoot, home: discovery.home, platform: discovery.platform })) ?? install?.runtime_path
    : undefined;
  if (federationRunner !== undefined && federationRuntimePath) {
    federation = {
      ...(await verifyFederationService({
        runtimePath: federationRuntimePath,
        globalRoot: discovery.globalRoot,
        runner: federationRunner,
      })),
      checked: true,
    };
  }
  return {
    ready: Boolean(pluginVersionReady && skills.ready && runtime.ready && harnessReady && migration.ready && ingestExclusions.ready && federation.ready),
    install,
    skills,
    runtime,
    harnesses,
    ingestExclusions,
    harvestAgent,
    migration,
    federation,
    native: { ready: runtime.ready, federation: federation.ready },
  };
}
