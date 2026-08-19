/**
 * Loam View app shell: chrome, hash routing, and the Query palette.
 *
 * The five area views are mounted but empty here on purpose. Each one owns a
 * `[data-mount]` element and is filled by its own module; this file only decides
 * which one is visible and hands it the current snapshot through the
 * `loam:render` event.
 *
 * No build step: this is a browser-native ES module, and the server's CSP allows
 * no inline script or style, so everything lives in files.
 */

import { initQuery } from './query.mjs';
import { load, refresh, state } from './store.mjs';

const ROUTES = ['pulse', 'atlas', 'work-stream', 'chronicle', 'stewardship'];
const DEFAULT_ROUTE = 'pulse';

export function routeFromHash(hash) {
  const candidate = String(hash ?? '').replace(/^#\/?/, '');
  return ROUTES.includes(candidate) ? candidate : DEFAULT_ROUTE;
}

/** `2026-07-17T15:30:00+02:00` -> `2026-07-17 15:30`. Workspace-local, no locale guessing. */
function shortTimestamp(value) {
  const match = /^(\d{4}-\d{2}-\d{2})T(\d{2}:\d{2})/.exec(String(value ?? ''));
  return match ? `${match[1]} ${match[2]}` : 'unknown';
}

function renderChrome(doc, snapshot) {
  const workspace = snapshot?.workspace ?? {};
  const git = workspace.git ?? {};

  doc.querySelector('[data-workspace-name]').textContent = workspace.name || 'Workspace';

  const context = [];
  if (git.branch) context.push(git.branch);
  if (git.state) context.push(git.dirty ? `${git.state} (${git.changed_count} changed)` : git.state);
  doc.querySelector('[data-workspace-context]').textContent = context.join(' · ');

  const qmd = snapshot?.capabilities?.qmd?.state ?? 'unknown';
  doc.querySelector('[data-freshness]').textContent =
    `Snapshot ${shortTimestamp(snapshot?.generated_at)} · qmd ${qmd}`;
  doc.querySelector('[data-freshness-dot]').dataset.state = qmd;
}

function renderNotice(doc, message) {
  const notice = doc.querySelector('[data-notice]');
  notice.textContent = message ?? '';
  notice.classList.toggle('is-visible', Boolean(message));
}

function renderRoute(doc, route) {
  for (const view of doc.querySelectorAll('[data-view]')) {
    view.classList.toggle('is-active', view.dataset.view === route);
  }
  for (const button of doc.querySelectorAll('[data-route]')) {
    const active = button.dataset.route === route;
    if (active) button.setAttribute('aria-current', 'page');
    else button.removeAttribute('aria-current');
  }
}

export async function boot(doc = globalThis.document) {
  const win = doc.defaultView ?? globalThis;
  let route = routeFromHash(win.location?.hash);

  function render() {
    renderChrome(doc, state.snapshot);
    renderNotice(doc, state.error);
    renderRoute(doc, route);
    // Area views (Pulse, Atlas, Work Stream, Chronicle, Stewardship) subscribe
    // to this instead of importing the shell.
    // The document's own CustomEvent constructor: a host-realm event is not a
    // valid argument to a jsdom document's dispatchEvent.
    doc.dispatchEvent(new win.CustomEvent('loam:render', {
      detail: { snapshot: state.snapshot, route },
    }));
  }

  win.addEventListener?.('hashchange', () => {
    route = routeFromHash(win.location?.hash);
    render();
  });

  for (const button of doc.querySelectorAll('[data-route]')) {
    button.addEventListener('click', () => {
      win.location.hash = `#/${button.dataset.route}`;
      // Programmatic hash writes do not always deliver hashchange in every
      // host (jsdom included), so route straight away and stay idempotent.
      route = button.dataset.route;
      render();
    });
  }

  const refreshButton = doc.querySelector('[data-refresh]');
  refreshButton?.addEventListener('click', async () => {
    refreshButton.disabled = true;
    refreshButton.textContent = 'Refreshing';
    try {
      await refresh();
    } finally {
      refreshButton.disabled = false;
      refreshButton.textContent = 'Refresh';
      render();
    }
  });

  initQuery({ root: doc, getSnapshot: () => state.snapshot });

  render();
  try {
    await load();
  } catch {
    // `state.error` carries the reason. The shell stays usable so the human can
    // retry with Refresh instead of staring at a blank page.
  }
  render();
  return { render, route: () => route };
}
