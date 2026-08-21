/**
 * Atlas: bounded, inspectable graph projections.
 *
 * These tests target data and DOM structure, never pixel layout. Cytoscape
 * draws to a canvas that jsdom cannot provide, so the graph runs headless here
 * and the assertions live where the information actually has to be complete:
 * the projection itself and the list/table parity that carries it to keyboard
 * and screen-reader users. Interactive verification is T18's browser pass.
 */

import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { afterEach, describe, it } from 'node:test';

import { JSDOM, VirtualConsole } from 'jsdom';

import { boot } from '../public/js/app.mjs';
import { initInspector } from '../public/js/inspector.mjs';
import { state } from '../public/js/store.mjs';
import { CLUSTERS, MAX_NODES, PROJECTIONS, canvasToken, emptyStateFor, initAtlas, projectGraph } from '../public/js/views/atlas.mjs';

const PUBLIC_ROOT = new URL('../public/', import.meta.url);
const html = await readFile(new URL('index.html', PUBLIC_ROOT), 'utf8');
const css = await readFile(new URL('styles/components.css', PUBLIC_ROOT), 'utf8');

const realFetch = globalThis.fetch;
const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

/* ---------- generated graph fixture (spec: "Verify with: generated graph fixture") ---------- */

const hex = (seed) => String(seed).padStart(2, '0').repeat(32).slice(0, 64);

function artifact(path, kind, attributes = {}, extra = {}) {
  return {
    id: path,
    path,
    kind,
    title: path.split('/').pop().replace(/\.md$/, ''),
    lifecycle_status: null,
    created_at: '2026-06-01T08:00:00+02:00',
    updated_at: '2026-07-15T10:00:00+02:00',
    captured_at: null,
    content_hash: hex(1),
    bytes: 500,
    attributes,
    parse_errors: [],
    ...extra,
  };
}

let edgeSeed = 0;
function relationship(from, to, kind, origin) {
  edgeSeed += 1;
  return {
    id: hex(edgeSeed % 90),
    from,
    to,
    kind,
    origin,
    evidence: { path: from, line: 4, section: null, field: null, content_hash: hex(2) },
    rule: origin === 'derived'
      ? { id: `${kind}-rule`, version: '1', generated_at: '2026-07-17T15:30:00+02:00', confidence: 0.9 }
      : null,
  };
}

const HUB = 'wiki/code/hub-service.md';
const MODULES = Array.from({ length: 30 }, (_, i) => `wiki/code/module-${String(i).padStart(2, '0')}.md`);
const STALE_MODULE = MODULES[0];
const CONCEPTS = ['wiki/concepts/bounded-projection.md', 'wiki/concepts/provenance.md'];
const TOPICS = ['wiki/topics/ingestion.md', 'wiki/topics/retrieval.md'];
const MISSING = 'wiki/topics/never-written.md';
const GOAL = 'goals/atlas.md';
const SPEC = 'specs/atlas.md';
const PLAN = 'plans/atlas.md';
const CHECKPOINT = 'wiki/checkpoints/checkpoint-2026-08-01-1200.md';

const snapshot = {
  profile: 'loam-view/v1',
  schema_version: '1',
  generated_at: '2026-08-01T12:00:00+02:00',
  status: 'ready',
  workspace: { name: 'atlas-fixture', root: '/tmp/atlas', git: { branch: 'main', state: 'clean', dirty: false, changed_count: 0 } },
  capabilities: { qmd: { state: 'ready', detail: null } },
  artifacts: [
    artifact(HUB, 'code', { source_path: 'src/hub-service.js', source_exists: true }),
    ...MODULES.map((path) => artifact(
      path,
      'code',
      { source_path: path.replace('wiki/code/', 'src/').replace('.md', '.js'), source_exists: path !== STALE_MODULE },
    )),
    ...CONCEPTS.map((path) => artifact(path, 'concept')),
    ...TOPICS.map((path) => artifact(path, 'topic')),
    artifact(GOAL, 'goal', {}, { lifecycle_status: 'active' }),
    artifact(SPEC, 'spec'),
    artifact(PLAN, 'plan'),
    artifact(CHECKPOINT, 'checkpoint'),
  ],
  relationships: [
    ...MODULES.map((path) => relationship(path, HUB, 'wikilink', 'explicit')),
    relationship(CONCEPTS[0], MODULES[1], 'wikilink', 'explicit'),
    relationship(CONCEPTS[0], MODULES[2], 'wikilink', 'explicit'),
    relationship(CONCEPTS[1], HUB, 'wikilink', 'explicit'),
    relationship(TOPICS[0], CONCEPTS[0], 'wikilink', 'explicit'),
    relationship(TOPICS[1], CONCEPTS[1], 'wikilink', 'explicit'),
    relationship(TOPICS[0], MISSING, 'wikilink', 'explicit'),
    relationship(GOAL, SPEC, 'goal-linked-spec', 'derived'),
    relationship(SPEC, PLAN, 'spec-linked-plan', 'derived'),
    relationship(PLAN, HUB, 'plan-touched-file', 'derived'),
    relationship(CHECKPOINT, PLAN, 'checkpoint-covers-plan', 'derived'),
  ],
  events: [{
    id: 'checkpoint-2026-08-01-1200',
    occurred_at: '2026-08-01T12:00:00+02:00',
    kind: 'checkpoint-captured',
    title: 'Checkpoint captured',
    artifact_id: CHECKPOINT,
    strength: 'strong',
    evidence: { path: CHECKPOINT, line: 1, section: null, field: null, content_hash: hex(3) },
  }],
  metrics: [],
  signals: [],
  hints: [],
  probes: [],
};

/** Every path the relationship set touches — the corpus Atlas must never draw whole. */
const ALL_ENDPOINTS = new Set(snapshot.relationships.flatMap((edge) => [edge.from, edge.to]));

function jsonResponse(body, status = 200) {
  return { ok: status >= 200 && status < 300, status, json: async () => body };
}

/** Boot the real shell, wire the Inspector as main.mjs does, then mount Atlas. */
async function mount(served = snapshot) {
  // jsdom reports the missing canvas backend as a jsdomError; Atlas is expected
  // to fall back to a headless graph, so the noise is not a test signal.
  const virtualConsole = new VirtualConsole();
  const dom = new JSDOM(html, { url: 'http://127.0.0.1:8000/', virtualConsole });
  globalThis.document = dom.window.document;
  globalThis.fetch = async (url) => {
    const { pathname } = new URL(url, 'http://127.0.0.1:8000');
    if (pathname === '/api/snapshot') return jsonResponse(served);
    return jsonResponse({ error: 'not_found' }, 404);
  };

  state.snapshot = null;
  state.error = null;
  state.refreshing = false;

  const doc = dom.window.document;
  await boot(doc);
  initInspector({ root: doc, getSnapshot: () => state.snapshot });
  const atlas = initAtlas({ root: doc });
  await flush();

  const rows = () => [...doc.querySelectorAll('[data-atlas-nodes] tbody tr')];
  return {
    dom,
    doc,
    atlas,
    rows,
    nodeRows: () => rows().filter((row) => row.dataset.row === 'node'),
    clusterRows: () => rows().filter((row) => row.dataset.row === 'cluster'),
    edgeRows: () => [...doc.querySelectorAll('[data-atlas-edges] li')],
    body: doc.querySelector('[data-inspector-body]'),
    panel: doc.querySelector('[data-inspector]'),
  };
}

afterEach(() => {
  globalThis.fetch = realFetch;
  delete globalThis.document;
});

/* ---------- graph palette ---------- */

describe('graph colours reach the canvas as sRGB', () => {
  /**
   * A stub 2D context that behaves like a modern browser's: it accepts OKLCH and
   * serialises it straight back (which is exactly why reading `fillStyle` is not
   * a conversion), and it paints a known pixel.
   */
  function stubDoc(tokens, pixel, { rejects = [] } = {}) {
    let fillStyle = '';
    const context = {
      globalCompositeOperation: 'source-over',
      // A real context silently ignores an unparseable assignment, leaving the
      // previous value standing; that is what the sentinel check relies on.
      get fillStyle() { return fillStyle; },
      set fillStyle(value) { if (!rejects.includes(value)) fillStyle = value; },
      fillRect() {},
      getImageData: () => ({ data: pixel }),
    };
    const doc = { createElement: () => ({ getContext: () => context }), documentElement: {} };
    const win = { getComputedStyle: () => ({ getPropertyValue: (name) => tokens[name] ?? '' }) };
    return { token: canvasToken(doc, win), context };
  }

  it('converts an OKLCH token through a painted pixel, not through fillStyle', () => {
    const { token, context } = stubDoc(
      { '--node-code': 'oklch(68% 0.11 244)' },
      [95, 165, 235, 255],
    );
    assert.equal(token('--node-code'), 'rgb(95, 165, 235)');
    assert.equal(context.globalCompositeOperation, 'copy', 'the fill must replace, not composite');
  });

  it('keeps a token’s alpha', () => {
    const { token } = stubDoc({ '--border': 'oklch(100% 0 0 / 0.09)' }, [255, 255, 255, 23]);
    assert.equal(token('--border'), 'rgba(255, 255, 255, 0.09)');
  });

  it('drops a token the browser will not parse rather than inventing one', () => {
    const { token } = stubDoc({ '--bogus': 'not-a-colour' }, [0, 0, 0, 255], { rejects: ['not-a-colour'] });
    assert.equal(token('--bogus'), '', 'an unparseable token is dropped, never guessed');
    assert.equal(token('--missing'), '');
  });
});

/* ---------- honest empty states ---------- */

describe('Atlas on a snapshot nobody could read', () => {
  it('renders nothing rather than claiming the workspace has no substrate', async () => {
    const { doc } = await mount(null);

    const target = doc.querySelector('[data-mount="atlas"]');
    assert.equal(
      target.textContent.trim(),
      '',
      '"no pages to connect" is a claim about the workspace; with no snapshot it is unfounded',
    );
    assert.equal(target.querySelector('[data-empty="atlas"]'), null);
  });
});

describe('an empty Atlas says why it is empty', () => {
  const capabilities = (overrides = {}) => ({
    wiki: { state: 'ready' },
    code_graph: { state: 'absent' },
    goals: { state: 'absent' },
    work: { state: 'absent' },
    ...overrides,
  });

  it('blames the missing capability, not the project, and names the next action', () => {
    assert.match(
      emptyStateFor({ capabilities: capabilities({ wiki: { state: 'absent' } }) }, 'architecture').command,
      /scaffolding-wiki/,
    );
    assert.match(emptyStateFor({ capabilities: capabilities() }, 'code').command, /ingesting-codebase/);
    assert.match(emptyStateFor({ capabilities: capabilities() }, 'work').command, /setting-goals/);

    // Memory and a code graph both present: the honest reason is that nothing links.
    const linked = emptyStateFor(
      { capabilities: capabilities({ code_graph: { state: 'ready' } }) },
      'code',
    );
    assert.equal(linked.command, null);
    assert.match(linked.message, /links|projection/);
  });

  it('renders the reason above the map when a projection has no nodes', async () => {
    const bare = {
      ...snapshot,
      capabilities: { ...snapshot.capabilities, wiki: { state: 'absent', required: true, reason: null, evidence: null } },
      artifacts: [],
      relationships: [],
    };
    const { doc } = await mount(bare);

    const note = doc.querySelector('[data-empty="atlas"]');
    assert.ok(note, 'an empty map must explain itself');
    assert.equal(note.hidden, false);
    assert.match(note.textContent, /no memory substrate/i);
    assert.match(note.querySelector('code').textContent, /loam::scaffolding-wiki/);
  });

  it('hides the explanation as soon as the projection has nodes', async () => {
    const { doc } = await mount();
    assert.equal(doc.querySelector('[data-empty="atlas"]').hidden, true);
  });
});

/* ---------- boundedness: the projection itself ---------- */

describe('Atlas projections are bounded', () => {
  it('opens on labeled clusters with a few anchors, never the corpus', () => {
    const view = projectGraph(snapshot, { projection: 'architecture' });

    assert.equal(view.mode, 'overview');
    assert.deepEqual(
      view.clusters.map((cluster) => cluster.label),
      CLUSTERS.map((cluster) => cluster.label),
      'Atlas starts from the five labeled clusters (spec: Code, Concepts, Domain, Work, Memory Health)',
    );
    assert.ok(view.nodes.length > 0, 'each populated cluster must expose anchor nodes');
    assert.ok(
      view.nodes.length < ALL_ENDPOINTS.size,
      `overview drew ${view.nodes.length} of ${ALL_ENDPOINTS.size} endpoints — it must be a sample, not the corpus`,
    );
    assert.ok(view.nodes.length <= MAX_NODES);
  });

  it('bounds a cluster neighborhood to one hop and a hard cap', () => {
    const view = projectGraph(snapshot, { projection: 'architecture', focus: 'cluster:code' });

    assert.equal(view.mode, 'neighborhood');
    assert.ok(view.nodes.length <= MAX_NODES, `cluster neighborhood drew ${view.nodes.length} nodes`);
    assert.ok(view.nodes.length < ALL_ENDPOINTS.size, 'a cluster click must never expand to the full corpus');
    assert.equal(view.truncated, true, 'a neighborhood larger than the cap must say so');

    // Every drawn node is a seed or one hop from a seed.
    const seeds = new Set(view.seeds);
    const drawn = new Set(view.nodes.map((node) => node.path));
    for (const path of drawn) {
      if (seeds.has(path)) continue;
      const adjacent = snapshot.relationships.some((edge) =>
        (edge.from === path && seeds.has(edge.to)) || (edge.to === path && seeds.has(edge.from)));
      assert.ok(adjacent, `${path} is more than one hop from the focus`);
    }
  });

  it('bounds the neighborhood of a high-degree node', () => {
    const view = projectGraph(snapshot, { projection: 'architecture', focus: HUB });

    assert.equal(view.mode, 'neighborhood');
    assert.deepEqual(view.seeds, [HUB]);
    assert.ok(view.nodes.length <= MAX_NODES, `the hub has 32 neighbours; Atlas drew ${view.nodes.length}`);
    assert.equal(view.truncated, true);
  });

  it('draws only edges whose endpoints are both visible', () => {
    for (const focus of [null, 'cluster:code', HUB, 'cluster:work']) {
      const view = projectGraph(snapshot, { projection: 'architecture', focus });
      const drawn = new Set(view.nodes.map((node) => node.path));
      for (const edge of view.edges) {
        assert.ok(drawn.has(edge.from) && drawn.has(edge.to), `dangling edge ${edge.from} -> ${edge.to}`);
      }
    }
  });

  it('takes graph data only from snapshot relationships', () => {
    const view = projectGraph({ ...snapshot, relationships: [] }, { projection: 'architecture' });
    assert.deepEqual(view.nodes, [], 'no relationships means no graph, however many artifacts exist');
    assert.deepEqual(view.edges, []);
  });
});

/* ---------- DOM: band, map card, and list/table parity ---------- */

describe('Atlas DOM', () => {
  it('renders a Neighborhood map band with projection filters over a bordered map card', async () => {
    const { doc } = await mount();

    const band = doc.querySelector('[data-mount="atlas"] .band');
    assert.ok(band, 'Atlas mounts a band');
    assert.match(band.querySelector('.band-title').textContent, /Neighborhood map/);

    const filters = band.querySelector('.band-head [data-atlas-projections]');
    assert.ok(filters, 'the projection filters live in the band header (spec: Atlas)');
    assert.deepEqual(
      [...filters.querySelectorAll('button')].map((button) => button.dataset.projection),
      PROJECTIONS.map((projection) => projection.id),
    );
    assert.deepEqual(
      PROJECTIONS.map((projection) => projection.label),
      ['Architecture', 'Code', 'Domain / concepts', 'Work', 'Change impact'],
    );

    const map = band.querySelector('[data-atlas-map]');
    assert.ok(map, 'the graph sits inside a bordered map card');
    assert.ok(map.classList.contains('atlas-map'));
    assert.ok(map.querySelector('[data-atlas-stage]'), 'the map card holds the graph stage');
    // The canvas carries no accessible content; the parity table is the text path.
    assert.equal(map.querySelector('[data-atlas-stage]').getAttribute('aria-hidden'), 'true');

    const mapRule = css.slice(css.indexOf('.atlas-map {'));
    assert.match(mapRule.slice(0, mapRule.indexOf('}')), /border:\s*var\(--rule\) solid var\(--border\)/);
  });

  it('carries the drawn graph in a list/table alternative, node for node and edge for edge', async () => {
    const { atlas, nodeRows, clusterRows, edgeRows } = await mount();
    const view = atlas.projection();

    assert.deepEqual(
      clusterRows().map((row) => row.dataset.cluster),
      view.clusters.map((cluster) => cluster.id),
      'every cluster drawn on the map has a row',
    );
    assert.deepEqual(
      nodeRows().map((row) => row.dataset.path),
      view.nodes.map((node) => node.path),
      'every node drawn on the map has a row',
    );
    assert.deepEqual(
      edgeRows().map((row) => row.dataset.edge),
      view.edges.map((edge) => edge.id),
      'every relationship drawn on the map has a row',
    );

    // The rows carry the information, not just the identifier.
    for (const row of nodeRows()) {
      const node = view.nodes.find((candidate) => candidate.path === row.dataset.path);
      assert.match(row.textContent, new RegExp(node.title.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')));
      assert.equal(row.dataset.kind, node.kind);
    }
    for (const row of edgeRows()) {
      const edge = view.edges.find((candidate) => candidate.id === row.dataset.edge);
      assert.equal(row.dataset.origin, edge.origin);
      assert.match(row.textContent, new RegExp(edge.kind));
    }

    // Screen-reader readable: a real table with a name, and a real list.
    const table = nodeRows()[0].closest('table');
    assert.ok(table.querySelector('caption') || table.getAttribute('aria-label'), 'the parity table must be named');
    assert.equal(edgeRows()[0].parentElement.tagName, 'UL');
  });

  it('distinguishes explicit from derived edges visually and in text', async () => {
    const { edgeRows } = await mount();

    const explicit = edgeRows().find((row) => row.dataset.origin === 'explicit');
    const derived = edgeRows().find((row) => row.dataset.origin === 'derived');
    assert.ok(explicit && derived, 'the fixture must exercise both edge origins');

    assert.equal(explicit.querySelector('.edge-sample').classList.contains('derived'), false);
    assert.equal(derived.querySelector('.edge-sample').classList.contains('derived'), true);
    assert.match(explicit.textContent, /explicit/i);
    assert.match(derived.textContent, /derived/i);

    // ...and the two marks resolve to the two graph edge tokens, not one style.
    const base = css.slice(css.indexOf('.edge-sample {'));
    assert.match(base.slice(0, base.indexOf('}')), /var\(--edge-explicit\)/);
    const dashed = css.slice(css.indexOf('.edge-sample.derived {'));
    const dashedRule = dashed.slice(0, dashed.indexOf('}'));
    assert.match(dashedRule, /var\(--edge-derived\)/);
    assert.match(dashedRule, /dashed/);
  });

  it('colors node rows by the graph palette and marks stale or unindexed endpoints', async () => {
    const { doc, atlas, nodeRows } = await mount();

    atlas.focus('cluster:code');
    const stale = nodeRows().find((row) => row.dataset.path === STALE_MODULE);
    assert.ok(stale, 'the stale module must be reachable from the code cluster');
    assert.equal(stale.dataset.stale, 'true', 'a node whose source file is gone reads as drifted');

    atlas.focus('cluster:domain');
    const broken = nodeRows().find((row) => row.dataset.path === MISSING);
    assert.ok(broken, 'an endpoint with no artifact still appears — a broken link is evidence');
    assert.equal(broken.dataset.indexed, 'false');
    assert.equal(broken.querySelector('button'), null, 'an unindexed endpoint has nothing to inspect');

    // The kind cell is the coloured mark; the node name stays in ink.
    for (const [cluster, token] of [['code', '--node-code'], ['work', '--node-work'], ['memory', '--node-memory']]) {
      const rule = css.slice(css.indexOf(`.atlas-node-row[data-cluster="${cluster}"] .atlas-row-kind {`));
      assert.match(rule.slice(0, rule.indexOf('}')), new RegExp(`var\\(${token}\\)`));
    }
    const staleRule = css.slice(css.indexOf('.atlas-node-row[data-stale="true"] .atlas-row-state {'));
    assert.match(staleRule.slice(0, staleRule.indexOf('}')), /var\(--state-drift\)/);
    // ...and the drift is never colour alone: the state cell names it.
    assert.match(stale.querySelector('.atlas-row-state').textContent, /source file missing/);
    assert.ok(doc.querySelector('[data-atlas-parity]'));
  });
});

/* ---------- interaction: bounded expansion and the Inspector ---------- */

describe('Atlas interaction', () => {
  it('opens a bounded neighborhood when a cluster is activated', async () => {
    const { doc, atlas, nodeRows, clusterRows } = await mount();

    const before = nodeRows().length;
    doc.querySelector('[data-atlas-focus="cluster:code"]').click();

    const view = atlas.projection();
    assert.equal(view.mode, 'neighborhood');
    assert.ok(nodeRows().length > before, 'the neighborhood shows more than the cluster anchors');
    assert.ok(nodeRows().length <= MAX_NODES, `a cluster click drew ${nodeRows().length} rows`);
    assert.ok(nodeRows().length < ALL_ENDPOINTS.size, 'a cluster click must never render the whole corpus');
    assert.equal(clusterRows().length, 0, 'a neighborhood replaces the cluster overview');
    assert.match(doc.querySelector('[data-atlas-scope]').textContent, /Code/);

    doc.querySelector('[data-atlas-back]').click();
    assert.equal(atlas.projection().mode, 'overview', 'Back returns to the clusters');
  });

  it('restricts the graph to the chosen projection', async () => {
    const { doc, atlas, edgeRows } = await mount();

    doc.querySelector('[data-projection="work"]').click();
    assert.equal(doc.querySelector('[data-projection="work"]').getAttribute('aria-pressed'), 'true');
    assert.equal(doc.querySelector('[data-projection="architecture"]').getAttribute('aria-pressed'), 'false');

    const kinds = new Set(atlas.projection().edges.map((edge) => edge.kind));
    assert.ok(kinds.size > 0, 'the Work projection must find the goal -> spec -> plan chain');
    for (const row of edgeRows()) assert.match(row.textContent, /goal|spec|plan|checkpoint/i);

    doc.querySelector('[data-projection="change-impact"]').click();
    const impacted = atlas.projection();
    assert.ok(
      impacted.edges.every((edge) => [edge.from, edge.to].some((path) =>
        path === CHECKPOINT || path === STALE_MODULE || path === MISSING)),
      'Change impact is scoped to changed, drifted, or missing endpoints',
    );
  });

  it('opens the Inspector on a node row and on an edge row', async () => {
    const { doc, panel, body, nodeRows, edgeRows } = await mount();

    const nodeButton = nodeRows().find((row) => row.dataset.path === HUB).querySelector('button');
    assert.equal(nodeButton.tagName, 'BUTTON', 'selection is keyboard-operable by construction');
    nodeButton.click();
    await flush();
    assert.equal(panel.classList.contains('is-open'), true);
    assert.match(body.querySelector('.inspector-title').textContent, /hub-service/);

    doc.querySelector('[data-inspector-close]').click();

    const derivedRow = edgeRows().find((row) => row.dataset.origin === 'derived');
    derivedRow.querySelector('button').click();
    await flush();
    assert.equal(panel.classList.contains('is-open'), true);
    assert.equal(body.querySelector('.inspector-kind').dataset.kind, 'relationship');
    // Derived edges carry their provenance into the Inspector.
    assert.match(body.querySelector('[data-section="rule"]').textContent, /Confidence\s*90%/);
    assert.match(body.querySelector('[data-section="origin"]').textContent, /derived/);
  });

  it('keeps the human in place when the snapshot is re-rendered', async () => {
    const { doc, atlas } = await mount();

    doc.querySelector('[data-projection="code"]').click();
    doc.querySelector('[data-atlas-focus="cluster:code"]')?.click();
    const focused = atlas.projection().focus;

    doc.dispatchEvent(new doc.defaultView.CustomEvent('loam:render', {
      detail: { snapshot, route: 'atlas' },
    }));
    assert.equal(atlas.projection().projection, 'code');
    assert.equal(atlas.projection().focus, focused);
  });
});
