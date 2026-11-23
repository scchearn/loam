import { discover } from './discovery.mjs';
import { executeSetup } from './transaction.mjs';
import { hasMigratableRuntime } from './migration.mjs';

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
      env: options.env,
    });

    if (parsed.command === 'update') {
      // `update` bumps an EXISTING install. It refuses only a truly fresh
      // machine — no config-dir ledger AND no migratable legacy state. A legacy
      // machine is upgraded (its ledger is seeded up front in the transaction),
      // not refused under the wrong verb.
      const migratable = await hasMigratableRuntime({
        globalRoot: discovery.globalRoot,
        home: discovery.home,
        env: discovery.env || process.env,
        platform: discovery.platform,
        arch: discovery.arch,
        target: discovery.target,
      });
      if (!migratable) {
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
