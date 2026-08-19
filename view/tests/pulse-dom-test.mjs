import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { afterEach, describe, it } from 'node:test';

import { JSDOM } from 'jsdom';

import { boot } from '../public/js/app.mjs';
import { initInspector } from '../public/js/inspector.mjs';
import { state } from '../public/js/store.mjs';
import { initPulse } from '../public/js/views/pulse.mjs';
import { promptFor } from '../public/js/copy-prompt.mjs';

const PUBLIC_ROOT = new URL('../public/', import.meta.url);
const html = await readFile(new URL('index.html', PUBLIC_ROOT), 'utf8');

const readyFull = JSON.parse(
  await readFile(new URL('fixtures/snapshots/valid/ready-full.json', import.meta.url), 'utf8'),
);
const sparse = JSON.parse(
  await readFile(new URL('fixtures/snapshots/valid/not-configured-sparse.json', import.meta.url), 'utf8'),
);

const realFetch = globalThis.fetch;

function jsonResponse(body, status = 200) {
  return { ok: status >= 200 && status < 300, status, json: async () => body };
}

const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

/** Boot the real shell with the real Inspector and Pulse wired exactly as main.mjs does. */
async function mount(snapshot) {
  const dom = new JSDOM(html, { url: 'http://127.0.0.1:8000/' });
  globalThis.document = dom.window.document;

  const copied = [];
  dom.window.navigator.clipboard = { writeText: async (text) => void copied.push(text) };

  globalThis.fetch = async (url) => {
    const { pathname } = new URL(url, 'http://127.0.0.1:8000');
    if (pathname === '/api/snapshot') return jsonResponse(snapshot);
    return jsonResponse({ error: 'not_found' }, 404);
  };

  state.snapshot = null;
  state.error = null;
  state.refreshing = false;

  const doc = dom.window.document;
  initInspector({ root: doc, getSnapshot: () => state.snapshot });
  initPulse({ root: doc });
  await boot(doc);
  await flush();

  return { dom, doc, copied, mount: doc.querySelector('[data-mount="pulse"]') };
}

afterEach(() => {
  globalThis.fetch = realFetch;
  delete globalThis.document;
});

const CHECKPOINT = {
  id: 'wiki/checkpoints/checkpoint-2026-08-01-1200.md',
  path: 'wiki/checkpoints/checkpoint-2026-08-01-1200.md',
  kind: 'checkpoint',
  title: 'Review the CLI output experience before execution begins',
  lifecycle_status: null,
  created_at: '2026-08-01T12:00:00+02:00',
  updated_at: '2026-08-01T12:00:00+02:00',
  captured_at: '2026-08-01T12:00:00+02:00',
  content_hash: '7016e77dc72f05f3dfe13a0b1dd3369d44ec7b30a4a7ac4b7b425d394222d085',
  bytes: 1024,
  attributes: { reason: 'pause' },
  parse_errors: [],
};

/** ready-full plus the actionable/healthy mix the copy-prompt scenario needs. */
function briefingSnapshot(overrides = {}) {
  return {
    ...readyFull,
    posture: 'needs-review',
    artifacts: [...readyFull.artifacts, CHECKPOINT],
    metrics: {
      ...readyFull.metrics,
      'wiki.concepts': { value: 0, unit: 'count', state: 'ready', evidence: null },
      'wiki.broken_wikilinks': { value: 2, unit: 'count', state: 'ready', evidence: null },
      'work.goals': { value: 0, unit: 'count', state: 'ready', evidence: null },
      'checkpoints.total': { value: 1, unit: 'count', state: 'ready', evidence: null },
      'checkpoints.actionable': { value: 1, unit: 'count', state: 'ready', evidence: null },
    },
    signals: [
      {
        id: 'goal-traceability',
        state: 'critical',
        message: 'Active specs and plans have no goal provenance.',
        evidence: { path: 'specs/example-spec.md' },
        command: '/loam::setting-goals',
      },
      {
        id: 'code-graph-drift',
        state: 'healthy',
        message: 'No stale, new, or orphan code pages.',
        evidence: null,
        command: null,
      },
      {
        id: 'checkpoint-state',
        state: 'watch',
        message: 'One workstream is ready to resume.',
        evidence: { path: CHECKPOINT.path },
        command: '/loam::resuming',
      },
    ],
    hints: [
      {
        kind: 'lint-stale',
        group: 'memory',
        severity: 'warn',
        message: 'wiki/log.md has no lint-check marker.',
        command: '/loam::linting-memory',
        evidence: { path: 'wiki/log.md' },
      },
    ],
    ...overrides,
  };
}

const cards = (root) => [...root.querySelectorAll('[data-pulse-card]')];
const cardFor = (root, id) => root.querySelector(`[data-pulse-card="${id}"]`);
const tileValue = (root, name) =>
  root.querySelector(`[data-pulse-tile="${name}"] .tile-value`)?.textContent;
const metricValue = (root, name) =>
  root.querySelector(`[data-pulse-metric="${name}"] .metric-value`)?.textContent;

describe('copy-prompt payload derivation', () => {
  it('strips the /loam:: namespace and lets the message supply the purpose', () => {
    assert.equal(
      promptFor('/loam::setting-goals', 'Active specs and plans have no goal provenance.'),
      'Run loam setting-goals to address: Active specs and plans have no goal provenance.',
    );
  });

  it('returns null for a null command, so no control can be built', () => {
    assert.equal(promptFor(null, 'No stale, new, or orphan code pages.'), null);
    assert.equal(promptFor('', 'anything'), null);
  });
});

describe('Pulse copy-prompt affordance', () => {
  it('gives every actionable card exactly one copy control carrying its mapped prompt', async () => {
    const { mount: root } = await mount(briefingSnapshot());

    for (const [id, command, message] of [
      ['goal-traceability', 'setting-goals', 'Active specs and plans have no goal provenance.'],
      ['checkpoint-state', 'resuming', 'One workstream is ready to resume.'],
      ['lint-stale', 'linting-memory', 'wiki/log.md has no lint-check marker.'],
    ]) {
      const card = cardFor(root, id);
      assert.ok(card, `${id} should render an advisor card`);
      const controls = card.querySelectorAll('[data-copy-prompt]');
      assert.equal(controls.length, 1, `${id} should expose exactly one copy control`);
      assert.equal(
        controls[0].dataset.copyPrompt,
        `Run loam ${command} to address: ${message}`,
      );
    }
  });

  it('gives healthy, non-actionable cards no copy control at all', async () => {
    const { mount: root } = await mount(briefingSnapshot());

    const healthy = cardFor(root, 'code-graph-drift');
    assert.ok(healthy, 'the healthy signal should still render as a card');
    assert.equal(healthy.dataset.severity, 'healthy');
    assert.equal(healthy.querySelectorAll('[data-copy-prompt]').length, 0);

    // And nothing anywhere on Pulse invents a control for a null command.
    const withoutCommand = cards(root).filter((card) => card.dataset.command === '');
    for (const card of withoutCommand) {
      assert.equal(card.querySelectorAll('[data-copy-prompt]').length, 0);
    }
  });

  it('writes the prompt to the clipboard, confirms, and toasts paste guidance', async () => {
    const { doc, mount: root, copied } = await mount(briefingSnapshot());

    const control = cardFor(root, 'goal-traceability').querySelector('[data-copy-prompt]');
    control.click();
    await flush();

    assert.deepEqual(copied, [
      'Run loam setting-goals to address: Active specs and plans have no goal provenance.',
    ]);
    assert.ok(control.classList.contains('copied'), 'the control should confirm the copy');

    const toast = doc.querySelector('[data-toast]');
    assert.ok(toast, 'a transient toast should appear');
    assert.equal(toast.getAttribute('role'), 'status');
    assert.match(toast.textContent, /paste/i);
  });
});

describe('Pulse briefing composition', () => {
  it('renders the overview box, the metrics band, the advisor band, and the return point', async () => {
    const { mount: root } = await mount(briefingSnapshot());

    assert.ok(root.querySelector('[data-pulse-overview].panel-dotted'), 'dotted overview box');
    assert.ok(root.querySelector('[data-pulse-metrics]'), 'evidence-metrics band');
    // The metrics row scrolls sideways and holds no control of its own, so it
    // needs its own tab stop or the cards past the fold are keyboard-only dead.
    assert.equal(root.querySelector('[data-pulse-metrics] .card-row').tabIndex, 0);
    assert.ok(root.querySelector('[data-pulse-advisor]'), 'advisor band');
    assert.ok(root.querySelector('[data-pulse-focus]'), 'current return point');

    assert.equal(
      root.querySelector('[data-pulse-overview] h2, [data-pulse-overview] [data-project-name]')
        ?.textContent,
      'loam',
    );
    assert.match(root.querySelector('[data-pulse-overview]').textContent, /main/);
    assert.match(root.querySelector('[data-pulse-overview]').textContent, /\u2318K/);

    assert.equal(tileValue(root, 'posture'), 'Needs review');
    assert.equal(tileValue(root, 'coverage'), '66.3%');
    assert.equal(tileValue(root, 'goals'), '0');
    assert.equal(tileValue(root, 'concepts'), '0');

    assert.equal(metricValue(root, 'memory'), '3');
    assert.equal(metricValue(root, 'checkpoints'), '1');

    assert.ok(
      root.querySelector('[data-pulse-advisor] [data-open-stewardship]'),
      'the advisor band offers an Open Stewardship action',
    );
  });

  it('routes to Stewardship from the advisor band without reimplementing routing', async () => {
    const { doc, mount: root } = await mount(briefingSnapshot());

    root.querySelector('[data-pulse-advisor] [data-open-stewardship]').click();

    assert.ok(doc.querySelector('[data-view="stewardship"]').classList.contains('is-active'));
  });

  it('builds the return point from the latest actionable checkpoint', async () => {
    const { mount: root } = await mount(briefingSnapshot());

    const focus = root.querySelector('[data-pulse-focus]');
    assert.match(focus.textContent, /Review the CLI output experience/);
    assert.equal(focus.querySelectorAll('[data-copy-prompt]').length, 1);
    assert.equal(
      focus.querySelector('[data-copy-prompt]').dataset.copyPrompt,
      'Run loam resuming to address: One workstream is ready to resume.',
    );
  });

  it('shows each fact once: no substrate counts in the overview, no freshness in the view', async () => {
    const { mount: root } = await mount(briefingSnapshot());

    const overview = root.querySelector('[data-pulse-overview]').textContent;
    assert.doesNotMatch(overview, /memory page|knowledge page/i);
    assert.doesNotMatch(overview, /Snapshot 20/);
    assert.doesNotMatch(root.textContent, /Snapshot 2026-07-17/);

    const tiles = [...root.querySelectorAll('[data-pulse-tile]')].map((t) => t.dataset.pulseTile);
    assert.deepEqual(tiles, ['posture', 'coverage', 'goals', 'concepts', 'checkpoint', 'retrieval']);
  });
});

describe('Pulse evidence paths', () => {
  it('makes an inventoried artifact path an Inspector entry point', async () => {
    const { doc, mount: root } = await mount(briefingSnapshot());

    const link = cardFor(root, 'checkpoint-state').querySelector(`[data-path="${CHECKPOINT.path}"]`);
    assert.ok(link, 'an inventoried evidence path should be actionable');
    assert.equal(link.tagName, 'BUTTON');

    link.click();
    assert.ok(doc.querySelector('[data-inspector]').classList.contains('is-open'));
  });

  it('leaves a non-inventoried path as plain text', async () => {
    const { mount: root } = await mount(briefingSnapshot());

    const card = cardFor(root, 'lint-stale');
    assert.match(card.textContent, /wiki\/log\.md/);
    assert.equal(card.querySelector('[data-path="wiki/log.md"]'), null);
  });
});

describe('Pulse honest unknown and unavailable rendering', () => {
  it('renders a dash for a missing metric and names the state, never a number', async () => {
    const { mount: root } = await mount(
      briefingSnapshot({
        posture: undefined,
        metrics: {
          'code.coverage_percent': { value: null, unit: 'percent', state: 'unavailable', evidence: null },
          'wiki.knowledge_pages': { value: null, unit: 'count', state: 'unknown', evidence: null },
        },
      }),
    );

    assert.equal(tileValue(root, 'posture'), 'Unknown');
    assert.equal(tileValue(root, 'coverage'), 'Unavailable');
    assert.equal(tileValue(root, 'goals'), '—');
    assert.equal(metricValue(root, 'memory'), '—');
    assert.doesNotMatch(root.querySelector('[data-pulse-metrics]').textContent, /\b0\b/);
  });

  it('shows honest empty states with exact next actions on a sparse workspace', async () => {
    const { mount: root } = await mount(sparse);

    // T6 landed the top-level posture field: the sparse fixture now carries
    // "not-configured", which outranks the pre-posture "Unknown" fallback.
    assert.equal(tileValue(root, 'posture'), 'Not configured');
    assert.equal(tileValue(root, 'coverage'), '—');

    const card = cardFor(root, 'no-memory');
    assert.ok(card, 'the no-memory hint should render as the next action');
    assert.equal(
      card.querySelector('[data-copy-prompt]').dataset.copyPrompt,
      'Run loam scaffolding-wiki to address: No wiki/ memory substrate found for this workspace.',
    );

    assert.match(root.querySelector('[data-pulse-focus]').textContent, /No actionable checkpoint/i);
    assert.doesNotMatch(root.querySelector('[data-pulse-metrics]').textContent, /\b0\b/);
  });
});
