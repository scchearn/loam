/**
 * Atlas — the bounded project map (spec: "Atlas"; DESIGN.md §2 Graph Palette).
 *
 * The whole design of this module is one rule: the corpus is never drawn. Atlas
 * opens on five labeled clusters showing a few anchor nodes each, and a click
 * expands exactly one hop from those anchors, capped at MAX_NODES. There is no
 * code path that renders every relationship in the snapshot.
 *
 * Graph data comes solely from `snapshot.relationships`. Artifacts are looked up
 * to label and colour an endpoint, never to invent one: an endpoint with no
 * artifact stays in the map as a broken link, which is evidence.
 *
 * Accessibility is not a fallback here. Cytoscape draws to a canvas that carries
 * no accessible content at all, so the stage is `aria-hidden` and the parity
 * table + relationship list beside it are the real interface: same nodes, same
 * edges, same order, keyboard-operable buttons into the shared Inspector.
 *
 * Security: every snapshot string reaches the DOM through `createTextNode` /
 * `textContent`. There is no `innerHTML` in this module.
 */

// Resolves to `/vendor/…` in the browser and to `view/vendor/…` on disk: the
// server serves view/vendor at that URL prefix precisely so this one specifier
// works in both. Pinned 3.34.0 (view/vendor/manifest.json).
import cytoscape from '../../../vendor/cytoscape/cytoscape.esm.min.mjs';
import { openInspector } from '../inspector.mjs';
import { state } from '../store.mjs';

/** Hard ceiling on nodes drawn at once. A neighborhood past this is truncated and says so. */
export const MAX_NODES = 24;
/** Anchors shown per cluster in the overview — "a few visible anchor nodes", not the cluster. */
export const ANCHORS_PER_CLUSTER = 3;

/**
 * The five clusters from the spec, each mapped onto the DESIGN.md graph palette.
 * Concepts and Domain share the concept green: the palette carries four node
 * hues, and the spec assigns green to "concepts", which both clusters are.
 */
export const CLUSTERS = [
  { id: 'code', label: 'Code', kinds: ['code'] },
  { id: 'concept', label: 'Concepts', kinds: ['concept'] },
  { id: 'domain', label: 'Domain', kinds: ['topic', 'entity', 'analysis'] },
  { id: 'work', label: 'Work', kinds: ['goal', 'spec', 'plan'] },
  { id: 'memory', label: 'Memory Health', kinds: ['checkpoint', 'guidance', 'wiki-index', 'wiki-schema', 'wiki-other'] },
];

export const PROJECTIONS = [
  { id: 'architecture', label: 'Architecture' },
  { id: 'code', label: 'Code' },
  { id: 'domain', label: 'Domain / concepts' },
  { id: 'work', label: 'Work' },
  { id: 'change-impact', label: 'Change impact' },
];

const CLUSTER_BY_KIND = new Map(CLUSTERS.flatMap(({ id, kinds }) => kinds.map((kind) => [kind, id])));
/** An endpoint we cannot classify is a memory-health question, so it lands there. */
const UNKNOWN_CLUSTER = 'memory';

/* ---------------------------------------------------------------- projection */

/** Degree first, then path, so a projection is stable across renders. */
const byWeight = (a, b) => b.degree - a.degree || a.path.localeCompare(b.path);

function nodeIndex(snapshot, edges) {
  const artifacts = new Map((snapshot?.artifacts ?? []).map((artifact) => [artifact.path, artifact]));
  const nodes = new Map();
  for (const edge of edges) {
    for (const path of [edge.from, edge.to]) {
      const existing = nodes.get(path);
      if (existing) {
        existing.degree += 1;
        continue;
      }
      const artifact = artifacts.get(path) ?? null;
      nodes.set(path, {
        path,
        artifact,
        title: artifact?.title || path.split('/').pop() || path,
        kind: artifact?.kind ?? 'unknown',
        cluster: artifact ? CLUSTER_BY_KIND.get(artifact.kind) ?? UNKNOWN_CLUSTER : UNKNOWN_CLUSTER,
        indexed: Boolean(artifact),
        // Dimmed-or-coral in the palette: the source file is gone, the page is
        // archived, or nothing was ever indexed at this path.
        stale: !artifact
          || artifact.attributes?.source_exists === false
          || artifact.lifecycle_status === 'archived',
        degree: 1,
      });
    }
  }
  return nodes;
}

/** The relationships a projection admits — the only place the graph is narrowed. */
function projectionEdges(snapshot, projection, nodes) {
  const all = snapshot?.relationships ?? [];
  const clusterOf = (path) => nodes.get(path)?.cluster;
  const touches = (edge, test) => [edge.from, edge.to].some(test);

  if (projection === 'code') return all.filter((edge) => touches(edge, (p) => clusterOf(p) === 'code'));
  if (projection === 'domain') {
    return all.filter((edge) => touches(edge, (p) => clusterOf(p) === 'concept' || clusterOf(p) === 'domain'));
  }
  if (projection === 'work') return all.filter((edge) => touches(edge, (p) => clusterOf(p) === 'work'));
  if (projection === 'change-impact') {
    // Baseline slot: what recently changed, plus what drifted or went missing.
    const changed = new Set((snapshot?.events ?? []).map((event) => event.artifact_id).filter(Boolean));
    return all.filter((edge) => touches(edge, (p) => changed.has(p) || nodes.get(p)?.stale));
  }
  return all;
}

/**
 * Build the bounded view for a projection and an optional focus.
 *
 * @param {object|null} snapshot
 * @param {{projection?: string, focus?: string|null}} options `focus` is either
 *   `cluster:<id>` or an artifact path; null opens the cluster overview.
 * @returns {{mode: 'overview'|'neighborhood', projection: string, focus: string|null,
 *   clusters: object[], seeds: string[], nodes: object[], edges: object[],
 *   total: number, truncated: boolean}}
 */
export function projectGraph(snapshot, { projection = 'architecture', focus = null } = {}) {
  const nodes = nodeIndex(snapshot, snapshot?.relationships ?? []);
  const edges = projectionEdges(snapshot, projection, nodes);
  const visibleNodes = nodeIndex(snapshot, edges);

  const clusters = CLUSTERS.map((cluster) => {
    const members = [...visibleNodes.values()].filter((node) => node.cluster === cluster.id).sort(byWeight);
    return { ...cluster, count: members.length, anchors: members.slice(0, ANCHORS_PER_CLUSTER) };
  });

  const seeds = focus?.startsWith('cluster:')
    ? (clusters.find((cluster) => `cluster:${cluster.id}` === focus)?.anchors ?? []).map((node) => node.path)
    : (visibleNodes.has(focus) ? [focus] : []);

  let drawn;
  let total;
  if (!focus || !seeds.length) {
    drawn = clusters.flatMap((cluster) => cluster.anchors);
    total = drawn.length;
  } else {
    // One hop from the seeds. Not two, and never the transitive closure.
    const seeded = new Set(seeds);
    const neighbours = new Map();
    for (const edge of edges) {
      if (seeded.has(edge.from) === seeded.has(edge.to)) continue;
      const other = seeded.has(edge.from) ? edge.to : edge.from;
      if (!neighbours.has(other)) neighbours.set(other, visibleNodes.get(other));
    }
    const ranked = [...neighbours.values()].sort(byWeight);
    total = seeds.length + ranked.length;
    drawn = [...seeds.map((path) => visibleNodes.get(path)), ...ranked].slice(0, MAX_NODES);
  }

  const visible = new Set(drawn.map((node) => node.path));
  return {
    mode: focus && seeds.length ? 'neighborhood' : 'overview',
    projection,
    focus: seeds.length ? focus : null,
    clusters,
    seeds,
    nodes: drawn,
    edges: edges.filter((edge) => visible.has(edge.from) && visible.has(edge.to)),
    total,
    truncated: total > drawn.length,
  };
}

/* -------------------------------------------------------------------- graph */

/**
 * Reads a design token as a colour Cytoscape can paint with.
 *
 * The tokens are authored in OKLCH (DESIGN.md: "the canonical source"), and
 * Cytoscape's colour parser understands only hex/rgb/hsl/named — it drops
 * anything else and falls back to its own defaults. So each token is
 * round-tripped through a 2D context, which is the browser's own colour parser,
 * rather than restating the palette here as sRGB literals that could drift from
 * tokens.css. A value the context will not take is dropped, never invented.
 */
function canvasToken(doc, win) {
  const styles = win.getComputedStyle?.(doc.documentElement);
  const context = doc.createElement('canvas').getContext('2d');
  return (name) => {
    const raw = styles?.getPropertyValue(name).trim() ?? '';
    if (!raw || !context) return '';
    // A rejected assignment leaves the previous value standing, so a sentinel
    // no token uses tells "parsed" and "rejected" apart.
    context.fillStyle = '#010101';
    context.fillStyle = raw;
    return context.fillStyle === '#010101' ? '' : context.fillStyle;
  };
}

/** The canvas styling, straight off the DESIGN.md graph palette. */
function graphStyle(token) {
  const rule = (selector, style) => ({
    selector,
    style: Object.fromEntries(Object.entries(style).filter(([, value]) => value !== '')),
  });
  return [
    rule('node', {
      shape: 'round-rectangle',
      width: 104,
      height: 40,
      'background-color': token('--surface-canvas'),
      'border-width': 1,
      'border-color': token('--node-code'),
      label: 'data(label)',
      color: token('--ink-primary'),
      'font-size': 10,
      'text-valign': 'center',
      'text-wrap': 'ellipsis',
      'text-max-width': '88px',
    }),
    rule('node[cluster="concept"], node[cluster="domain"]', { 'border-color': token('--node-concept') }),
    rule('node[cluster="work"]', { 'border-color': token('--node-work') }),
    rule('node[cluster="memory"]', { 'border-color': token('--node-memory') }),
    rule('node.stale', { 'border-color': token('--state-drift'), opacity: 0.6 }),
    rule('node.seed', { 'border-width': 2 }),
    rule('node.cluster', {
      'background-opacity': 0.04,
      'border-color': token('--border'),
      color: token('--ink-tertiary'),
      'font-size': 9,
      'text-valign': 'top',
      'text-transform': 'uppercase',
    }),
    rule('edge', {
      width: 1.5,
      'curve-style': 'bezier',
      'line-color': token('--edge-explicit'),
    }),
    rule('edge[origin="derived"]', {
      'line-color': token('--edge-derived'),
      'line-style': 'dashed',
    }),
  ];
}

function graphElements(view) {
  const elements = [];
  if (view.mode === 'overview') {
    for (const cluster of view.clusters) {
      if (!cluster.anchors.length) continue;
      elements.push({ data: { id: `cluster:${cluster.id}`, label: cluster.label, cluster: cluster.id }, classes: 'cluster' });
    }
  }
  for (const node of view.nodes) {
    elements.push({
      data: {
        id: node.path,
        label: node.title,
        cluster: node.cluster,
        ...(view.mode === 'overview' ? { parent: `cluster:${node.cluster}` } : {}),
      },
      classes: [node.stale ? 'stale' : '', view.seeds.includes(node.path) ? 'seed' : ''].filter(Boolean).join(' '),
    });
  }
  for (const edge of view.edges) {
    elements.push({ data: { id: edge.id, source: edge.from, target: edge.to, origin: edge.origin, label: edge.kind } });
  }
  return elements;
}

/* --------------------------------------------------------------------- view */

export function initAtlas({ root = document, getSnapshot = () => state.snapshot } = {}) {
  const mount = root.querySelector('[data-mount="atlas"]');
  if (!mount) return null;

  const doc = mount.ownerDocument;
  const win = doc.defaultView ?? globalThis;
  const el = (tag, className, text) => {
    const node = doc.createElement(tag);
    if (className) node.className = className;
    if (text != null) node.appendChild(doc.createTextNode(String(text)));
    return node;
  };

  const chosen = { projection: 'architecture', focus: null };
  let view = projectGraph(null, chosen);
  let cy = null;

  /* ---- static chrome: built once, refilled per render ---- */

  const band = el('section', 'band');
  const head = el('header', 'band-head');
  const title = el('h2', 'band-title');
  const drag = el('span', 'drag', '⠿');
  drag.setAttribute('aria-hidden', 'true');
  title.append(drag, doc.createTextNode('Neighborhood map'));
  const filters = el('div', 'view-actions');
  filters.dataset.atlasProjections = '';
  filters.setAttribute('role', 'group');
  filters.setAttribute('aria-label', 'Atlas projections');
  for (const projection of PROJECTIONS) {
    const button = el('button', 'filter-button', projection.label);
    button.type = 'button';
    button.dataset.projection = projection.id;
    button.addEventListener('click', () => {
      // A focus belongs to the projection it was opened in, so switching
      // projections returns to that projection's clusters.
      chosen.projection = projection.id;
      chosen.focus = null;
      render();
    });
    filters.appendChild(button);
  }
  head.append(title, filters);

  const layout = el('div', 'atlas-layout');
  const map = el('div', 'atlas-map');
  map.dataset.atlasMap = '';
  const stage = el('div', 'atlas-stage');
  stage.dataset.atlasStage = '';
  // The canvas exposes nothing to assistive tech; the parity table is the path.
  stage.setAttribute('aria-hidden', 'true');
  const scopeBar = el('div', 'atlas-scope');
  const scope = el('p', 'mono-label');
  scope.dataset.atlasScope = '';
  const back = el('button', 'ghost-btn', 'Back to clusters');
  back.type = 'button';
  back.dataset.atlasBack = '';
  back.addEventListener('click', () => {
    chosen.focus = null;
    render();
  });
  scopeBar.append(scope, back);
  map.append(stage, scopeBar);

  const parity = el('aside', 'atlas-marginalia');
  parity.dataset.atlasParity = '';
  parity.append(el('p', 'mono-label', 'Text alternative'));
  const table = el('table', 'atlas-table');
  table.dataset.atlasNodes = '';
  table.setAttribute('aria-label', 'Nodes in the visible Atlas projection');
  const headRow = doc.createElement('tr');
  for (const label of ['Kind', 'Node', 'State']) {
    const cell = el('th', null, label);
    cell.setAttribute('scope', 'col');
    headRow.appendChild(cell);
  }
  const thead = doc.createElement('thead');
  thead.appendChild(headRow);
  const tbody = doc.createElement('tbody');
  table.append(thead, tbody);

  const edgeHeading = el('p', 'mono-label', 'Relationships');
  const edgeList = el('ul', 'atlas-edge-list');
  edgeList.dataset.atlasEdges = '';
  edgeList.setAttribute('aria-label', 'Relationships in the visible Atlas projection');

  const key = el('ul', 'edge-key');
  for (const [className, label] of [['edge-sample', 'Explicit link, written in the document'], ['edge-sample derived', 'Derived link, with rule and confidence']]) {
    const item = doc.createElement('li');
    const sample = el('span', className);
    sample.setAttribute('aria-hidden', 'true');
    item.append(sample, el('span', null, label));
    key.appendChild(item);
  }

  parity.append(table, edgeHeading, edgeList, key);
  layout.append(map, parity);
  band.append(head, layout);
  mount.replaceChildren(band);

  /* ---- per-render fills ---- */

  function nodeCell(node) {
    const cell = doc.createElement('td');
    if (!node.indexed) {
      // Nothing was indexed at this path, so there is nothing to inspect.
      cell.appendChild(el('span', 'atlas-row-text', node.title));
      return cell;
    }
    const button = el('button', 'atlas-row-button', node.title);
    button.type = 'button';
    button.addEventListener('click', () => openInspector(node.artifact));
    cell.appendChild(button);
    return cell;
  }

  function nodeState(node) {
    if (!node.indexed) return 'not indexed — broken link';
    if (node.artifact?.attributes?.source_exists === false) return 'source file missing';
    if (node.artifact?.lifecycle_status) return node.artifact.lifecycle_status;
    return `${node.degree} link${node.degree === 1 ? '' : 's'}`;
  }

  function renderTable() {
    const rows = [];
    if (view.mode === 'overview') {
      for (const cluster of view.clusters) {
        const row = el('tr', 'atlas-cluster-row');
        row.dataset.row = 'cluster';
        row.dataset.cluster = cluster.id;
        const label = el('th', null, cluster.label);
        label.setAttribute('scope', 'row');
        const action = doc.createElement('td');
        if (cluster.count) {
          const button = el('button', 'atlas-row-button', `Open the ${cluster.label} neighborhood`);
          button.type = 'button';
          button.dataset.atlasFocus = `cluster:${cluster.id}`;
          button.addEventListener('click', () => {
            chosen.focus = `cluster:${cluster.id}`;
            render();
          });
          action.appendChild(button);
        } else {
          action.appendChild(el('span', 'atlas-row-text', 'No nodes in this projection'));
        }
        row.append(label, action, el('td', null, `${cluster.count} node${cluster.count === 1 ? '' : 's'}`));
        rows.push(row);

        for (const node of cluster.anchors) rows.push(nodeRow(node));
      }
    } else {
      for (const node of view.nodes) rows.push(nodeRow(node));
    }
    tbody.replaceChildren(...rows);
  }

  function nodeRow(node) {
    const row = el('tr', 'atlas-node-row');
    row.dataset.row = 'node';
    row.dataset.path = node.path;
    row.dataset.kind = node.kind;
    row.dataset.cluster = node.cluster;
    row.dataset.stale = String(node.stale);
    row.dataset.indexed = String(node.indexed);
    row.append(el('td', 'atlas-row-kind', node.kind), nodeCell(node), el('td', 'atlas-row-state', nodeState(node)));
    return row;
  }

  function renderEdges() {
    const titles = new Map(view.nodes.map((node) => [node.path, node.title]));
    const items = view.edges.map((edge) => {
      const item = doc.createElement('li');
      item.dataset.edge = edge.id;
      item.dataset.origin = edge.origin;
      const sample = el('span', edge.origin === 'derived' ? 'edge-sample derived' : 'edge-sample');
      sample.setAttribute('aria-hidden', 'true');
      const button = el(
        'button',
        'atlas-row-button',
        `${edge.kind} · ${edge.origin} · ${titles.get(edge.from) ?? edge.from} → ${titles.get(edge.to) ?? edge.to}`,
      );
      button.type = 'button';
      button.addEventListener('click', () => openInspector(edge));
      item.append(sample, button);
      return item;
    });
    if (!items.length) {
      const empty = doc.createElement('li');
      empty.className = 'atlas-empty';
      empty.appendChild(doc.createTextNode('No relationships in this projection.'));
      items.push(empty);
    }
    edgeList.replaceChildren(...items);
  }

  function renderScope() {
    const label = view.mode === 'overview'
      ? 'All clusters'
      : `Neighborhood · ${view.focus.startsWith('cluster:')
        ? view.clusters.find((cluster) => `cluster:${cluster.id}` === view.focus)?.label ?? view.focus
        : view.nodes[0]?.title ?? view.focus}`;
    // A truncated neighborhood says so in the count itself rather than claiming
    // the map is the whole of it.
    const nodes = view.truncated ? `${view.nodes.length} of ${view.total} nodes` : `${view.nodes.length} node${view.nodes.length === 1 ? '' : 's'}`;
    const edges = `${view.edges.length} relationship${view.edges.length === 1 ? '' : 's'}`;
    scope.textContent = `${label} · ${nodes} · ${edges}`;
    back.hidden = view.mode === 'overview';
  }

  function renderGraph() {
    if (!cy) {
      try {
        cy = cytoscape({ container: stage, style: graphStyle(canvasToken(doc, win)) });
      } catch {
        // No 2D canvas in this host (a test DOM). The graph still exists as a
        // model, so selection and parity stay identical; nothing is painted,
        // which is also why it needs no style.
        cy = cytoscape({ headless: true });
      }
      cy.on('tap', 'node', (event) => {
        const id = event.target.id();
        if (id.startsWith('cluster:')) {
          chosen.focus = id;
          render();
          return;
        }
        const node = view.nodes.find((candidate) => candidate.path === id);
        if (node?.artifact) openInspector(node.artifact);
      });
      cy.on('tap', 'edge', (event) => {
        const edge = view.edges.find((candidate) => candidate.id === event.target.id());
        if (edge) openInspector(edge);
      });
    }

    cy.elements().remove();
    cy.add(graphElements(view));
    cy.layout(view.mode === 'overview'
      ? { name: 'cose', animate: false, randomize: false, padding: 24 }
      : { name: 'concentric', animate: false, padding: 24, concentric: (node) => (node.hasClass('seed') ? 2 : 1) })
      .run();
  }

  function render() {
    view = projectGraph(getSnapshot(), chosen);
    // A focus that the current snapshot no longer resolves falls back to clusters.
    chosen.focus = view.focus;
    for (const button of filters.querySelectorAll('button')) {
      button.setAttribute('aria-pressed', String(button.dataset.projection === view.projection));
    }
    renderScope();
    renderTable();
    renderEdges();
    renderGraph();
  }

  doc.addEventListener('loam:render', render);
  render();

  return {
    render,
    projection: () => view,
    focus: (value) => {
      chosen.focus = value;
      render();
    },
    graph: () => cy,
  };
}
