/**
 * Reader: the full-screen in-app document surface.
 *
 * Contract (spec: "Global Query, Inspector, and Reader"):
 *   - every open is one fresh `/api/document` read — there is no document cache
 *     and no document ID scheme; the artifact path is the identity;
 *   - it lives inside the shell as an overlay, keeps its own Back control, and
 *     restores the exact context it was opened from;
 *   - wikilink navigation is ordinary hash history, so browser back/forward
 *     work without a bespoke navigation stack;
 *   - content reaches the DOM only as a sanitized fragment from markdown.mjs.
 *
 * Openers announce themselves with a `loam:open-reader` CustomEvent (the Query
 * palette's older `loam:open-document` is accepted as the same contract). Any
 * `detail.return` payload is stored untouched and handed back on
 * `loam:reader-closed`, so a caller such as the Inspector can restore whatever
 * state it had without Reader needing to understand it.
 */

import { createRenderer, outlineOf, readFrontMatter, splitFrontMatter } from './markdown.mjs';
import { refresh } from './store.mjs';

const READER_PREFIX = '#/reader/';

/** `#/reader/wiki%2Fpage.md#heading` -> `{path, fragment}` (null when not a Reader route). */
export function readerRoute(hash) {
  const value = String(hash ?? '');
  if (!value.startsWith(READER_PREFIX)) return null;
  const [target, fragment = ''] = value.slice(READER_PREFIX.length).split('#');
  if (!target) return null;
  try {
    return { path: decodeURIComponent(target), fragment };
  } catch {
    return null;
  }
}

function slug(value) {
  return String(value ?? '').trim().toLowerCase().replace(/\.md$/, '').replace(/[\s_]+/g, '-');
}

/**
 * Wikilink resolution against the snapshot inventory: basename or path match,
 * case-insensitive, and unique. An ambiguous target is treated as unresolved
 * rather than guessed — the spec makes ambiguity a diagnostic, not a redirect.
 */
export function resolverFor(snapshot) {
  const artifacts = snapshot?.artifacts ?? [];
  return (target) => {
    const wanted = slug(target);
    const matches = artifacts.filter((artifact) => {
      const path = String(artifact.path ?? '');
      return slug(path) === wanted || slug(path.split('/').pop()) === wanted || slug(artifact.title) === wanted;
    });
    return matches.length === 1 ? { path: matches[0].path, title: matches[0].title } : null;
  };
}

export function initReader({ root = document, getSnapshot = () => null, refreshSnapshot = refresh } = {}) {
  const surface = root.querySelector('[data-reader]');
  if (!surface) return null;

  const doc = surface.ownerDocument;
  const win = doc.defaultView ?? globalThis;
  const shell = root.querySelector('[data-app-shell]');
  const titleEl = surface.querySelector('[data-reader-title]');
  const pathEl = surface.querySelector('[data-reader-path]');
  const bannerEl = surface.querySelector('[data-reader-banner]');
  const bannerText = surface.querySelector('[data-reader-banner-text]');
  const refreshButton = surface.querySelector('[data-reader-refresh]');
  const backButton = surface.querySelector('[data-reader-back]');
  const metaEl = surface.querySelector('[data-reader-frontmatter]');
  const metaBody = surface.querySelector('[data-reader-frontmatter-body]');
  const outlineEl = surface.querySelector('[data-reader-outline]');
  const outlineList = surface.querySelector('[data-reader-outline-list]');
  const article = surface.querySelector('[data-reader-doc]');
  const statusEl = surface.querySelector('[data-reader-status]');

  let open = false;
  let currentPath = null;
  /** Hash we wrote ourselves: the host's own hashchange must not re-read it. */
  let expectedHash = null;
  /** Where Back returns to, and the opener's own payload, verbatim. */
  let returnContext = { hash: '', detail: null };

  function setStatus(message) {
    statusEl.textContent = message ?? '';
    statusEl.classList.toggle('is-visible', Boolean(message));
  }

  function setBanner(message) {
    bannerText.textContent = message ?? '';
    bannerEl.hidden = !message;
  }

  function renderFrontMatter(frontMatter) {
    metaBody.replaceChildren();
    if (!frontMatter.trim()) {
      metaEl.hidden = true;
      return;
    }
    metaEl.hidden = false;
    const { data, error } = readFrontMatter(frontMatter);
    if (!data) {
      const note = doc.createElement('p');
      note.className = 'reader-meta-error';
      note.textContent = `Front matter not shown: ${error ?? 'unreadable'}`;
      metaBody.appendChild(note);
      return;
    }
    // Front matter is displayed, never executed: every key and value is text.
    for (const [key, value] of Object.entries(data)) {
      const term = doc.createElement('dt');
      term.textContent = key;
      const definition = doc.createElement('dd');
      definition.textContent = Array.isArray(value)
        ? value.map((entry) => (typeof entry === 'string' ? entry : JSON.stringify(entry))).join(', ')
        : (typeof value === 'string' ? value : JSON.stringify(value));
      metaBody.append(term, definition);
    }
  }

  function renderOutline() {
    outlineList.replaceChildren();
    const headings = outlineOf(article);
    outlineEl.hidden = headings.length === 0;
    for (const heading of headings) {
      const item = doc.createElement('li');
      item.className = 'reader-outline-item';
      item.dataset.depth = String(heading.depth);
      const link = doc.createElement('a');
      link.href = `#${heading.id}`;
      link.textContent = heading.text;
      item.appendChild(link);
      outlineList.appendChild(item);
    }
  }

  /** Document ids are namespaced by the sanitizer, so try both spellings. */
  function headingFor(fragment) {
    if (!fragment) return null;
    return article.querySelector(`[id="${CSS?.escape ? CSS.escape(fragment) : fragment}"]`)
      ?? article.querySelector(`[id="user-content-${CSS?.escape ? CSS.escape(fragment) : fragment}"]`);
  }

  function show() {
    if (open) return;
    open = true;
    surface.hidden = false;
    shell?.setAttribute('aria-hidden', 'true');
    doc.body.classList.add('reader-open');
  }

  function hide() {
    open = false;
    surface.hidden = true;
    shell?.removeAttribute('aria-hidden');
    doc.body.classList.remove('reader-open');
    article.replaceChildren();
    currentPath = null;
  }

  async function loadDocument(path, fragment = '') {
    show();
    currentPath = path;
    titleEl.textContent = path.split('/').pop() ?? path;
    pathEl.textContent = path;
    setBanner('');
    setStatus('Reading…');

    let payload;
    try {
      const response = await fetch(`/api/document?path=${encodeURIComponent(path)}`, {
        headers: { accept: 'application/json' },
      });
      const body = await response.json().catch(() => null);
      if (!response.ok) throw new Error([body?.error, body?.message].filter(Boolean).join(': ') || `HTTP ${response.status}`);
      payload = body;
    } catch (error) {
      article.replaceChildren();
      renderOutline();
      metaEl.hidden = true;
      setStatus(`This document could not be read: ${error.message}`);
      return null;
    }

    if (currentPath !== path) return null; // a newer open won

    const { frontMatter, body } = splitFrontMatter(payload.content ?? '');
    renderFrontMatter(frontMatter);

    const renderer = createRenderer({
      window: win,
      basePath: path,
      resolve: resolverFor(getSnapshot()),
    });
    // The only insertion point for document content: a sanitized fragment.
    article.replaceChildren(renderer.render(body));
    renderOutline();
    setStatus('');
    setBanner(payload.changed_since_snapshot
      ? 'Changed since snapshot — showing current file content.'
      : '');

    const heading = headingFor(fragment);
    if (heading) heading.scrollIntoView?.({ block: 'start' });
    else article.focus?.();
    return payload;
  }

  function openPath(path, { detail = null, fragment = '' } = {}) {
    const hash = String(win.location?.hash ?? '');
    if (!open) {
      // Remember exactly where the human was before Reader covered the shell.
      returnContext = { hash, detail: detail?.return ?? detail?.context ?? detail ?? null };
    }
    const target = `#/reader/${encodeURIComponent(path)}${fragment ? `#${fragment}` : ''}`;
    if (hash !== target) {
      // Render straight away rather than waiting on hashchange, which not every
      // host delivers for a programmatic write; `expectedHash` keeps the one
      // that *is* delivered from reading the same document twice.
      expectedHash = target;
      win.location.hash = target;
    }
    return loadDocument(path, fragment);
  }

  function back() {
    if (!open) return;
    const context = returnContext;
    hide();
    if (String(win.location?.hash ?? '') !== context.hash) win.location.hash = context.hash;
    const CustomEventCtor = doc.defaultView?.CustomEvent ?? CustomEvent;
    doc.dispatchEvent(new CustomEventCtor('loam:reader-closed', { detail: context.detail }));
  }

  backButton.addEventListener('click', back);

  refreshButton.addEventListener('click', async () => {
    const path = currentPath;
    refreshButton.disabled = true;
    try {
      await refreshSnapshot();
    } finally {
      refreshButton.disabled = false;
    }
    if (path) await loadDocument(path);
  });

  surface.addEventListener('keydown', (event) => {
    if (event.key === 'Escape') {
      event.preventDefault();
      back();
    }
  });

  // In-document fragment links stay inside Reader: following them as real hash
  // navigation would look like leaving the Reader route.
  article.addEventListener('click', (event) => {
    const link = event.target.closest?.('a[href^="#"]');
    if (!link) return;
    const href = link.getAttribute('href');
    if (href.startsWith(READER_PREFIX)) return; // a wikilink: let hash history handle it
    event.preventDefault();
    headingFor(href.slice(1))?.scrollIntoView?.({ block: 'start' });
  });

  function onHashChange() {
    const hash = String(win.location?.hash ?? '');
    const wasExpected = hash === expectedHash;
    expectedHash = null;
    if (wasExpected) return;
    const route = readerRoute(hash);
    if (route) {
      // Fresh disk read on every open, including back/forward between documents.
      if (!open) returnContext = { hash: '', detail: null };
      loadDocument(route.path, route.fragment);
    } else if (open) {
      hide();
    }
  }

  win.addEventListener?.('hashchange', onHashChange);

  for (const eventName of ['loam:open-reader', 'loam:open-document']) {
    doc.addEventListener(eventName, (event) => {
      const detail = event.detail ?? {};
      if (!detail.path) return;
      openPath(detail.path, { detail, fragment: detail.fragment ?? '' });
    });
  }

  // A deep link straight into a document (`#/reader/...`) opens on boot.
  const initial = readerRoute(win.location?.hash);
  if (initial) loadDocument(initial.path, initial.fragment);

  return { openPath, back, isOpen: () => open, currentPath: () => currentPath };
}
