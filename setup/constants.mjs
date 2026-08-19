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
  npx @scchearn/loam update
  npx @scchearn/loam update --dry-run
  npx @scchearn/loam doctor
  npx @scchearn/loam uninstall
  npx @scchearn/loam uninstall --yes
  npx @scchearn/loam view [workspace-root]
  npx @scchearn/loam --help
  npx @scchearn/loam --version

Commands:
  setup       Install or reconcile global Loam skills, runtime, and integrations.
  install     Alias for setup.
  update      Refresh Loam skills, runtime, integrations, and marketplace plugins.
  doctor      Check the global Loam installation without changing it.
  uninstall   Remove global Loam skills, runtime, integration, and hook entries.
  view        Launch the local read-only Loam View at the given workspace root
              (default: current directory).

Options:
  --yes       Accept changes without interactive confirmation.
  --dry-run   Preview setup or update changes without mutation or downloads.
  --help      Show this help without network access.
  --version   Show the setup package version without network access.
`;
