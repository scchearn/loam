import { lstat, readdir } from 'node:fs/promises';
import { join, resolve } from 'node:path';

import { assertPhysicalInside } from './paths.mjs';

async function inspect(candidate, workspace, kind, report) {
  let info;
  try {
    info = await lstat(candidate);
  } catch {
    return false;
  }

  try {
    await assertPhysicalInside(workspace, candidate, kind);
  } catch (error) {
    const reason = error?.code === 'PATH_ESCAPE' ? 'path escapes workspace' : 'unresolvable path';
    report.unsafe.push({ path: candidate, kind, reason });
    return false;
  }
  return info;
}

export async function detectLegacyShadow(workspaceRoot) {
  const workspace = resolve(workspaceRoot);
  const report = { shadows: [], unsafe: [] };
  const skillsRoot = join(workspace, '.agents', 'skills');
  const skillsInfo = await inspect(skillsRoot, workspace, 'project-skills', report);
  if (skillsInfo && skillsInfo.isDirectory()) {
    for (const entry of await readdir(skillsRoot, { withFileTypes: true })) {
      if (!entry.name.startsWith('loam-')) continue;
      const path = join(skillsRoot, entry.name);
      if (await inspect(path, workspace, 'project-skill', report)) {
        report.shadows.push({ path, kind: 'project-skill' });
      }
    }
  }

  for (const [path, kind] of [
    [join(workspace, '.agents', 'loam'), 'project-runtime'],
    [join(workspace, '.opencode', 'plugins', 'loam.js'), 'project-opencode-plugin'],
  ]) {
    if (await inspect(path, workspace, kind, report)) report.shadows.push({ path, kind });
  }
  return report;
}
