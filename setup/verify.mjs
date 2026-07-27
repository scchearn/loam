import { readFile, stat } from 'node:fs/promises';
import { spawn } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';

import { readInstallMetadata, readRequiredVersion, readSkillContent } from '../integration/metadata.mjs';
import { resolveExclusions } from '../integration/ingest.mjs';
import { checkReadiness, probeState } from '../integration/runtime.mjs';
import { verifyGlobalSkills } from './skills.mjs';

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

async function executeCodexAdapter(assetPath, workspace, integrationPath) {
  return new Promise((resolvePromise) => {
    const child = spawn(process.execPath, [assetPath], {
      stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true,
      env: { ...process.env, LOAM_INGEST_BACKGROUND: '0', ...(integrationPath ? { LOAM_INTEGRATION_PATH: integrationPath } : {}) },
    });
    let stdout = '';
    let stderr = '';
    child.stdout.on('data', (chunk) => { stdout += chunk.toString().slice(0, 2048 - stdout.length); });
    child.stderr.on('data', (chunk) => { stderr += chunk.toString().slice(0, 2048 - stderr.length); });
    const timer = setTimeout(() => { child.kill(); resolvePromise({ code: null, stdout, stderr, error: new Error('adapter verification timed out') }); }, 5000);
    child.once('error', (error) => { clearTimeout(timer); resolvePromise({ code: null, stdout, stderr, error }); });
    child.once('close', (code) => { clearTimeout(timer); resolvePromise({ code, stdout, stderr }); });
    child.stdin.end(JSON.stringify({ cwd: workspace, session_id: 'verify', stop_hook_active: false }));
  });
}

async function verifyCodexSessionEnvelope(assetPath, workspace) {
  const module = await import(`${pathToFileURL(assetPath).href}?verify-session=${randomUUID()}`);
  const context = '<LOAM_IMPORTANT>\nverification context\n</LOAM_IMPORTANT>';
  const result = await module.handleMarketplaceHook(
    { cwd: workspace },
    { harness: 'codex', getContext: async () => context },
  );
  return result?.hookSpecificOutput?.hookEventName === 'SessionStart'
    && result.hookSpecificOutput.additionalContext === context;
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
  if (id === 'claude') {
    const result = await module.handleClaudeHook({ cwd: workspace }, { getContext: async () => context });
    const hadPath = Object.hasOwn(process.env, 'LOAM_INTEGRATION_PATH');
    const previousPath = process.env.LOAM_INTEGRATION_PATH;
    if (integrationPath) process.env.LOAM_INTEGRATION_PATH = integrationPath;
    let stop;
    try {
      stop = await import(`${pathToFileURL(join(assetPath, '..', 'claude-stop.mjs')).href}?verify-stop=${randomUUID()}`);
    } finally {
      if (hadPath) process.env.LOAM_INTEGRATION_PATH = previousPath;
      else delete process.env.LOAM_INTEGRATION_PATH;
    }
    const stopped = await stop.main({ env: { ...process.env, LOAM_INGEST_BACKGROUND: '0', ...(globalRoot ? { LOAM_INGEST_GLOBAL_ROOT: globalRoot } : {}) }, payload: { cwd: workspace } });
    return result?.hookSpecificOutput?.hookEventName === 'SessionStart'
      && result.hookSpecificOutput.additionalContext === context
      && stopped?.reason === 'disabled';
  }
  if (id === 'codex') {
    const result = await executeCodexAdapter(assetPath, workspace, integrationPath);
    return result.code === 0 && result.stdout === '{}';
  }
  const result = await module.handleCursorHook({ cwd: workspace }, { getContext: async () => context });
  return result?.additional_context === context;
}

async function verifyHarness(id, harness, { packageRoot, globalRoot, install, workspace }) {
  if (harness.state === 'absent') return { ...harness, ready: true };
  if (!install) return { ...harness, ready: false, category: 'install_metadata_missing' };
  const assetRoot = install.adapter_root;
  const assetName = id === 'opencode' ? 'opencode.mjs' : `${id}-session-start.mjs`;
  const assetPath = join(assetRoot, id === 'codex' ? 'codex-stop.mjs' : assetName);
  const sessionAssetPath = join(assetRoot, assetName);
  try {
    if (id === 'opencode') {
      const stablePath = join(harness.root, 'plugins', 'loam.mjs');
      const [actual, expected] = await Promise.all([
        readFile(stablePath, 'utf8'),
        readFile(join(packageRoot, 'adapters', 'opencode.mjs'), 'utf8'),
      ]);
      if (actual !== expected) return { ...harness, ready: false, category: 'registration_mismatch' };
    } else if (id === 'codex') {
      const config = JSON.parse(await readFile(join(harness.root, 'hooks.json'), 'utf8'));
      const stopCommands = Array.isArray(config.hooks?.Stop) ? config.hooks.Stop : [];
      const ownedStop = stopCommands.filter((entry) => ownsCommand(entry, assetPath));
      if (ownedStop.length !== 1) return { ...harness, ready: false, category: ownedStop.length ? 'registration_duplicate' : 'registration_missing' };
      const sessionCommands = Array.isArray(config.hooks?.SessionStart) ? config.hooks.SessionStart : [];
      const ownedSession = sessionCommands.filter((entry) => ownsCommand(entry, sessionAssetPath));
      const expectedSessionCount = harness.marketplaceOwned ? 0 : 1;
      if (ownedSession.length !== expectedSessionCount) {
        return { ...harness, ready: false, category: ownedSession.length ? 'registration_duplicate' : 'registration_missing' };
      }
    } else {
      const configPath = id === 'claude' ? join(harness.root, 'settings.json') : join(harness.root, 'hooks.json');
      const config = JSON.parse(await readFile(configPath, 'utf8'));
      const commands = id === 'claude'
        ? (Array.isArray(config.hooks?.SessionStart) ? config.hooks.SessionStart : []).flatMap((entry) => Array.isArray(entry?.hooks) ? entry.hooks : [])
        : (Array.isArray(config.hooks?.sessionStart) ? config.hooks.sessionStart : []);
      const owned = commands.filter((entry) => ownsCommand(entry, assetPath));
      const expectedSessionCount = id === 'claude' && harness.marketplaceOwned ? 0 : 1;
      if (owned.length !== expectedSessionCount) {
        return {
          ...harness,
          ready: false,
          category: owned.length ? 'registration_duplicate' : 'registration_missing',
        };
      }
      if (id === 'claude') {
        const stopPath = join(assetRoot, 'claude-stop.mjs');
        const stopHooks = (Array.isArray(config.hooks?.Stop) ? config.hooks.Stop : [])
          .flatMap((entry) => Array.isArray(entry?.hooks) ? entry.hooks : []);
        if (stopHooks.filter((entry) => ownsCommand(entry, stopPath)).length !== 1) {
          return { ...harness, ready: false, category: 'registration_missing' };
        }
      }
    }

    if (!(await fileExists(assetPath)) || !(await verifyAdapterEnvelope(id, assetPath, workspace, install.integration_path, globalRoot))) {
      return { ...harness, ready: false, category: 'adapter_envelope_invalid' };
    }
    if (id === 'codex' && (!(await fileExists(sessionAssetPath)) || !(await verifyCodexSessionEnvelope(sessionAssetPath, workspace)))) {
      return { ...harness, ready: false, category: 'adapter_envelope_invalid' };
    }
    return { ...harness, ready: true, owner: harness.marketplaceOwned ? 'marketplace' : 'setup' };
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
