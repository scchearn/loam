import { chmod, copyFile, lstat, mkdir, readdir, readFile, rm, stat } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { dirname, isAbsolute, join, relative, resolve } from 'node:path';

import { assertPhysicalInside, detectTarget } from '../integration/paths.mjs';
import { readLedger, runtimeStorePath, writeLedger } from '../integration/ledger.mjs';
import { loadSkillInventory } from './inventory.mjs';
import { publishJson } from './atomic.mjs';
import { configRoot } from './profile.mjs';
import { SEMVER } from './constants.mjs';
import { listSkills, skillEntryAliases } from './skills.mjs';
import { runSkills } from './process.mjs';

async function fileExists(path) {
  try {
    return (await stat(path)).isFile();
  } catch {
    return false;
  }
}

// One-time seed of the config-dir runtime ledger for a machine that has a legacy
// runtime but no ledger yet. It runs up front in the install/update transaction,
// under setup.lock — never from readiness/hook paths (which run concurrently and
// latency-bounded). Idempotent: an existing ledger is authoritative and is never
// overwritten. The legacy binary is COPIED, never moved, so already-injected
// hook paths and the federation service keep working until the same transaction
// regenerates them. The seed target comes from install.json when present, else
// from the legacy store-path `<version>` directory (pre-T1 runtimes emit no
// self-report, so the binary cannot seed itself); the sha is recomputed from
// disk as the integrity proof. See plans/runtime-channel-ledger.md.
export async function migrateRuntimeLedger({
  globalRoot,
  env = process.env,
  home,
  platform = process.platform,
  arch = process.arch,
  target,
} = {}) {
  const config = configRoot({ env, home, platform });
  if (!config) return { migrated: false, reason: 'no_config_dir' };
  if (await readLedger({ root: config })) return { migrated: false, reason: 'ledger_present' };

  const legacyRoot = resolve(globalRoot);
  const executable = platform === 'win32' ? 'loam.exe' : 'loam';
  const selectedTarget = target || detectTarget({ platform, arch, override: env.LOAM_TARGET });

  let install = null;
  try {
    install = JSON.parse(await readFile(join(legacyRoot, 'install.json'), 'utf8'));
  } catch {
    install = null;
  }

  // Prefer install.json's recorded version; else discover the binary-only case
  // from the legacy `bin/<version>/<target>/` path structure.
  let version = typeof install?.runtime_version === 'string' && SEMVER.test(install.runtime_version)
    ? install.runtime_version
    : null;
  let legacyBinary = version && typeof install?.runtime_path === 'string' ? install.runtime_path : null;
  let seededFrom = version && legacyBinary ? 'install.json' : 'binary';

  if (!version || !(await fileExists(legacyBinary))) {
    version = null;
    legacyBinary = null;
    seededFrom = 'binary';
    const binRoot = join(legacyRoot, 'bin');
    for (const entry of await readdir(binRoot, { withFileTypes: true }).catch(() => [])) {
      if (!entry.isDirectory() || !SEMVER.test(entry.name)) continue;
      const candidate = join(binRoot, entry.name, selectedTarget, executable);
      if (await fileExists(candidate)) {
        version = entry.name;
        legacyBinary = candidate;
        break;
      }
    }
  }

  if (!version || !legacyBinary || !(await fileExists(legacyBinary))) {
    return { migrated: false, reason: 'no_legacy_runtime' };
  }

  // Recompute the sha from the on-disk binary — the integrity proof, and the
  // only trustworthy source for the binary-only case.
  const sha256 = createHash('sha256').update(await readFile(legacyBinary)).digest('hex');
  const storePath = runtimeStorePath({ version, target: selectedTarget, platform, root: config });
  await mkdir(dirname(storePath), { recursive: true, mode: 0o700 });
  await copyFile(legacyBinary, storePath);
  await chmod(storePath, 0o700).catch((error) => {
    if (platform !== 'win32') throw error;
  });

  // Channel is provenance only, derived from the version string (a migrated
  // legacy runtime carries no pin record).
  const channel = version.includes('-') ? 'next' : 'latest';
  await writeLedger({ channel, target: version, sha256, store_path: storePath }, { root: config });

  // Rewrite install.json as schema 2, dropping the now-non-authoritative
  // runtime_* fields (lossless, silent conversion).
  if (install) {
    const { runtime_version: _v, runtime_path: _p, runtime_sha256: _s, ...rest } = install;
    await publishJson({ filePath: join(legacyRoot, 'install.json'), value: { ...rest, schema_version: 2 } });
  }

  return { migrated: true, version, target: selectedTarget, sha256, storePath, from: seededFrom };
}

// True when this machine has a runtime `update` can bump: a config-dir ledger,
// or migratable legacy state (install.json, or a binary under bin/). The
// verb-dispatch refusal uses this so a legacy machine is upgraded (its ledger
// is seeded up front in the transaction), not refused as if it were fresh.
export async function hasMigratableRuntime({
  globalRoot,
  env = process.env,
  home,
  platform = process.platform,
  arch = process.arch,
  target,
} = {}) {
  const config = configRoot({ env, home, platform });
  if (config && await readLedger({ root: config })) return true;
  const legacyRoot = resolve(globalRoot);
  try {
    const install = JSON.parse(await readFile(join(legacyRoot, 'install.json'), 'utf8'));
    if (install && typeof install === 'object' && !Array.isArray(install)) return true;
  } catch {
    // No readable install.json.
  }
  const executable = platform === 'win32' ? 'loam.exe' : 'loam';
  const selectedTarget = target || detectTarget({ platform, arch, override: env.LOAM_TARGET });
  const binRoot = join(legacyRoot, 'bin');
  for (const entry of await readdir(binRoot, { withFileTypes: true }).catch(() => [])) {
    if (!entry.isDirectory() || !SEMVER.test(entry.name)) continue;
    if (await fileExists(join(binRoot, entry.name, selectedTarget, executable))) return true;
  }
  return false;
}

export const LEGACY_MARKERS = Object.freeze([
  ['.opencode/plugins/loam.js', 'plugin-marker'],
  ['.claude-plugin/plugin.json', 'plugin-marker'],
  ['.codex-plugin/plugin.json', 'plugin-marker'],
  ['.cursor-plugin/plugin.json', 'plugin-marker'],
]);

export async function isOwnedLegacyMarker(path, relativePath) {
  try {
    const text = await readFile(path, 'utf8');
    if (relativePath === '.opencode/plugins/loam.js') return /\bLoamPlugin\b/.test(text);
    const manifest = JSON.parse(text);
    return manifest?.name === 'loam' && (
      manifest.repository === 'https://github.com/scchearn/loam' ||
      manifest.hooks === './hooks/hooks.json' ||
      manifest.skills === './skills/' ||
      (Array.isArray(manifest.skills) && manifest.skills.some((entry) => typeof entry === 'string' && entry.startsWith('./skills/')))
    );
  } catch {
    return false;
  }
}

function inside(root, candidate) {
  const relativePath = relative(resolve(root), resolve(candidate));
  return !relativePath.startsWith('..') && !isAbsolute(relativePath);
}

async function safePath(workspace, candidate, kind, report) {
  const path = resolve(candidate);
  if (!inside(workspace, path)) {
    report.unsafe.push({ path, kind, reason: 'path escapes workspace' });
    return false;
  }
  try {
    await lstat(path);
    await assertPhysicalInside(workspace, path, kind);
  } catch (error) {
    if (error?.code === 'ENOENT') return false;
    const reason = error?.code === 'PATH_ESCAPE' ? 'path escapes workspace' : 'path cannot be resolved';
    report.unsafe.push({ path, kind, reason });
  }
  return report.unsafe.every((entry) => entry.path !== path);
}

async function markerPaths(workspace, report) {
  for (const [relativePath, kind] of LEGACY_MARKERS) {
    const path = join(workspace, relativePath);
    if (!(await safePath(workspace, path, kind, report))) continue;
    if (await isOwnedLegacyMarker(path, relativePath)) report.markers.push({ path, kind });
  }
}

export async function detectLegacyProject({ workspace, packageRoot, runner } = {}) {
  const root = resolve(workspace);
  const report = {
    workspace: root,
    skillNames: [],
    listedSkillNames: [],
    paths: [],
    markers: [],
    unsafe: [],
  };
  if (root === resolve(packageRoot)) return { ...report, sourceRepository: true, ready: true };

  const inventory = await loadSkillInventory({ packageRoot });
  const aliases = new Map(inventory.skills.flatMap((skill) => skill.aliases.map((alias) => [alias, skill])));
  const listed = await listSkills({ global: false, cwd: root, runner });
  report.list = listed;
  if (!listed.ok) return { ...report, ready: false, category: listed.category || 'skills_list_failed' };

  for (const entry of listed.entries) {
    const alias = skillEntryAliases(entry).find((candidate) => aliases.has(candidate));
    if (!alias) continue;
    const name = entry.name || alias;
    if (!report.listedSkillNames.includes(name)) report.listedSkillNames.push(name);
    if (!report.skillNames.includes(name)) report.skillNames.push(name);
    const path = entry.path
      ? (isAbsolute(entry.path) ? entry.path : resolve(root, entry.path))
      : join(root, '.agents', 'skills', aliases.get(alias).directoryName);
    if (await safePath(root, path, 'project-skill', report)) {
      report.paths.push({ path: resolve(path), kind: 'project-skill' });
    } else {
      report.skillNames = report.skillNames.filter((candidate) => candidate !== name);
    }
  }

  for (const relativePath of ['.agents/loam', '.agents/loamstate']) {
    const path = join(root, relativePath);
    if (await safePath(root, path, 'project-runtime', report)) report.paths.push({ path, kind: 'project-runtime' });
  }
  await markerPaths(root, report);
  return {
    ...report,
    ready: report.unsafe.length === 0 && report.listedSkillNames.length === 0 && report.paths.length === 0 && report.markers.length === 0,
  };
}

export async function migrateLegacyProject({
  workspace,
  packageRoot,
  yes = false,
  prompt = async () => false,
  runner,
} = {}) {
  const report = await detectLegacyProject({ workspace, packageRoot, runner });
  if (report.category) return { ...report, ready: false, migrated: false, leftovers: report.paths };
  if (report.ready) return { ...report, migrated: false, leftovers: [] };
  if (report.unsafe.length) return { ...report, ready: false, migrated: false, category: 'unsafe_legacy_path', leftovers: report.unsafe };

  const authorized = yes || await prompt(report);
  if (!authorized) return { ...report, ready: false, migrated: false, category: 'migration_declined', leftovers: [...report.paths, ...report.markers] };

  const leftovers = [];
  for (const name of report.skillNames) {
    const removed = await runSkills(['remove', name, '--yes'], { cwd: report.workspace, runner });
    if (!removed.ok) leftovers.push({ name, detail: removed.stderr || 'Skills CLI removal failed' });
  }
  if (leftovers.length) {
    return { ...report, ready: false, migrated: false, category: 'migration_failed', leftovers: [...leftovers, ...report.paths, ...report.markers] };
  }

  const afterSkills = await detectLegacyProject({ workspace: report.workspace, packageRoot, runner });
  if (afterSkills.category || afterSkills.unsafe.length || afterSkills.listedSkillNames.length) {
    return {
      ...afterSkills,
      ready: false,
      migrated: false,
      category: afterSkills.category || 'migration_incomplete',
      leftovers: [...afterSkills.unsafe, ...afterSkills.listedSkillNames],
    };
  }

  for (const entry of report.paths.filter((candidate) => candidate.kind === 'project-runtime')) {
    await rm(entry.path, { recursive: true, force: true });
  }
  for (const marker of report.markers) await rm(marker.path, { force: true });
  const verification = await detectLegacyProject({ workspace: report.workspace, packageRoot, runner });
  const remaining = [...verification.unsafe, ...verification.listedSkillNames, ...verification.paths, ...verification.markers];
  return {
    ...verification,
    ready: remaining.length === 0,
    migrated: true,
    leftovers: remaining,
  };
}
