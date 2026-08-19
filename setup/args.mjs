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
  const requestedCommand = args[0];
  const command = requestedCommand === 'install' ? 'setup' : requestedCommand;
  if (command === 'help') return { command: 'help' };
  if (command === 'version') return { command: 'version' };
  if (command === 'view') {
    const rest = args.slice(1);
    const flag = rest.find((value) => value.startsWith('--'));
    if (flag) throw new UsageError(`unknown option: ${flag}`);
    if (rest.length > 1) throw new UsageError('view accepts at most one workspace-root argument');
    return { command: 'view', workspace: rest[0] };
  }
  if (command !== 'setup' && command !== 'update' && command !== 'doctor' && command !== 'uninstall') {
    throw new UsageError(`unknown command: ${requestedCommand}`);
  }

  let yes = false;
  let dryRun = false;
  for (const flag of args.slice(1)) {
    if (!knownFlags.has(flag)) throw new UsageError(`unknown option: ${flag}`);
    if (flag === '--yes') yes = true;
    if (flag === '--dry-run') dryRun = true;
  }

  return { command, dryRun, yes };
}

export { UsageError };
