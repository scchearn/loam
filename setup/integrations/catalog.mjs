// The optional-integrations catalog seam.
//
// Unit A ships this as a declared-but-EMPTY registry. Unit B implements
// `loam-optional-integrations` (QMD, grep.app, …) by adding entries here — the
// configurator (setup/configure.mjs) already iterates the catalog to enable and
// disable entries, so Unit B is additive: no configurator changes required to
// wire a new entry, only a catalog entry that satisfies the contract below.
//
// Contract every entry must satisfy (pinned by tests/integrations-catalog.test.mjs):
//   id         string   stable, unique, kebab-case identifier (the --integration value)
//   label      string   human name for wizard copy
//   capability string   the loam capability it serves (e.g. 'code-search')
//   enable     async ({ discovery, install, dryRun, harnesses, runner, output }) => { ready, category?, detail?, rollback? }
//   disable    async ({ discovery, install, dryRun, purge, harnesses, runner, output }) => { ready, category?, detail?, leftovers? }
//   verify     async ({ discovery, install, harnesses, runner }) => { ready, present, registered, detail? }
//
// Symmetric-disable is part of the contract: `disable` must reverse everything
// `enable` did (MCP deregistration across every harness, managed-tool removal,
// loam-written config entries), and `verify` must be able to confirm absence.
export const CATALOG = [];

// Lookup by id. Returns undefined for an unknown id so callers can report a
// precise "unknown integration" error instead of silently ignoring it.
export function catalogEntry(id) {
  return CATALOG.find((entry) => entry.id === id);
}

// Required shape of a catalog entry — used by the seam contract test and by the
// configurator's defensive validation so a malformed Unit B entry fails loud.
export const CATALOG_ENTRY_CONTRACT = Object.freeze({
  fields: Object.freeze(['id', 'label', 'capability']),
  methods: Object.freeze(['enable', 'disable', 'verify']),
});

export function isValidCatalogEntry(entry) {
  if (!entry || typeof entry !== 'object') return false;
  for (const field of CATALOG_ENTRY_CONTRACT.fields) {
    if (typeof entry[field] !== 'string' || !entry[field]) return false;
  }
  for (const method of CATALOG_ENTRY_CONTRACT.methods) {
    if (typeof entry[method] !== 'function') return false;
  }
  return true;
}
