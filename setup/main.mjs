import { readFile } from 'node:fs/promises';
import { join } from 'node:path';

import { discover } from './discovery.mjs';
import { executeSetup } from './transaction.mjs';

// install and update share the staged install transaction. The only difference
// visible here is the refusal guard: `update` is a bump of an EXISTING install,
// so on a machine with no install.json it refuses with a hint to `install`
// rather than performing a first-time installation under the wrong verb.
export async function runSetup(parsed, options = {}) {
  const errorOutput = options.errorOutput || process.stderr;
  const action = parsed.command === 'update' ? 'Update' : 'Install';
  try {
    const discovery = await discover({
      home: options.home,
      workspace: options.workspace,
      packageRoot: options.packageRoot,
      target: options.target,
      platform: options.platform,
      arch: options.arch,
      runner: options.runner,
    });

    if (parsed.command === 'update') {
      let hasInstall = false;
      try {
        JSON.parse(await readFile(join(discovery.globalRoot, 'install.json'), 'utf8'));
        hasInstall = true;
      } catch {}
      if (!hasInstall) {
        errorOutput.write(
          `No Loam installation found at ${discovery.globalRoot}.\n`
          + 'update bumps an existing install; run `npx @scchearn/loam install` first.\n',
        );
        return 1;
      }
    }

    return await executeSetup(parsed, discovery, options);
  } catch (error) {
    errorOutput.write(`${action} failed: ${error instanceof Error ? error.message : String(error)}\n`);
    return 1;
  }
}
