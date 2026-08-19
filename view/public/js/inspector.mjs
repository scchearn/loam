/**
 * The shared Inspector: a full-height right-side modal side sheet that floats
 * above whatever view is on screen (DESIGN.md §4 "inspector", §5 "Inspector and
 * Reader are top-level overlays above the shell (not grid columns)").
 *
 * It never reflows, resizes, or repositions the view beneath. The panel and its
 * scrim are `position: fixed` siblings of `.app-shell`, and the only thing that
 * happens to the shell when the Inspector opens is `inert` — modality without a
 * pixel of movement.
 *
 * Security: every string here comes from the snapshot or from a workspace
 * Markdown file. All of it is written through `createTextNode` / `textContent`.
 * There is no `innerHTML` in this module, and there must never be.
 *
 * Reader (T13) is not implemented here. Loam Markdown references are rendered as
 * buttons that dispatch `loam:open-reader` with `{path, kind, title, line}`;
 * underlying source-code paths stay plain text, since Reader renders only Loam
 * Markdown artifacts.
 */

import { summarize } from './summary.mjs';

/** `2026-07-15T10:00:00+02:00` -> `2026-07-15 10:00`. Workspace-local, no locale guessing. */
function shortTimestamp(value) {
  const match = /^(\d{4}-\d{2}-\d{2})T(\d{2}:\d{2})/.exec(String(value ?? ''));
  return match ? `${match[1]} ${match[2]}` : null;
}

function isEdge(target) {
  return Boolean(target && target.from && target.to && target.origin);
}

/** `path`, `path:12`, or null when there is nothing to point at. */
function locate(path, line) {
  if (!path) return null;
  return line ? `${path}:${line}` : path;
}

let current = null;

export function initInspector({ root = document, getSnapshot = () => null } = {}) {
  const panel = root.querySelector('[data-inspector]');
  const scrim = root.querySelector('[data-inspector-scrim]');
  const body = root.querySelector('[data-inspector-body]');
  const shell = root.querySelector('[data-app-shell]');
  if (!panel || !scrim || !body) return null;

  const doc = panel.ownerDocument;
  const win = doc.defaultView ?? globalThis;
  let invoker = null;
  let generation = 0;

  const el = (tag, className, text) => {
    const node = doc.createElement(tag);
    if (className) node.className = className;
    if (text != null) node.appendChild(doc.createTextNode(String(text)));
    return node;
  };

  const artifactsByPath = () => {
    const index = new Map();
    for (const artifact of getSnapshot()?.artifacts ?? []) index.set(artifact.path, artifact);
    return index;
  };

  /**
   * A Loam Markdown reference the snapshot inventories becomes a Reader entry
   * point. Everything else — an uninventoried path, a source file — is plain
   * text, because Reader can only open what View indexed.
   */
  function reference(path, { line = null, label = null, index } = {}) {
    const artifact = path ? index.get(path) : null;
    const text = label ?? locate(path, line) ?? '';
    if (!artifact) return el('span', 'inspector-path', text);

    const button = el('button', 'file-link', text);
    button.type = 'button';
    button.dataset.path = path;
    button.addEventListener('click', () => {
      doc.dispatchEvent(new win.CustomEvent('loam:open-reader', {
        detail: { path, kind: artifact.kind, title: artifact.title, line },
      }));
    });
    return button;
  }

  function addSection(key, heading, build) {
    const wrapper = el('section', 'inspector-section');
    wrapper.dataset.section = key;
    wrapper.appendChild(el('h3', null, heading));
    build(wrapper);
    body.appendChild(wrapper);
  }

  function list(className, rows) {
    const ul = el('ul', className);
    for (const row of rows) {
      const li = doc.createElement('li');
      for (const part of row) li.append(part);
      ul.appendChild(li);
    }
    return ul;
  }

  function renderHead(kind, title) {
    body.replaceChildren();
    const kindLabel = el('span', 'inspector-kind', kind ?? 'artifact');
    kindLabel.dataset.kind = kind ?? '';
    body.appendChild(kindLabel);
    body.appendChild(el('h2', 'inspector-title', title ?? '(untitled)'));
  }

  function renderSummary(text, present) {
    const paragraph = el('p', 'inspector-summary', text);
    paragraph.dataset.present = String(present);
    body.appendChild(paragraph);
    return paragraph;
  }

  function renderNode(artifact) {
    const index = artifactsByPath();
    const snapshot = getSnapshot();
    const relationships = snapshot?.relationships ?? [];
    const attributes = artifact.attributes ?? {};

    renderHead(artifact.kind, artifact.title || artifact.path);
    const summary = renderSummary('Reading document…', true);

    addSection('source', 'Source path', (wrapper) => {
      wrapper.appendChild(reference(artifact.path, { index }));
      // The underlying source file is evidence, not a Reader document.
      if (attributes.source_path) {
        wrapper.appendChild(el('p', 'inspector-path', attributes.source_path));
      }
    });

    addSection('freshness', 'Freshness', (wrapper) => {
      const parts = [];
      const indexed = shortTimestamp(artifact.updated_at) ?? shortTimestamp(artifact.created_at);
      parts.push(indexed ? `Indexed ${indexed}` : 'No indexed time recorded');
      if (attributes.source_exists === false) parts.push('source file missing');
      else if (attributes.source_path) parts.push('source-backed');
      if (artifact.lifecycle_status) parts.push(artifact.lifecycle_status);
      wrapper.appendChild(el('p', 'inspector-freshness', parts.join(' · ')));
    });

    // Everything the graph says touches this node, in both directions.
    const touching = relationships.filter((edge) => edge.from === artifact.path || edge.to === artifact.path);

    addSection('context', 'Linked project context', (wrapper) => {
      const rows = touching.map((edge) => {
        const otherPath = edge.from === artifact.path ? edge.to : edge.from;
        const other = index.get(otherPath);
        const label = other?.title || otherPath;
        return [el('strong', null, other?.kind ?? 'unknown'), doc.createTextNode(' '), reference(otherPath, { label, index })];
      });
      if (!rows.length) {
        wrapper.appendChild(el('p', 'inspector-path', 'Nothing links to this artifact in the current snapshot.'));
        return;
      }
      wrapper.appendChild(list('relationship-list', rows));
    });

    addSection('evidence', 'Evidence and relationships', (wrapper) => {
      const rows = touching.map((edge) => [
        el('span', 'evidence-mode', edge.origin),
        doc.createTextNode(' '),
        el('span', 'evidence-kind', edge.kind),
        doc.createTextNode(' '),
        reference(edge.evidence?.path, { line: edge.evidence?.line, index }),
      ]);
      if (!rows.length) {
        wrapper.appendChild(el('p', 'inspector-path', 'No relationship evidence recorded for this artifact.'));
        return;
      }
      wrapper.appendChild(list('evidence-list', rows));
    });

    return summary;
  }

  function renderEdge(edge) {
    const index = artifactsByPath();
    const snapshot = getSnapshot();

    renderHead('relationship', edge.kind);
    renderSummary(`${edge.origin === 'explicit' ? 'Explicit' : 'Derived'} relationship recorded in the snapshot.`, true);

    addSection('endpoints', 'Endpoints', (wrapper) => {
      wrapper.appendChild(list('relationship-list', [
        [el('strong', null, 'From'), doc.createTextNode(' '), reference(edge.from, { index })],
        [el('strong', null, 'To'), doc.createTextNode(' '), reference(edge.to, { index })],
      ]));
    });

    addSection('origin', 'Origin', (wrapper) => {
      wrapper.appendChild(el('p', 'inspector-freshness', edge.origin === 'explicit'
        ? 'explicit — written in the document itself'
        : 'derived — inferred by a Loam State rule'));
    });

    addSection('evidence', 'Evidence location', (wrapper) => {
      const evidence = edge.evidence ?? {};
      wrapper.appendChild(reference(evidence.path, { line: evidence.line, index }));
      const detail = [evidence.section, evidence.field].filter(Boolean).join(' · ');
      if (detail) wrapper.appendChild(el('p', 'inspector-path', detail));
      if (!evidence.path && !detail) wrapper.appendChild(el('p', 'inspector-path', 'No evidence location recorded.'));
    });

    addSection('rule', 'Rule', (wrapper) => {
      const rule = edge.rule;
      if (!rule) {
        wrapper.appendChild(el('p', 'inspector-freshness', 'No rule recorded — this edge is read straight from the document.'));
      } else {
        wrapper.appendChild(list('relationship-list', [
          [el('strong', null, 'Identity'), doc.createTextNode(` ${rule.id} v${rule.version}`)],
          [el('strong', null, 'Confidence'), doc.createTextNode(` ${Math.round(rule.confidence * 100)}%`)],
          [el('strong', null, 'Generated'), doc.createTextNode(` ${shortTimestamp(rule.generated_at) ?? 'unknown'}`)],
        ]));
      }
      wrapper.appendChild(el('p', 'inspector-freshness', `Schema version ${snapshot?.schema_version ?? 'unknown'}`));
    });
  }

  /** Pull the document and replace the placeholder with a real (or honestly missing) summary. */
  async function fillSummary(artifact, paragraph, ticket) {
    let content = null;
    let failure = null;
    try {
      const response = await fetch(`/api/document?path=${encodeURIComponent(artifact.path)}`);
      const payload = await response.json().catch(() => null);
      if (!response.ok) failure = payload?.error ?? `HTTP ${response.status}`;
      else content = payload?.content ?? null;
    } catch (error) {
      failure = error.message;
    }
    if (ticket !== generation) return;

    if (failure) {
      paragraph.textContent = `Summary unavailable: ${failure}`;
      paragraph.dataset.present = 'false';
      return;
    }
    const { text, present } = summarize({ kind: artifact.kind, content });
    paragraph.textContent = text;
    paragraph.dataset.present = String(present);
  }

  function open(target) {
    const ticket = ++generation;
    if (!invoker) invoker = doc.activeElement;

    if (isEdge(target)) renderEdge(target);
    else {
      const artifact = typeof target === 'string'
        ? artifactsByPath().get(target)
        : target;
      if (!artifact) return;
      const paragraph = renderNode(artifact);
      fillSummary(artifact, paragraph, ticket);
    }

    panel.classList.add('is-open');
    panel.setAttribute('aria-hidden', 'false');
    scrim.classList.add('is-open');
    // Modality without layout change: the view beneath keeps its exact box.
    shell?.setAttribute('inert', '');
    panel.focus();
  }

  function close() {
    if (!panel.classList.contains('is-open')) return;
    generation += 1;
    panel.classList.remove('is-open');
    panel.setAttribute('aria-hidden', 'true');
    scrim.classList.remove('is-open');
    shell?.removeAttribute('inert');
    // Focus is restored only after the shell is interactive again.
    invoker?.focus?.();
    invoker = null;
  }

  scrim.addEventListener('click', close);
  for (const button of root.querySelectorAll('[data-inspector-close]')) {
    button.addEventListener('click', close);
  }
  doc.addEventListener('keydown', (event) => {
    if (event.key === 'Escape' && panel.classList.contains('is-open')) {
      event.preventDefault();
      close();
    }
  });

  current = { openInspector: open, close, isOpen: () => panel.classList.contains('is-open') };
  return current;
}

/**
 * Open the Inspector on a node (artifact or its path) or an edge (relationship).
 * The area views (Atlas, Work Stream, Chronicle, Stewardship) call this; they do
 * not need a handle on the controller.
 */
export function openInspector(nodeOrEdge) {
  current?.openInspector(nodeOrEdge);
}

export function closeInspector() {
  current?.close();
}
