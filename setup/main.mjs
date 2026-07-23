import { discover } from './discovery.mjs';
import { executeSetup } from './transaction.mjs';

export async function runSetup(parsed, options = {}) {
  const errorOutput = options.errorOutput || process.stderr;
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
    return await executeSetup(parsed, discovery, options);
  } catch (error) {
    errorOutput.write(`Setup failed: ${error instanceof Error ? error.message : String(error)}\n`);
    return 1;
  }
}
