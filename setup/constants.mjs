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

export const HELP_TEXT = `Loam CLI

Usage:
  npx @scchearn/loam setup
  npx @scchearn/loam install
  npx @scchearn/loam setup --yes
  npx @scchearn/loam setup --dry-run
  npx @scchearn/loam doctor
  npx @scchearn/loam uninstall
  npx @scchearn/loam uninstall --yes
  npx @scchearn/loam --help
  npx @scchearn/loam --version

Commands:
  setup       Install or reconcile global Loam skills, runtime, and integrations.
  install     Alias for setup.
  doctor      Check the global Loam installation without changing it.
  uninstall   Remove global Loam skills, runtime, integration, and hook entries.

Options:
  --yes       Accept changes without interactive confirmation.
  --dry-run   Preview setup changes without mutation or downloads.
  --help      Show this help without network access.
  --version   Show the setup package version without network access.
`;
