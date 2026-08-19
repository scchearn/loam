/**
 * Stewardship — the trust room.
 *
 * The room reads as conservation status for a knowledge archive, not as a wall
 * of red admin errors. Six fixed trust domains always appear (freshness, code
 * graph, memory links, open questions, archive & corrections, retrieval), so the
 * human can see what was checked as well as what was found. Anything else the
 * snapshot emitted follows in the same grid, so no finding is quietly dropped.
 *
 * Two rules shape everything below:
 *
 * 1. The frontend judges nothing. A card's state is the emitted signal state,
 *    the emitted hint severity, or the emitted capability state — never a verdict
 *    derived from a metric here. A domain with no emitted finding says `unknown`;
 *    an absent optional substrate says its own word and stays neutral.
 * 2. Each finding appears once. A domain adopts the first emitted item that
 *    belongs to it (signals before hints); further matches render as their own
 *    cards rather than being merged away.
 *
 * Cards are the Pulse advisor card system (DESIGN.md §4 advisor/issue card) and
 * every actionable one carries the shared copy-prompt control.
 */

import { createCopyPrompt } from '../copy-prompt.mjs';
import { inspectButton, maker } from './dom.mjs';

const SVG_NS = 'http://www.w3.org/2000/svg';

/** `code-graph-drift` -> `Code graph drift`. */
function humanize(value) {
  const words = String(value ?? '').replace(/[-_.]+/g, ' ').trim();
  return words ? words[0].toUpperCase() + words.slice(1) : '';
}

function icon(doc, id, className = 'icon') {
  const svg = doc.createElementNS(SVG_NS, 'svg');
  svg.setAttribute('class', className);
  const use = doc.createElementNS(SVG_NS, 'use');
  use.setAttribute('href', `#${id}`);
  svg.append(use);
  return svg;
}

/**
 * The trust domains, in reading order. `match` claims an emitted signal id or
 * hint kind/group; `capability` supplies the fallback state word when nothing
 * was emitted; `metric` is the optional count the card may display.
 *
 * ponytail: only count-unit metrics are shown, so the big display number never
 * needs a unit word beside it. Add a unit line here if a non-count metric earns
 * a place on a card.
 */
const DOMAINS = [
  {
    id: 'freshness',
    label: 'Freshness',
    glyph: 'i-database',
    match: /lint|fresh/,
    capability: 'wiki',
    metric: null,
    quiet: 'No memory-freshness signal in this snapshot.',
  },
  {
    id: 'code-graph',
    label: 'Code graph',
    glyph: 'i-code',
    match: /code/,
    capability: 'code_graph',
    metric: 'code.stale',
    quiet: 'No code-graph drift signal in this snapshot.',
  },
  {
    id: 'wikilinks',
    label: 'Memory links',
    glyph: 'i-graph',
    match: /wikilink|link/,
    capability: 'wiki',
    metric: 'wiki.broken_wikilinks',
    quiet: 'No wikilink-health signal in this snapshot.',
  },
  {
    id: 'questions',
    label: 'Open questions',
    glyph: 'i-book',
    match: /question|contradict|unresolved/,
    capability: null,
    metric: null,
    quiet: 'No unresolved-question signal in this snapshot.',
  },
  {
    id: 'archive',
    label: 'Archive & corrections',
    glyph: 'i-flag',
    match: /archive|correction|amend/,
    capability: 'wiki',
    metric: 'wiki.archived_pages',
    quiet: 'No archive or correction signal in this snapshot.',
  },
  {
    id: 'retrieval',
    label: 'Retrieval',
    glyph: 'i-search',
    match: /retriev|qmd|index/,
    capability: 'qmd',
    metric: null,
    quiet: 'No retrieval signal in this snapshot.',
  },
];

/** Category glyphs for findings that belong to no fixed domain. */
const EXTRA_GLYPH = [
  [/goal/, 'i-target'],
  [/concept/, 'i-shapes'],
  [/checkpoint|resum/, 'i-flag'],
  [/spec|plan|work/, 'i-branch'],
  [/wiki|memory|log/, 'i-database'],
];

const BADGE_CLASS = {
  critical: 'badge-critical',
  watch: 'badge-watch',
  healthy: 'badge-healthy',
};

/** Hint routing severity is not view health state; this is the one mapping. */
const HINT_SEVERITY = { action: 'critical', warn: 'watch', info: 'neutral' };

/** Only these two states earn a coloured card body; everything else stays quiet. */
const CARD_TONE = { critical: 'critical', watch: 'warn' };

/** A capability that simply is not set up reads as neutral, never as a fault. */
const NEUTRAL_CAPABILITY = /^(absent|unconfigured|not-configured|unavailable|disabled)$/;

/** Evidence is an object, an array of them, or null; only `path` is rendered. */
function evidencePaths(evidence) {
  const items = Array.isArray(evidence) ? evidence : [evidence];
  return items.map((item) => item?.path).filter((path) => typeof path === 'string' && path);
}

/** The findings this snapshot emitted, normalised to one card shape. */
function findings(snapshot) {
  const signals = (snapshot.signals ?? []).map((signal) => ({
    key: signal.id,
    label: humanize(signal.id),
    severity: signal.state ?? 'unknown',
    word: signal.state ?? 'unknown',
    message: signal.message,
    command: signal.command ?? null,
    evidence: signal.evidence,
  }));
  const hints = (snapshot.hints ?? []).map((hint) => ({
    key: hint.kind,
    label: humanize(hint.group ?? hint.kind),
    severity: HINT_SEVERITY[hint.severity] ?? 'neutral',
    // The badge says the emitted routing word, not the mapped view state, so the
    // card never claims a severity the producer did not write.
    word: hint.severity ?? 'unknown',
    message: hint.message,
    command: hint.command ?? null,
    evidence: hint.evidence,
  }));
  return [...signals, ...hints];
}

/** A ready count metric, or null — a metric in any other state shows nothing. */
function countMetric(snapshot, key) {
  const metric = key ? snapshot?.metrics?.[key] : null;
  if (!metric || metric.state !== 'ready' || typeof metric.value !== 'number') return null;
  return String(metric.value);
}

function card(doc, { id, domain, label, glyph, severity, word, message, command, metric, evidence }, inventory) {
  const el = maker(doc);
  const article = el('article', 'issue-card');
  const tone = CARD_TONE[severity];
  if (tone) article.classList.add(tone);
  article.dataset.stewardshipCard = id;
  if (domain) article.dataset.stewardshipDomain = domain;
  article.dataset.severity = severity;
  article.dataset.command = command ?? '';
  article.setAttribute('role', 'listitem');

  const head = el('div', 'issue-head');
  const category = el('span', 'issue-cat');
  category.append(icon(doc, glyph, 'icon issue-glyph'), doc.createTextNode(label));

  const actions = el('span', 'issue-head-actions');
  actions.append(el('span', `badge ${BADGE_CLASS[severity] ?? 'badge-neutral'}`, humanize(word)));
  const copy = createCopyPrompt(doc, { command, message });
  if (copy) actions.append(copy);

  head.append(category, actions);
  article.append(head);

  if (metric) article.append(el('span', 'issue-metric', metric));
  article.append(el('h3', 'issue-title', message));

  const paths = evidencePaths(evidence);
  if (paths.length) {
    const desc = el('p', 'issue-desc');
    for (const [index, path] of paths.entries()) {
      if (index) desc.append(doc.createTextNode(' · '));
      const artifact = inventory.get(path);
      // An inventoried artifact is an Inspector entry point; a source file or an
      // uninventoried path stays a plain chip rather than a door onto nothing.
      desc.append(artifact ? inspectButton(doc, artifact, { label: path, className: 'file-link' })
        : el('code', 'code-chip', path));
    }
    article.append(desc);
  }

  return article;
}

/** One card per fixed domain, plus every finding no domain claimed. */
export function stewardshipCards(snapshot) {
  const unclaimed = findings(snapshot);
  const rows = [];

  for (const domain of DOMAINS) {
    const index = unclaimed.findIndex((item) => domain.match.test(String(item.key)));
    const found = index >= 0 ? unclaimed.splice(index, 1)[0] : null;
    const capability = domain.capability ? snapshot?.capabilities?.[domain.capability]?.state : null;
    const quietState = capability && NEUTRAL_CAPABILITY.test(capability) ? 'neutral' : 'unknown';

    rows.push({
      id: found?.key ?? domain.id,
      domain: domain.id,
      label: domain.label,
      glyph: domain.glyph,
      severity: found?.severity ?? quietState,
      word: found?.word ?? (quietState === 'neutral' ? capability : 'unknown'),
      message: found?.message ?? domain.quiet,
      command: found?.command ?? null,
      metric: countMetric(snapshot, domain.metric),
      evidence: found?.evidence ?? null,
    });
  }

  for (const item of unclaimed) {
    rows.push({
      id: item.key,
      domain: null,
      label: item.label,
      glyph: EXTRA_GLYPH.find(([pattern]) => pattern.test(String(item.key)))?.[1] ?? 'i-shield',
      severity: item.severity,
      word: item.word,
      message: item.message,
      command: item.command,
      metric: null,
      evidence: item.evidence,
    });
  }

  return rows;
}

const POSTURE_BADGE = {
  healthy: 'badge-healthy',
  'needs-review': 'badge-watch',
  'at-risk': 'badge-critical',
};

/** Conservation status: the emitted posture, then a tally of what is on screen. */
function renderSummary(doc, snapshot, rows) {
  const el = maker(doc);
  const posture = snapshot.posture ?? 'unknown';

  const section = el('section', 'band');
  section.dataset.stewardshipSummary = '';
  section.setAttribute('aria-label', 'Conservation status');

  const head = el('header', 'band-head');
  const title = el('h2', 'band-title');
  const drag = el('span', 'drag', '⠿');
  drag.setAttribute('aria-hidden', 'true');
  title.append(drag, doc.createTextNode('Conservation status'));

  const badge = el('span', `badge ${POSTURE_BADGE[posture] ?? 'badge-neutral'}`, humanize(posture));
  badge.dataset.stewardshipPosture = '';
  title.append(badge);

  const tally = ['critical', 'watch', 'healthy', 'neutral', 'unknown']
    .map((state) => [state, rows.filter((row) => row.severity === state).length])
    .filter(([, count]) => count > 0)
    .map(([state, count]) => `${count} ${state}`)
    .join(' · ');

  head.append(title, el('span', 'pill', tally));
  section.append(head);
  return section;
}

export function renderStewardship(doc, snapshot, target) {
  if (!target) return;
  if (!snapshot) {
    target.replaceChildren();
    return;
  }

  const inventory = new Map((snapshot.artifacts ?? []).map((artifact) => [artifact.path, artifact]));
  const rows = stewardshipCards(snapshot);

  const grid = maker(doc)('div', 'issue-grid');
  grid.dataset.stewardshipGrid = '';
  grid.setAttribute('role', 'list');
  for (const row of rows) grid.append(card(doc, row, inventory));

  const band = maker(doc)('section', 'band');
  band.setAttribute('aria-label', 'Trust signals');
  band.append(grid);

  target.replaceChildren(renderSummary(doc, snapshot, rows), band);
}

/** Subscribes to the shell's render event; the shell knows nothing about this view. */
export function initStewardship({ root = document } = {}) {
  const doc = root.ownerDocument ?? root;
  const target = root.querySelector('[data-mount="stewardship"]');
  doc.addEventListener('loam:render', (event) => {
    renderStewardship(doc, event.detail?.snapshot, target);
  });
}
