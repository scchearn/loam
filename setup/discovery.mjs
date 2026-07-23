import { readdir, stat } from 'node:fs/promises';
import { join, resolve } from 'node:path';

import { PACKAGE_ROOT, PACKAGE_VERSION } from './constants.mjs';
import { detectHarnesses } from './harnesses.mjs';
import { detectLegacyProject, isOwnedLegacyMarker, LEGACY_MARKERS } from './migration.mjs';
import { loadSkillInventory } from './inventory.mjs';
import { readRequiredVersion } from '../integration/metadata.mjs';
import { detectTarget } from './target.mjs';

async function exists(path) {
  try {
    await stat(path);
    return true;
  } catch {
    return false;
  }
}

async function hasLegacyEvidence(workspace, packageRoot) {
  const candidates = [
    join(workspace, '.agents', 'loam'),
    join(workspace, '.agents', 'loamstate'),
  ];
  if (await Promise.any(candidates.map((path) => exists(path))).catch(() => false)) return true;

  for (const [relativePath] of LEGACY_MARKERS) {
    const path = join(workspace, relativePath);
    if (await exists(path) && await isOwnedLegacyMarker(path, relativePath)) return true;
  }

  const skillsRoot = join(workspace, '.agents', 'skills');
  try {
    const inventory = await loadSkillInventory({ packageRoot });
    const ownedDirectories = new Set(inventory.skills.map((skill) => skill.directoryName));
    return (await readdir(skillsRoot, { withFileTypes: true })).some(
      (entry) => entry.isDirectory() && ownedDirectories.has(entry.name),
    );
  } catch {
    return false;
  }
}

export async function discover({
  home = process.env.HOME || process.env.USERPROFILE,
  workspace = process.cwd(),
  packageRoot = PACKAGE_ROOT,
  target,
  platform = process.platform,
  arch = process.arch,
  runner,
} = {}) {
  const resolvedHome = resolve(home);
  const resolvedWorkspace = resolve(workspace);
  const globalRoot = join(resolvedHome, '.agents', 'loam');
  const skillsRoot = join(resolvedHome, '.agents', 'skills');
  let requiredVersion = '';
  try {
    requiredVersion = await readRequiredVersion({ skillsRoot });
  } catch {}

  const sourceRepository = resolvedWorkspace === resolve(packageRoot);
  const hasEvidence = !sourceRepository && await hasLegacyEvidence(resolvedWorkspace, packageRoot);
  const legacy = hasEvidence
    ? await detectLegacyProject({ workspace: resolvedWorkspace, packageRoot, runner })
    : { workspace: resolvedWorkspace, ready: true, needed: false, skillNames: [], paths: [], markers: [], unsafe: [] };

  return {
    packageRoot,
    packageVersion: PACKAGE_VERSION,
    home: resolvedHome,
    workspace: resolvedWorkspace,
    globalRoot,
    skillsRoot,
    target: target || detectTarget({ platform, arch }),
    platform,
    arch,
    requiredVersion,
    node: process.version,
    npm: process.env.npm_execpath || 'npx',
    harnesses: await detectHarnesses({ home: resolvedHome }),
    legacy: { ...legacy, needed: hasEvidence, sourceRepository },
  };
}
