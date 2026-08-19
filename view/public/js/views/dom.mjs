/**
 * The three DOM moves every area view needs: build an element, open the shared
 * Inspector on an artifact, and turn an inventoried Loam Markdown artifact into
 * a Reader entry point.
 *
 * Security: every snapshot string reaches the document through `createTextNode`.
 * There is no `innerHTML` in the view modules, and there must never be.
 */

import { openInspector } from '../inspector.mjs';

/** `el('p', 'river-detail', text)` — className and text are both optional. */
export function maker(doc) {
  return (tag, className, text) => {
    const node = doc.createElement(tag);
    if (className) node.className = className;
    if (text != null) node.appendChild(doc.createTextNode(String(text)));
    return node;
  };
}

export function inspectButton(doc, artifact, { label = null, className = 'inspect-link' } = {}) {
  const button = maker(doc)('button', className, label ?? artifact.title ?? artifact.path);
  button.type = 'button';
  button.dataset.inspect = artifact.path;
  button.addEventListener('click', () => openInspector(artifact));
  return button;
}

/**
 * Reader renders only Loam Markdown that the snapshot inventories, so this takes
 * an artifact — never a bare path. Callers keep uninventoried paths as text.
 */
export function readerLink(doc, artifact, { label = null, line = null } = {}) {
  const win = doc.defaultView ?? globalThis;
  const button = maker(doc)('button', 'file-link', label ?? artifact.path);
  button.type = 'button';
  button.dataset.path = artifact.path;
  button.addEventListener('click', () => {
    doc.dispatchEvent(new win.CustomEvent('loam:open-reader', {
      detail: { path: artifact.path, kind: artifact.kind, title: artifact.title, line },
    }));
  });
  return button;
}
