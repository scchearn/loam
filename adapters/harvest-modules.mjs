import { readFile } from 'node:fs/promises';
import { homedir } from 'node:os';
import { isAbsolute, join } from 'node:path';
import { pathToFileURL } from 'node:url';

export async function loadHarvestModules() {
  const roots = [];
  const globalRoot = process.env.LOAM_HOME && isAbsolute(process.env.LOAM_HOME)
    ? process.env.LOAM_HOME : null;
  try {
    const metadata = JSON.parse(await readFile(join(globalRoot || join(homedir(), '.agents', 'loam'), 'install.json'), 'utf8'));
    if (typeof metadata.integration_path === 'string') {
      roots.unshift(new URL('./', pathToFileURL(metadata.integration_path)));
    }
  } catch {}
  if (process.env.LOAM_INTEGRATION_PATH) {
    roots.unshift(new URL('./', pathToFileURL(process.env.LOAM_INTEGRATION_PATH)));
  }
  for (const root of roots) {
    try {
      const [paths, harvest] = await Promise.all([
        import(new URL('paths.mjs', root).href),
        import(new URL('harvest.mjs', root).href),
      ]);
      return { ...paths, ...harvest };
    } catch {}
  }
  throw new Error('loam harvest integration is unavailable');
}
