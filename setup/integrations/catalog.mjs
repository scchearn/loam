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
//   mcpName    string?  present = loam registers an MCP server for this entry
//   tool       object?  present = this entry has a companion binary; a tool with
//                       `pkg` is installable, one without is DETECTION-ONLY
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
    // Detection-only: a companion tool, no MCP lane, and loam never installs it.
    // hcom is a Rust binary distributed by brew, per-OS installer scripts and
    // PyPI — none of which installNodeTool()'s npm lane can drive, and piping an
    // installer script from setup would cross the egress-consent line. So enable
    // detects and records, or refuses with the recipe; the user installs.
    //
    // No `mcpName`: hcom ships no MCP server command (checked against 0.7.25),
    // so there is nothing stable for loam to register. The lane is data-switched,
    // so the day hcom ships one this entry gains two fields and nothing else.
    //
    // No `caches` either: ~/.hcom is message history and live agent state — user
    // data, not a derived cache like qmd's models — so disable never offers it.
    id: 'hcom',
    label: 'hcom agent messaging',
    capability: 'agent-messaging',
    egress: false,
    tool: {
      binName: 'hcom',
      // `hcom version` is not a command; `--version` is the health check.
      healthArgs: ['--version'],
      // Install sites off PATH. Both official installers target ~/.local/bin on
      // every OS (the PowerShell one included), and HCOM_INSTALL_DIR overrides
      // it — flat for the default layout, `bin/` for the prefixed one.
      dirs: (home, env = {}) => [
        ...(env.HCOM_INSTALL_DIR ? [join(env.HCOM_INSTALL_DIR, 'bin'), env.HCOM_INSTALL_DIR] : []),
        join(home, '.local', 'bin'),
      ],
      install: [
        { label: 'macOS (Homebrew)', command: 'brew install aannoo/hcom/hcom' },
        { label: 'macOS, Linux, Termux, WSL', command: 'curl -fsSL https://github.com/aannoo/hcom/releases/latest/download/hcom-installer.sh | sh' },
        { label: 'Windows (PowerShell)', command: 'irm https://github.com/aannoo/hcom/releases/latest/download/hcom-installer.ps1 | iex' },
        { label: 'Python packaging', command: 'uv tool install hcom   # or: pip install hcom' },
      ],
    },
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
  // An entry has to DO something: register an MCP (mcpName + descriptor) or
  // carry a companion tool (tool.binName), or both. Neither means enable would
  // record an empty ledger entry and report success — the silent no-op the
  // engine exists to prevent.
  const mcpLane = Boolean(entry.mcpName) && entry.descriptor != null;
  const toolLane = Boolean(entry.tool?.binName);
  return mcpLane || toolLane;
}
