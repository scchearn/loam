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
import { initReader } from './reader.mjs';
import { load, refresh, state } from './store.mjs';

const ROUTES = ['pulse', 'atlas', 'work-stream', 'chronicle', 'stewardship'];
const DEFAULT_ROUTE = 'pulse';

export function routeFromHash(hash, current = DEFAULT_ROUTE) {
  const value = String(hash ?? '');
  // Reader is an overlay, not an area: its route leaves the view underneath as
  // it was, so Back returns to the originating view rather than to Pulse.
  if (value.startsWith('#/reader/')) return current;
  const candidate = value.replace(/^#\/?/, '');
  if (ROUTES.includes(candidate)) return candidate;
  // An in-document anchor — the skip link's `#workspace`, or anything a human
  // pastes — is not a route. Falling back to Pulse here would throw away the
  // view the first tab stop was supposed to skip into.
  return value.startsWith('#/') || value === '' ? DEFAULT_ROUTE : current;
}

/** `2026-07-17T15:30:00+02:00` -> `2026-07-17 15:30`. Workspace-local, no locale guessing. */
function shortTimestamp(value) {
  const match = /^(\d{4}-\d{2}-\d{2})T(\d{2}:\d{2})/.exec(String(value ?? ''));
  return match ? `${match[1]} ${match[2]}` : 'unknown';
}

function renderChrome(doc, snapshot, error) {
  const workspace = snapshot?.workspace ?? {};
  const git = workspace.git ?? {};

  doc.querySelector('[data-workspace-name]').textContent = workspace.name || 'Workspace';

  const context = [];
  if (git.branch) context.push(git.branch);
  if (git.state) context.push(git.dirty ? `${git.state} (${git.changed_count} changed)` : git.state);
  doc.querySelector('[data-workspace-context]').textContent = context.join(' · ');

  const qmd = snapshot?.capabilities?.qmd?.state ?? 'unknown';
  // The chip states what is on screen. A failed read leaves a stale snapshot
  // showing, and saying only when it was taken would read as current.
  const freshness = !snapshot
    ? 'No snapshot'
    : `Snapshot ${shortTimestamp(snapshot.generated_at)} · qmd ${qmd}${error ? ' · stale' : ''}`;
  doc.querySelector('[data-freshness]').textContent = freshness;
  doc.querySelector('[data-freshness-dot]').dataset.state = error ? 'stale' : qmd;
}

/** One period, wherever the reason came from. */
function sentence(reason) {
  const text = String(reason ?? '').trim();
  return /[.!?]$/.test(text) ? text : `${text}.`;
}

/**
 * A raw fetch string ("Failed to fetch", "HTTP 500") names neither what broke
 * nor what is still on screen. The notice says both, plus the way out.
 */
export function noticeMessage(error, snapshot) {
  if (!error) return '';
  if (!snapshot) return `Could not read the snapshot: ${sentence(error)} Press Refresh to retry.`;
  return `Refresh failed: ${sentence(error)} Showing the snapshot from `
    + `${shortTimestamp(snapshot.generated_at)}. Press Refresh to retry.`;
}

function renderNotice(doc, message) {
  const notice = doc.querySelector('[data-notice]');
  notice.textContent = message ?? '';
  notice.classList.toggle('is-visible', Boolean(message));
}

function renderRoute(doc, route, readable = true) {
  for (const view of doc.querySelectorAll('[data-view]')) {
    view.classList.toggle('is-active', readable && view.dataset.view === route);
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
    // A refresh failure keeps the previous snapshot, so this stays true and the
    // views keep rendering it; only a workspace that was never read is degraded.
    const readable = Boolean(state.snapshot);
    renderChrome(doc, state.snapshot, state.error);
    renderNotice(doc, noticeMessage(state.error, state.snapshot));
    renderRoute(doc, route, readable);

    const unreadable = doc.querySelector('[data-unreadable]');
    if (unreadable) unreadable.hidden = readable;
    if (!readable) {
      const reason = doc.querySelector('[data-unreadable-reason]');
      if (reason) {
        // The notice above already carries the full sentence; under the panel's
        // own "No snapshot" heading the reason alone is enough.
        reason.textContent = state.error
          ? `${sentence(state.error)} Press Refresh to retry.`
          : 'Reading the workspace…';
      }
      // Never hand the areas a null snapshot: each one would render its own
      // "this workspace has none of X" over data nobody has read.
      return;
    }

    // Area views (Pulse, Atlas, Work Stream, Chronicle, Stewardship) subscribe
    // to this instead of importing the shell.
    // The document's own CustomEvent constructor: a host-realm event is not a
    // valid argument to a jsdom document's dispatchEvent.
    doc.dispatchEvent(new win.CustomEvent('loam:render', {
      detail: { snapshot: state.snapshot, route },
    }));
  }

  win.addEventListener?.('hashchange', () => {
    route = routeFromHash(win.location?.hash, route);
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
    refreshButton.setAttribute('aria-busy', 'true');
    refreshButton.textContent = 'Refreshing';
    try {
      await refresh();
    } finally {
      refreshButton.disabled = false;
      refreshButton.removeAttribute('aria-busy');
      refreshButton.textContent = 'Refresh';
      // Disabling the control drops focus to <body>; hand it back so a
      // keyboard-only refresh does not restart the tab order.
      if (doc.activeElement === doc.body || !doc.activeElement) refreshButton.focus();
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
  // After the snapshot, never before: Reader admits a path only if the
  // inventory lists it, and a deep link must be checked against a real one.
  initReader({ root: doc, getSnapshot: () => state.snapshot });
  return { render, route: () => route };
}
