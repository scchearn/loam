/**
 * Reader sanitization contract (spec: "sanitize executable Markdown content",
 * "Reader parsing and vendored libraries").
 *
 * Every case here is adversarial: the fixtures under `fixtures/adversarial/`
 * are what a hostile or careless Markdown artifact would contain, and the
 * assertions are what must survive sanitization. The DOM is a jsdom window
 * created with `runScripts: 'dangerously'`, so a script node that survived
 * sanitization would actually execute when the fragment is inserted — the
 * `window.__pwned` assertions are live, not cosmetic.
 */

import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { describe, it } from 'node:test';

import { JSDOM } from 'jsdom';

import { createRenderer, readFrontMatter, splitFrontMatter } from '../public/js/markdown.mjs';

const FIXTURES = new URL('fixtures/adversarial/', import.meta.url);

async function fixture(name) {
  return readFile(new URL(`${name}.md`, FIXTURES), 'utf8');
}

/** Resolves anything under `wiki/` so link tests exercise the resolved branch too. */
function resolve(target) {
  const slug = String(target).trim().toLowerCase().replace(/\s+/g, '-');
  return slug.startsWith('missing') ? null : { path: `wiki/${slug}.md`, title: target };
}

/**
 * Render a fixture into a *live* document and hand back both the host element
 * and the window, so tests can assert on markup and on whether anything ran.
 */
function renderInLiveDom(markdown) {
  const dom = new JSDOM('<body><main id="host"></main></body>', {
    url: 'http://127.0.0.1:8000/',
    runScripts: 'dangerously',
  });
  const renderer = createRenderer({ window: dom.window, resolve });
  const host = dom.window.document.getElementById('host');
  host.replaceChildren(renderer.render(markdown));
  return { dom, window: dom.window, host, html: host.innerHTML };
}

async function renderFixture(name) {
  return renderInLiveDom(await fixture(name));
}

const tags = (host, selector) => [...host.querySelectorAll(selector)];

describe('reader sanitization: no document-provided code executes', () => {
  it('drops script elements and leaves nothing executed', async () => {
    const { host, window, html } = await renderFixture('scripts-and-handlers');
    assert.equal(window.__pwned, undefined);
    assert.equal(tags(host, 'script').length, 0);
    assert.ok(!/<script/i.test(html));
    assert.ok(!html.includes('window.__pwned = true;'), 'script body must not survive as text either');
  });

  it('strips every on* event handler attribute, whatever its case', async () => {
    const { host } = await renderFixture('scripts-and-handlers');
    for (const element of tags(host, '*')) {
      for (const attribute of element.attributes) {
        assert.ok(
          !/^on/i.test(attribute.name),
          `handler attribute survived: <${element.tagName.toLowerCase()} ${attribute.name}>`,
        );
      }
    }
  });

  it('strips style attributes and style elements', async () => {
    const { host, html } = await renderFixture('scripts-and-handlers');
    assert.equal(tags(host, 'style').length, 0);
    assert.equal(tags(host, '[style]').length, 0);
    assert.ok(!/javascript:/i.test(html));
  });

  it('keeps fenced and inline JavaScript as inert text', async () => {
    const { host, window, html } = await renderFixture('code-inert');
    assert.equal(window.__pwned, undefined);
    assert.equal(tags(host, 'script').length, 0);

    const code = tags(host, 'code').map((element) => element.textContent).join('\n');
    assert.ok(code.includes('window.__pwned = true;'), 'fenced JS must still be readable as text');
    assert.ok(code.includes('<script>window.__pwned = true</script>'), 'inline code keeps its literal text');
    assert.ok(host.querySelector('pre code'), 'fenced block renders as pre > code');
    assert.ok(!/<script/i.test(html));
  });
});

describe('reader sanitization: URL schemes', () => {
  it('removes unsafe schemes from markdown links', async () => {
    const { host, html } = await renderFixture('schemes');
    assert.ok(!/javascript:/i.test(html), 'javascript: must not appear in any attribute');
    assert.ok(!/vbscript:/i.test(html));
    assert.ok(!/data:text\/html/i.test(html));
    for (const anchor of tags(host, 'a[href]')) {
      const href = anchor.getAttribute('href');
      assert.ok(
        /^(?:#|https?:|mailto:)/i.test(href),
        `anchor kept a disallowed href: ${href}`,
      );
    }
  });

  it('keeps the blocked link visible as text rather than silently dropping it', async () => {
    const { host } = await renderFixture('schemes');
    assert.ok(host.textContent.includes('javascript link'), 'blocked link text stays visible');
    assert.ok(host.textContent.includes('vbscript'), 'blocked link text stays visible');
  });

  it('allows http, https, mailto and fragments, with rel on external anchors', async () => {
    const { host } = await renderFixture('schemes');
    const external = tags(host, 'a[href^="https:"]');
    assert.ok(external.length >= 2, 'both markdown and raw-HTML external anchors survive');
    for (const anchor of external) {
      assert.equal(anchor.getAttribute('rel'), 'noopener noreferrer');
    }
    assert.ok(host.querySelector('a[href^="mailto:"]'), 'mailto survives');
    assert.ok(host.querySelector('a[href^="#"]'), 'fragment link survives');
  });

  it('rejects protocol-relative and file URLs', async () => {
    const { host } = await renderFixture('schemes');
    assert.equal(tags(host, 'a[href^="//"]').length, 0);
    assert.equal(tags(host, 'a[href^="file:"]').length, 0);
  });
});

describe('reader sanitization: embeds, frames and controls', () => {
  const FORBIDDEN = [
    'iframe', 'frame', 'frameset', 'object', 'embed', 'applet',
    'form', 'input', 'button', 'select', 'option', 'textarea', 'label', 'fieldset',
    'audio', 'video', 'source', 'track', 'canvas',
    'base', 'meta', 'link', 'style', 'script', 'template', 'noscript', 'math',
  ];

  it('blocks every forbidden element', async () => {
    const { host, window, html } = await renderFixture('embeds');
    assert.equal(window.__pwned, undefined);
    for (const tag of FORBIDDEN) {
      assert.equal(tags(host, tag).length, 0, `<${tag}> survived sanitization`);
      assert.ok(!new RegExp(`<${tag}[\\s>]`, 'i').test(html), `<${tag}> survived in markup`);
    }
  });

  it('strips srcdoc and formaction', async () => {
    const { host, html } = await renderFixture('embeds');
    assert.equal(tags(host, '[srcdoc]').length, 0);
    assert.equal(tags(host, '[formaction]').length, 0);
    assert.ok(!/srcdoc/i.test(html));
  });
});

describe('reader sanitization: SVG allowlist', () => {
  it('keeps the allowed shape vocabulary', async () => {
    const { host } = await renderFixture('svg');
    const svg = host.querySelector('svg');
    assert.ok(svg, 'inline svg survives');
    for (const tag of ['title', 'desc', 'g', 'path', 'circle', 'rect', 'line', 'polyline', 'polygon', 'ellipse']) {
      assert.ok(svg.querySelector(tag), `allowed SVG element <${tag}> was dropped`);
    }
    assert.equal(svg.querySelector('g').getAttribute('transform'), 'translate(2,2)');
    assert.equal(svg.querySelector('g').getAttribute('stroke'), 'currentColor');
    assert.equal(svg.querySelector('path').getAttribute('d'), 'M1 1 L10 10');
  });

  it('drops script, style, animation, foreignObject, image and use from SVG', async () => {
    const { host, window, html } = await renderFixture('svg');
    assert.equal(window.__pwned, undefined);
    for (const tag of ['script', 'style', 'foreignObject', 'animate', 'set', 'use', 'image']) {
      assert.equal(host.querySelectorAll(tag).length, 0, `<${tag}> survived inside SVG`);
    }
    assert.ok(!/javascript:/i.test(html));
  });

  it('allows only fragment hrefs inside SVG', async () => {
    const { host } = await renderFixture('svg');
    const svg = host.querySelector('svg');
    for (const element of svg.querySelectorAll('[href], [xlink\\:href]')) {
      const href = element.getAttribute('href') ?? element.getAttribute('xlink:href');
      assert.ok(href.startsWith('#'), `non-fragment SVG href survived: ${href}`);
    }
  });
});

describe('reader sanitization: attributes', () => {
  it('namespaces document ids so they cannot clobber app DOM properties', async () => {
    const { host } = await renderFixture('attributes');
    assert.equal(host.querySelector('#para'), null, 'a raw document id must not land in the app namespace');
    assert.ok(host.querySelector('#user-content-para'), 'document ids keep working under the safe prefix');
  });

  it('drops arbitrary data-* attributes but keeps id, class, title, lang and ARIA', async () => {
    const { host } = await renderFixture('attributes');
    const paragraph = host.querySelector('#user-content-para');
    assert.ok(paragraph, 'id survives');
    assert.equal(paragraph.getAttribute('class'), 'note');
    assert.equal(paragraph.getAttribute('title'), 'a title');
    assert.equal(paragraph.getAttribute('aria-label'), 'described');
    assert.equal(paragraph.getAttribute('role'), 'note');
    assert.equal(paragraph.getAttribute('lang'), 'en');
    assert.equal(paragraph.hasAttribute('data-loam'), false);
    assert.equal(paragraph.hasAttribute('data-x'), false);
    assert.equal(host.querySelectorAll('[data-loam], [data-x]').length, 0);
  });

  it('keeps table and details structure with their attributes', async () => {
    const { host } = await renderFixture('attributes');
    assert.ok(host.querySelector('table caption'));
    assert.equal(host.querySelector('th').getAttribute('scope'), 'col');
    assert.equal(host.querySelector('th').getAttribute('colspan'), '2');
    assert.equal(host.querySelector('td').getAttribute('rowspan'), '1');
    assert.ok(host.querySelector('details > summary'));
  });

  it('degrades images to escaped alt-text placeholders that fetch nothing', async () => {
    const { host, html } = await renderFixture('attributes');
    assert.equal(host.querySelectorAll('img').length, 0, 'no image element may remain');
    assert.ok(!/evil\.example\.com\/tracker\.png/.test(html), 'no image URL may remain in the DOM');
    const placeholder = host.querySelector('.md-image');
    assert.ok(placeholder, 'markdown image renders as a text placeholder');
    assert.ok(placeholder.textContent.includes('alt text with <script>alert(1)</script>'));
    assert.ok(!/<script/i.test(html));
  });
});

describe('reader front matter', () => {
  it('splits front matter from the body without treating it as content', async () => {
    const source = await fixture('frontmatter');
    const { frontMatter, body } = splitFrontMatter(source);
    assert.ok(frontMatter.includes('title: Hostile front matter'));
    assert.ok(body.startsWith('# Body after front matter'));
    assert.ok(!body.includes('---'));
  });

  it('refuses aliases, custom tags and over-deep documents', async () => {
    const source = await fixture('frontmatter');
    const { frontMatter } = splitFrontMatter(source);
    const result = readFrontMatter(frontMatter);
    assert.equal(result.data, null);
    assert.ok(result.error, 'hostile front matter reports an error instead of parsing');
  });

  it('parses ordinary front matter as failsafe strings only', () => {
    const result = readFrontMatter('title: Note\ncount: 3\nready: true\ntags:\n  - a\n  - b\n');
    assert.equal(result.error, null);
    assert.deepEqual(result.data, { title: 'Note', count: '3', ready: 'true', tags: ['a', 'b'] });
  });

  it('rejects nesting deeper than the bound', () => {
    const deep = 'a:\n b:\n  c:\n   d:\n    e:\n     f:\n      g: 1\n';
    assert.ok(readFrontMatter(deep).error, 'depth bound must reject');
    assert.equal(readFrontMatter('a:\n b:\n  c: 1\n').error, null);
  });

  it('never renders front matter through the markdown pipeline', async () => {
    const { host } = await renderFixture('frontmatter');
    assert.ok(!host.textContent.includes('Hostile front matter'));
    assert.ok(host.textContent.includes('Body after front matter'));
  });
});

describe('reader wikilinks', () => {
  const markdown = '[[Alpha Page]] and [[Missing Page|the label]] and [ordinary](./notes.md)\n';

  it('renders resolvable wikilinks as same-reader hash links', () => {
    const { host } = renderInLiveDom(markdown);
    const link = host.querySelector('a.wikilink');
    assert.ok(link, 'resolvable wikilink renders as an anchor');
    assert.equal(link.getAttribute('href'), '#/reader/wiki%2Falpha-page.md');
    assert.ok(link.classList.contains('is-resolved'));
    assert.equal(link.hasAttribute('rel'), false, 'in-app links need no rel');
  });

  it('marks broken wikilinks visibly, with evidence, and without a link target', () => {
    const { host } = renderInLiveDom(markdown);
    const broken = host.querySelector('.wikilink.is-broken');
    assert.ok(broken, 'broken wikilink stays visible');
    assert.equal(broken.tagName.toLowerCase(), 'span', 'a broken link must not be activatable');
    assert.ok(broken.textContent.includes('the label'));
    assert.ok(/unresolved|broken/i.test(broken.textContent), 'evidence is text, not colour alone');
  });

  it('routes relative markdown links to the same Reader surface', () => {
    const { host } = renderInLiveDom(markdown);
    const link = [...host.querySelectorAll('a')].find((a) => a.textContent === 'ordinary');
    assert.ok(link.getAttribute('href').startsWith('#/reader/'), 'relative .md links open in Reader');
  });
});
