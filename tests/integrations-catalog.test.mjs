import assert from 'node:assert/strict';
import { test } from 'node:test';

import {
  CATALOG,
  catalogEntry,
  isValidCatalogEntry,
  CATALOG_ENTRY_CONTRACT,
} from '../setup/integrations/catalog.mjs';

// Unit A ships the catalog as a declared-but-EMPTY registry seam. Unit B fills
// it. This test pins the SEAM SHAPE so Unit B's entries plug in without any
// configurator change and so a malformed entry is caught.

test('the catalog is an empty array seam in Unit A', () => {
  assert.ok(Array.isArray(CATALOG));
  assert.equal(CATALOG.length, 0, 'Unit A ships zero catalog entries');
});

test('catalogEntry returns undefined for an unknown id', () => {
  assert.equal(catalogEntry('qmd'), undefined);
  assert.equal(catalogEntry('grep'), undefined);
});

test('the entry contract names the required fields and lifecycle methods', () => {
  assert.deepEqual([...CATALOG_ENTRY_CONTRACT.fields], ['id', 'label', 'capability']);
  assert.deepEqual([...CATALOG_ENTRY_CONTRACT.methods], ['enable', 'disable', 'verify']);
});

test('isValidCatalogEntry accepts a well-formed entry and rejects malformed ones', () => {
  const good = {
    id: 'qmd',
    label: 'QMD',
    capability: 'markdown-search',
    enable: async () => ({ ready: true }),
    disable: async () => ({ ready: true }),
    verify: async () => ({ ready: true }),
  };
  assert.equal(isValidCatalogEntry(good), true);

  assert.equal(isValidCatalogEntry(null), false);
  assert.equal(isValidCatalogEntry({ ...good, id: '' }), false);
  assert.equal(isValidCatalogEntry({ ...good, capability: 42 }), false);
  const { disable, ...noDisable } = good;
  assert.equal(isValidCatalogEntry(noDisable), false, 'symmetric-disable: an entry MUST provide disable');
  const { verify, ...noVerify } = good;
  assert.equal(isValidCatalogEntry(noVerify), false, 'verify is required to confirm absence after disable');
});

test('every shipped catalog entry (none in Unit A) satisfies the contract', () => {
  for (const entry of CATALOG) assert.equal(isValidCatalogEntry(entry), true, `invalid entry: ${entry?.id}`);
});
