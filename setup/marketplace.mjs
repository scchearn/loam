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
} = {}) {
  const result = {};
  for (const id of ['claude', 'codex']) {
    const harness = harnesses[id];
    if (!harness?.marketplaceInstalled && !harness?.marketplaceConfigured) continue;
    const run = await runCommand({ command: id, args: removals[id], cwd, runner });
    result[id] = run.ok
      ? { ...harness, state: 'removed' }
      : { ...harness, state: 'partial', detail: run.stderr || run.category };
  }
  return result;
}
