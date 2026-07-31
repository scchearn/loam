import { readFile, stat } from 'node:fs/promises';
import { randomUUID } from 'node:crypto';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';

import { readInstallMetadata, readRequiredVersion, readSkillContent } from '../integration/metadata.mjs';
import { resolveExclusions } from '../integration/ingest.mjs';
import { checkReadiness, probeState } from '../integration/runtime.mjs';
import { verifyGlobalSkills } from './skills.mjs';
import { isOwnedCommand } from './harnesses.mjs';

async function fileExists(path) {
  try {
    return (await stat(path)).isFile();
  } catch {
    return false;
  }
}

async function localSkills(skillsRoot) {
  try {
    const requiredVersion = await readRequiredVersion({ skillsRoot });
    await readSkillContent({ skillsRoot });
    return { ready: true, requiredVersion };
  } catch (error) {
    return { ready: false, category: 'skills_missing', detail: error instanceof Error ? error.message : String(error) };
  }
}

function ownsCommand(entry, path) {
  if (Array.isArray(entry?.hooks)) return entry.hooks.some((hook) => ownsCommand(hook, path));
  if (entry?.type !== 'command') return false;
  const candidates = [];
  if (entry.command === 'node' && Array.isArray(entry.args) && entry.args.length === 1) candidates.push(entry.args[0]);
  for (const value of [entry.command, entry.commandWindows]) {
    if (typeof value !== 'string' || !value.startsWith('node ')) continue;
    const raw = value.slice(5).trim();
    try { candidates.push(JSON.parse(raw)); } catch { candidates.push(raw); }
  }
  return candidates.some((candidate) => candidate === path || candidate === path.replaceAll('/', '\\'));
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
    const adapter = await module.createOpenCodeAdapter({ getContext: async () => context })({ directory: workspace });
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

async function verifyHarness(id, harness, { packageRoot, globalRoot, install, workspace }) {
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
  const assetRoot = install.adapter_root;
  const assetName = id === 'opencode' ? 'opencode.mjs' : `${id}-session-start.mjs`;
  const assetPath = join(assetRoot, id === 'codex' ? 'codex-stop.mjs' : assetName);
  const sessionAssetPath = join(assetRoot, assetName);
  try {
    if (id === 'claude' || id === 'codex') {
      if (!harness.marketplaceReady) return { ...harness, ready: false, category: 'plugin_incomplete' };
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
      return { ...harness, ready: true, owner: 'marketplace' };
    }
    if (id === 'opencode') {
      const stablePath = join(harness.root, 'plugins', 'loam.js');
      const [actual, expected] = await Promise.all([
        readFile(stablePath, 'utf8'),
        readFile(join(packageRoot, 'adapters', 'opencode.mjs'), 'utf8'),
      ]);
      if (actual !== expected) return { ...harness, ready: false, category: 'registration_mismatch' };
    } else {
      const configPath = id === 'claude' ? join(harness.root, 'settings.json') : join(harness.root, 'hooks.json');
      const config = JSON.parse(await readFile(configPath, 'utf8'));
      const commands = id === 'claude'
        ? (Array.isArray(config.hooks?.SessionStart) ? config.hooks.SessionStart : []).flatMap((entry) => Array.isArray(entry?.hooks) ? entry.hooks : [])
        : (Array.isArray(config.hooks?.sessionStart) ? config.hooks.sessionStart : []);
      const owned = commands.filter((entry) => ownsCommand(entry, assetPath));
      const expectedSessionCount = 1;
      if (owned.length !== expectedSessionCount) {
        return {
          ...harness,
          ready: false,
          category: owned.length ? 'registration_duplicate' : 'registration_missing',
        };
      }
    }

    if (!(await fileExists(assetPath)) || !(await verifyAdapterEnvelope(id, assetPath, workspace, install.integration_path, globalRoot))) {
      return { ...harness, ready: false, category: 'adapter_envelope_invalid' };
    }
    return { ...harness, ready: true, owner: 'setup' };
  } catch (error) {
    return { ...harness, ready: false, category: 'adapter_envelope_invalid', detail: error instanceof Error ? error.message : String(error) };
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
    : await verifyGlobalSkills({ packageRoot, skillsRoot: discovery.skillsRoot, runner });
  let runtime = install
    ? await checkReadiness({
        globalRoot: discovery.globalRoot,
        skillsRoot: discovery.skillsRoot,
        target: discovery.target,
        platform: discovery.platform,
        arch: discovery.arch,
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
    });
  }
  const migration = legacy || discovery.legacy;
  const harnessReady = Object.values(harnesses).every((harness) => harness.ready);
  const pluginVersionReady = install?.plugin_version === discovery.packageVersion;
  const ingestExclusions = await verifyIngestExclusions(discovery.skillsRoot);
  return {
    ready: Boolean(pluginVersionReady && skills.ready && runtime.ready && harnessReady && migration.ready && ingestExclusions.ready),
    install,
    skills,
    runtime,
    harnesses,
    ingestExclusions,
    migration,
    native: { ready: runtime.ready },
  };
}
