/**
 * Chronicle: a vertical, grouped timeline of how project understanding changed
 * (spec: "grouped, evidenced, and readable like an operations log ...
 * provenance-first rather than literary").
 *
 * The only input is the snapshot's `events` array. The producer emits an event
 * only from a parseable log heading, an artifact lifecycle field, a checkpoint,
 * a goal review, or a git commit — filesystem mtime never creates one — and this
 * view holds the same line: it renders what it is given, drops anything it
 * cannot place in time, and shows an honest empty state rather than
 * reconstructing a story out of artifact timestamps.
 *
 * Within a day, `strong` lifecycle evidence leads and `source` (git) evidence
 * follows, labelled as such, so the weaker record never reads as the authority.
 */

import { maker, readerLink } from './dom.mjs';

const MONTHS = ['JAN', 'FEB', 'MAR', 'APR', 'MAY', 'JUN', 'JUL', 'AUG', 'SEP', 'OCT', 'NOV', 'DEC'];
const STRENGTH_RANK = { strong: 0, source: 1 };
const TIMESTAMP = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}:\d{2})/;

/** `2026-07-05T14:00:00+02:00` -> `{day, date, time}`, or null when unusable. */
function moment(value) {
  const match = TIMESTAMP.exec(String(value ?? ''));
  if (!match) return null;
  const [, year, month, day, time] = match;
  const index = Number(month) - 1;
  if (!MONTHS[index]) return null;
  return { day: `${year}-${month}-${day}`, label: `${day} ${MONTHS[index]} ${year}`, time };
}

/**
 * Events the snapshot can actually place: a parseable timestamp and a declared
 * strength. Anything else is a producer diagnostic, not project history.
 */
export function usableEvents(events) {
  return (events ?? [])
    .map((event) => ({ event, at: moment(event?.occurred_at) }))
    .filter(({ event, at }) => at && event.strength in STRENGTH_RANK)
    .sort((left, right) => (
      right.at.day.localeCompare(left.at.day)
      || STRENGTH_RANK[left.event.strength] - STRENGTH_RANK[right.event.strength]
      || right.at.time.localeCompare(left.at.time)
      || String(left.event.id).localeCompare(String(right.event.id))
    ));
}

/** Same order, folded into one group per day. */
export function groupByDay(events) {
  const groups = [];
  for (const entry of usableEvents(events)) {
    const last = groups.at(-1);
    if (last?.day === entry.at.day) last.entries.push(entry);
    else groups.push({ day: entry.at.day, label: entry.at.label, entries: [entry] });
  }
  return groups;
}

const readable = (kind) => String(kind ?? 'event').replace(/[-_]/g, ' ');

export function initChronicle({ root = document } = {}) {
  const mount = root.querySelector('[data-mount="chronicle"]');
  if (!mount) return null;
  const doc = mount.ownerDocument;
  const el = maker(doc);

  // The snapshot currently being rendered, so evidence paths can be checked
  // against the inventory without threading it through every helper.
  let shown = {};

  function renderEvent({ event, at }, index) {
    const article = el('article', 'timeline-event');
    article.dataset.event = event.id ?? `event-${index}`;
    article.dataset.strength = event.strength;
    article.dataset.kind = event.kind ?? '';

    // The day is the group heading; the row states only the time (DESIGN.md §5:
    // each fact appears once).
    const time = el('time', 'timeline-date', at.time);
    time.setAttribute('datetime', event.occurred_at);
    article.appendChild(time);

    const copy = el('div', 'timeline-copy');
    const head = el('div', 'timeline-head');
    head.appendChild(el('span', 'timeline-kind', readable(event.kind)));
    head.appendChild(el(
      'span',
      event.strength === 'strong' ? 'badge badge-healthy' : 'badge badge-neutral',
      event.strength === 'strong' ? 'strong' : 'source',
    ));
    copy.appendChild(head);
    copy.appendChild(el('h4', null, event.title || readable(event.kind)));

    const evidence = event.evidence ?? {};
    const artifact = (shown.artifacts ?? []).find((entry) => entry.path === evidence.path);
    const location = evidence.line ? `${evidence.path}:${evidence.line}` : evidence.path;
    const provenance = el('p', 'timeline-evidence');
    // v1 only emits `source` for git commits, but the label follows the kind so
    // a later source never gets announced as something it is not.
    provenance.appendChild(el('span', 'timeline-strength', event.strength === 'strong'
      ? 'Strong evidence · lifecycle, log, or checkpoint record'
      : `Source evidence · ${String(event.kind ?? '').startsWith('git') ? 'git commit' : readable(event.kind)}`));
    if (artifact) provenance.appendChild(readerLink(doc, artifact, { label: location, line: evidence.line ?? null }));
    else if (evidence.path) provenance.appendChild(el('span', 'timeline-path', location));
    if (evidence.field) provenance.appendChild(el('code', 'code-chip', evidence.field));
    copy.appendChild(provenance);

    article.appendChild(copy);
    return article;
  }

  function render(snapshot) {
    shown = snapshot ?? {};
    mount.replaceChildren();

    const groups = groupByDay(shown.events);
    const counted = groups.flatMap((group) => group.entries);

    const band = el('section', 'band');
    const head = el('header', 'band-head');
    const title = el('h2', 'band-title');
    const handle = el('span', 'drag', '⠿');
    handle.setAttribute('aria-hidden', 'true');
    title.append(handle, doc.createTextNode('Project understanding events'));
    head.appendChild(title);

    const strong = counted.filter(({ event }) => event.strength === 'strong').length;
    const summary = el('span', 'pill', `${counted.length} event${counted.length === 1 ? '' : 's'} · ${strong} strong · ${counted.length - strong} source-strength`);
    summary.dataset.eventSummary = 'true';
    head.appendChild(summary);
    band.appendChild(head);

    if (!counted.length) {
      const empty = el('p', 'view-empty', 'No evidence-backed events in this snapshot. Chronicle reads recorded events only — lifecycle fields, log headings, checkpoints, goal reviews, and git commits. File modification times are not project history and are never turned into events.');
      empty.dataset.empty = 'chronicle';
      // Honest empty states name the move that would fill them.
      empty.append(doc.createTextNode(' Record one with '), el('code', 'code-chip', '/loam::checkpointing'), doc.createTextNode(', then Refresh.'));
      band.appendChild(empty);
      mount.appendChild(band);
      return;
    }

    const timeline = el('div', 'timeline');
    for (const group of groups) {
      const section = el('section', 'timeline-day');
      section.dataset.day = group.day;
      section.appendChild(el('h3', 'timeline-day-label', group.label));
      group.entries.forEach((entry, index) => section.appendChild(renderEvent(entry, index)));
      timeline.appendChild(section);
    }
    band.appendChild(timeline);
    mount.appendChild(band);
  }

  root.addEventListener('loam:render', (event) => render(event.detail?.snapshot));
  return { render };
}
