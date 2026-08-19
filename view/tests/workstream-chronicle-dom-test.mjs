/**
 * Work Stream and Chronicle (T16).
 *
 * Both views render straight from the snapshot: Work Stream from artifacts +
 * relationships, Chronicle from `events` only. The assertions here are about
 * honesty as much as layout — a missing goal has to show up as a gap with its
 * evidence, and Chronicle must not invent chronology the producer never emitted.
 */

import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { afterEach, describe, it } from 'node:test';

import { JSDOM } from 'jsdom';

import { boot } from '../public/js/app.mjs';
import { initInspector } from '../public/js/inspector.mjs';
import { state } from '../public/js/store.mjs';
import { initChronicle } from '../public/js/views/chronicle.mjs';
import { initWorkStream } from '../public/js/views/workstream.mjs';

const html = await readFile(new URL('../public/index.html', import.meta.url), 'utf8');
const base = JSON.parse(
  await readFile(new URL('fixtures/snapshots/valid/ready-full.json', import.meta.url), 'utf8'),
);

const realFetch = globalThis.fetch;

const artifact = (path, kind, title, extra = {}) => ({
  id: path,
  path,
  kind,
  title,
  lifecycle_status: null,
  created_at: '2026-07-01T09:00:00+02:00',
  updated_at: '2026-07-06T09:00:00+02:00',
  captured_at: null,
  content_hash: 'a'.repeat(64),
  bytes: 512,
  attributes: {},
  parse_errors: [],
  ...extra,
});

const edge = (from, to, kind, evidence = {}) => ({
  id: `${kind}:${from}:${to}`,
  from,
  to,
  kind,
  origin: 'derived',
  evidence: { path: from, line: 4, section: null, field: null, content_hash: 'b'.repeat(64), ...evidence },
  rule: { id: 'view-structural', version: 1, generated_at: '2026-07-17T15:30:00+02:00', confidence: 1 },
});

/**
 * One fully linked chain (goal -> spec -> plan -> tasks/checkpoint -> touched
 * files -> validation) and one plan with no provenance at all, so both the
 * complete and the honestly-broken shapes are exercised.
 */
const ARTIFACTS = [
  artifact('goals/alpha.md', 'goal', 'Alpha goal', {
    lifecycle_status: 'active',
    attributes: { linked_specs: ['specs/alpha.md'], linked_plans: ['plans/alpha.md'] },
  }),
  artifact('specs/alpha.md', 'spec', 'Alpha spec', {
    lifecycle_status: 'approved',
    attributes: { goal: 'goals/alpha.md', research: [] },
  }),
  artifact('plans/alpha.md', 'plan', 'Alpha plan', {
    lifecycle_status: 'in-progress',
    attributes: {
      spec: 'specs/alpha.md',
      goal: 'goals/alpha.md',
      task_count_declared: 3,
      task_count_observed: 3,
      task_statuses: ['complete', 'complete', 'pending'],
      touched_files: ['src/alpha.js', 'src/untracked.js'],
      acceptance_criteria: { total: 2, done: 1 },
    },
  }),
  artifact('plans/orphan.md', 'plan', 'Orphan plan', {
    attributes: {
      spec: null,
      goal: null,
      task_count_declared: null,
      task_count_observed: 0,
      task_statuses: [],
      touched_files: [],
      acceptance_criteria: { total: 0, done: 0 },
    },
  }),
  artifact('wiki/code/alpha.md', 'code', 'alphaService', {
    attributes: {
      source_path: 'src/alpha.js',
      ingested_at: '1752570000',
      source_size: 256,
      source_hash: 'c'.repeat(64),
      source_exists: true,
    },
  }),
  artifact('wiki/checkpoints/checkpoint-2026-07-05-1400.md', 'checkpoint', 'Alpha checkpoint', {
    captured_at: '2026-07-05T14:00:00+02:00',
    attributes: {
      reason: 'context switch',
      scope: 'Alpha',
      intended_return: 'finish task 3',
      previous: null,
      supersedes: null,
      workstreams: [
        { name: 'Alpha', status: 'active', next: 'finish task 3', pointers: ['plans/alpha.md'] },
      ],
    },
  }),
];

const RELATIONSHIPS = [
  edge('plans/alpha.md', 'specs/alpha.md', 'plan-spec'),
  edge('plans/alpha.md', 'goals/alpha.md', 'plan-goal'),
  edge('goals/alpha.md', 'specs/alpha.md', 'goal-linked-spec'),
  edge('goals/alpha.md', 'plans/alpha.md', 'goal-linked-plan'),
  edge('plans/alpha.md', 'wiki/code/alpha.md', 'plan-touched-file'),
];

const event = (id, occurred_at, kind, title, extra = {}) => ({
  id,
  occurred_at,
  kind,
  title,
  artifact_id: extra.artifact_id ?? null,
  strength: extra.strength ?? 'strong',
  evidence: extra.evidence ?? { path: null, line: null, section: null, field: null, content_hash: null },
});

const EVENTS = [
  event('e-goal', '2026-07-01T09:00:00+02:00', 'goal-created', 'Goal created: Alpha goal', {
    artifact_id: 'goals/alpha.md',
    evidence: { path: 'goals/alpha.md', line: 3, section: null, field: 'created', content_hash: 'a'.repeat(64) },
  }),
  event('e-commit', '2026-07-05T09:30:00+02:00', 'git-commit', 'feat(alpha): retry classification', {
    strength: 'source',
    artifact_id: null,
    evidence: { path: null, line: null, section: null, field: '4d2dcbc', content_hash: null },
  }),
  event('e-checkpoint', '2026-07-05T14:00:00+02:00', 'checkpoint-captured', 'Checkpoint captured: Alpha', {
    artifact_id: 'wiki/checkpoints/checkpoint-2026-07-05-1400.md',
    evidence: {
      path: 'wiki/checkpoints/checkpoint-2026-07-05-1400.md',
      line: 1,
      section: null,
      field: 'Captured',
      content_hash: 'd'.repeat(64),
    },
  }),
  event('e-plan', '2026-07-06T11:00:00+02:00', 'plan-completed', 'Plan completed: Alpha plan', {
    artifact_id: 'plans/alpha.md',
    evidence: { path: 'plans/alpha.md', line: 9, section: null, field: 'status', content_hash: 'e'.repeat(64) },
  }),
];

const snapshotWith = (overrides = {}) => ({
  ...base,
  artifacts: ARTIFACTS,
  relationships: RELATIONSHIPS,
  events: EVENTS,
  ...overrides,
});

function jsonResponse(body, status = 200) {
  return { ok: status >= 200 && status < 300, status, json: async () => body };
}

/** Boot the real shell, then mount both area views exactly as main.mjs does. */
async function mount(snapshot = snapshotWith()) {
  const dom = new JSDOM(html, { url: 'http://127.0.0.1:8000/' });
  globalThis.document = dom.window.document;
  globalThis.fetch = async (url) => {
    const { pathname } = new URL(url, 'http://127.0.0.1:8000');
    if (pathname === '/api/snapshot') return jsonResponse(snapshot);
    return jsonResponse({ error: 'not_found' }, 404);
  };

  state.snapshot = null;
  state.error = null;
  state.refreshing = false;

  const doc = dom.window.document;
  initInspector({ root: doc, getSnapshot: () => state.snapshot });
  initWorkStream({ root: doc });
  initChronicle({ root: doc });
  await boot(doc);

  const readerEvents = [];
  doc.addEventListener('loam:open-reader', (openEvent) => readerEvents.push(openEvent.detail));

  return {
    dom,
    doc,
    readerEvents,
    workStream: doc.querySelector('[data-mount="work-stream"]'),
    chronicle: doc.querySelector('[data-mount="chronicle"]'),
    panel: doc.querySelector('[data-inspector]'),
  };
}

const text = (node) => node.textContent.replace(/\s+/g, ' ').trim();

/** The chain band for one plan, by the plan path it is built from. */
const chainFor = (root, path) => root.querySelector(`[data-chain="${path}"]`);
const stage = (chain, name) => chain.querySelector(`[data-stage="${name}"]`);

afterEach(() => {
  globalThis.fetch = realFetch;
  delete globalThis.document;
});

describe('Work Stream traceability river', () => {
  it('renders the goal -> spec -> plan -> task/checkpoint -> files -> validation chain in order', async () => {
    const { workStream } = await mount();
    const chain = chainFor(workStream, 'plans/alpha.md');

    assert.ok(chain, 'a linked plan must render as one traceability chain');
    assert.deepEqual(
      [...chain.querySelectorAll('[data-stage]')].map((node) => node.dataset.stage),
      ['goal', 'spec', 'plan', 'tasks', 'files', 'validation'],
      'the river runs left to right through every traceability stage',
    );

    assert.match(text(stage(chain, 'goal')), /Alpha goal/);
    assert.match(text(stage(chain, 'spec')), /Alpha spec/);
    assert.match(text(stage(chain, 'plan')), /Alpha plan/);
    // Task progress is counted, never estimated: 2 of 3 observed statuses done.
    assert.match(text(stage(chain, 'tasks')), /2\D+3/);
    assert.match(text(stage(chain, 'tasks')), /Alpha checkpoint|checkpoint-2026-07-05-1400/);
    assert.match(text(stage(chain, 'files')), /src\/alpha\.js/);
    assert.match(text(stage(chain, 'validation')), /1\D+2/);
  });

  it('accents active work and seals completed work with a done mark', async () => {
    const { workStream } = await mount();
    const chain = chainFor(workStream, 'plans/alpha.md');

    assert.equal(stage(chain, 'spec').dataset.state, 'sealed', 'an approved spec reads as settled');
    assert.ok(stage(chain, 'spec').querySelector('.river-seal'), 'sealed stages carry the done mark');

    assert.equal(stage(chain, 'tasks').dataset.state, 'active', '2 of 3 tasks is live work');
    assert.equal(stage(chain, 'tasks').querySelector('.river-seal'), null, 'active work is not sealed');
  });

  it('exposes a missing goal as a gap with its evidence instead of hiding or inventing one', async () => {
    const { workStream } = await mount();
    const chain = chainFor(workStream, 'plans/orphan.md');
    const goal = stage(chain, 'goal');

    assert.equal(goal.dataset.state, 'gap');
    assert.match(text(goal), /plans\/orphan\.md/, 'the gap names the artifact that lacks provenance');
    assert.doesNotMatch(text(goal), /Alpha goal/, 'a gap is never filled in from a neighbouring chain');

    // The empty stages downstream stay honest too, rather than reading as done.
    assert.equal(stage(chain, 'files').dataset.state, 'gap');
    assert.equal(stage(chain, 'validation').dataset.state, 'gap');
    assert.match(text(stage(chain, 'validation')), /acceptance criteria/i);
  });

  it('opens artifacts in the Inspector and inventoried documents in Reader', async () => {
    const { workStream, panel, readerEvents } = await mount();
    const chain = chainFor(workStream, 'plans/alpha.md');

    stage(chain, 'spec').querySelector('[data-inspect]').click();
    assert.ok(panel.classList.contains('is-open'), 'a stage artifact opens the shared Inspector');

    // A touched source path is evidence; the code page that documents it is the
    // Reader entry point (Reader renders only Loam Markdown).
    const files = stage(chain, 'files');
    const link = files.querySelector('button.file-link[data-path="wiki/code/alpha.md"]');
    assert.ok(link, 'a mapped touched file offers its code page as a Reader entry point');
    link.click();
    assert.deepEqual(readerEvents.at(-1), {
      path: 'wiki/code/alpha.md',
      kind: 'code',
      title: 'alphaService',
      line: null,
    });

    assert.match(text(files), /src\/untracked\.js/, 'an unmapped touched file is still shown');
    assert.equal(
      files.querySelector('button.file-link[data-path="src/untracked.js"]'),
      null,
      'a path with no inventoried document never becomes a Reader link',
    );
  });

  it('stays a read-only view: no mutation affordances anywhere in the river', async () => {
    const { workStream } = await mount();

    assert.equal(workStream.querySelectorAll('input, textarea, select, form, [contenteditable]').length, 0);
    for (const button of workStream.querySelectorAll('button')) {
      assert.ok(
        button.dataset.inspect !== undefined || button.classList.contains('file-link'),
        `every button is an Inspector or Reader affordance, found: ${button.className}`,
      );
    }
  });

  it('writes snapshot strings as text, never as markup', async () => {
    const hostile = snapshotWith({
      artifacts: ARTIFACTS.map((entry) =>
        entry.path === 'plans/alpha.md' ? { ...entry, title: '<img src=x onerror=alert(1)>' } : entry),
    });
    const { workStream } = await mount(hostile);

    assert.equal(workStream.querySelector('img'), null, 'a hostile title must not become an element');
    assert.match(text(workStream), /<img src=x onerror=alert\(1\)>/);
  });
});

describe('Work Stream on a workspace with no work', () => {
  it('says what is missing and names the next action', async () => {
    const { workStream } = await mount(snapshotWith({ artifacts: [], relationships: [] }));

    const empty = workStream.querySelector('[data-empty="work"]');
    assert.ok(empty, 'an empty river must explain itself');
    assert.match(text(empty), /No goals, specs, or plans/);
    assert.match(empty.querySelector('code')?.textContent ?? '', /loam::setting-goals/);
  });
});

describe('Chronicle evidence timeline', () => {
  it('groups events by day, newest first, with strong sources leading each group', async () => {
    const { chronicle } = await mount();

    const groups = [...chronicle.querySelectorAll('[data-day]')];
    assert.deepEqual(
      groups.map((group) => group.dataset.day),
      ['2026-07-06', '2026-07-05', '2026-07-01'],
      'the timeline runs newest to oldest, one group per day',
    );

    const july5 = groups[1];
    assert.deepEqual(
      [...july5.querySelectorAll('[data-event]')].map((node) => node.dataset.event),
      ['e-checkpoint', 'e-commit'],
      'a strong lifecycle event leads the day; the git commit follows it',
    );
  });

  it('labels git commits by source strength and links inventoried evidence into Reader', async () => {
    const { chronicle, readerEvents } = await mount();

    const commit = chronicle.querySelector('[data-event="e-commit"]');
    assert.equal(commit.dataset.strength, 'source');
    assert.match(text(commit), /source/i, 'a commit says out loud that it is a source-strength event');

    const checkpoint = chronicle.querySelector('[data-event="e-checkpoint"]');
    assert.equal(checkpoint.dataset.strength, 'strong');
    const link = checkpoint.querySelector('button.file-link');
    assert.ok(link, 'evidence that the snapshot inventories is a Reader entry point');
    link.click();
    assert.equal(readerEvents.at(-1).path, 'wiki/checkpoints/checkpoint-2026-07-05-1400.md');

    // The commit's evidence is not a Loam document, so it stays plain text.
    assert.equal(commit.querySelector('button.file-link'), null);
  });

  it('never infers an event from artifact timestamps when the snapshot has none', async () => {
    const { chronicle } = await mount(snapshotWith({ events: [] }));

    assert.equal(chronicle.querySelectorAll('[data-event]').length, 0, 'no event is synthesized');
    assert.equal(chronicle.querySelectorAll('[data-day]').length, 0);
    const empty = chronicle.querySelector('[data-empty]');
    assert.ok(empty, 'an empty timeline says why it is empty');
    assert.match(text(empty), /modification time|mtime/i, 'and names the chronology it refuses to infer');
    assert.match(empty.querySelector('code')?.textContent ?? '', /loam::checkpointing/, 'and names the next action');
  });

  it('reports the evidence mix honestly instead of a bare count', async () => {
    const { chronicle } = await mount();
    const summary = text(chronicle.querySelector('[data-event-summary]'));

    assert.match(summary, /4 events/);
    assert.match(summary, /3 strong/);
    assert.match(summary, /1 .*source/);
  });

  it('drops an event the producer could not timestamp rather than guessing its place', async () => {
    const broken = [...EVENTS, event('e-broken', 'not-a-timestamp', 'goal-created', 'Undated goal')];
    const { chronicle } = await mount(snapshotWith({ events: broken }));

    assert.equal(chronicle.querySelector('[data-event="e-broken"]'), null);
    assert.equal(chronicle.querySelectorAll('[data-event]').length, 4);
  });
});
