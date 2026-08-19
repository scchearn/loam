import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { afterEach, describe, it } from 'node:test';
import { fileURLToPath } from 'node:url';

import { JSDOM } from 'jsdom';

import { boot, routeFromHash } from '../public/js/app.mjs';
import { state } from '../public/js/store.mjs';

const PUBLIC_ROOT = new URL('../public/', import.meta.url);
const ROUTES = ['pulse', 'atlas', 'work-stream', 'chronicle', 'stewardship'];

const html = await readFile(new URL('index.html', PUBLIC_ROOT), 'utf8');
const snapshot = JSON.parse(
  await readFile(new URL('fixtures/snapshots/valid/ready-full.json', import.meta.url), 'utf8'),
);

const realFetch = globalThis.fetch;

function jsonResponse(body, status = 200) {
  return {
    ok: status >= 200 && status < 300,
    status,
    json: async () => body,
  };
}

/**
 * Boot the real shell against a fresh jsdom document with a scripted server.
 * `routes` maps a pathname to a handler, so each test states exactly which
 * server behaviour it is exercising.
 */
async function mountShell({ routes = {}, hash = '' } = {}) {
  const dom = new JSDOM(html, { url: `http://127.0.0.1:8000/${hash}` });
  globalThis.document = dom.window.document;

  const calls = [];
  globalThis.fetch = async (url, options = {}) => {
    const { pathname, searchParams } = new URL(url, 'http://127.0.0.1:8000');
    calls.push({ pathname, method: options.method ?? 'GET', searchParams });
    const handler = routes[pathname];
    if (!handler) return jsonResponse({ error: 'not_found' }, 404);
    return handler({ searchParams, options });
  };

  state.snapshot = null;
  state.error = null;
  state.refreshing = false;

  const shell = await boot(dom.window.document);
  return { dom, doc: dom.window.document, shell, calls };
}

afterEach(() => {
  globalThis.fetch = realFetch;
  delete globalThis.document;
});

const okSnapshot = () => ({ '/api/snapshot': () => jsonResponse(snapshot) });

describe('app shell routing', () => {
  it('mounts every one of the five areas, exactly one at a time', async () => {
    const { doc } = await mountShell({ routes: okSnapshot() });

    for (const route of ROUTES) {
      doc.querySelector(`[data-route="${route}"]`).click();

      const active = [...doc.querySelectorAll('[data-view]')]
        .filter((view) => view.classList.contains('is-active'))
        .map((view) => view.dataset.view);
      assert.deepEqual(active, [route], `only ${route} should be mounted`);

      const current = [...doc.querySelectorAll('[data-route]')]
        .filter((button) => button.getAttribute('aria-current') === 'page')
        .map((button) => button.dataset.route);
      assert.deepEqual(current, [route], `only the ${route} rail item should be current`);

      assert.ok(
        doc.querySelector(`[data-view="${route}"] [data-mount="${route}"]`),
        `${route} must expose a mount point for its view module`,
      );
    }
  });

  it('boots on the hash route and falls back to Pulse for anything unknown', async () => {
    const { doc } = await mountShell({ routes: okSnapshot(), hash: '#/chronicle' });
    assert.equal(doc.querySelector('.view.is-active').dataset.view, 'chronicle');

    assert.equal(routeFromHash('#/stewardship'), 'stewardship');
    assert.equal(routeFromHash('#/not-a-view'), 'pulse');
    assert.equal(routeFromHash(''), 'pulse');
    assert.equal(routeFromHash(undefined), 'pulse');
  });
});

describe('topbar chrome', () => {
  it('states workspace, branch, and snapshot freshness once each', async () => {
    const { doc } = await mountShell({ routes: okSnapshot() });

    assert.equal(doc.querySelector('[data-workspace-name]').textContent, 'loam');
    assert.equal(doc.querySelector('[data-workspace-context]').textContent, 'main · clean');
    assert.equal(
      doc.querySelector('[data-freshness]').textContent,
      'Snapshot 2026-07-17 15:30 · qmd absent',
    );
    assert.equal(doc.querySelector('[data-freshness-dot]').dataset.state, 'absent');
  });
});

describe('refresh', () => {
  it('surfaces the failure without dropping the prior render', async () => {
    const { doc, calls } = await mountShell({
      routes: {
        ...okSnapshot(),
        '/api/refresh': () =>
          jsonResponse({ error: 'refresh_failed', message: 'loam state --view exited with code 1' }, 500),
      },
    });

    const refreshButton = doc.querySelector('[data-refresh]');
    refreshButton.click();
    await new Promise((resolve) => setTimeout(resolve, 0));

    const notice = doc.querySelector('[data-notice]');
    assert.ok(notice.classList.contains('is-visible'), 'the failure must be visible');
    assert.match(notice.textContent, /refresh_failed: loam state --view exited with code 1/);

    // The prior snapshot survived: chrome still reads the workspace it had.
    assert.equal(state.snapshot.workspace.name, 'loam');
    assert.equal(doc.querySelector('[data-workspace-name]').textContent, 'loam');
    assert.equal(doc.querySelector('[data-freshness]').textContent, 'Snapshot 2026-07-17 15:30 · qmd absent');
    assert.equal(refreshButton.disabled, false, 'Refresh must be usable again after a failure');
    assert.equal(calls.filter((call) => call.pathname === '/api/refresh').length, 1);
  });

  it('re-reads the snapshot after a successful refresh', async () => {
    const refreshed = { ...snapshot, generated_at: '2026-08-19T09:05:00+02:00' };
    let served = snapshot;
    const { doc } = await mountShell({
      routes: {
        '/api/snapshot': () => jsonResponse(served),
        '/api/refresh': () => {
          served = refreshed;
          return { ok: true, status: 204, json: async () => null };
        },
      },
    });

    doc.querySelector('[data-refresh]').click();
    await new Promise((resolve) => setTimeout(resolve, 0));

    assert.equal(doc.querySelector('[data-freshness]').textContent, 'Snapshot 2026-08-19 09:05 · qmd absent');
    assert.equal(doc.querySelector('[data-notice]').classList.contains('is-visible'), false);
  });
});

describe('Query palette', () => {
  const hostile = {
    path: 'wiki/topics/xss.md',
    kind: 'topic',
    title: '<img src=x onerror="globalThis.pwned = true">',
    snippet: 'raw <script>globalThis.pwned = true</script> and <b>markup</b> from the corpus',
  };
  const plain = { path: 'wiki/code/example-service.md', kind: 'code', title: 'exampleService', snippet: 'second' };

  const searchRoutes = (results) => ({
    ...okSnapshot(),
    '/api/search': () => jsonResponse({ results }),
  });

  it('opens on Cmd+K and closes on Escape', async () => {
    const { doc, dom } = await mountShell({ routes: searchRoutes([]) });
    const dialog = doc.querySelector('[data-query-dialog]');
    assert.equal(dialog.hasAttribute('open'), false);

    doc.dispatchEvent(new dom.window.KeyboardEvent('keydown', { key: 'k', metaKey: true, bubbles: true }));
    assert.equal(dialog.hasAttribute('open'), true, 'Cmd+K must open the palette');

    dialog.dispatchEvent(new dom.window.KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
    assert.equal(dialog.hasAttribute('open'), false, 'Escape must close the palette');
  });

  it('inserts every server string as a text node and keeps the API order', async () => {
    const { doc } = await mountShell({ routes: searchRoutes([hostile, plain]) });
    const input = doc.querySelector('[data-query-input]');
    input.value = 'xss';
    doc.querySelector('[data-query-open]').click();
    await new Promise((resolve) => setTimeout(resolve, 0));

    const dialog = doc.querySelector('[data-query-dialog]');
    assert.equal(dialog.querySelectorAll('img, script, b').length, 0, 'no markup may be parsed out of results');
    assert.equal(globalThis.pwned, undefined);

    const items = [...dialog.querySelectorAll('.query-result')];
    assert.equal(items.length, 2);
    assert.deepEqual(
      items.map((item) => item.querySelector('.result-copy strong').textContent),
      [hostile.title, plain.title],
      'results render in the order the deterministic search returned them',
    );
    assert.equal(items[0].querySelector('.result-copy span').textContent, hostile.snippet);
    assert.equal(items[0].getAttribute('aria-selected'), 'true');
  });

  it('walks results with the arrow keys and hands the chosen path to Reader', async () => {
    const { doc, dom } = await mountShell({ routes: searchRoutes([hostile, plain]) });
    const input = doc.querySelector('[data-query-input]');
    input.value = 'service';
    doc.querySelector('[data-query-open]').click();
    await new Promise((resolve) => setTimeout(resolve, 0));

    const dialog = doc.querySelector('[data-query-dialog]');
    dialog.dispatchEvent(new dom.window.KeyboardEvent('keydown', { key: 'ArrowDown', bubbles: true }));
    assert.equal(dialog.querySelectorAll('.query-result')[1].getAttribute('aria-selected'), 'true');
    assert.equal(input.getAttribute('aria-activedescendant'), 'query-result-1');

    const opened = [];
    doc.addEventListener('loam:open-document', (event) => opened.push(event.detail));
    dialog.dispatchEvent(new dom.window.KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));

    assert.deepEqual(opened, [{ path: plain.path, kind: plain.kind, title: plain.title }]);
    assert.equal(dialog.hasAttribute('open'), false, 'choosing a result closes the palette');
  });

  it('offers the kinds present in the snapshot and passes the filter to the API', async () => {
    const { doc, calls } = await mountShell({ routes: searchRoutes([plain]) });
    doc.querySelector('[data-query-open]').click();

    const filters = [...doc.querySelectorAll('[data-query-filters] .filter-button')].map((b) => b.dataset.kind);
    const kinds = [...new Set(snapshot.artifacts.map((artifact) => artifact.kind))];
    assert.equal(filters[0], '', 'the first filter clears the kind');
    assert.deepEqual(new Set(filters.slice(1)), new Set(kinds));

    const input = doc.querySelector('[data-query-input]');
    input.value = 'example';
    doc.querySelector('[data-query-filters] .filter-button[data-kind="code"]').click();
    await new Promise((resolve) => setTimeout(resolve, 0));

    const search = calls.filter((call) => call.pathname === '/api/search').at(-1);
    assert.equal(search.searchParams.get('kind'), 'code');
    assert.equal(search.searchParams.get('q'), 'example');
  });

  it('reports a failed search instead of rendering stale results', async () => {
    const { doc } = await mountShell({
      routes: { ...okSnapshot(), '/api/search': () => jsonResponse({ error: 'search_unavailable' }, 503) },
    });
    const input = doc.querySelector('[data-query-input]');
    input.value = 'anything';
    doc.querySelector('[data-query-open]').click();
    await new Promise((resolve) => setTimeout(resolve, 0));

    assert.equal(doc.querySelectorAll('.query-result').length, 0);
    assert.match(doc.querySelector('[data-query-empty]').textContent, /search_unavailable/);
  });
});

describe('stylesheets', () => {
  const stripComments = (css) => css.replace(/\/\*[\s\S]*?\*\//g, '');

  it('keeps every colour in tokens.css', async () => {
    const css = stripComments(await readFile(new URL('styles/components.css', PUBLIC_ROOT), 'utf8'));

    // `color-mix(in oklch, var(--token) …)` derives from a token, so it survives:
    // neither `oklch(` nor `color(` matches it.
    const literals = css.match(/#[0-9a-fA-F]{3,8}\b|\b(?:oklch|rgba?|hsla?|lab|lch|color)\(/g) ?? [];
    assert.deepEqual(literals, [], 'components.css must not carry colour literals');

    const named = css.match(/:\s*(?:white|black|red|green|blue|gray|grey|silver)\b/g) ?? [];
    assert.deepEqual(named, [], 'components.css must not carry named colours');
  });

  it('references only custom properties that tokens.css defines', async () => {
    const tokens = await readFile(new URL('styles/tokens.css', PUBLIC_ROOT), 'utf8');
    const components = stripComments(await readFile(new URL('styles/components.css', PUBLIC_ROOT), 'utf8'));

    const defined = new Set([...tokens.matchAll(/^\s*(--[\w-]+)\s*:/gm)].map((match) => match[1]));
    const referenced = new Set([...components.matchAll(/var\((--[\w-]+)/g)].map((match) => match[1]));
    const missing = [...referenced].filter((name) => !defined.has(name)).sort();

    assert.deepEqual(missing, [], 'every var() in components.css must resolve to a token');
    assert.ok(defined.has('--accent'), 'tokens.css must define the DESIGN.md accent');
  });

  it('transcribes the DESIGN.md palette verbatim', async () => {
    const tokens = await readFile(new URL('styles/tokens.css', PUBLIC_ROOT), 'utf8');
    const design = await readFile(new URL('../../raw/DESIGN.md', import.meta.url), 'utf8');

    // The frontmatter `colors:` block is the canonical palette; a token that
    // drifts from it fails here. (`borders:` restates two of the same values
    // under different names, so it is not a second source of truth.)
    const palette = design.slice(design.indexOf('colors:'), design.indexOf('\nfonts:'));
    const declared = [...palette.matchAll(/^ {2}([a-z][\w-]*): "(oklch\([^"]+\))"/gm)];
    assert.ok(declared.length >= 25, 'the DESIGN.md palette should be read, not silently skipped');

    for (const [, name, value] of declared) {
      assert.match(
        tokens,
        new RegExp(`--${name}:\\s*${value.replace(/[()%.[\]*+?^$|\\/]/g, '\\$&')};`),
        `--${name} must be ${value} exactly as DESIGN.md states`,
      );
    }
  });
});

describe('served shell', () => {
  it('is what the local server hands the browser', async () => {
    const { createServer } = await import('../server/server.mjs');
    const server = createServer({
      workspaceRoot: fileURLToPath(new URL('..', import.meta.url)),
      initialSnapshot: snapshot,
      refreshProducer: async () => snapshot,
      buildSearchIndex: () => null,
      stderr: { write() {} },
    });
    await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
    const base = `http://127.0.0.1:${server.address().port}`;

    try {
      const index = await realFetch(`${base}/`);
      assert.equal(index.status, 200);
      assert.match(index.headers.get('content-type'), /text\/html/);
      const body = await index.text();
      assert.match(body, /\/styles\/tokens\.css/);
      assert.match(body, /\/js\/main\.mjs/);
      assert.match(body, /<symbol id="i-pulse"/, 'the Phosphor sprite ships inline');

      for (const [path, type] of [
        ['/styles/tokens.css', /text\/css/],
        ['/styles/components.css', /text\/css/],
        ['/js/main.mjs', /javascript/],
        ['/js/app.mjs', /javascript/],
        ['/js/store.mjs', /javascript/],
        ['/js/query.mjs', /javascript/],
        ['/loam.svg', /image\/svg/],
      ]) {
        const asset = await realFetch(`${base}${path}`);
        assert.equal(asset.status, 200, `${path} must be served`);
        assert.match(asset.headers.get('content-type'), type, `${path} content type`);
      }
    } finally {
      await new Promise((resolve) => server.close(resolve));
    }
  });
});
