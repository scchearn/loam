import { readFile, stat } from 'node:fs/promises';
import { randomUUID } from 'node:crypto';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';

import { readInstallMetadata, readRequiredVersion, readSkillContent } from '../integration/metadata.mjs';
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

function hookCommand(path) {
  return `node ${JSON.stringify(path)}`;
}

async function verifyAdapterEnvelope(id, assetPath, workspace) {
  const module = await import(`${pathToFileURL(assetPath).href}?verify=${randomUUID()}`);
  const context = '<LOAM_IMPORTANT>\nverification context\n</LOAM_IMPORTANT>';
  if (id === 'opencode') {
    const adapter = await module.createOpenCodeAdapter({ getContext: async () => context })({ directory: workspace });
    const output = { messages: [{ info: { role: 'user' }, parts: [{ type: 'text', text: 'prompt' }] }] };
    await adapter['experimental.chat.messages.transform']({}, output);
    return output.messages[0].parts.filter((part) => part.type === 'text' && part.text === context).length === 1;
  }
  if (id === 'claude') {
    const result = await module.handleClaudeHook({ cwd: workspace }, { getContext: async () => context });
    return result?.hookSpecificOutput?.hookEventName === 'SessionStart' && result.hookSpecificOutput.additionalContext === context;
  }
  const result = await module.handleCursorHook({ cwd: workspace }, { getContext: async () => context });
  return result?.additional_context === context;
}

async function verifyHarness(id, harness, { packageRoot, globalRoot, install, workspace }) {
  if (harness.state === 'absent') return { ...harness, ready: true };
  if (!install) return { ...harness, ready: false, category: 'install_metadata_missing' };
  const assetRoot = install.adapter_root;
  const assetName = id === 'opencode' ? 'opencode.mjs' : `${id}-session-start.mjs`;
  const assetPath = join(assetRoot, assetName);
  try {
    if (id === 'opencode') {
      const stablePath = join(harness.root, 'plugins', 'loam.mjs');
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
      const owned = commands.filter((entry) => entry?.type === 'command' && entry.command === hookCommand(assetPath));
      if (owned.length !== 1) {
        return {
          ...harness,
          ready: false,
          category: owned.length ? 'registration_duplicate' : 'registration_missing',
        };
      }
    }

    if (!(await fileExists(assetPath)) || !(await verifyAdapterEnvelope(id, assetPath, workspace))) {
      return { ...harness, ready: false, category: 'adapter_envelope_invalid' };
    }
    return { ...harness, ready: true };
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
  return {
    ready: Boolean(install && skills.ready && runtime.ready && harnessReady && migration.ready),
    install,
    skills,
    runtime,
    harnesses,
    migration,
    native: { ready: runtime.ready },
  };
}
