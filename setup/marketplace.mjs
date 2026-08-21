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
    if (!harness?.marketplaceInstalled && !harness?.marketplaceConfigured) continue;

    if (id === 'claude') {
      const registry = await claudeRegistryState(harness.root, readRegistry);
      if (registry && !registry.hasUserInstall) {
        try {
          // Only a registry with no loam entries can prove this cache is orphaned;
          // a project-scoped entry may still own the shared cache.
          if (!registry.hasAnyInstall) await removeClaudeOrphanCache(harness.root, removePath);
          result[id] = { ...harness, state: 'removed' };
        } catch (error) {
          result[id] = {
            ...harness,
            state: 'partial',
            detail: error instanceof Error ? error.message : String(error),
          };
        }
        continue;
      }
    }

    const run = await runCommand({ command: id, args: removals[id], cwd, runner });
    result[id] = run.ok
      ? { ...harness, state: 'removed' }
      : { ...harness, state: 'partial', detail: run.stderr || run.category };
  }
  return result;
}
