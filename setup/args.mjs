import { UsageError } from './errors.mjs';

const knownFlags = new Set(['--yes', '--dry-run']);

export function parseArgs(argv) {
  const args = [...argv];
  const wantsHelp = args.includes('--help');
  const wantsVersion = args.includes('--version');

  if (wantsHelp && wantsVersion) {
    throw new UsageError('choose either --help or --version');
  }
  if (wantsHelp) return { command: 'help' };
  if (wantsVersion) return { command: 'version' };

  if (args.length === 0) return { command: 'help' };
  if (args[0] !== 'setup') {
    throw new UsageError(`unknown command: ${args[0]}`);
  }

  let yes = false;
  let dryRun = false;
  for (const flag of args.slice(1)) {
    if (!knownFlags.has(flag)) throw new UsageError(`unknown option: ${flag}`);
    if (flag === '--yes') yes = true;
    if (flag === '--dry-run') dryRun = true;
  }

  return { command: 'setup', dryRun, yes };
}

export { UsageError };
