import { readdir, rm, stat } from 'node:fs/promises';
import { join } from 'node:path';

import { MCP_HARNESSES, detectMcpEntry, registerMcpEntry, deregisterMcpEntry } from './mcp.mjs';
import { installNodeTool, resolveTool, removeManagedTool, managedBinPath } from './tools.mjs';
import { readLedger, recordIntegration, clearIntegration, ownedRecord } from './ledger.mjs';
import { confirmAction, stepDone, stepDetail, stepSkip } from '../wizard.mjs';

// Shared enable/verify/disable engine for catalog entries. An entry is data:
//   { id, label, capability, egress, mcpName?, tool?, descriptor?, caches? }
//   tool:       { pkg, binName, healthArgs } installable Node tool (e.g. qmd)
//               | { binName, healthArgs, dirs, install } DETECTION-ONLY (e.g. hcom)
//               | undefined (remote-only, e.g. grep)
//   descriptor: object (remote) | (toolPath) => object (local, e.g. qmd)
//   caches:     [{ label, path: (home) => absPath }]  large derived artifacts
//
// Two independent lanes, each switched by the data:
//   MCP lane   — on when `mcpName` is set. Off means loam registers no MCP for
//                this entry, so there is no harness to target, nothing to
//                deregister, and doctor reports it as not applicable.
//   tool lane  — `tool.pkg` means loam may install it into the managed prefix;
//                a tool WITHOUT `pkg` is detection-only: loam never installs it
//                (its real installers are brew/OS scripts/PyPI, outside the npm
//                lane and outside setup's egress consent), so enable either
//                finds it or refuses with the install recipe.
//
// grep, qmd and hcom are thin wrappers over this engine (catalog.mjs).

// Which harnesses to register into: the ones loam actually configured, filtered
// to the four MCP-capable harnesses; falls back to every detected non-absent
// harness when configured_harnesses is empty.
function targetHarnesses(ctx) {
  const configured = (ctx.install?.configured_harnesses || []).filter((id) => MCP_HARNESSES.includes(id));
  if (configured.length) return configured;
  return MCP_HARNESSES.filter((id) => ctx.discovery?.harnesses?.[id] && ctx.discovery.harnesses[id].state !== 'absent');
}

async function dirSize(path) {
  let bytes = 0;
  let files = 0;
  async function walk(dir) {
    let entries;
    try { entries = await readdir(dir, { withFileTypes: true }); }
    catch { return; }
    for (const entry of entries) {
      const full = join(dir, entry.name);
      if (entry.isDirectory()) await walk(full);
      else {
        try { bytes += (await stat(full)).size; files += 1; } catch {}
      }
    }
  }
  try { await stat(path); } catch { return { bytes: 0, files: 0 }; }
  await walk(path);
  return { bytes, files };
}

function humanBytes(bytes) {
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) { value /= 1024; unit += 1; }
  return `${value.toFixed(value < 10 && unit > 0 ? 1 : 0)} ${units[unit]}`;
}

// ---- enable ---------------------------------------------------------------

export async function enableIntegration(entry, ctx) {
  const { discovery, dryRun, output } = ctx;
  const globalRoot = discovery.globalRoot;
  const home = discovery.home;
  const usesMcp = Boolean(entry.mcpName);
  // An entry with no MCP lane needs no harness, so the missing-harness refusal
  // must not fire on it — it would refuse an entry that has nothing to register.
  const harnesses = usesMcp ? targetHarnesses(ctx) : [];
  if (usesMcp && !harnesses.length) {
    return { ready: false, category: 'no_target_harness', detail: 'no configured harness to register the MCP into' };
  }

  // 1) Tool: resolve (managed prefix first, then PATH), install into the managed
  //    prefix only when absent. Install/verify BEFORE any MCP registration.
  let toolPath = null;
  let toolRecord = null;
  if (entry.tool && !entry.tool.pkg) {
    // Detection-only: resolve, never install. Absence is a typed refusal with
    // the per-OS recipes printed — the user installs it, then re-runs enable.
    const resolved = await resolveTool({
      globalRoot,
      id: entry.id,
      binName: entry.tool.binName,
      healthArgs: entry.tool.healthArgs,
      runner: ctx.toolRunner,
      platform: discovery.platform,
      env: ctx.env,
      extraDirs: entry.tool.dirs?.(home, ctx.env || process.env) || [],
    });
    if (!resolved.present) {
      // The reason lands after this block (configure.mjs writes the failure
      // detail), so without a header the user's first sight of the refusal is a
      // wall of install commands with nothing saying why they are there.
      stepDetail(output, `${entry.tool.binName} is not installed — loam detects it but never installs it. Install it with one of these, then re-run:`);
      for (const recipe of entry.tool.install || []) {
        stepDetail(output, `${recipe.label}: ${recipe.command}`);
      }
      return {
        ready: false,
        category: 'tool-not-installed',
        detail: `${entry.tool.binName} is not installed — loam detects it but never installs it; install it with one of the commands above, then re-run this command`,
      };
    }
    toolPath = resolved.path;
    toolRecord = { managed: false, pkg: null, path: resolved.path };
    stepDetail(output, `${entry.tool.binName} found (${resolved.source}) → ${resolved.path}`);
  } else if (entry.tool) {
    if (dryRun) {
      // Preview only — no resolution spawn, no install, no download.
      toolPath = managedBinPath(globalRoot, entry.id, entry.tool.binName, discovery.platform);
      toolRecord = { managed: true, pkg: entry.tool.pkg, path: toolPath };
      stepDetail(output, `would install ${entry.tool.pkg} into the loam-managed prefix (or use a pre-existing ${entry.tool.binName} on PATH)`);
    } else {
      const resolved = await resolveTool({ globalRoot, id: entry.id, binName: entry.tool.binName, healthArgs: entry.tool.healthArgs, runner: ctx.toolRunner, platform: discovery.platform, env: ctx.env });
      if (resolved.present) {
        toolPath = resolved.path;
        toolRecord = { managed: resolved.managed, pkg: entry.tool.pkg, path: resolved.path };
        stepDetail(output, `${entry.tool.binName} ${resolved.managed ? 'already installed (loam-managed)' : 'found on PATH (pre-existing)'} → ${resolved.path}`);
      } else {
        const installed = await installNodeTool({
          pkg: entry.tool.pkg,
          id: entry.id,
          binName: entry.tool.binName,
          healthArgs: entry.tool.healthArgs,
          globalRoot,
          runner: ctx.toolRunner,
          platform: discovery.platform,
        });
        if (!installed.ready) {
          // Roll back only THIS integration's managed prefix and register NO MCP;
          // the caller continues with other integrations.
          await removeManagedTool({ globalRoot, id: entry.id });
          return { ready: false, category: installed.category, detail: installed.detail };
        }
        toolPath = installed.path;
        toolRecord = { managed: true, pkg: entry.tool.pkg, path: installed.path };
        stepDone(output, `${entry.tool.binName} installed and verified → ${installed.path}`);
      }
    }
  }

  const descriptor = typeof entry.descriptor === 'function' ? entry.descriptor(toolPath) : entry.descriptor;

  // 2) Register the MCP per harness, ownership-aware and non-clobbering.
  const ledger = await readLedger(globalRoot);
  const owned = ownedRecord(ledger, entry.id);
  const mcpRecord = {};
  const failures = [];
  if (!usesMcp) stepDetail(output, `${entry.id} registers no MCP server — detection only`);
  for (const harness of harnesses) {
    const isOwned = owned?.mcp?.[harness] === entry.mcpName;
    const detected = await detectMcpEntry({ harness, home, name: entry.mcpName });
    if (detected.present && !isOwned) {
      stepSkip(output, `${harness}: existing ${entry.mcpName} MCP is user-owned — left untouched`);
      continue; // satisfied; never recorded as ours, never overwritten
    }
    if (dryRun) {
      stepDetail(output, `would register ${entry.mcpName} MCP for ${harness}`);
      mcpRecord[harness] = entry.mcpName;
      continue;
    }
    try {
      await registerMcpEntry({ harness, home, name: entry.mcpName, descriptor });
      mcpRecord[harness] = entry.mcpName;
      stepDone(output, `${harness}: ${entry.mcpName} MCP registered`);
    } catch (error) {
      // Policy-owned config fails closed here (mergeJsonConfig throws); record it
      // and keep going for the other harnesses.
      failures.push({ harness, detail: error instanceof Error ? error.message : String(error) });
    }
  }

  if (!dryRun) await recordIntegration(globalRoot, entry.id, { mcp: mcpRecord, tool: toolRecord });
  if (failures.length) {
    return { ready: false, category: 'mcp_register_failed', detail: failures.map((f) => `${f.harness}: ${f.detail}`).join('; '), registered: Object.keys(mcpRecord) };
  }
  return { ready: true, registered: Object.keys(mcpRecord), tool: toolRecord };
}

// ---- verify (doctor; non-failing) ----------------------------------------

export async function verifyIntegration(entry, ctx) {
  const { discovery } = ctx;
  const home = discovery.home;
  const registered = {};
  if (entry.mcpName) {
    for (const harness of targetHarnesses(ctx)) {
      registered[harness] = (await detectMcpEntry({ harness, home, name: entry.mcpName })).present;
    }
  }
  let tool = { present: true, managed: false };
  if (entry.tool) {
    tool = await resolveTool({
      globalRoot: discovery.globalRoot,
      id: entry.id,
      binName: entry.tool.binName,
      healthArgs: entry.tool.healthArgs,
      runner: ctx.toolRunner,
      platform: discovery.platform,
      env: ctx.env,
      extraDirs: entry.tool.dirs?.(home, ctx.env || process.env) || [],
    });
  }
  return { ready: true, id: entry.id, tool, registered };
}

// ---- disable (symmetric, verified, cache-aware) --------------------------

export async function disableIntegration(entry, ctx) {
  const { discovery, dryRun, purge, output } = ctx;
  const globalRoot = discovery.globalRoot;
  const home = discovery.home;
  const ledger = await readLedger(globalRoot);
  const owned = ownedRecord(ledger, entry.id);

  // No ledger record → loam never enabled this integration, so there is nothing
  // of loam's to reverse. Deregistering by name here would delete a user's OWN
  // same-name MCP entry and falsely report a clean disable — exactly what the
  // ledger-as-ownership doctrine forbids. Report a success no-op and leave every
  // config untouched, surfacing any same-name entries as user-owned.
  if (!owned) {
    for (const harness of (entry.mcpName ? targetHarnesses(ctx) : [])) {
      if ((await detectMcpEntry({ harness, home, name: entry.mcpName })).present) {
        stepDetail(output, `${harness}: an existing ${entry.mcpName} MCP is user-owned — left untouched`);
      }
    }
    stepDetail(output, `${entry.id} was not enabled by loam — nothing to disable`);
    return { ready: true, noop: true };
  }

  // Deregister exactly the harnesses loam recorded (never a user-owned entry).
  const harnesses = Object.keys(owned.mcp || {});

  if (dryRun) {
    for (const harness of harnesses) stepDetail(output, `would deregister ${entry.mcpName} MCP from ${harness}`);
    if (owned?.tool?.managed) stepDetail(output, `would remove the loam-managed ${entry.tool?.binName}`);
    for (const cache of entry.caches || []) {
      const size = await dirSize(cache.path(home));
      if (size.bytes > 0) stepDetail(output, `${cache.label}: ${humanBytes(size.bytes)} — ${purge ? 'would remove (--purge)' : 'would keep (default)'}`);
    }
    return { ready: true };
  }

  for (const harness of harnesses) {
    await deregisterMcpEntry({ harness, home, name: entry.mcpName });
    stepDone(output, `${harness}: ${entry.mcpName} MCP deregistered`);
  }

  // Remove the tool only if loam installed it (managed); a pre-existing PATH tool
  // is never touched. For a detection-only entry that is the whole story: loam
  // created nothing but the ledger record, so disable removes nothing but the
  // ledger record — the tool, its config and its live state stay as they are.
  if (owned?.tool?.managed) {
    await removeManagedTool({ globalRoot, id: entry.id });
    stepDone(output, `removed the loam-managed ${entry.tool?.binName || 'tool'}`);
  } else if (entry.tool && !entry.tool.pkg) {
    stepDetail(output, `${entry.tool.binName} was detected, never installed by loam — leaving it and its data untouched`);
  }

  // Large derived caches: offered with size shown, default KEEP (expensive to
  // recreate), --purge removes. Interactive confirm when a TTY is available.
  const caches = [];
  for (const cache of entry.caches || []) {
    const cachePath = cache.path(home);
    const size = await dirSize(cachePath);
    if (size.bytes === 0) continue;
    let remove = purge;
    if (!purge) {
      remove = await confirmAction({
        confirm: ctx.confirm,
        input: ctx.input || process.stdin,
        output,
        promptText: `Remove ${cache.label} (${humanBytes(size.bytes)})? Default keep. [y/N] `,
        nonInteractiveMessage: `${cache.label}: ${humanBytes(size.bytes)} kept (rerun disable with --purge to remove).`,
      });
    }
    if (remove) {
      await rm(cachePath, { recursive: true, force: true });
      caches.push({ path: cachePath, action: 'removed', bytes: size.bytes });
      stepDone(output, `${cache.label} removed (${humanBytes(size.bytes)})`);
    } else {
      caches.push({ path: cachePath, action: 'kept', bytes: size.bytes });
      stepDetail(output, `${cache.label} kept (${humanBytes(size.bytes)})`);
    }
  }

  // Absence verify: nothing loam-owned may remain registered/installed.
  const leftovers = [];
  for (const harness of harnesses) {
    if ((await detectMcpEntry({ harness, home, name: entry.mcpName })).present) {
      leftovers.push({ kind: 'mcp', harness });
    }
  }
  if (owned?.tool?.managed) {
    try { await stat(managedBinPath(globalRoot, entry.id, entry.tool.binName, discovery.platform)); leftovers.push({ kind: 'tool', path: entry.tool.binName }); }
    catch {}
  }

  if (leftovers.length) {
    // Keep the ledger so a retry can finish the job; never report clean with residue.
    return { ready: false, category: 'disable_incomplete', leftovers, caches };
  }
  await clearIntegration(globalRoot, entry.id);
  return { ready: true, caches };
}

export { targetHarnesses, dirSize, humanBytes };
