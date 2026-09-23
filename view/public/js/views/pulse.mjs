/**
 * Pulse — the landing view: "here is what matters now."
 *
 * Composition follows specs/loam-view.md: one dotted overview box (project
 * header + stat tiles), an evidence-metrics band, an advisor band of
 * severity-badged signal cards, and the current return point.
 *
 * Two rules shape everything below:
 *
 * 1. The frontend computes nothing. Every number, state, and severity is read
 *    from the snapshot as emitted; a missing, `unknown`, or `unavailable` value
 *    renders as a dash or its state word, never as zero.
 * 2. Each fact appears once. Substrate counts live only in the metrics band and
 *    snapshot freshness only in the topbar, so the overview box repeats neither.
 */

import { createCopyPrompt } from '../copy-prompt.mjs';
import { openInspector } from '../inspector.mjs';

const SVG_NS = 'http://www.w3.org/2000/svg';

function el(doc, tag, className, text) {
  const node = doc.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

function icon(doc, id) {
  const svg = doc.createElementNS(SVG_NS, 'svg');
  svg.setAttribute('class', 'icon');
  const use = doc.createElementNS(SVG_NS, 'use');
  use.setAttribute('href', `#${id}`);
  svg.append(use);
  return svg;
}

/** `at-risk` -> `At risk`. Used for postures, capability states, and signal ids. */
function humanize(value) {
  const words = String(value ?? '').replace(/[-_.]+/g, ' ').trim();
  return words ? words[0].toUpperCase() + words.slice(1) : '';
}

/** `2026-08-01T12:00:00+02:00` -> `2026-08-01 12:00`. Workspace-local, no locale guessing. */
function shortTimestamp(value) {
  const match = /^(\d{4}-\d{2}-\d{2})T(\d{2}:\d{2})/.exec(String(value ?? ''));
  return match ? `${match[1]} ${match[2]}` : null;
}

/**
 * The single rendering rule for a metric. An absent metric and a null value are
 * a dash; a check that did not complete says so in words. Nothing here invents
 * a number.
 */
function metricText(snapshot, key, { numericOnly = false } = {}) {
  const metric = snapshot?.metrics?.[key];
  if (!metric) return '—';
  // The hero value is display type: a state word does not belong at 1.9rem, so
  // it renders as a dash there and says `Unknown`/`Unavailable` in small type.
  if (metric.state !== 'ready') return numericOnly ? '—' : humanize(metric.state);
  if (metric.value === null || metric.value === undefined) return '—';
  if (metric.unit === 'percent') return `${metric.value}%`;
  if (typeof metric.value === 'string') return shortTimestamp(metric.value) ?? metric.value;
  return String(metric.value);
}

function metricNumber(snapshot, key) {
  const metric = snapshot?.metrics?.[key];
  return metric?.state === 'ready' && typeof metric.value === 'number' ? metric.value : null;
}

/* ---------- Overview box ---------- */

const POSTURE_TONE = {
  healthy: 'ok',
  'needs-review': 'watch',
  'not-configured': 'watch',
  'at-risk': 'drift',
};

/** Readiness of retrieval: the emitted signal if there is one, else the capability. */
function retrievalState(snapshot) {
  const signal = (snapshot.signals ?? []).find((item) => item.id === 'retrieval');
  const state = signal?.state ?? snapshot.capabilities?.qmd?.state ?? 'unknown';
  return { state, tone: state === 'healthy' || state === 'ready' ? 'ok' : '' };
}

function tile(doc, { name, label, value, glyph, tone }) {
  const node = el(doc, 'div', 'tile');
  node.dataset.pulseTile = name;

  const iconBox = el(doc, 'span', tone ? `tile-icon ${tone}` : 'tile-icon');
  iconBox.setAttribute('aria-hidden', 'true');
  iconBox.append(icon(doc, glyph));

  const body = el(doc, 'div', 'tile-body');
  body.append(el(doc, 'span', 'tile-label', label), el(doc, 'span', 'tile-value', value));

  node.append(iconBox, body);
  return node;
}

function renderOverview(doc, snapshot) {
  const workspace = snapshot.workspace ?? {};
  const posture = snapshot.posture ?? 'unknown';
  const retrieval = retrievalState(snapshot);

  const box = el(doc, 'section', 'panel panel-dotted');
  box.dataset.pulseOverview = '';
  box.setAttribute('aria-label', 'Project overview');

  const title = el(doc, 'h2', 'overview-title', workspace.name || 'Workspace');
  title.dataset.projectName = '';

  const sub = el(doc, 'div', 'overview-sub');
  const context = [workspace.root, workspace.git?.branch].filter(Boolean).join(' · ');
  sub.append(el(doc, 'span', 'overview-url', context));

  // The palette is already wired to the topbar trigger; Pulse only points at it.
  const search = el(doc, 'button', 'ghost-btn', 'Search ');
  search.type = 'button';
  search.dataset.pulseSearch = '';
  // The glyph is the DESIGN.md label; the accessible name states the real
  // bindings instead of a Mac-only symbol every platform would hear read out.
  search.setAttribute('aria-label', 'Search');
  search.setAttribute('aria-keyshortcuts', 'Control+K Meta+K');
  search.append(el(doc, 'kbd', null, '⌘K'));
  search.addEventListener('click', () => doc.querySelector('[data-query-open]')?.click());
  sub.append(search);

  const grid = el(doc, 'div', 'tile-grid');
  grid.append(
    tile(doc, {
      name: 'posture',
      label: 'Status',
      value: humanize(posture),
      glyph: 'i-pulse',
      tone: POSTURE_TONE[posture] ?? '',
    }),
    tile(doc, {
      name: 'coverage',
      label: 'Code coverage',
      value: metricText(snapshot, 'code.coverage_percent'),
      glyph: 'i-code',
    }),
    tile(doc, {
      name: 'goals',
      label: 'First-class goals',
      value: metricText(snapshot, 'work.goals'),
      glyph: 'i-target',
    }),
    tile(doc, {
      name: 'concepts',
      label: 'Concept pages',
      value: metricText(snapshot, 'wiki.concepts'),
      glyph: 'i-shapes',
    }),
    tile(doc, {
      name: 'checkpoint',
      label: 'Last checkpoint',
      value: metricText(snapshot, 'checkpoints.latest_at'),
      glyph: 'i-flag',
    }),
    tile(doc, {
      name: 'retrieval',
      label: 'Retrieval',
      // The emitted retrieval signal first; the qmd capability only as a
      // fallback, so this tile reads as readiness rather than a second copy of
      // the topbar's qmd state.
      value: humanize(retrieval.state),
      glyph: 'i-search',
      tone: retrieval.tone,
    }),
  );

  box.append(title, sub, grid);
  return box;
}

/* ---------- Evidence metrics band ---------- */

const METRIC_CARDS = [
  {
    name: 'memory',
    label: 'Memory',
    value: 'wiki.knowledge_pages',
    subs: [['Broken', 'wiki.broken_wikilinks'], ['Archived', 'wiki.archived_pages']],
  },
  {
    name: 'code',
    label: 'Code graph',
    value: 'code.source_backed_pages',
    subs: [['Coverage', 'code.coverage_percent'], ['Stale', 'code.stale']],
  },
  {
    name: 'work',
    label: 'Work',
    value: 'work.plans',
    subs: [['Specs', 'work.specs'], ['Active', 'work.active_plans']],
  },
  {
    name: 'goals',
    label: 'Goals',
    value: 'work.goals',
    subs: [['Active', 'work.active_goals']],
  },
  {
    name: 'checkpoints',
    label: 'Checkpoints',
    value: 'checkpoints.total',
    subs: [['Actionable', 'checkpoints.actionable']],
  },
];

function band(doc, { label, title, action }) {
  const section = el(doc, 'section', 'band');
  section.setAttribute('aria-label', label);

  const head = el(doc, 'header', 'band-head');
  const heading = el(doc, 'h2', 'band-title');
  const drag = el(doc, 'span', 'drag', '⠿');
  drag.setAttribute('aria-hidden', 'true');
  heading.append(drag, doc.createTextNode(title));
  head.append(heading);
  if (action) head.append(action);

  section.append(head);
  return section;
}

function renderMetrics(doc, snapshot) {
  const section = band(doc, { label: 'Evidence metrics', title: 'Evidence' });
  section.dataset.pulseMetrics = '';

  const row = el(doc, 'div', 'card-row');
  row.setAttribute('role', 'list');
  // This row scrolls horizontally and holds no control of its own, so without a
  // tab stop a keyboard-only human cannot reach the cards past the fold. Newer
  // Chrome focuses such scrollers by itself; this makes it true everywhere.
  row.tabIndex = 0;

  for (const card of METRIC_CARDS) {
    const article = el(doc, 'article', 'metric-card');
    article.dataset.pulseMetric = card.name;
    article.setAttribute('role', 'listitem');
    article.append(
      el(doc, 'span', 'metric-label', card.label),
      el(doc, 'span', 'metric-value', metricText(snapshot, card.value, { numericOnly: true })),
    );

    const subs = el(doc, 'div', 'metric-subs');
    for (const [label, key] of card.subs) {
      const sub = el(doc, 'span');
      const dot = el(doc, 'i', 'dot');
      dot.setAttribute('aria-hidden', 'true');
      sub.append(dot, doc.createTextNode(`${label} ${metricText(snapshot, key)}`));
      subs.append(sub);
    }
    article.append(subs);
    row.append(article);
  }

  section.append(row);
  return section;
}

/* ---------- Advisor band ---------- */

const SIGNAL_BADGE = {
  critical: 'badge-critical',
  watch: 'badge-watch',
  healthy: 'badge-healthy',
};

const HINT_SEVERITY = { action: 'critical', warn: 'watch', info: 'neutral' };

const CATEGORY_GLYPH = [
  [/goal/, 'i-target'],
  [/concept/, 'i-shapes'],
  [/code/, 'i-code'],
  [/retriev|qmd|search|index/, 'i-search'],
  [/checkpoint|resum/, 'i-flag'],
  [/wikilink|graph|link/, 'i-graph'],
  [/wiki|memory|lint|log/, 'i-database'],
  [/spec|plan|work/, 'i-branch'],
];

function glyphFor(key) {
  return CATEGORY_GLYPH.find(([pattern]) => pattern.test(key))?.[1] ?? 'i-shield';
}

function advisorCard(doc, { id, category, severity, badgeClass, badgeLabel, message, command }) {
  const article = el(doc, 'article', 'issue-card');
  if (severity === 'critical') article.classList.add('critical');
  if (severity === 'watch') article.classList.add('warn');
  article.dataset.pulseCard = id;
  article.dataset.severity = severity;
  article.dataset.command = command ?? '';
  article.setAttribute('role', 'listitem');

  const head = el(doc, 'div', 'issue-head');
  const cat = el(doc, 'span', 'issue-cat');
  const glyph = icon(doc, glyphFor(id));
  glyph.setAttribute('class', 'icon issue-glyph');
  cat.append(glyph, doc.createTextNode(category));

  const actions = el(doc, 'span', 'issue-head-actions');
  actions.append(el(doc, 'span', `badge ${badgeClass}`, badgeLabel));
  const copy = createCopyPrompt(doc, { command, message });
  if (copy) actions.append(copy);

  head.append(cat, actions);
  // Pulse is the glance surface: the signal's headline carries the count
  // ("82 artifact(s)..."); the per-file evidence list lives in Stewardship, so
  // it is deliberately omitted here to keep the card compact.
  article.append(head, el(doc, 'h3', 'issue-title', message));

  return article;
}

function renderAdvisor(doc, snapshot) {
  const signals = snapshot.signals ?? [];
  const hints = snapshot.hints ?? [];
  const total = signals.length + hints.length;

  const action = el(doc, 'button', 'ghost-btn accent', ' Open Stewardship');
  action.type = 'button';
  action.dataset.openStewardship = '';
  action.prepend(icon(doc, 'i-shield'));
  // Routing lives in the shell; re-using its rail button keeps one implementation.
  action.addEventListener('click', () => doc.querySelector('[data-route="stewardship"]')?.click());

  const section = band(doc, {
    label: 'Stewardship signals',
    title: total ? `Stewardship found ${total} ${total === 1 ? 'signal' : 'signals'}` : 'Stewardship signals',
    action,
  });
  section.dataset.pulseAdvisor = '';

  const row = el(doc, 'div', 'card-row');
  row.setAttribute('role', 'list');

  for (const signal of signals) {
    row.append(advisorCard(doc, {
      id: signal.id,
      category: humanize(signal.id),
      severity: signal.state,
      badgeClass: SIGNAL_BADGE[signal.state] ?? 'badge-neutral',
      badgeLabel: humanize(signal.state),
      message: signal.message,
      command: signal.command,
    }));
  }

  for (const hint of hints) {
    row.append(advisorCard(doc, {
      id: hint.kind,
      category: humanize(hint.group),
      severity: HINT_SEVERITY[hint.severity] ?? 'neutral',
      badgeClass: SIGNAL_BADGE[HINT_SEVERITY[hint.severity]] ?? 'badge-neutral',
      badgeLabel: humanize(hint.severity),
      message: hint.message,
      command: hint.command,
    }));
  }

  if (!total) {
    section.append(el(doc, 'p', 'empty-note', 'No stewardship signals in this snapshot.'));
    return section;
  }

  section.append(row);
  return section;
}

/* ---------- Current return point ---------- */

/** The newest checkpoint artifact, unless the snapshot says none is actionable. */
function returnPoint(snapshot) {
  if (metricNumber(snapshot, 'checkpoints.actionable') === 0) return null;
  return (snapshot.artifacts ?? [])
    .filter((artifact) => artifact.kind === 'checkpoint')
    .sort((a, b) => String(b.captured_at ?? b.updated_at ?? '').localeCompare(String(a.captured_at ?? a.updated_at ?? '')))
    .at(0) ?? null;
}

/** The checkpoint card borrows the resume signal's own command — it invents none. */
function resumeAction(snapshot) {
  const candidates = [...(snapshot.signals ?? []), ...(snapshot.hints ?? [])];
  return candidates.find(
    (item) => item.command && /checkpoint|resum/.test(String(item.id ?? item.kind)),
  ) ?? null;
}

function renderFocus(doc, snapshot) {
  const section = band(doc, { label: 'Current return point', title: 'Current return point' });
  section.dataset.pulseFocus = '';

  const checkpoint = returnPoint(snapshot);
  if (!checkpoint) {
    section.append(el(
      doc,
      'p',
      'empty-note',
      'No actionable checkpoint. Capture one with loam checkpointing when you pause work.',
    ));
    return section;
  }

  const card = el(doc, 'article', 'focus-card');

  const meta = el(doc, 'div', 'focus-meta');
  meta.append(el(doc, 'span', 'badge badge-neutral', 'Checkpoint'));
  const captured = shortTimestamp(checkpoint.captured_at ?? checkpoint.updated_at);
  if (captured) meta.append(el(doc, 'span', null, `Captured ${captured}`));
  if (checkpoint.attributes?.reason) {
    meta.append(el(doc, 'span', null, `Reason: ${checkpoint.attributes.reason}`));
  }

  card.append(meta, el(doc, 'h3', 'focus-title', checkpoint.title || checkpoint.path));

  const actions = el(doc, 'div', 'focus-actions');
  const inspect = el(doc, 'button', 'ghost-btn', 'Inspect checkpoint');
  inspect.type = 'button';
  inspect.dataset.path = checkpoint.path;
  inspect.addEventListener('click', () => openInspector(checkpoint));
  actions.append(inspect);

  const action = resumeAction(snapshot);
  const copy = action && createCopyPrompt(doc, { command: action.command, message: action.message });
  if (copy) actions.append(copy);

  card.append(actions);
  section.append(card);
  return section;
}

/* ---------- Mount ---------- */

export function renderPulse(doc, snapshot, target) {
  if (!target) return;
  if (!snapshot) {
    target.replaceChildren();
    return;
  }

  target.replaceChildren(
    renderOverview(doc, snapshot),
    renderMetrics(doc, snapshot),
    renderAdvisor(doc, snapshot),
    renderFocus(doc, snapshot),
  );
}

/** Subscribes to the shell's render event; the shell knows nothing about Pulse. */
export function initPulse({ root = document } = {}) {
  const doc = root.ownerDocument ?? root;
  const target = root.querySelector('[data-mount="pulse"]');
  doc.addEventListener('loam:render', (event) => {
    renderPulse(doc, event.detail?.snapshot, target);
  });
}
