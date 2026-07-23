import { lstat, readFile, realpath, rm } from 'node:fs/promises';
import { isAbsolute, join, relative, resolve } from 'node:path';

import { loadSkillInventory } from './inventory.mjs';
import { listSkills, skillEntryAliases } from './skills.mjs';
import { runSkills } from './process.mjs';

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
    const physical = await realpath(path);
    if (!inside(workspace, physical)) {
      report.unsafe.push({ path, kind, reason: 'path escapes workspace' });
      return false;
    }
  } catch (error) {
    if (error?.code === 'ENOENT') return false;
    report.unsafe.push({ path, kind, reason: 'path cannot be resolved' });
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
