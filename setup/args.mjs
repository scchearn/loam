import { UsageError } from './errors.mjs';

// One job per verb (spec: loam-cli-verbs):
//   install  — first-time installation + same-version repair (idempotent).
//   update   — version bump of an existing install, and only that.
//   setup    — the configurator (federation + integrations + harness selection);
//              never installs or updates core loam, never touches versions.
const COMMANDS = new Set(['install', 'update', 'setup', 'doctor', 'uninstall', 'view']);

// Flags each command accepts. Unknown command/flag combinations are a usage error
// so automation fails loud instead of silently ignoring a misplaced flag.
const FLAGS_BY_COMMAND = {
  install: new Set(['--yes', '--dry-run']),
  update: new Set(['--yes', '--dry-run']),
  setup: new Set(['--yes', '--dry-run', '--purge']),
  doctor: new Set([]),
  uninstall: new Set(['--yes', '--purge']),
};

// setup value flags (configurator): --federation enable|disable, repeatable
// --integration <id> (enable) and --disable-integration <id>. Parsed positionally
// as `<flag> <value>`.
const SETUP_VALUE_FLAGS = new Set(['--federation', '--integration', '--disable-integration']);

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
  const command = args[0];
  if (command === 'help') return { command: 'help' };
  if (command === 'version') return { command: 'version' };
  if (command === 'view') {
    const rest = args.slice(1);
    const flag = rest.find((value) => value.startsWith('--'));
    if (flag) throw new UsageError(`unknown option: ${flag}`);
    if (rest.length > 1) throw new UsageError('view accepts at most one workspace-root argument');
    return { command: 'view', workspace: rest[0] };
  }
  if (!COMMANDS.has(command)) {
    throw new UsageError(`unknown command: ${command}`);
  }

  const known = FLAGS_BY_COMMAND[command];
  const parsed = { command, dryRun: false, yes: false, purge: false };
  if (command === 'setup') {
    parsed.federation = null; // null | 'enable' | 'disable'
    parsed.integrations = []; // ids to enable
    parsed.disableIntegrations = []; // ids to disable
  }

  const rest = args.slice(1);
  for (let i = 0; i < rest.length; i += 1) {
    const flag = rest[i];
    if (command === 'setup' && SETUP_VALUE_FLAGS.has(flag)) {
      const value = rest[i + 1];
      if (value === undefined || value.startsWith('--')) {
        throw new UsageError(`${flag} needs a value`);
      }
      i += 1;
      if (flag === '--federation') {
        if (value !== 'enable' && value !== 'disable') {
          throw new UsageError(`--federation expects enable or disable, got: ${value}`);
        }
        parsed.federation = value;
      } else if (flag === '--disable-integration') {
        parsed.disableIntegrations.push(value);
      } else {
        parsed.integrations.push(value);
      }
      continue;
    }
    if (!known.has(flag)) throw new UsageError(`unknown option for ${command}: ${flag}`);
    if (flag === '--yes') parsed.yes = true;
    if (flag === '--dry-run') parsed.dryRun = true;
    if (flag === '--purge') parsed.purge = true;
  }

  return parsed;
}

export { UsageError };
