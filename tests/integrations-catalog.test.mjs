import assert from 'node:assert/strict';
import { test } from 'node:test';

import {
  CATALOG,
  catalogEntry,
  isValidCatalogEntry,
  CATALOG_ENTRY_CONTRACT,
} from '../setup/integrations/catalog.mjs';

// The catalog is populated with grep, hcom and qmd. This pins the seam shape and
// that every shipped entry satisfies the contract the configurator relies on.

test('the catalog ships grep, hcom and qmd', () => {
  assert.ok(Array.isArray(CATALOG));
  const ids = CATALOG.map((e) => e.id).sort();
  assert.deepEqual(ids, ['grep', 'hcom', 'qmd']);
});

test('ids are unique', () => {
  const ids = CATALOG.map((e) => e.id);
  assert.equal(new Set(ids).size, ids.length);
});

test('catalogEntry resolves known ids and returns undefined otherwise', () => {
  assert.equal(catalogEntry('grep')?.id, 'grep');
  assert.equal(catalogEntry('qmd')?.id, 'qmd');
  assert.equal(catalogEntry('hcom')?.id, 'hcom');
  assert.equal(catalogEntry('nope'), undefined);
});

test('the entry contract names the required fields and lifecycle methods', () => {
  assert.deepEqual([...CATALOG_ENTRY_CONTRACT.fields], ['id', 'label', 'capability']);
  assert.deepEqual([...CATALOG_ENTRY_CONTRACT.methods], ['enable', 'disable', 'verify']);
});

test('every shipped catalog entry satisfies the contract', () => {
  for (const entry of CATALOG) assert.equal(isValidCatalogEntry(entry), true, `invalid entry: ${entry?.id}`);
});

test('grep is a remote code-search MCP with egress; qmd is a local markdown-search tool with no egress', () => {
  const grep = catalogEntry('grep');
  assert.equal(grep.capability, 'code-search');
  assert.equal(grep.egress, true);
  assert.equal(grep.descriptor.transport, 'remote');
  assert.equal(grep.descriptor.url, 'https://mcp.grep.app');
  assert.equal(grep.tool, undefined);

  const qmd = catalogEntry('qmd');
  assert.equal(qmd.capability, 'markdown-search');
  assert.equal(qmd.egress, false);
  assert.equal(qmd.tool.pkg, '@tobilu/qmd');
  assert.equal(qmd.tool.binName, 'qmd');
  // descriptor is a function of the resolved absolute tool path.
  assert.deepEqual(qmd.descriptor('/abs/qmd'), { transport: 'local', command: '/abs/qmd', args: ['mcp'] });
  assert.ok(qmd.caches?.length, 'qmd declares its large model cache for disable');
});

test('hcom is a detection-only agent-messaging tool with no MCP lane', () => {
  const hcom = catalogEntry('hcom');
  assert.equal(hcom.capability, 'agent-messaging');
  assert.equal(hcom.egress, false);
  // No MCP lane at all: hcom ships no MCP server command, so loam registers none.
  assert.equal(hcom.mcpName, undefined);
  assert.equal(hcom.descriptor, undefined);
  // Detection-only: a binary to find, never a package to install.
  assert.equal(hcom.tool.binName, 'hcom');
  assert.equal(hcom.tool.pkg, undefined);
  assert.deepEqual(hcom.tool.healthArgs, ['--version']);
  // ~/.hcom is user data, not a derived cache, so disable never offers to purge it.
  assert.equal(hcom.caches, undefined);
  // Every supported install route has a runnable recipe, not a label.
  assert.ok(hcom.tool.install.length >= 4);
  for (const recipe of hcom.tool.install) {
    assert.ok(recipe.label && recipe.command, `incomplete recipe: ${JSON.stringify(recipe)}`);
  }
  // Detection sites: the installer default, plus the documented override.
  assert.deepEqual(hcom.tool.dirs('/home/u', {}), ['/home/u/.local/bin']);
  assert.deepEqual(hcom.tool.dirs('/home/u', { HCOM_INSTALL_DIR: '/opt/h' }), ['/opt/h/bin', '/opt/h', '/home/u/.local/bin']);
});

test('isValidCatalogEntry rejects malformed entries', () => {
  const good = { id: 'x', label: 'X', capability: 'c', mcpName: 'x', descriptor: {}, enable: async () => {}, disable: async () => {}, verify: async () => {} };
  assert.equal(isValidCatalogEntry(good), true);
  assert.equal(isValidCatalogEntry(null), false);
  assert.equal(isValidCatalogEntry({ ...good, id: '' }), false);
  const { disable, ...noDisable } = good;
  assert.equal(isValidCatalogEntry(noDisable), false);
});

test('an entry must carry an MCP lane or a tool lane, never neither', () => {
  // The lane switch is data, so the validator has to be the thing that keeps an
  // inert entry — one that would enable to an empty ledger record — out.
  const base = { id: 'x', label: 'X', capability: 'c', enable: async () => {}, disable: async () => {}, verify: async () => {} };
  assert.equal(isValidCatalogEntry(base), false, 'neither lane');
  assert.equal(isValidCatalogEntry({ ...base, mcpName: 'x' }), false, 'named MCP with no descriptor');
  assert.equal(isValidCatalogEntry({ ...base, mcpName: 'x', descriptor: { transport: 'remote', url: 'https://x' } }), true);
  assert.equal(isValidCatalogEntry({ ...base, tool: { binName: 'x' } }), true, 'detection-only tool lane');
  assert.equal(isValidCatalogEntry({ ...base, tool: {} }), false, 'tool with no binary name');
});
