import { join } from 'node:path';

import { enableIntegration, disableIntegration, verifyIntegration } from './registrar.mjs';

// The optional-integrations catalog (spec: loam-optional-integrations). Each
// entry is DATA over the shared registrar engine (registrar.mjs), which owns the
// install→verify→register / symmetric-disable / absence-verify lifecycle. Adding
// a future entry is data-only; the configurator iterates this array unchanged.
//
// Entry contract (pinned by tests/integrations-catalog.test.mjs):
//   id         string   stable, unique, kebab-case identifier (the --integration value)
//   label      string   human name for wizard copy
//   capability string   the loam capability it serves (e.g. 'code-search')
//   enable     async (ctx) => { ready, category?, detail?, registered? }
//   disable    async (ctx) => { ready, category?, detail?, leftovers?, caches? }
//   verify     async (ctx) => { ready, tool, registered }
//
// Symmetric-disable is part of the contract: disable reverses everything enable
// did (MCP deregistration across every harness, managed-tool removal), verify
// confirms absence, and large derived caches are offered (default keep, --purge).

function makeEntry(spec) {
  return {
    ...spec,
    enable: (ctx) => enableIntegration(spec, ctx),
    disable: (ctx) => disableIntegration(spec, ctx),
    verify: (ctx) => verifyIntegration(spec, ctx),
  };
}

export const CATALOG = [
  makeEntry({
    // Remote MCP, no tool. grep.app indexes public GitHub repos for fast code
    // search. Privacy: queries egress to a third-party public-repo index — the
    // opt-in selection is the consent boundary (wizard copy states it).
    id: 'grep',
    label: 'grep.app code search',
    capability: 'code-search',
    egress: true,
    mcpName: 'grep',
    descriptor: { transport: 'remote', url: 'https://mcp.grep.app' },
  }),
  makeEntry({
    // Local Node tool + local stdio MCP. Fully local, no egress. First query
    // auto-downloads ~2–3GB of GGUF models to ~/.cache/qmd/models (a large
    // derived cache offered on disable).
    id: 'qmd',
    label: 'QMD markdown search',
    capability: 'markdown-search',
    egress: false,
    mcpName: 'qmd',
    tool: { pkg: '@tobilu/qmd', binName: 'qmd', healthArgs: ['--version'] },
    descriptor: (toolPath) => ({ transport: 'local', command: toolPath, args: ['mcp'] }),
    caches: [{ label: 'QMD model cache', path: (home) => join(home, '.cache', 'qmd', 'models') }],
  }),
];

// Lookup by id. Returns undefined for an unknown id so callers can report a
// precise "unknown integration" error instead of silently ignoring it.
export function catalogEntry(id) {
  return CATALOG.find((entry) => entry.id === id);
}

// Required shape of a catalog entry — used by the seam contract test and by the
// configurator's defensive validation so a malformed entry fails loud.
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
