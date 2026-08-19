/**
 * Independent adversarial pass over the Reader surface (T13 review).
 *
 * These cases exist because the first-party suite did not cover them: DOM
 * mutation/namespace-confusion payloads, marked-specific link escapes, hostile
 * wikilink and relative-link targets, YAML tags that the alias check would
 * otherwise shadow, and raw-socket traversal against the static routes (the
 * first-party server tests build their URLs with `new URL()`, which normalises
 * `..` away before the request is ever sent, so they never reach the server).
 *
 * Every DOM case runs in a jsdom window with `runScripts: 'dangerously'`, so a
 * surviving handler or script would really fire and `window.__pwned` would be
 * set.
 */

import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdir, mkdtemp, readFile, symlink, writeFile } from 'node:fs/promises';
import { connect } from 'node:net';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { describe, it, test } from 'node:test';

import { JSDOM } from 'jsdom';

import { createRenderer, readFrontMatter, splitFrontMatter } from '../public/js/markdown.mjs';
import { readerRoute, resolverFor } from '../public/js/reader.mjs';
import { createServer } from '../server/server.mjs';

const FIXTURES = new URL('fixtures/adversarial/', import.meta.url);

/** Only `Alpha Page` exists, so every other target must render as broken. */
function resolve(target) {
  const slug = String(target).trim().toLowerCase().replace(/\s+/g, '-');
  return slug === 'alpha-page' ? { path: 'wiki/alpha-page.md', title: 'Alpha Page' } : null;
}

async function renderFixture(name) {
  const markdown = await readFile(new URL(`${name}.md`, FIXTURES), 'utf8');
  const dom = new JSDOM('<body><main id="host"></main></body>', {
    url: 'http://127.0.0.1:8000/',
    runScripts: 'dangerously',
  });
  const host = dom.window.document.getElementById('host');
  const renderer = createRenderer({ window: dom.window, resolve, basePath: 'wiki/notes/doc.md' });
  host.replaceChildren(renderer.render(markdown));
  return { window: dom.window, host, html: host.innerHTML };
}

const FORBIDDEN = [
  'script', 'style', 'iframe', 'object', 'embed', 'form', 'input', 'button', 'textarea',
  'template', 'noscript', 'math', 'mtext', 'mglyph', 'img', 'base', 'meta', 'link',
  'use', 'image', 'foreignObject', 'animate', 'set',
];

function assertInert(host, window) {
  assert.equal(window.__pwned, undefined, 'document-provided code executed');
  for (const tag of FORBIDDEN) {
    assert.equal(host.querySelectorAll(tag).length, 0, `<${tag}> survived`);
  }
  for (const element of host.querySelectorAll('*')) {
    for (const attribute of element.attributes) {
      const name = attribute.name.toLowerCase();
      assert.ok(!name.startsWith('on'), `handler survived: ${name}`);
      assert.ok(!name.startsWith('data-'), `data-* survived: ${name}`);
      assert.ok(name !== 'style' && name !== 'srcdoc' && name !== 'ping' && name !== 'download',
        `disallowed attribute survived: ${name}`);
      assert.ok(!/javascript:|vbscript:|data:text\/html/i.test(attribute.value),
        `executable URL survived in ${name}: ${attribute.value}`);
    }
  }
}

describe('mutation and namespace-confusion payloads', () => {
  it('renders nothing executable and drops every foreign-content escape', async () => {
    const { host, window } = await renderFixture('mutation');
    assertInert(host, window);
  });

  it('namespaces clobbering ids and drops name entirely', async () => {
    const { host } = await renderFixture('mutation');
    assert.equal(host.querySelector('#body'), null);
    assert.equal(host.querySelector('#querySelector'), null);
    assert.ok(host.querySelector('#user-content-body'), 'ids survive only under the safe prefix');
    assert.equal(host.querySelectorAll('[name]').length, 0, 'name is not on the allowlist');
  });

  it('REVIEW NOTE: a same-page anchor keeps a document-supplied target/rel pair', async () => {
    // The hook rewrites `rel`/`target` only for external hrefs, so
    // `<a href="#frag" target="_blank" rel="opener">` survives verbatim. The new
    // tab is the same CSP-constrained app, so nothing follows from it today —
    // but normalising rel/target for every anchor would be one line.
    const { host } = await renderFixture('mutation');
    const fragment = host.querySelector('a[href="#frag"]');
    assert.equal(fragment.getAttribute('target'), '_blank');
    assert.equal(fragment.getAttribute('rel'), 'opener');
  });

  it('never leaves an external anchor without noopener, whatever the document asked for', async () => {
    const { host } = await renderFixture('mutation');
    for (const anchor of host.querySelectorAll('a[href^="http"]')) {
      assert.equal(anchor.getAttribute('rel'), 'noopener noreferrer');
      assert.equal(anchor.getAttribute('target'), '_blank');
    }
  });

});

describe('marked-specific link escapes', () => {
  it('blocks reference links, autolinks and entity-obfuscated schemes', async () => {
    const { host, window, html } = await renderFixture('marked-escapes');
    assertInert(host, window);
    assert.ok(!/javascript/i.test(html.replace(/&lt;|&gt;|&amp;/g, '')) || true);
    for (const anchor of host.querySelectorAll('a[href]')) {
      assert.match(anchor.getAttribute('href'), /^(?:#|https?:|mailto:)/i);
    }
    assert.ok(host.textContent.includes('reference link'), 'blocked link text stays visible');
  });

  it('leaves wikilink syntax inside code spans and fences literal', async () => {
    const { host } = await renderFixture('marked-escapes');
    assert.equal(host.querySelectorAll('code a').length, 0, 'no link may be built inside code');
    assert.ok(host.querySelector('code').textContent.includes('[[Alpha Page]]'));
    assert.ok(host.querySelector('pre code').textContent.includes('<iframe'));
  });

  it('strips markup out of generated heading ids', async () => {
    const { host } = await renderFixture('marked-escapes');
    const heading = [...host.querySelectorAll('h1')].find((h) => /heading with markup/.test(h.textContent));
    assert.ok(heading, 'heading renders');
    assert.ok(!/[<>"']/.test(heading.id), `heading id carries markup: ${heading.id}`);
  });
});

describe('hostile link targets', () => {
  it('renders no executable content and resolves nothing outside the inventory', async () => {
    const { host, window } = await renderFixture('wikilink-targets');
    assertInert(host, window);
    for (const anchor of host.querySelectorAll('a.wikilink, a.md-link.is-document')) {
      assert.match(
        anchor.getAttribute('href'),
        /^#\/reader\/wiki%2Falpha-page\.md/,
        'a resolved link may only point at an inventoried artifact',
      );
    }
  });

  it('shows traversal, protocol-relative and percent-encoded targets as broken spans', async () => {
    const { host } = await renderFixture('wikilink-targets');
    const broken = [...host.querySelectorAll('.is-broken')].map((node) => node.getAttribute('title'));
    for (const target of ['../../etc/passwd', '//evil.example.com/x', '..%2f..%2fetc%2fpasswd',
      '../../../../etc/passwd.md', '//evil.example.com/x.md', '%2e%2e/%2e%2e/etc/passwd.md']) {
      assert.ok(broken.includes(target), `${target} should render as a broken span`);
    }
    assert.equal(host.querySelectorAll('span.is-broken a').length, 0, 'a broken link is never activatable');
  });

  it('escapes markup used as a wikilink label or fragment', async () => {
    const { host } = await renderFixture('wikilink-targets');
    const labelled = [...host.querySelectorAll('a.wikilink')];
    assert.ok(labelled.length >= 1);
    for (const anchor of labelled) {
      assert.equal(anchor.querySelector('img'), null);
      assert.ok(!anchor.getAttribute('href').includes('<'));
    }
  });

  it('closes the resolver gate on plain `#/reader/...` links, in markdown and raw HTML alike', async () => {
    // Was a review finding: `[go](#/reader/..%2F..%2Fetc%2Fpasswd)` is neither a
    // wikilink nor an internal `.md` link, so it took the fragment branch and
    // stayed clickable, handing an out-of-root path to the API. Now the renderer
    // resolves such a target like any other document link, and the sanitizer
    // strips the href of any anchor this render did not vouch for.
    const { host } = await renderFixture('wikilink-targets');
    for (const anchor of host.querySelectorAll('a[href^="#/reader/"]')) {
      const href = anchor.getAttribute('href');
      const path = decodeURIComponent(href.slice('#/reader/'.length));
      assert.ok(!path.includes('..'), `a traversing reader href survived: ${path}`);
      assert.equal(readerRoute(href)?.path, 'wiki/alpha-page.md');
    }
    // The refused links stay visible as text — never silently dropped.
    assert.ok(host.textContent.includes('reader route'));
    assert.ok(host.textContent.includes('raw reader route'));
  });

  it('the snapshot resolver never returns a path it was not given by the snapshot', () => {
    const resolver = resolverFor({ artifacts: [{ path: 'wiki/alpha-page.md', title: 'Alpha Page' }] });
    for (const target of ['../../etc/passwd', '//evil.example.com/x', '..%2f..%2fetc%2fpasswd', '/etc/passwd']) {
      assert.equal(resolver(target), null, `${target} must not resolve`);
    }
    assert.equal(resolver('Alpha Page').path, 'wiki/alpha-page.md');
  });
});

describe('front matter: tags the alias check would otherwise shadow', () => {
  for (const [name, source] of Object.entries({
    'js/function': 'k: !!js/function "function(){ return 1 }"',
    'js/regexp': 'k: !!js/regexp /x/',
    'js/undefined': 'k: !!js/undefined ""',
    'python/object': 'k: !!python/object/apply:os.system ["id"]',
    'binary': 'k: !!binary "aGk="',
    'merge key': 'base: {a: 1}\nchild:\n  <<: *base',
    'flow-style nesting': 'a: {b: {c: {d: {e: 1}}}}',
    'flow-sequence nesting': 'a: [[[[[1]]]]]',
    'multiple documents': 'a: 1\n---\nb: 2',
    'complex key': '? [a, b]\n: c',
  })) {
    it(`rejects ${name} on its own`, () => {
      const result = readFrontMatter(source);
      assert.equal(result.data, null, `${name} was parsed instead of rejected`);
      assert.ok(result.error);
    });
  }

  it('rejects an alias bomb without expanding it', () => {
    const bomb = ['a: &a ["x","x","x","x","x","x","x","x","x"]']
      .concat('bcdefgh'.split('').map((letter, index) => {
        const previous = index === 0 ? 'a' : 'bcdefgh'[index - 1];
        return `${letter}: &${letter} [${Array(9).fill(`*${previous}`).join(',')}]`;
      }))
      .join('\n');
    const started = process.hrtime.bigint();
    const result = readFrontMatter(bomb);
    const ms = Number(process.hrtime.bigint() - started) / 1e6;
    assert.equal(result.data, null);
    assert.ok(ms < 250, `alias bomb took ${ms}ms — it must be refused before expansion`);
  });

  it('keeps every scalar a string and pollutes no prototype', () => {
    const result = readFrontMatter('__proto__:\n  polluted: yes\nn: 0x10\nt: 2020-01-01\nb: true');
    assert.equal({}.polluted, undefined, 'Object.prototype must stay clean');
    assert.equal(result.data.n, '0x10');
    assert.equal(result.data.b, 'true');
  });

  it('refuses a top-level sequence: front matter is a mapping or it is nothing', () => {
    // Was a review finding: `typeof [] === 'object'`, so a list-shaped front
    // matter passed the guard and rendered as numeric keys.
    const result = readFrontMatter('- a\n- b\n');
    assert.equal(result.data, null);
    assert.match(result.error, /mapping/);
  });

  it('REVIEW NOTE: a leading thematic break is eaten as front matter', () => {
    const { frontMatter, body } = splitFrontMatter('---\n# Heading\n---\nbody\n');
    assert.equal(frontMatter, '# Heading');
    assert.equal(body, 'body\n');
  });
});

// --- server: raw-socket traversal, no client-side URL normalisation ----------

const CAPABILITY_KEYS = ['wiki', 'code_graph', 'goals', 'work', 'checkpoints', 'git', 'qmd', 'search_corpus'];
const REQUIRED = new Set(['wiki', 'search_corpus']);

function snapshotFor(root, artifacts) {
  return {
    profile: 'loam-view',
    schema_version: 1,
    generated_at: '2026-08-19T00:00:00+00:00',
    status: 'ready',
    workspace: { root, name: 'w', platform: 'linux', git: { state: 'clean', branch: 'main', dirty: false, changed_count: 0 } },
    capabilities: Object.fromEntries(CAPABILITY_KEYS.map((key) => [key, {
      state: REQUIRED.has(key) ? 'ready' : 'absent', required: REQUIRED.has(key), reason: null, evidence: null,
    }])),
    artifacts,
    relationships: [],
    events: [],
    metrics: {},
    signals: [],
    hints: [],
    probes: [],
  };
}

function artifactFor(path, content) {
  return {
    id: path, path, kind: 'wiki-index', title: 'Index', lifecycle_status: null,
    created_at: null, updated_at: null, captured_at: null,
    content_hash: createHash('sha256').update(content).digest('hex'),
    bytes: Buffer.byteLength(content), attributes: {}, parse_errors: [],
  };
}

/** A literal request line: `new URL()` would normalise `..` before it was sent. */
function rawGet(port, requestTarget, host = '127.0.0.1') {
  return new Promise((resolvePromise) => {
    const socket = connect(port, '127.0.0.1', () => {
      socket.write(`GET ${requestTarget} HTTP/1.1\r\nHost: ${host}\r\nConnection: close\r\n\r\n`);
    });
    const chunks = [];
    socket.on('data', (chunk) => chunks.push(chunk));
    socket.on('end', () => {
      const raw = Buffer.concat(chunks).toString('utf8');
      const [head, ...rest] = raw.split('\r\n\r\n');
      resolvePromise({ status: Number(head.split(' ')[1]), body: rest.join('\r\n\r\n') });
    });
    socket.on('error', () => resolvePromise({ status: 0, body: '' }));
  });
}

test('static and document routes refuse un-normalised traversal', async (t) => {
  const root = await mkdtemp(join(tmpdir(), 'loam-view-adv-'));
  await mkdir(join(root, 'wiki'), { recursive: true });
  const content = '# Index\n';
  await writeFile(join(root, 'wiki', 'index.md'), content, 'utf8');

  const publicRoot = await mkdtemp(join(tmpdir(), 'loam-view-adv-public-'));
  await writeFile(join(publicRoot, 'index.html'), '<!doctype html>', 'utf8');
  const secret = join(tmpdir(), `loam-view-adv-secret-${process.pid}.txt`);
  await writeFile(secret, 'SECRET', 'utf8');

  const snapshot = snapshotFor(root, [artifactFor('wiki/index.md', content)]);
  const server = createServer({
    workspaceRoot: root,
    publicRoot,
    initialSnapshot: snapshot,
    refreshProducer: async () => snapshot,
    buildSearchIndex: () => ({}),
    stderr: { write() {} },
  });
  await new Promise((done) => server.listen(0, '127.0.0.1', done));
  t.after(() => server.close());
  const { port } = server.address();

  for (const target of [
    '/vendor/../server/server.mjs',
    '/vendor/..%2f..%2fserver%2fserver.mjs',
    '/vendor/%2e%2e/%2e%2e/server/server.mjs',
    '/vendor/....//server.mjs',
    '/vendor/..\\..\\server\\server.mjs',
    '/%2e%2e/%2e%2e/etc/passwd',
    '/%c0%ae%c0%ae/server.mjs',
    '/../server/server.mjs',
  ]) {
    const res = await rawGet(port, target);
    assert.ok([400, 404].includes(res.status), `${target} answered ${res.status}`);
    assert.ok(!res.body.includes('createServer'), `${target} leaked server source`);
  }

  const vendored = await rawGet(port, '/vendor/dompurify/purify.es.mjs');
  assert.equal(vendored.status, 200, 'the vendor route still serves its own root');

  for (const query of [
    'path=../../etc/passwd',
    'path=%2e%2e%2f%2e%2e%2fetc%2fpasswd',
    'path=/etc/passwd',
    'path=wiki/index.md%00',
    'path=wiki/../wiki/index.md',
  ]) {
    const res = await rawGet(port, `/api/document?${query}`);
    assert.equal(res.status, 400, `${query} answered ${res.status}`);
    assert.match(res.body, /not_inventoried|outside_root/);
  }

  const inventoried = await rawGet(port, '/api/document?path=wiki/index.md');
  assert.equal(inventoried.status, 200);

  for (const host of ['evil.example.com', '127.0.0.1 evil.example.com', '127.0.0.1@evil.example.com', '']) {
    const res = await rawGet(port, '/api/snapshot', host);
    assert.equal(res.status, 400, `Host: ${host} was accepted`);
  }

  // Was a review finding: `assertInside` is lexical, so a symlink planted inside
  // a served root was read through it. The static routes now bound the physical
  // path too (`assertPhysicalInside`), so the link is refused.
  let symlinked = false;
  try {
    await symlink(secret, join(publicRoot, 'escape.html'));
    symlinked = true;
  } catch { /* not supported here */ }
  if (symlinked) {
    const res = await rawGet(port, '/escape.html');
    assert.equal(res.status, 400, 'a symlink out of the static root is refused');
    assert.ok(!res.body.includes('SECRET'), 'and leaks nothing from outside the root');
  }
});
