import assert from 'node:assert/strict';
import { test } from 'node:test';

import { optionalIntegrationSummary, selectIntegrations } from '../setup/wizard.mjs';

test('an empty integration catalog produces no summary or prompt', async () => {
  let prompted = false;
  assert.equal(optionalIntegrationSummary([], []), '');
  assert.deepEqual(await selectIntegrations({
    catalog: [],
    select: async () => {
      prompted = true;
      return ['qmd'];
    },
  }), []);
  assert.equal(prompted, false);
});

test('integration summary tolerates missing labels and capabilities', () => {
  const summary = optionalIntegrationSummary([
    { id: 'qmd' },
    { id: 'grep', label: 'grep code search' },
    { id: 'hcom', capability: 'agent messaging' },
  ], []);

  assert.match(summary, /Optional integrations — enable anytime:/u);
  assert.match(summary, /setup --integration qmd/);
  assert.match(summary, /setup --integration grep/);
  assert.match(summary, /setup --integration hcom/);
  assert.doesNotMatch(summary, /undefined/);
});

test('integration summary does not suggest an integration already enabled by an earlier install', () => {
  const summary = optionalIntegrationSummary([
    { id: 'qmd', label: 'QMD markdown search', capability: 'markdown-search' },
    { id: 'grep', label: 'grep code search', capability: 'code-search' },
  ], [], ['qmd']);

  assert.doesNotMatch(summary, /setup --integration qmd/);
  assert.match(summary, /setup --integration grep/);
});

test('integration selection uses the injected selector and exposes catalog options', async () => {
  const selected = await selectIntegrations({
    catalog: [
      { id: 'grep', label: 'grep code search', capability: 'code-search' },
      { id: 'qmd', label: 'QMD markdown search', capability: 'markdown-search' },
    ],
    select: async ({ message, options, initialValues, required }) => {
      assert.equal(message, 'Enable optional integrations');
      assert.deepEqual(options.map(({ value, label }) => ({ value, label })), [
        { value: 'grep', label: 'grep code search' },
        { value: 'qmd', label: 'QMD markdown search' },
      ]);
      assert.deepEqual(initialValues, []);
      assert.equal(required, false);
      return ['qmd'];
    },
  });

  assert.deepEqual(selected, ['qmd']);
});
