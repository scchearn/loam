import { readdir, stat } from 'node:fs/promises';
import { join, resolve } from 'node:path';

import { PACKAGE_ROOT, PACKAGE_VERSION } from './constants.mjs';
import { detectHarnesses } from './harnesses.mjs';
import { federationDefinitionExists } from './federation.mjs';
import { detectLegacyProject, isOwnedLegacyMarker, LEGACY_MARKERS } from './migration.mjs';
import { loadSkillInventory } from './inventory.mjs';
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
  // The durable federation profile root in the config dir (survives uninstall),
  // resolved with the same ladder the runtime uses; `null` when no config basis
  // resolves. Setup never writes it; uninstall preserves it (and `--purge`
  // destroys it).
  const { profileRoot, configRoot } = await import('./profile.mjs');
  const federationProfileRoot = profileRoot({
    env: process.env,
    home: resolvedHome,
    platform,
  });
  // The durable config-dir loam root. With the global root these are the roots
  // the legacy-project sweep must never treat as a removable project (#125):
  // run from $HOME, <workspace>/.agents/loam resolves ONTO the global install.
  const loamConfigRoot = configRoot({ env: process.env, home: resolvedHome, platform });
  const protectedRoots = [globalRoot, loamConfigRoot].filter(Boolean);
  // Whether federation was already enabled here (a file-based service definition
  // exists) BEFORE this run. Captured up front so the post-setup verify can fail
  // if the definition vanished mid-transaction (#125 — the enrollment-state wipe
  // deleted the plist; verify passed over the gutted tree because it never
  // asserted this). win32's Task Scheduler definition is out of scope (#100).
  const { exists: federationEnabled } = await federationDefinitionExists({ globalRoot, platform });
  const sourceRepository = resolvedWorkspace === resolve(packageRoot);
  const hasEvidence = !sourceRepository && await hasLegacyEvidence(resolvedWorkspace, packageRoot);
  const legacy = hasEvidence
    ? await detectLegacyProject({ workspace: resolvedWorkspace, packageRoot, protectedRoots, runner })
    : { workspace: resolvedWorkspace, ready: true, needed: false, skillNames: [], paths: [], markers: [], unsafe: [] };

  return {
    packageRoot,
    packageVersion: PACKAGE_VERSION,
    home: resolvedHome,
    workspace: resolvedWorkspace,
    globalRoot,
    skillsRoot,
    federationProfileRoot,
    configRoot: loamConfigRoot,
    protectedRoots,
    federationEnabled,
    target: target || detectTarget({ platform, arch }),
    platform,
    arch,
    node: process.version,
    npm: process.env.npm_execpath || 'npx',
    harnesses: await detectHarnesses({ home: resolvedHome, pluginVersion: PACKAGE_VERSION }),
    legacy: { ...legacy, needed: hasEvidence, sourceRepository },
  };
}
