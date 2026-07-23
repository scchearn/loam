import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

const packageRoot = fileURLToPath(new URL('..', import.meta.url));
const packageJson = JSON.parse(readFileSync(new URL('../package.json', import.meta.url), 'utf8'));

export const PACKAGE_ROOT = packageRoot;
export const PACKAGE_VERSION = packageJson.version;
export const SKILLS_CLI_VERSION = '1.5.20';

export const EXIT_CODES = Object.freeze({
  OK: 0,
  FAILURE: 1,
  USAGE: 64,
  CANCELLED: 130,
});

export const HELP_TEXT = `Loam Setup

Usage:
  npx @scchearn/loam setup
  npx @scchearn/loam setup --yes
  npx @scchearn/loam setup --dry-run
  npx @scchearn/loam uninstall
  npx @scchearn/loam uninstall --yes
  npx @scchearn/loam --help
  npx @scchearn/loam --version

Commands:
  setup       Install or reconcile global Loam skills, runtime, and integrations.
  uninstall   Remove global Loam runtime, integration, and harness hook entries.

Options:
  --yes       Accept changes without interactive confirmation.
  --dry-run   Preview checks and changes without mutation or downloads.
  --help      Show this help without network access.
  --version   Show the setup package version without network access.
`;
