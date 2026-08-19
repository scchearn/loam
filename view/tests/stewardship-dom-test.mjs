/**
 * Stewardship — the trust room.
 *
 * The fixture below is the spec's Stewardship scenario: memory with a stale
 * lint marker, broken wikilinks, archived pages, and code-graph drift, plus a
 * healthy retrieval signal and a domain nothing reported on. The assertions are
 * about honesty as much as layout: each emitted signal keeps its own state word,
 * actionable cards carry exactly one copy-prompt control, healthy and neutral
 * cards carry none, and a domain with no emitted finding says `unknown` rather
 * than inventing a verdict.
 */

import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { afterEach, describe, it } from 'node:test';

import { JSDOM } from 'jsdom';

import { boot } from '../public/js/app.mjs';
import { initInspector } from '../public/js/inspector.mjs';
import { state } from '../public/js/store.mjs';
import { initStewardship } from '../public/js/views/stewardship.mjs';

const PUBLIC_ROOT = new URL('../public/', import.meta.url);
const html = await readFile(new URL('index.html', PUBLIC_ROOT), 'utf8');

const readyFull = JSON.parse(
  await readFile(new URL('fixtures/snapshots/valid/ready-full.json', import.meta.url), 'utf8'),
);
const sparse = JSON.parse(
  await readFile(new URL('fixtures/snapshots/valid/not-configured-sparse.json', import.meta.url), 'utf8'),
);

const realFetch = globalThis.fetch;
const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

function jsonResponse(body, status = 200) {
  return { ok: status >= 200 && status < 300, status, json: async () => body };
}

/** Boot the real shell with the real Inspector and Stewardship, exactly as main.mjs does. */
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
  initStewardship({ root: doc });
  await boot(doc);
  await flush();

  return { dom, doc, copied, mount: doc.querySelector('[data-mount="stewardship"]') };
}

afterEach(() => {
  globalThis.fetch = realFetch;
  delete globalThis.document;
});

/** An inventoried artifact: evidence pointing here is an Inspector entry point. */
const INVENTORIED = readyFull.artifacts.find((artifact) => artifact.kind === 'code').path;

/** The spec scenario: stale, broken, archived, and drift cases in one snapshot. */
function trustSnapshot(overrides = {}) {
  return {
    ...readyFull,
    metrics: {
      ...readyFull.metrics,
      'wiki.broken_wikilinks': { value: 3, unit: 'count', state: 'ready', evidence: null },
      'wiki.archived_pages': { value: 2, unit: 'count', state: 'ready', evidence: null },
      'code.stale': { value: 12, unit: 'count', state: 'ready', evidence: null },
    },
    signals: [
      {
        id: 'memory-lint',
        state: 'watch',
        message: 'No lint marker found in wiki/log.md.',
        evidence: { path: 'wiki/log.md' },
        command: '/loam::linting-memory',
      },
      {
        id: 'wikilink-health',
        state: 'critical',
        message: '3 broken wikilinks in memory.',
        evidence: [{ path: INVENTORIED }, { path: 'wiki/topics/ghost-topic.md' }],
        command: '/loam::amending-memory',
      },
      {
        id: 'code-graph-drift',
        state: 'watch',
        message: '12 code pages are stale.',
        evidence: { path: 'view/public/js/app.mjs' },
        command: '/loam::syncing-code-graph',
      },
      {
        id: 'retrieval',
        state: 'healthy',
        message: 'The qmd collection is ready and mapped to this workspace.',
        evidence: null,
        command: null,
      },
    ],
    hints: [
      {
        kind: 'archive-state',
        group: 'memory',
        severity: 'info',
        message: '2 archived pages recorded under wiki/.archive/.',
        command: null,
        evidence: { path: 'wiki/.archive' },
      },
      {
        kind: 'log-rotation',
        group: 'memory',
        severity: 'warn',
        message: 'wiki/log.md is long enough to rotate.',
        command: '/loam::normalizing-memory',
        evidence: { path: 'wiki/log.md' },
      },
    ],
    ...overrides,
  };
}

const cards = (root) => [...root.querySelectorAll('[data-stewardship-card]')];
const cardFor = (root, id) => root.querySelector(`[data-stewardship-card="${id}"]`);
const badgeText = (card) => card.querySelector('.badge')?.textContent ?? '';

describe('Stewardship signal mapping', () => {
  it('maps every emitted signal to a card with its own state word and severity', async () => {
    const { mount: root } = await mount(trustSnapshot());

    for (const [id, severity, message] of [
      ['memory-lint', 'watch', 'No lint marker found in wiki/log.md.'],
      ['wikilink-health', 'critical', '3 broken wikilinks in memory.'],
      ['code-graph-drift', 'watch', '12 code pages are stale.'],
      ['retrieval', 'healthy', 'The qmd collection is ready and mapped to this workspace.'],
    ]) {
      const card = cardFor(root, id);
      assert.ok(card, `${id} should render a card`);
      assert.equal(card.dataset.severity, severity);
      // The badge text is the emitted word: colour is never the only encoding.
      assert.equal(badgeText(card).toLowerCase(), severity);
      assert.equal(card.querySelector('.issue-title')?.textContent, message);
      assert.ok(card.querySelector('.issue-cat'), `${id} should carry a category label`);
      assert.ok(card.querySelector('.issue-cat .icon'), `${id} should carry a category icon`);
    }

    assert.equal(cardFor(root, 'wikilink-health').classList.contains('critical'), true);
    assert.equal(cardFor(root, 'code-graph-drift').classList.contains('warn'), true);
    assert.equal(cardFor(root, 'retrieval').classList.contains('critical'), false);
  });

  it('renders emitted hints beside signals, with their routing severity mapped', async () => {
    const { mount: root } = await mount(trustSnapshot());

    const archive = cardFor(root, 'archive-state');
    assert.ok(archive, 'the archive hint should render a card');
    assert.equal(archive.dataset.severity, 'neutral');

    const rotation = cardFor(root, 'log-rotation');
    assert.ok(rotation, 'a hint outside the fixed domains still renders');
    assert.equal(rotation.dataset.severity, 'watch');
    assert.equal(rotation.querySelector('.issue-title')?.textContent, 'wiki/log.md is long enough to rotate.');
  });

  it('renders every emitted signal and hint exactly once', async () => {
    const snapshot = trustSnapshot();
    const { mount: root } = await mount(snapshot);

    const emitted = [
      ...snapshot.signals.map((signal) => signal.id),
      ...snapshot.hints.map((hint) => hint.kind),
    ];
    for (const id of emitted) {
      assert.equal(
        root.querySelectorAll(`[data-stewardship-card="${id}"]`).length,
        1,
        `${id} should appear exactly once`,
      );
    }
  });

  it('shows the optional count metric a domain has, and none where the snapshot has no count', async () => {
    const { mount: root } = await mount(trustSnapshot());

    assert.equal(cardFor(root, 'wikilink-health').querySelector('.issue-metric')?.textContent, '3');
    assert.equal(cardFor(root, 'code-graph-drift').querySelector('.issue-metric')?.textContent, '12');
    assert.equal(cardFor(root, 'archive-state').querySelector('.issue-metric')?.textContent, '2');
    assert.equal(cardFor(root, 'retrieval').querySelector('.issue-metric'), null);
  });
});

describe('Stewardship copy-prompt affordance', () => {
  it('gives every actionable card exactly one control carrying its mapped loam skill', async () => {
    const { mount: root } = await mount(trustSnapshot());

    for (const [id, skill, message] of [
      ['memory-lint', 'linting-memory', 'No lint marker found in wiki/log.md.'],
      ['wikilink-health', 'amending-memory', '3 broken wikilinks in memory.'],
      ['code-graph-drift', 'syncing-code-graph', '12 code pages are stale.'],
      ['log-rotation', 'normalizing-memory', 'wiki/log.md is long enough to rotate.'],
    ]) {
      const card = cardFor(root, id);
      const controls = card.querySelectorAll('[data-copy-prompt]');
      assert.equal(controls.length, 1, `${id} should expose exactly one copy control`);
      assert.equal(controls[0].dataset.copyPrompt, `Run loam ${skill} to address: ${message}`);
    }
  });

  it('gives healthy and non-actionable cards no copy control at all', async () => {
    const { mount: root } = await mount(trustSnapshot());

    for (const id of ['retrieval', 'archive-state']) {
      const card = cardFor(root, id);
      assert.equal(card.querySelectorAll('[data-copy-prompt]').length, 0, `${id} should offer no action`);
    }

    // Nothing anywhere in the room invents a control for a null command.
    for (const card of cards(root).filter((node) => node.dataset.command === '')) {
      assert.equal(card.querySelectorAll('[data-copy-prompt]').length, 0);
    }
  });

  it('copies the prompt, confirms on the control, and toasts paste guidance', async () => {
    const { doc, mount: root, copied } = await mount(trustSnapshot());

    const control = cardFor(root, 'wikilink-health').querySelector('[data-copy-prompt]');
    control.click();
    await flush();

    assert.deepEqual(copied, ['Run loam amending-memory to address: 3 broken wikilinks in memory.']);
    assert.ok(control.classList.contains('copied'), 'the control should confirm the copy');

    const toast = doc.querySelector('[data-toast]');
    assert.ok(toast, 'a transient toast should appear');
    assert.match(toast.textContent, /paste/i);
  });
});

describe('Stewardship evidence', () => {
  it('opens the Inspector for inventoried evidence and leaves source paths plain', async () => {
    const { doc, mount: root } = await mount(trustSnapshot());

    const card = cardFor(root, 'wikilink-health');
    const inventoried = card.querySelector(`[data-inspect="${INVENTORIED}"]`);
    assert.ok(inventoried, 'an inventoried evidence path is an Inspector entry point');

    const drift = cardFor(root, 'code-graph-drift');
    assert.equal(drift.querySelector('[data-inspect]'), null, 'a source path offers no door');
    const chip = drift.querySelector('code.code-chip');
    assert.equal(chip?.textContent, 'view/public/js/app.mjs');

    inventoried.click();
    await flush();
    assert.equal(doc.querySelector('[data-inspector]').getAttribute('aria-hidden'), 'false');
  });
});

describe('Stewardship conservation-status tone', () => {
  it('covers every trust domain, even the ones this snapshot said nothing about', async () => {
    const { mount: root } = await mount(trustSnapshot());

    for (const domain of ['freshness', 'code-graph', 'wikilinks', 'questions', 'archive', 'retrieval']) {
      assert.ok(
        root.querySelector(`[data-stewardship-domain="${domain}"]`),
        `the ${domain} domain should be represented`,
      );
    }
  });

  it('says unknown for a domain nothing reported on, and offers no action there', async () => {
    const { mount: root } = await mount(trustSnapshot());

    const questions = root.querySelector('[data-stewardship-domain="questions"]');
    assert.equal(questions.dataset.severity, 'unknown');
    assert.equal(badgeText(questions).toLowerCase(), 'unknown');
    assert.equal(questions.querySelectorAll('[data-copy-prompt]').length, 0);
    assert.equal(questions.classList.contains('critical'), false, 'unknown is not an error');
  });

  it('renders an absent optional capability as neutral, not as a failure', async () => {
    const { mount: root } = await mount(trustSnapshot({
      signals: [],
      hints: [],
      capabilities: {
        ...readyFull.capabilities,
        qmd: { state: 'absent', required: false, reason: 'no qmd config found', evidence: null },
      },
    }));

    const retrieval = root.querySelector('[data-stewardship-domain="retrieval"]');
    assert.equal(retrieval.dataset.severity, 'neutral');
    assert.equal(badgeText(retrieval).toLowerCase(), 'absent');
    assert.equal(retrieval.classList.contains('critical'), false);
    assert.equal(retrieval.querySelectorAll('[data-copy-prompt]').length, 0);
  });

  it('shows the emitted posture, and falls back to unknown when the snapshot carries none', async () => {
    const withPosture = await mount(trustSnapshot({ posture: 'needs-review' }));
    assert.match(
      withPosture.mount.querySelector('[data-stewardship-summary]').textContent,
      /Needs review/,
    );

    // readyFull now carries the T6 top-level posture; drop it to exercise the fallback.
    const { posture: _posture, ...postureless } = trustSnapshot();
    const withoutPosture = await mount(postureless);
    assert.equal(withoutPosture.mount.querySelector('[data-stewardship-posture]').textContent, 'Unknown');
  });

  it('holds its shape on a sparse workspace: domains present, no invented findings', async () => {
    const { mount: root } = await mount(sparse);

    assert.ok(cards(root).length >= 6, 'every domain still renders');
    // The one emitted finding stays critical; the unmeasured domains do not
    // borrow its colour — an absent optional substrate is neutral, not a fault.
    assert.equal(cardFor(root, 'no-memory').dataset.severity, 'critical');
    assert.equal(root.querySelectorAll('[data-stewardship-domain].critical').length, 0);
    for (const card of cards(root)) {
      assert.ok(
        ['healthy', 'watch', 'critical', 'neutral', 'unknown'].includes(card.dataset.severity),
        `unexpected severity ${card.dataset.severity}`,
      );
    }
  });
});
