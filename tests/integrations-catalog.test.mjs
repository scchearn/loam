import assert from 'node:assert/strict';
import { test } from 'node:test';

import {
  CATALOG,
  catalogEntry,
  isValidCatalogEntry,
  CATALOG_ENTRY_CONTRACT,
} from '../setup/integrations/catalog.mjs';

// The catalog is now populated (Unit B) with grep + qmd. This pins the seam
// shape and that every shipped entry satisfies the contract the configurator
// relies on.

test('the catalog ships grep and qmd', () => {
  assert.ok(Array.isArray(CATALOG));
  const ids = CATALOG.map((e) => e.id).sort();
  assert.deepEqual(ids, ['grep', 'qmd']);
});

test('ids are unique', () => {
  const ids = CATALOG.map((e) => e.id);
  assert.equal(new Set(ids).size, ids.length);
});

test('catalogEntry resolves known ids and returns undefined otherwise', () => {
  assert.equal(catalogEntry('grep')?.id, 'grep');
  assert.equal(catalogEntry('qmd')?.id, 'qmd');
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

test('isValidCatalogEntry rejects malformed entries', () => {
  const good = { id: 'x', label: 'X', capability: 'c', enable: async () => {}, disable: async () => {}, verify: async () => {} };
  assert.equal(isValidCatalogEntry(good), true);
  assert.equal(isValidCatalogEntry(null), false);
  assert.equal(isValidCatalogEntry({ ...good, id: '' }), false);
  const { disable, ...noDisable } = good;
  assert.equal(isValidCatalogEntry(noDisable), false);
});
