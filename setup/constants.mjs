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

Three verbs, one job each:
  npx @scchearn/loam install     First-time install; re-run repairs a same-version install.
  npx @scchearn/loam update      Bump an existing install to this version, and nothing else.
  npx @scchearn/loam setup       Configure an existing install: federation, integrations, harnesses.

Usage:
  npx @scchearn/loam install [--yes] [--dry-run]
  npx @scchearn/loam update [--yes] [--dry-run]
  npx @scchearn/loam setup [--federation enable|disable] [--integration <id>]... [--purge] [--yes] [--dry-run]
  npx @scchearn/loam doctor
  npx @scchearn/loam uninstall [--yes] [--purge]
  npx @scchearn/loam --help
  npx @scchearn/loam --version

Commands:
  install     First-time installation of global Loam skills, runtime, harness
              adapters, and shared integration. Idempotent — re-running repairs a
              damaged install at the same version; a healthy install is a fast no-op.
  update      Move an existing install to this package's version: skills pin,
              runtime binary, regenerated adapters, refreshed service definitions.
              Refuses with a hint when no install exists; changes nothing else.
  setup       Configure an existing install without touching versions: enable or
              disable federation and optional integrations, and select harnesses.
  doctor      Check the global Loam installation without changing it.
  uninstall   Remove global Loam skills, runtime, integration, and hook entries
              (preserves the federation profile; --purge destroys it).

Options:
  --yes           Accept changes without interactive confirmation.
  --dry-run       Preview changes without mutation or downloads.
  --federation    With setup, enable or disable the federation connector service.
  --integration   With setup, select an optional integration to enable (repeatable).
  --purge         With uninstall, destroy the federation profile in the config dir.
                  With setup --federation disable, also remove large derived caches.
  --help          Show this help without network access.
  --version       Show the package version without network access.
`;
