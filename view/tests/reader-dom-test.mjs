/**
 * Reader surface behaviour (spec scenarios: "read a Loam Markdown artifact",
 * "Reader wikilinks and broken links", plus the snapshot-mismatch banner).
 *
 * The sanitization contract lives in security-contract-test.mjs; this file is
 * about the surface around it — fresh reads, return context, outline, links,
 * and failure states.
 */

import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { afterEach, describe, it } from 'node:test';

import { JSDOM } from 'jsdom';

import { initReader, readerRoute, resolverFor } from '../public/js/reader.mjs';

const html = await readFile(new URL('../public/index.html', import.meta.url), 'utf8');
const realFetch = globalThis.fetch;

const SNAPSHOT = {
  artifacts: [
    { path: 'wiki/alpha.md', title: 'Alpha', kind: 'topic', content_hash: 'aaa' },
    { path: 'wiki/beta.md', title: 'Beta', kind: 'topic', content_hash: 'bbb' },
    { path: 'wiki/code/one.md', title: 'One', kind: 'code', content_hash: 'ccc' },
  ],
};

const DOCUMENTS = {
  'wiki/alpha.md': {
    content: [
      '---',
      'title: Alpha',
      'kind: topic',
      '---',
      '',
      '# Alpha',
      '',
      'Links to [[Beta]] and [[Nowhere]] and [one](./code/one.md).',
      '',
      'Also [escape](../../etc/passwd.md), [gone](./gone.md), and [[Beta#Detail]].',
      '',
      '## Detail',
      '',
      'Body text.',
      '',
    ].join('\n'),
    content_hash: 'aaa',
    snapshot_hash: 'aaa',
    changed_since_snapshot: false,
  },
  'wiki/beta.md': {
    content: '# Beta\n\nBack to [[Alpha]].\n',
    content_hash: 'bbb-new',
    snapshot_hash: 'bbb',
    changed_since_snapshot: true,
  },
};

function mount({ hash = '' } = {}) {
  const dom = new JSDOM(html, { url: `http://127.0.0.1:8000/${hash}` });
  const reads = [];
  let refreshes = 0;

  globalThis.fetch = async (url) => {
    const { pathname, searchParams } = new URL(url, 'http://127.0.0.1:8000');
    if (pathname === '/api/document') {
      const path = searchParams.get('path');
      reads.push(path);
      const body = DOCUMENTS[path];
      if (!body) return { ok: false, status: 400, json: async () => ({ error: 'not_inventoried' }) };
      return { ok: true, status: 200, json: async () => ({ path, ...body }) };
    }
    return { ok: false, status: 404, json: async () => ({ error: 'not_found' }) };
  };

  const reader = initReader({
    root: dom.window.document,
    getSnapshot: () => SNAPSHOT,
    refreshSnapshot: async () => { refreshes += 1; return true; },
  });
  return { dom, doc: dom.window.document, win: dom.window, reader, reads, refreshCount: () => refreshes };
}

/** The Query palette and Inspector both announce an open through an event. */
function announce(dom, detail, name = 'loam:open-reader') {
  dom.window.document.dispatchEvent(new dom.window.CustomEvent(name, { detail }));
  return new Promise((resolve) => setTimeout(resolve, 0));
}

afterEach(() => {
  globalThis.fetch = realFetch;
});

describe('reader route parsing', () => {
  it('reads path and fragment out of a reader hash', () => {
    assert.deepEqual(readerRoute('#/reader/wiki%2Falpha.md'), { path: 'wiki/alpha.md', fragment: '' });
    assert.deepEqual(readerRoute('#/reader/wiki%2Falpha.md#detail'), { path: 'wiki/alpha.md', fragment: 'detail' });
    assert.equal(readerRoute('#/atlas'), null);
    assert.equal(readerRoute(''), null);
  });
});

describe('wikilink resolution', () => {
  const resolve = resolverFor(SNAPSHOT);

  it('resolves by basename, path and title, case-insensitively', () => {
    assert.deepEqual(resolve('Alpha'), { path: 'wiki/alpha.md', title: 'Alpha' });
    assert.deepEqual(resolve('wiki/beta.md'), { path: 'wiki/beta.md', title: 'Beta' });
    assert.deepEqual(resolve('ONE'), { path: 'wiki/code/one.md', title: 'One' });
  });

  it('treats unknown and ambiguous targets as unresolved', () => {
    assert.equal(resolve('Nowhere'), null);
    const ambiguous = resolverFor({ artifacts: [{ path: 'a/x.md' }, { path: 'b/x.md' }] });
    assert.equal(ambiguous('x'), null);
  });
});

describe('reader surface', () => {
  it('opens a document, renders it, and keeps the shell covered', async () => {
    const { dom, doc, reads } = mount();
    await announce(dom, { path: 'wiki/alpha.md', title: 'Alpha' });

    assert.deepEqual(reads, ['wiki/alpha.md']);
    assert.equal(doc.querySelector('[data-reader]').hidden, false);
    assert.equal(doc.querySelector('[data-app-shell]').getAttribute('aria-hidden'), 'true');
    assert.equal(doc.querySelector('[data-reader-path]').textContent, 'wiki/alpha.md');
    assert.ok(doc.querySelector('[data-reader-doc] h1').textContent.includes('Alpha'));
    assert.equal(dom.window.location.hash, '#/reader/wiki%2Falpha.md');
  });

  it('shows front matter as compact metadata, not as document body', async () => {
    const { dom, doc } = mount();
    await announce(dom, { path: 'wiki/alpha.md' });

    const meta = doc.querySelector('[data-reader-frontmatter]');
    assert.equal(meta.hidden, false);
    assert.equal(meta.tagName.toLowerCase(), 'details', 'metadata is collapsible');
    const terms = [...meta.querySelectorAll('dt')].map((dt) => dt.textContent);
    assert.deepEqual(terms, ['title', 'kind']);
    assert.ok(!doc.querySelector('[data-reader-doc]').textContent.includes('kind: topic'));
  });

  it('builds an outline from heading anchors', async () => {
    const { dom, doc } = mount();
    await announce(dom, { path: 'wiki/alpha.md' });

    const items = [...doc.querySelectorAll('[data-reader-outline-list] a')];
    assert.deepEqual(items.map((a) => a.textContent), ['Alpha', 'Detail']);
    for (const link of items) {
      assert.ok(doc.querySelector(`[data-reader-doc] [id="${link.getAttribute('href').slice(1)}"]`),
        'every outline entry points at a heading that exists');
    }
  });

  it('marks resolvable wikilinks green-capable and broken ones visibly broken', async () => {
    const { dom, doc } = mount();
    await announce(dom, { path: 'wiki/alpha.md' });

    const resolved = doc.querySelector('[data-reader-doc] a.wikilink.is-resolved');
    assert.equal(resolved.getAttribute('href'), '#/reader/wiki%2Fbeta.md');
    const broken = doc.querySelector('[data-reader-doc] .wikilink.is-broken');
    assert.equal(broken.tagName.toLowerCase(), 'span');
    assert.ok(/unresolved/i.test(broken.textContent));
    const relative = [...doc.querySelectorAll('[data-reader-doc] a')].find((a) => a.textContent === 'one');
    assert.equal(relative.getAttribute('href'), '#/reader/wiki%2Fcode%2Fone.md');
  });

  it('marks out-of-root and missing document links as broken, with the reason in text', async () => {
    const { dom, doc } = mount();
    await announce(dom, { path: 'wiki/alpha.md' });

    const broken = [...doc.querySelectorAll('[data-reader-doc] .wikilink.is-broken')];
    const escaping = broken.find((node) => node.textContent.startsWith('escape'));
    assert.ok(escaping, 'an out-of-root link is never offered as a link');
    assert.match(escaping.textContent, /leaves the workspace/);
    const missing = broken.find((node) => node.textContent.startsWith('gone'));
    assert.match(missing.textContent, /missing document/);
    assert.equal(doc.querySelector('[data-reader-doc] a[href*="passwd"]'), null);
  });

  it('carries a wikilink heading fragment into the reader route', async () => {
    const { dom, doc } = mount();
    await announce(dom, { path: 'wiki/alpha.md' });
    const link = [...doc.querySelectorAll('[data-reader-doc] a.wikilink')]
      .find((a) => a.getAttribute('href').includes('%23') || a.getAttribute('href').includes('#detail'));
    assert.equal(link.getAttribute('href'), '#/reader/wiki%2Fbeta.md#detail');
  });

  it('navigates between documents through hash history, re-reading each time', async () => {
    const { dom, win, doc, reads } = mount();
    await announce(dom, { path: 'wiki/alpha.md' });

    win.location.hash = '#/reader/wiki%2Fbeta.md';
    await new Promise((resolve) => setTimeout(resolve, 0));
    assert.deepEqual(reads, ['wiki/alpha.md', 'wiki/beta.md']);
    assert.ok(doc.querySelector('[data-reader-doc] h1').textContent.includes('Beta'));

    win.location.hash = '#/reader/wiki%2Falpha.md';
    await new Promise((resolve) => setTimeout(resolve, 0));
    assert.deepEqual(reads, ['wiki/alpha.md', 'wiki/beta.md', 'wiki/alpha.md'], 'every open is a fresh read');
  });

  it('shows the changed-since-snapshot banner and refreshes from it', async () => {
    const { dom, doc, reads, refreshCount } = mount();
    await announce(dom, { path: 'wiki/beta.md' });

    const banner = doc.querySelector('[data-reader-banner]');
    assert.equal(banner.hidden, false);
    assert.match(banner.textContent, /Changed since snapshot/);

    doc.querySelector('[data-reader-refresh]').click();
    await new Promise((resolve) => setTimeout(resolve, 0));
    assert.equal(refreshCount(), 1, 'Refresh reruns the snapshot');
    assert.deepEqual(reads, ['wiki/beta.md', 'wiki/beta.md'], 'and re-reads the document from disk');
  });

  it('returns to the exact originating context on Back', async () => {
    const { dom, doc, win } = mount({ hash: '#/atlas' });
    const inspectorState = { view: 'atlas', node: 'wiki/alpha.md', inspector: { tab: 'evidence' } };
    const closed = [];
    doc.addEventListener('loam:reader-closed', (event) => closed.push(event.detail));

    await announce(dom, { path: 'wiki/alpha.md', return: inspectorState });
    assert.equal(win.location.hash, '#/reader/wiki%2Falpha.md');

    doc.querySelector('[data-reader-back]').click();
    assert.equal(win.location.hash, '#/atlas', 'the originating route comes back');
    assert.equal(doc.querySelector('[data-reader]').hidden, true);
    assert.equal(doc.querySelector('[data-app-shell]').hasAttribute('aria-hidden'), false);
    assert.deepEqual(closed, [inspectorState], 'the opener state is handed back untouched');
  });

  it('takes the shell out of the tab order and moves focus into Reader', async () => {
    const { dom, doc } = mount({ hash: '#/pulse' });
    const shell = doc.querySelector('[data-app-shell]');
    const invoker = doc.querySelector('[data-refresh]');
    invoker.focus();

    await announce(dom, { path: 'wiki/alpha.md' });
    assert.equal(shell.hasAttribute('inert'), true, 'the covered shell must be inert, not just aria-hidden');
    assert.ok(
      doc.querySelector('[data-reader]').contains(doc.activeElement),
      'focus must move inside Reader, not stay on the covered shell',
    );

    doc.querySelector('[data-reader-back]').click();
    assert.equal(shell.hasAttribute('inert'), false, 'Back must hand the shell back');
    assert.equal(doc.activeElement, invoker, 'Back must restore focus to the control that opened Reader');
  });

  it('moves focus into Reader even when the document cannot be read', async () => {
    const { dom, doc } = mount({ hash: '#/pulse' });
    await announce(dom, { path: 'wiki/missing.md' });
    assert.ok(
      doc.querySelector('[data-reader]').contains(doc.activeElement),
      'an error state must still be reachable by keyboard',
    );
    assert.match(doc.querySelector('[data-reader-status]').textContent, /could not be read/);
  });

  it('accepts the Query palette event name as the same contract', async () => {
    const { dom, doc } = mount();
    await announce(dom, { path: 'wiki/alpha.md' }, 'loam:open-document');
    assert.equal(doc.querySelector('[data-reader]').hidden, false);
  });

  it('closes on Escape', async () => {
    const { dom, doc } = mount({ hash: '#/pulse' });
    await announce(dom, { path: 'wiki/alpha.md' });
    doc.querySelector('[data-reader]').dispatchEvent(
      new dom.window.KeyboardEvent('keydown', { key: 'Escape', bubbles: true }),
    );
    assert.equal(doc.querySelector('[data-reader]').hidden, true);
  });

  it('reports unreadable and out-of-root documents instead of failing silently', async () => {
    const { dom, doc } = mount();
    await announce(dom, { path: '../outside.md' });

    assert.equal(doc.querySelector('[data-reader]').hidden, false, 'Reader stays open with the failure visible');
    assert.match(doc.querySelector('[data-reader-status]').textContent, /could not be read/i);
    assert.match(doc.querySelector('[data-reader-status]').textContent, /not_inventoried/);
    assert.equal(doc.querySelector('[data-reader-doc]').childElementCount, 0);
  });

  it('opens straight into a document from a deep link', async () => {
    const { doc, reads } = mount({ hash: '#/reader/wiki%2Falpha.md' });
    await new Promise((resolve) => setTimeout(resolve, 0));
    assert.deepEqual(reads, ['wiki/alpha.md']);
    assert.equal(doc.querySelector('[data-reader]').hidden, false);
  });
});
