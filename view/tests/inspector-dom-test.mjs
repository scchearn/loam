import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { afterEach, describe, it } from 'node:test';

import { JSDOM } from 'jsdom';

import { boot } from '../public/js/app.mjs';
import { initInspector, openInspector } from '../public/js/inspector.mjs';
import { summarize } from '../public/js/summary.mjs';
import { state } from '../public/js/store.mjs';

const PUBLIC_ROOT = new URL('../public/', import.meta.url);

const html = await readFile(new URL('index.html', PUBLIC_ROOT), 'utf8');
const baseSnapshot = JSON.parse(
  await readFile(new URL('fixtures/snapshots/valid/ready-full.json', import.meta.url), 'utf8'),
);

const realFetch = globalThis.fetch;

/**
 * The shipped fixture's relationships point at artifacts it does not inventory,
 * which is the honest shape of a real snapshot. These extra artifacts make the
 * "resolvable references become Reader entry points, unresolvable ones stay
 * plain text" distinction testable in both directions.
 */
const snapshot = {
  ...baseSnapshot,
  artifacts: [
    ...baseSnapshot.artifacts,
    {
      id: 'wiki/topics/example-topic.md',
      path: 'wiki/topics/example-topic.md',
      kind: 'topic',
      title: 'Example topic',
      lifecycle_status: null,
      created_at: '2026-06-02T08:00:00+02:00',
      updated_at: '2026-07-12T08:00:00+02:00',
      captured_at: null,
      content_hash: 'df5468228c97a571cfec38d32bba625f607d45bab38f83dbeab77eefdf9c0c21',
      bytes: 640,
      attributes: {},
      parse_errors: [],
    },
  ],
};

const CODE_NODE = snapshot.artifacts.find((artifact) => artifact.kind === 'code');
const WIKILINK_EDGE = snapshot.relationships.find((edge) => edge.kind === 'wikilink');
const GOAL_EDGE = snapshot.relationships.find((edge) => edge.kind === 'goal-linked-spec');

function jsonResponse(body, status = 200) {
  return { ok: status >= 200 && status < 300, status, json: async () => body };
}

const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

/** Boot the real shell, then wire the Inspector to it exactly as main.mjs does. */
async function mount({ documents = {} } = {}) {
  const dom = new JSDOM(html, { url: 'http://127.0.0.1:8000/' });
  globalThis.document = dom.window.document;

  globalThis.fetch = async (url) => {
    const { pathname, searchParams } = new URL(url, 'http://127.0.0.1:8000');
    if (pathname === '/api/snapshot') return jsonResponse(snapshot);
    if (pathname === '/api/document') {
      const path = searchParams.get('path');
      if (!(path in documents)) return jsonResponse({ error: 'not_inventoried' }, 400);
      return jsonResponse({
        path,
        content: documents[path],
        content_hash: 'a'.repeat(64),
        snapshot_hash: 'a'.repeat(64),
        changed_since_snapshot: false,
      });
    }
    return jsonResponse({ error: 'not_found' }, 404);
  };

  state.snapshot = null;
  state.error = null;
  state.refreshing = false;

  await boot(dom.window.document);
  const inspector = initInspector({ root: dom.window.document, getSnapshot: () => state.snapshot });

  const doc = dom.window.document;
  return {
    dom,
    doc,
    inspector,
    shell: doc.querySelector('[data-app-shell]'),
    panel: doc.querySelector('[data-inspector]'),
    scrim: doc.querySelector('[data-inspector-scrim]'),
    body: doc.querySelector('[data-inspector-body]'),
    press: (key, target = doc) =>
      target.dispatchEvent(new dom.window.KeyboardEvent('keydown', { key, bubbles: true })),
  };
}

afterEach(() => {
  globalThis.fetch = realFetch;
  delete globalThis.document;
});

describe('Inspector overlay', () => {
  it('floats above the view without reflowing, resizing, or repositioning it', async () => {
    const { doc, shell, panel, scrim } = await mount();

    assert.ok(panel, 'the shell must ship an Inspector panel');
    assert.ok(scrim, 'the shell must ship a dimming scrim');
    // Top-level overlays, not grid columns (DESIGN.md §5, spec: Inspector overlay).
    assert.equal(shell.contains(panel), false, 'the panel must sit outside the app shell');
    assert.equal(shell.contains(scrim), false, 'the scrim must sit outside the app shell');

    const before = { html: shell.innerHTML, className: shell.className, style: shell.getAttribute('style') };
    openInspector(CODE_NODE);
    await flush();

    assert.equal(panel.classList.contains('is-open'), true, 'the panel must open');
    assert.equal(shell.innerHTML, before.html, 'the view beneath must not be re-rendered');
    assert.equal(shell.className, before.className, 'the shell must not change layout class');
    assert.equal(shell.getAttribute('style'), before.style, 'the shell must not be resized inline');
    // The one permitted mutation: modality. `inert` removes the view beneath from
    // the tab order and the a11y tree without moving a single pixel of it.
    assert.equal(shell.hasAttribute('inert'), true, 'the view beneath must be inert while open');

    // ...and no stylesheet may reflow the shell off Inspector state either.
    const css = (await readFile(new URL('styles/components.css', PUBLIC_ROOT), 'utf8'))
      .replace(/\/\*[\s\S]*?\*\//g, '');
    assert.equal(
      /\.app-shell[^{,]*inspector/.test(css),
      false,
      'no rule may restyle .app-shell from Inspector state',
    );
    for (const selector of ['.inspector', '.inspector-scrim']) {
      const rule = css.slice(css.indexOf(`${selector} {`));
      assert.match(
        rule.slice(0, rule.indexOf('}')),
        /position:\s*fixed/,
        `${selector} must be fixed so the view beneath cannot reflow`,
      );
    }

    doc.querySelector('[data-inspector-close]').click();
    assert.equal(panel.classList.contains('is-open'), false);
    assert.equal(shell.hasAttribute('inert'), false, 'closing must hand the view back');
  });

  it('dims the app with a scrim only while it is open', async () => {
    const { panel, scrim } = await mount();

    assert.equal(scrim.classList.contains('is-open'), false);
    assert.equal(panel.getAttribute('aria-hidden'), 'true');

    openInspector(CODE_NODE);
    await flush();
    assert.equal(scrim.classList.contains('is-open'), true, 'the scrim must dim the app while open');
    assert.equal(panel.getAttribute('aria-hidden'), 'false');
    assert.equal(panel.getAttribute('role'), 'dialog');
    assert.equal(panel.getAttribute('aria-modal'), 'true');
  });

  it('closes on the close control, Escape, and a scrim click, restoring focus each time', async () => {
    const { doc, panel, scrim, press } = await mount();
    const invoker = doc.querySelector('[data-route="atlas"]');

    for (const close of [
      () => doc.querySelector('[data-inspector-close]').click(),
      () => press('Escape'),
      () => scrim.click(),
    ]) {
      invoker.focus();
      openInspector(CODE_NODE);
      await flush();
      assert.equal(panel.classList.contains('is-open'), true);
      assert.notEqual(doc.activeElement, invoker, 'focus must move into the panel');

      close();
      assert.equal(panel.classList.contains('is-open'), false);
      assert.equal(scrim.classList.contains('is-open'), false);
      assert.equal(doc.activeElement, invoker, 'focus must return to the invoking element');
    }
  });

  it('ignores Escape when it is already closed', async () => {
    const { doc, panel, press } = await mount();
    const invoker = doc.querySelector('[data-route="atlas"]');
    invoker.focus();
    press('Escape');
    assert.equal(panel.classList.contains('is-open'), false);
    assert.equal(doc.activeElement, invoker);
  });
});

describe('Inspector node content', () => {
  const codePage = [
    '# exampleService',
    '',
    '## Summary',
    '',
    'Wraps the vendor API and retries idempotent reads.',
    '',
    '## Role',
    '',
    'Service.',
    '',
  ].join('\n');

  it('shows summary, source path, freshness, linked context, and evidence', async () => {
    const { doc, body } = await mount({ documents: { [CODE_NODE.path]: codePage } });

    openInspector(CODE_NODE);
    await flush();

    assert.equal(body.querySelector('.inspector-title').textContent, 'exampleService');
    assert.equal(body.querySelector('.inspector-kind').textContent, 'code');
    assert.equal(
      body.querySelector('.inspector-summary').textContent,
      'Wraps the vendor API and retries idempotent reads.',
    );

    const headings = [...body.querySelectorAll('.inspector-section h3')].map((h) => h.textContent);
    assert.deepEqual(headings, [
      'Source path',
      'Freshness',
      'Linked project context',
      'Evidence and relationships',
    ]);

    const source = body.querySelector('[data-section="source"]');
    assert.match(source.textContent, /wiki\/code\/example-service\.md/, 'the memory page path');
    assert.match(source.textContent, /src\/example-service\.js/, 'the underlying source file');

    assert.match(body.querySelector('[data-section="freshness"]').textContent, /2026-07-15/);

    // The wikilink edge lands on this node from a topic page: that is its linked context.
    assert.match(
      body.querySelector('[data-section="context"]').textContent,
      /Example topic/,
      'inbound linked context must be listed',
    );

    const evidence = body.querySelector('[data-section="evidence"]');
    assert.match(evidence.textContent, /explicit/i);
    assert.match(evidence.textContent, /wiki\/topics\/example-topic\.md:5/);

    assert.equal(doc.querySelectorAll('[data-inspector] script, [data-inspector] img').length, 0);
  });

  it('degrades honestly when the page carries no summary', async () => {
    const { body } = await mount({ documents: { [CODE_NODE.path]: '# exampleService\n\nNo section here.\n' } });

    openInspector(CODE_NODE);
    await flush();

    const summary = body.querySelector('.inspector-summary');
    assert.equal(summary.dataset.present, 'false');
    assert.match(summary.textContent, /no summary/i);
    assert.doesNotMatch(summary.textContent, /vendor API/);
  });

  it('reports an unreadable document instead of inventing a summary', async () => {
    const { body } = await mount({ documents: {} });

    openInspector(CODE_NODE);
    await flush();

    const summary = body.querySelector('.inspector-summary');
    assert.equal(summary.dataset.present, 'false');
    assert.match(summary.textContent, /not_inventoried|unavailable/i);
  });

  it('inserts every server string as a text node', async () => {
    const hostile = {
      ...CODE_NODE,
      title: '<img src=x onerror="globalThis.pwned = true">',
    };
    const { doc, body } = await mount({
      documents: {
        [CODE_NODE.path]: '# x\n\n## Summary\n\n<script>globalThis.pwned = true</script> and <b>markup</b>\n',
      },
    });

    openInspector(hostile);
    await flush();

    assert.equal(body.querySelectorAll('img, script, b').length, 0, 'no markup may be parsed out of content');
    assert.equal(globalThis.pwned, undefined);
    assert.equal(body.querySelector('.inspector-title').textContent, hostile.title);
    assert.match(body.querySelector('.inspector-summary').textContent, /<b>markup<\/b>/);
    assert.ok(doc);
  });
});

describe('Inspector edge content', () => {
  it('shows kind, origin, evidence, rule identity, confidence, time, and schema version', async () => {
    const { body } = await mount();

    openInspector(GOAL_EDGE);
    await flush();

    assert.equal(body.querySelector('.inspector-kind').textContent, 'relationship');
    assert.equal(body.querySelector('.inspector-title').textContent, 'goal-linked-spec');

    const text = body.textContent;
    assert.match(text, /derived/, 'explicit vs derived must be stated');
    assert.match(text, /goals\/example-goal\.md:12/, 'evidence location');
    assert.match(text, /Linked work/, 'evidence section');
    assert.match(text, /goal-linked-work v1/, 'rule identity and version');
    assert.match(text, /Confidence 100%/, 'confidence');
    assert.match(text, /2026-07-17 15:30/, 'generated time');
    assert.match(text, /Schema version[\s\S]*\b1\b/, 'snapshot schema version');
  });

  it('states plainly when a derived edge has no rule recorded', async () => {
    const { body } = await mount();

    openInspector(WIKILINK_EDGE);
    await flush();

    assert.match(body.textContent, /explicit/);
    assert.match(body.textContent, /no rule/i, 'a null rule must be reported, not blanked');
  });
});

describe('Reader entry points', () => {
  it('links resolvable Loam Markdown references and leaves source code plain', async () => {
    const { doc, body } = await mount({ documents: { [CODE_NODE.path]: '# x\n\n## Summary\n\nS.\n' } });

    openInspector(CODE_NODE);
    await flush();

    const links = [...body.querySelectorAll('.file-link')].map((link) => link.dataset.path);
    assert.ok(links.includes(CODE_NODE.path), 'the artifact path is a Reader entry point');
    assert.ok(links.includes('wiki/topics/example-topic.md'), 'resolvable evidence is a Reader entry point');
    assert.equal(
      links.includes('src/example-service.js'),
      false,
      'underlying source code is not a Reader target',
    );

    const opened = [];
    doc.addEventListener('loam:open-reader', (event) => opened.push(event.detail));
    body.querySelector(`.file-link[data-path="${CODE_NODE.path}"]`).click();
    assert.deepEqual(opened, [{ path: CODE_NODE.path, kind: 'code', title: 'exampleService', line: null }]);
  });

  it('leaves an uninventoried Markdown path as plain text', async () => {
    const { body } = await mount();

    openInspector(GOAL_EDGE);
    await flush();

    // specs/example-spec.md is referenced by the edge but absent from the inventory.
    assert.match(body.textContent, /specs\/example-spec\.md/);
    assert.equal(
      body.querySelector('.file-link[data-path="specs/example-spec.md"]'),
      null,
      'an unresolvable reference must not pretend to open Reader',
    );
  });
});

describe('per-kind summary extraction', () => {
  const cases = [
    {
      kind: 'code',
      content: '# svc\n\n## Summary\n\nOne line about the module.\n\n## Role\n\nx\n',
      text: 'One line about the module.',
    },
    {
      kind: 'topic',
      content: '# Retry policy\n\nHow retries are bounded across connectors.\n\n## Detail\n\nx\n',
      text: 'How retries are bounded across connectors.',
    },
    {
      kind: 'topic',
      content: '# Retry policy\n\n## Summary\n\nFalls back to the Summary section.\n',
      text: 'Falls back to the Summary section.',
    },
    {
      kind: 'entity',
      content: '# Acme\n\nThe vendor behind the ingest API.\n',
      text: 'The vendor behind the ingest API.',
    },
    {
      kind: 'analysis',
      content: '# Cost review\n\nVerdict: the pipeline is over-provisioned.\n\nMore prose.\n',
      text: 'Verdict: the pipeline is over-provisioned.',
    },
    {
      kind: 'analysis',
      content: '# Cost review\n\nNo verdict line, so the opening paragraph stands in.\n',
      text: 'No verdict line, so the opening paragraph stands in.',
    },
    {
      kind: 'spec',
      content: '---\ntitle: X\n---\n\n# X\n\n## Problem\n\nThe ingest job silently drops rows.\n\n## Scope\n\nx\n',
      text: 'The ingest job silently drops rows.',
    },
    {
      kind: 'plan',
      content: '---\ntitle: X\ndescription: Split the ingest job into three stages.\nstatus: pending\n---\n\n# X\n',
      text: 'Split the ingest job into three stages.',
    },
    {
      kind: 'plan',
      content: '---\ntitle: X\ndescription: >\n  Folded description\n  over two lines.\nstatus: pending\n---\n\n# X\n',
      text: 'Folded description over two lines.',
    },
    {
      kind: 'checkpoint',
      content: '# Checkpoint\n\n- Captured: 2026-08-01 12:00 +02:00\n- Reason: pause\n- Scope: inspector overlay\n- Intended return: wire the Reader handoff\n',
      text: 'inspector overlay — wire the Reader handoff',
    },
    {
      kind: 'goal',
      content: '---\nstatus: active\n---\n\n# Goal\n\n## Intent\n\nMake provenance one interaction away.\n\n## Validation\n\nx\n',
      text: 'Make provenance one interaction away.',
    },
  ];

  for (const { kind, content, text } of cases) {
    it(`pulls the ${kind} summary from where that kind keeps it`, () => {
      assert.deepEqual(summarize({ kind, content }), { text, present: true });
    });
  }

  it('never fabricates a summary that is not in the document', () => {
    for (const kind of ['code', 'topic', 'entity', 'analysis', 'spec', 'plan', 'checkpoint', 'goal']) {
      const result = summarize({ kind, content: '# Title only\n' });
      assert.equal(result.present, false, `${kind} must report a missing summary`);
      assert.match(result.text, /^No /, `${kind} must say what is missing`);
    }
  });

  it('says a source file has no code page rather than guessing', () => {
    const result = summarize({ kind: 'code', content: null });
    assert.equal(result.present, false);
    assert.match(result.text, /no code page ingested/i);
  });

  it('ignores fenced code and front matter when reading the opening paragraph', () => {
    const result = summarize({
      kind: 'topic',
      content: '---\ntags: [x]\n---\n\n# T\n\n```\nnot the summary\n```\n\nThe real opening paragraph.\n',
    });
    assert.deepEqual(result, { text: 'The real opening paragraph.', present: true });
  });
});
