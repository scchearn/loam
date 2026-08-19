/**
 * Reader rendering pipeline: Marked first, DOMPurify last.
 *
 * The order is the whole security story (spec: "Reader parsing and vendored
 * libraries"). Marked tokenizes and emits markup — it explicitly does not
 * sanitize — and DOMPurify is the final gate that returns a `DocumentFragment`
 * the app inserts with `replaceChildren`. Nothing in this module or its callers
 * may assign unsanitized `innerHTML`, and nothing may modify sanitized markup
 * afterwards except application-owned focus/listener wiring.
 *
 * The allowlists below are transcribed from the spec's sanitizer contract. They
 * are allowlists on purpose: an unknown tag, attribute, or URL scheme is denied
 * by default rather than matched against a list of known-bad shapes.
 */

import createDOMPurify from '../../vendor/dompurify/purify.es.mjs';
import { Marked } from '../../vendor/marked/marked.esm.js';
import { FAILSAFE_SCHEMA, EVENT_ALIAS, EVENT_MAPPING, EVENT_POP, EVENT_SEQUENCE, load, parseEvents } from '../../vendor/js-yaml/js-yaml.esm.min.mjs';

/** Ordinary Markdown structure plus the spec's inline-SVG vocabulary. */
const ALLOWED_TAGS = [
  'a', 'p', 'br', 'hr', 'span', 'strong', 'em', 'del', 's', 'sup', 'sub', 'abbr', 'kbd',
  'h1', 'h2', 'h3', 'h4', 'h5', 'h6',
  'ul', 'ol', 'li', 'dl', 'dt', 'dd',
  'blockquote', 'pre', 'code',
  'table', 'thead', 'tbody', 'tfoot', 'tr', 'th', 'td', 'caption', 'colgroup', 'col',
  'details', 'summary',
  // Inline SVG allowlist — exactly the spec's list. `use`, `image`,
  // `foreignObject`, animation elements, script and style are absent on purpose.
  'svg', 'g', 'path', 'circle', 'rect', 'line', 'polyline', 'polygon', 'ellipse', 'title', 'desc',
];

/** Text, link, ARIA, table, geometry, paint, transform, id and class. Nothing else. */
const ALLOWED_ATTR = [
  // text + link
  'href', 'rel', 'target', 'title', 'lang', 'dir', 'open', 'start', 'reversed', 'value',
  // ARIA (aria-* comes from ALLOW_ARIA_ATTR; role must be named)
  'role',
  // table
  'colspan', 'rowspan', 'scope', 'headers', 'abbr', 'span',
  // svg geometry
  'viewBox', 'preserveAspectRatio', 'x', 'y', 'x1', 'y1', 'x2', 'y2', 'cx', 'cy',
  'r', 'rx', 'ry', 'width', 'height', 'd', 'points', 'xmlns',
  // svg paint + transform
  'fill', 'fill-rule', 'fill-opacity', 'stroke', 'stroke-width', 'stroke-linecap',
  'stroke-linejoin', 'stroke-dasharray', 'stroke-dashoffset', 'stroke-opacity',
  'opacity', 'vector-effect', 'transform',
  // identity
  'id', 'class',
];

/**
 * Redundant given the allowlist above, and kept anyway: if a future edit widens
 * ALLOWED_TAGS these stay denied, and the list documents the contract.
 */
const FORBID_TAGS = [
  'script', 'style', 'iframe', 'frame', 'frameset', 'object', 'embed', 'applet',
  'form', 'input', 'button', 'select', 'option', 'optgroup', 'textarea', 'label',
  'fieldset', 'legend', 'output', 'audio', 'video', 'source', 'track', 'canvas',
  'base', 'meta', 'link', 'template', 'noscript', 'math', 'marquee', 'portal', 'dialog',
  'use', 'image', 'foreignObject', 'animate', 'animateTransform', 'animateMotion', 'set', 'filter',
];

const FORBID_ATTR = ['style', 'srcdoc', 'src', 'formaction', 'action', 'xlink:href', 'ping', 'sandbox', 'allow'];

/** Same-Reader fragments, plus user-activated http/https/mailto. */
const ALLOWED_URI_REGEXP = /^(?:#|https?:|mailto:)/i;
const EXTERNAL_URI_REGEXP = /^(?:https?:|mailto:)/i;

/**
 * DOMPurify runs `ALLOWED_URI_REGEXP` against every attribute value that is not
 * declared URI-safe, so narrowing that regexp to Reader's three schemes would
 * also delete `transform="translate(2,2)"`, `colspan="2"`, and every other
 * ordinary value. These attributes never carry a URL, so they are declared safe
 * and the strict URI test stays reserved for the ones that do (`href`).
 */
const URI_SAFE_ATTR = [
  'lang', 'dir', 'open', 'start', 'reversed', 'rel', 'target',
  'colspan', 'rowspan', 'scope', 'headers', 'abbr', 'span',
  'viewBox', 'preserveAspectRatio', 'x', 'y', 'x1', 'y1', 'x2', 'y2', 'cx', 'cy',
  'r', 'rx', 'ry', 'width', 'height', 'd', 'points', 'transform', 'xmlns',
  'fill', 'fill-rule', 'fill-opacity', 'stroke', 'stroke-width', 'stroke-linecap',
  'stroke-linejoin', 'stroke-dasharray', 'stroke-dashoffset', 'stroke-opacity',
  'opacity', 'vector-effect',
];

/** Paint attributes accept `url(...)` references; Reader does not. */
const PAINT_ATTR = new Set([
  'fill', 'stroke', 'fill-rule', 'fill-opacity', 'stroke-width', 'stroke-linecap',
  'stroke-linejoin', 'stroke-dasharray', 'stroke-dashoffset', 'stroke-opacity',
  'opacity', 'vector-effect', 'transform',
]);

export const SANITIZER_CONFIG = Object.freeze({
  ALLOWED_TAGS,
  ALLOWED_ATTR,
  FORBID_TAGS,
  FORBID_ATTR,
  ALLOWED_URI_REGEXP,
  ADD_URI_SAFE_ATTR: URI_SAFE_ATTR,
  ALLOW_DATA_ATTR: false,
  ALLOW_ARIA_ATTR: true,
  ALLOW_UNKNOWN_PROTOCOLS: false,
  ALLOW_SELF_CLOSE_IN_ATTR: false,
  SANITIZE_NAMED_PROPS: true,
  SANITIZE_DOM: true,
  KEEP_CONTENT: true,
  RETURN_DOM_FRAGMENT: true,
  IN_PLACE: false,
});

const SVG_NS = 'http://www.w3.org/2000/svg';

export function escapeHtml(value) {
  return String(value ?? '')
    .replaceAll('&', '&amp;')
    .replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;')
    .replaceAll('"', '&quot;')
    .replaceAll("'", '&#39;');
}

/** GitHub-style, deterministic: the same heading text always yields the same id. */
export function slugify(text) {
  return String(text ?? '')
    .toLowerCase()
    .replace(/[^\p{L}\p{N}\s-]/gu, '')
    .trim()
    .replace(/\s+/g, '-')
    .replace(/-{2,}/g, '-') || 'section';
}

/** `#/reader/<encoded path>` — Reader navigation is ordinary hash history. */
export function readerHref(path) {
  const [target, fragment] = String(path).split('#');
  return `#/reader/${encodeURIComponent(target)}${fragment ? `#${fragment}` : ''}`;
}

/**
 * Resolve `../x.md` against the directory of the document being read. Returns
 * null when the link climbs out of the workspace root: an escaping path is a
 * broken link to show, never a path to normalize and follow.
 */
export function resolveRelative(basePath, href) {
  const segments = String(basePath ?? '').split('/').slice(0, -1);
  for (const part of String(href).split('/')) {
    if (part === '' || part === '.') continue;
    if (part === '..') {
      if (segments.length === 0) return null;
      segments.pop();
    } else {
      segments.push(part);
    }
  }
  return segments.join('/') || null;
}

/** One shape for every unavailable link: visible text, plus the reason in text. */
function brokenLink(label, target, reason) {
  return `<span class="wikilink is-broken" title="${escapeHtml(target)}">${label}` +
    `<span class="wikilink-note"> (${escapeHtml(reason)})</span></span>`;
}

const READER_HREF_PREFIX = '#/reader/';

/**
 * A workspace-relative document path and nothing else: no traversal, no
 * absolute or scheme-bearing form, no separators the server would read
 * differently from us.
 */
export function isSafeDocumentPath(path) {
  const value = String(path ?? '');
  if (!value || value.length > 1024) return false;
  if (/[\u0000-\u001f\\]/.test(value)) return false;
  if (value.startsWith('/') || /^[a-z][a-z0-9+.-]*:/i.test(value)) return false;
  return !value.split('/').some((segment) => segment === '' || segment === '.' || segment === '..');
}

/** The path a `#/reader/...` href asks for, or null when it is not one. */
export function readerHrefPath(href) {
  const value = String(href ?? '');
  if (!value.toLowerCase().startsWith(READER_HREF_PREFIX)) return null;
  const [target] = value.slice(READER_HREF_PREFIX.length).split('#');
  try {
    return decodeURIComponent(target);
  } catch {
    return '';
  }
}

function isInternalDocument(href) {
  return /^[^:#?]*\.md(?:#[^#]*)?$/i.test(href);
}

// --- front matter -----------------------------------------------------------

const MAX_FRONT_MATTER_DEPTH = 4;

/** Front matter is metadata, never document content: it never reaches Marked. */
export function splitFrontMatter(text) {
  const source = String(text ?? '').replace(/^﻿/, '');
  const match = /^---[ \t]*\r?\n([\s\S]*?)\r?\n---[ \t]*(?:\r?\n|$)/.exec(source);
  if (!match) return { frontMatter: '', body: source };
  return { frontMatter: match[1], body: source.slice(match[0].length).replace(/^\s*\n/, '') };
}

/**
 * Parse front matter for *display only*: FAILSAFE_SCHEMA (everything is a
 * string, no type coercion and no custom tags), aliases rejected, and nesting
 * bounded. js-yaml 5 has no alias or depth option, so the event stream is
 * scanned before the value is constructed — an alias-bomb never gets built.
 */
export function readFrontMatter(yamlText, { maxDepth = MAX_FRONT_MATTER_DEPTH } = {}) {
  const source = String(yamlText ?? '').trim();
  if (!source) return { data: null, error: null };

  try {
    let depth = 0;
    for (const event of parseEvents(source, { schema: FAILSAFE_SCHEMA })) {
      if (event.type === EVENT_ALIAS) throw new Error('YAML aliases are not allowed in front matter');
      if (event.type === EVENT_MAPPING || event.type === EVENT_SEQUENCE) {
        depth += 1;
        if (depth > maxDepth) throw new Error(`front matter nests deeper than ${maxDepth} levels`);
      } else if (event.type === EVENT_POP) {
        depth -= 1;
      }
    }
    const data = load(source, { schema: FAILSAFE_SCHEMA });
    if (data === null || typeof data !== 'object' || Array.isArray(data)) {
      return { data: null, error: 'front matter is not a mapping' };
    }
    return { data, error: null };
  } catch (error) {
    return { data: null, error: error?.message ?? 'front matter could not be read' };
  }
}

// --- rendering --------------------------------------------------------------

/**
 * Build a renderer bound to one window.
 *
 * `resolve(target)` maps a wikilink target to `{path, title}` when the
 * workspace has that artifact, or null when it does not — a null is rendered as
 * a visibly broken link rather than silently dropped.
 */
export function createRenderer({ window: win = globalThis.window, resolve = () => null, basePath = '' } = {}) {
  const purify = createDOMPurify(win);
  /**
   * Reader targets this render vouched for. The renderer emits a `#/reader/...`
   * href only after the resolver admitted the path, so the sanitizer can hold
   * every anchor — including raw-HTML ones the renderer never saw — to exactly
   * that list.
   */
  let vouchedReaderHrefs = new Set();

  purify.addHook('afterSanitizeAttributes', (node) => {
    if (typeof node.getAttribute !== 'function') return;

    // Belt and braces over the allowlist: handlers, inline style, srcdoc and
    // arbitrary data-* can never appear, whatever a later config edit allows.
    for (const attribute of [...(node.attributes ?? [])]) {
      const name = attribute.name.toLowerCase();
      if (name.startsWith('on') || name.startsWith('data-') || name === 'style' || name === 'srcdoc') {
        node.removeAttribute(attribute.name);
      } else if (PAINT_ATTR.has(name) && /url\s*\(|:/i.test(attribute.value)) {
        // A paint or transform value may not reference an external (or any)
        // resource: `fill="url(...)"` is the one place these attributes could
        // point somewhere.
        node.removeAttribute(attribute.name);
      }
    }

    if (node.hasAttribute('xlink:href')) node.removeAttribute('xlink:href');
    if (!node.hasAttribute('href')) return;

    const href = node.getAttribute('href').trim();
    if (node.namespaceURI === SVG_NS) {
      // SVG href is fragment-only: no remote document, no scheme, ever.
      if (!href.startsWith('#')) node.removeAttribute('href');
      return;
    }
    if (EXTERNAL_URI_REGEXP.test(href)) {
      node.setAttribute('rel', 'noopener noreferrer');
      node.setAttribute('target', '_blank');
      return;
    }
    if (!href.startsWith('#')) {
      node.removeAttribute('href');
      return;
    }
    // Raw-HTML anchors never pass through the renderer, so the resolver gate on
    // `#/reader/...` targets is applied here too: a document may not hand the
    // Reader a path this render did not resolve.
    if (readerHrefPath(href) !== null && !vouchedReaderHrefs.has(href)) node.removeAttribute('href');
  });

  /** The only sanitization entry point. Returns a fragment, never a string. */
  function sanitize(html) {
    return purify.sanitize(String(html ?? ''), SANITIZER_CONFIG);
  }

  /** Record a Reader target this render resolved, and hand it back. */
  function vouch(href) {
    vouchedReaderHrefs.add(href);
    return href;
  }

  function markedFor() {
    const seen = new Map();
    const marked = new Marked({ gfm: true, breaks: false });

    marked.use({
      extensions: [{
        name: 'wikilink',
        level: 'inline',
        start(src) { return src.indexOf('[['); },
        tokenizer(src) {
          const match = /^\[\[([^[\]|\n]+?)(?:\|([^[\]\n]*?))?\]\]/.exec(src);
          if (!match) return undefined;
          const target = match[1].trim();
          return { type: 'wikilink', raw: match[0], target, label: (match[2] ?? target).trim() };
        },
        renderer(token) {
          const [target, fragment] = token.target.split('#');
          const hit = resolve(target);
          const label = escapeHtml(token.label);
          if (!hit?.path) return brokenLink(label, token.target, 'unresolved link');
          const href = vouch(readerHref(fragment ? `${hit.path}#${slugify(fragment)}` : hit.path));
          return `<a class="wikilink is-resolved" href="${escapeHtml(href)}">${label}</a>`;
        },
      }],
      renderer: {
        heading(token) {
          const text = this.parser.parseInline(token.tokens);
          const base = slugify(token.text);
          const count = (seen.get(base) ?? 0) + 1;
          seen.set(base, count);
          const id = count === 1 ? base : `${base}-${count}`;
          return `<h${token.depth} id="${escapeHtml(id)}">${text}</h${token.depth}>\n`;
        },
        link(token) {
          const text = this.parser.parseInline(token.tokens);
          const href = String(token.href ?? '').trim();
          const title = token.title ? ` title="${escapeHtml(token.title)}"` : '';
          if (isInternalDocument(href)) {
            const [target, fragment] = href.split('#');
            const path = resolveRelative(basePath, target);
            // Same treatment as a wikilink: out-of-root and missing documents
            // are shown as broken rather than offered as working links.
            if (!path) return brokenLink(text, href, 'link leaves the workspace');
            const hit = resolve(path);
            if (!hit?.path) return brokenLink(text, href, 'missing document');
            const readerTarget = vouch(readerHref(fragment ? `${hit.path}#${fragment}` : hit.path));
            return `<a class="md-link is-document" href="${escapeHtml(readerTarget)}"${title}>${text}</a>`;
          }
          const readerPath = readerHrefPath(href);
          if (readerPath !== null) {
            // A hand-written `#/reader/...` link is a document link like any
            // other and passes the same resolver gate, not the fragment branch.
            const hit = isSafeDocumentPath(readerPath) ? resolve(readerPath) : null;
            if (hit?.path !== readerPath) return brokenLink(text, href, 'unresolved link');
            return `<a class="md-link is-document" href="${escapeHtml(vouch(href))}"${title}>${text}</a>`;
          }
          if (href.startsWith('#')) return `<a class="md-link" href="${escapeHtml(href)}"${title}>${text}</a>`;
          if (EXTERNAL_URI_REGEXP.test(href)) {
            return `<a class="md-link is-external" href="${escapeHtml(href)}" rel="noopener noreferrer" target="_blank"${title}>${text}</a>`;
          }
          // Unsafe or unknown scheme: keep the text, drop the target, say so.
          return `<span class="md-link is-blocked">${text}<span class="md-link-note"> (blocked link)</span></span>`;
        },
        // v1 renders no images: an alt-text placeholder cannot fetch a tracker.
        image(token) {
          const alt = escapeHtml(token.text || token.href || 'image');
          return `<span class="md-image" role="img" aria-label="${alt}">[image: ${alt}]</span>`;
        },
      },
    });
    return marked;
  }

  /**
   * Markdown in, sanitized DocumentFragment out. Front matter is split off here
   * as well as by the caller: metadata is never document content, so it can
   * never reach Marked by accident.
   */
  function render(markdown) {
    const { body } = splitFrontMatter(markdown);
    vouchedReaderHrefs = new Set();
    return sanitize(markedFor().parse(body, { async: false }));
  }

  return { render, sanitize };
}

/** Headings of an already-sanitized, already-inserted document, for the outline. */
export function outlineOf(container) {
  return [...container.querySelectorAll('h1[id], h2[id], h3[id], h4[id], h5[id], h6[id]')].map((heading) => ({
    id: heading.id,
    depth: Number(heading.tagName.slice(1)),
    text: heading.textContent.trim(),
  }));
}
