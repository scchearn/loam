import { readFile, rm } from 'node:fs/promises';
import { join } from 'node:path';

import { runCommand } from './process.mjs';

const installs = Object.freeze({
  claude: [
    ['plugin', 'marketplace', 'add', 'scchearn/loam'],
    ['plugin', 'install', 'loam@loam', '--scope', 'user'],
  ],
  codex: [
    ['plugin', 'marketplace', 'add', 'scchearn/loam'],
    ['plugin', 'add', 'loam@loam'],
  ],
});

const removals = Object.freeze({
  claude: ['plugin', 'uninstall', 'loam@loam', '--scope', 'user', '--yes'],
  codex: ['plugin', 'remove', 'loam@loam'],
});

const marketplaceRemovals = Object.freeze({
  claude: ['plugin', 'marketplace', 'remove', 'loam'],
  codex: ['plugin', 'marketplace', 'remove', 'loam'],
});

const updates = Object.freeze({
  claude: [
    ['plugin', 'update', 'loam@loam', '--scope', 'user'],
    ['plugin', 'enable', 'loam@loam', '--scope', 'user'],
  ],
  codex: [
    ['plugin', 'marketplace', 'upgrade', 'loam'],
    ['plugin', 'add', 'loam@loam'],
  ],
});

// Claude needs a registry preflight because its CLI treats stale cache bytes as
// absent. Codex's native remove is idempotent across the same stale state, so it
// remains the explicit goal-state check below.

// Claude's uninstall CLI trusts installed_plugins.json, not cache bytes. A cache
// without a user-scoped registry entry is therefore already at the requested
// uninstall goal, and invoking the CLI turns that harmless drift into a failure.
async function claudeRegistryState(root, readRegistry) {
  if (typeof root !== 'string' || !root) return null;
  try {
    const registry = JSON.parse(await readRegistry(join(root, 'plugins', 'installed_plugins.json'), 'utf8'));
    const plugins = registry?.plugins;
    if (plugins != null && (typeof plugins !== 'object' || Array.isArray(plugins))) return null;
    const entries = plugins?.['loam@loam'];
    if (entries !== undefined && !Array.isArray(entries)) return null;
    const installs = entries || [];
    return {
      hasUserInstall: installs.some((entry) => entry?.scope === 'user'),
      hasAnyInstall: installs.length > 0,
    };
  } catch (error) {
    // A missing registry is the orphan-cache state. Other read/parse failures
    // leave the goal unknown, so native removal still gets a chance to report it.
    return error?.code === 'ENOENT' ? { hasUserInstall: false, hasAnyInstall: false } : null;
  }
}

async function removeClaudeOrphanCache(root, removePath) {
  await removePath(join(root, 'plugins', 'cache', 'loam'), { recursive: true, force: true });
}

function goalAlreadyReached(run) {
  if (run.ok) return true;
  const detail = `${run.stderr || ''}\n${run.stdout || ''}`;
  return /\b(?:not found|does not exist|no such (?:plugin|marketplace|source)|no (?:plugin|marketplace|source)|unknown (?:plugin|marketplace)|already (?:removed|gone|uninstalled|absent)|not (?:installed|configured|registered)|cannot find (?:plugin|marketplace|source))\b/i.test(detail);
}

function removalResult(run, action) {
  return {
    state: goalAlreadyReached(run) ? 'removed' : 'partial',
    action: goalAlreadyReached(run) ? (run.ok ? action : 'already-absent') : 'failed',
    ...(run.stderr || run.stdout ? { detail: run.stderr || run.stdout } : {}),
  };
}

export async function installMarketplacePlugins({
  selected = [],
  harnesses = {},
  refresh = false,
  cwd = process.cwd(),
  runner,
} = {}) {
  const result = {};
  for (const id of selected) {
    const harness = harnesses[id];
    if (!harness || !installs[id]) continue;
    if (harness.marketplaceReady && !refresh) {
      result[id] = { ...harness, state: 'ready', action: 'existing' };
      continue;
    }
    const commands = harness.marketplaceInstalled ? updates[id] : installs[id];
    let failure;
    for (const args of commands) {
      const run = await runCommand({ command: id, args, cwd, runner });
      if (!run.ok) {
        failure = run;
        break;
      }
    }
    result[id] = failure
      ? { ...harness, state: 'partial', action: 'failed', detail: failure.stderr || failure.category }
      : {
          ...harness,
          state: 'ready',
          action: harness.marketplaceInstalled ? 'updated' : 'installed',
          marketplaceOwned: true,
          marketplaceInstalled: true,
          marketplaceReady: true,
        };
  }
  return result;
}

export async function removeMarketplacePlugins({
  harnesses = {},
  cwd = process.cwd(),
  runner,
  readRegistry = readFile,
  removePath = rm,
} = {}) {
  const result = {};
  for (const id of ['claude', 'codex']) {
    const harness = harnesses[id];
    const hasPluginState = Boolean(harness?.marketplaceInstalled || harness?.marketplaceConfigured);
    const hasMarketplaceRegistration = harness?.marketplaceRegistered === true;
    if (!hasPluginState && !hasMarketplaceRegistration) continue;

    let pluginRemoval;
    let pluginRemovalInvoked = false;

    if (id === 'claude') {
      const registry = await claudeRegistryState(harness.root, readRegistry);
      if (registry && !registry.hasUserInstall) {
        try {
          // Only a registry with no loam entries can prove this cache is orphaned;
          // a project-scoped entry may still own the shared cache.
          if (!registry.hasAnyInstall) await removeClaudeOrphanCache(harness.root, removePath);
          pluginRemoval = { state: 'removed', action: 'already-absent' };
        } catch (error) {
          result[id] = { ...harness, state: 'partial', detail: error instanceof Error ? error.message : String(error) };
          continue;
        }
      } else if (hasPluginState) {
        pluginRemovalInvoked = true;
        const run = await runCommand({ command: id, args: removals[id], cwd, runner });
        pluginRemoval = removalResult(run, 'removed');
        if (pluginRemoval.state === 'partial') {
          result[id] = { ...harness, state: 'partial', detail: pluginRemoval.detail || run.category };
          continue;
        }
      }
    } else if (hasPluginState) {
      pluginRemovalInvoked = true;
      const run = await runCommand({ command: id, args: removals[id], cwd, runner });
      pluginRemoval = removalResult(run, 'removed');
      if (pluginRemoval.state === 'partial') {
        result[id] = { ...harness, state: 'partial', detail: pluginRemoval.detail || run.category };
        continue;
      }
    }

    // Plugin removal can leave the source registration behind. Remove only the
    // named Loam marketplace, and treat a missing registration as success.
    if (hasMarketplaceRegistration || pluginRemovalInvoked) {
      const run = await runCommand({ command: id, args: marketplaceRemovals[id], cwd, runner });
      const marketplaceRemoval = removalResult(run, 'removed');
      if (marketplaceRemoval.state === 'partial') {
        result[id] = {
          ...harness,
          state: 'removed',
          pluginRemoval,
          marketplaceRemoval,
          warnings: [{ kind: 'marketplace_registration', detail: marketplaceRemoval.detail || run.category || 'unknown failure' }],
        };
        continue;
      }
      result[id] = { ...harness, state: 'removed', pluginRemoval, marketplaceRemoval };
      continue;
    }

    result[id] = { ...harness, state: 'removed', pluginRemoval };
  }
  return result;
}
