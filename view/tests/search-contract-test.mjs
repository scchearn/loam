import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';

import { buildSearchIndex, DEFAULT_LIMIT, MAX_LIMIT, search } from '../server/search.mjs';

function sha256(content) {
  return createHash('sha256').update(content).digest('hex');
}

function artifact(path, { kind = 'topic', title = 'Untitled' } = {}) {
  return { id: path, path, kind, title };
}

async function writeWorkspaceFile(root, path, content) {
  await mkdir(join(root, path, '..'), { recursive: true });
  await writeFile(join(root, path), content, 'utf8');
}

async function makeWorkspace() {
  const root = await mkdtemp(join(tmpdir(), 'loam-view-search-'));
  return root;
}

const BASE_CAPABILITIES = {
  wiki: { state: 'ready', required: true, reason: null, evidence: null },
  code_graph: { state: 'absent', required: false, reason: null, evidence: null },
  goals: { state: 'absent', required: false, reason: null, evidence: null },
  work: { state: 'absent', required: false, reason: null, evidence: null },
  checkpoints: { state: 'absent', required: false, reason: null, evidence: null },
  git: { state: 'ready', required: false, reason: null, evidence: null },
  qmd: { state: 'ready', required: false, reason: null, evidence: null },
  search_corpus: { state: 'ready', required: true, reason: null, evidence: null },
};

function snapshotWith(artifacts, { qmd = 'ready' } = {}) {
  return {
    profile: 'loam-view',
    schema_version: 1,
    generated_at: '2026-08-19T00:00:00+00:00',
    status: 'ready',
    workspace: { root: '/workspace', name: 'workspace', platform: 'linux', git: { state: 'clean', branch: 'main', dirty: false, changed_count: 0 } },
    capabilities: { ...BASE_CAPABILITIES, qmd: { ...BASE_CAPABILITIES.qmd, state: qmd } },
    artifacts,
    relationships: [],
    events: [],
    metrics: {},
    signals: [],
    hints: [],
    probes: [],
  };
}

test('excludes wiki/SCHEMA.md from the search corpus', async () => {
  const root = await makeWorkspace();
  try {
    await writeWorkspaceFile(root, 'wiki/SCHEMA.md', '# Schema\n\nUniqueSchemaTerm lives only here.\n');
    await writeWorkspaceFile(root, 'wiki/topics/other.md', '# Other\n\nNothing special.\n');
    const snapshot = snapshotWith([
      artifact('wiki/SCHEMA.md', { kind: 'wiki-schema', title: 'Schema' }),
      artifact('wiki/topics/other.md', { title: 'Other' }),
    ]);
    const index = await buildSearchIndex(snapshot, { workspaceRoot: root });
    const results = search(index, { q: 'UniqueSchemaTerm' });
    assert.deepEqual(results, []);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test('identical results regardless of qmd readiness in the snapshot', async () => {
  const root = await makeWorkspace();
  try {
    await writeWorkspaceFile(root, 'wiki/topics/alpha.md', '# Alpha\n\nAlpha content about determinism.\n');
    await writeWorkspaceFile(root, 'wiki/topics/beta.md', '# Beta\n\nBeta content, unrelated.\n');
    const artifacts = [
      artifact('wiki/topics/alpha.md', { title: 'Alpha' }),
      artifact('wiki/topics/beta.md', { title: 'Beta' }),
    ];
    const states = ['ready', 'absent', 'degraded'];
    const runs = [];
    for (const qmd of states) {
      const index = await buildSearchIndex(snapshotWith(artifacts, { qmd }), { workspaceRoot: root });
      runs.push(search(index, { q: 'alpha' }));
    }
    assert.deepEqual(runs[0], runs[1]);
    assert.deepEqual(runs[1], runs[2]);
    assert.equal(runs[0].length, 1);
    assert.equal(runs[0][0].path, 'wiki/topics/alpha.md');
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test('ranks a title match above a body-only match for the same term', async () => {
  const root = await makeWorkspace();
  try {
    await writeWorkspaceFile(root, 'wiki/topics/lantern.md', '# Lantern\n\nA page about a lantern.\n');
    await writeWorkspaceFile(root, 'wiki/topics/mentions.md', '# Something Else\n\nThis page mentions lantern once in passing.\n');
    const snapshot = snapshotWith([
      artifact('wiki/topics/lantern.md', { title: 'Lantern' }),
      artifact('wiki/topics/mentions.md', { title: 'Something Else' }),
    ]);
    const index = await buildSearchIndex(snapshot, { workspaceRoot: root });
    const results = search(index, { q: 'lantern' });
    assert.equal(results.length, 2);
    assert.equal(results[0].path, 'wiki/topics/lantern.md');
    assert.equal(results[1].path, 'wiki/topics/mentions.md');
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test('combines multi-term queries with AND', async () => {
  const root = await makeWorkspace();
  try {
    await writeWorkspaceFile(root, 'wiki/topics/both.md', '# Both\n\nThis page has zebra and giraffe together.\n');
    await writeWorkspaceFile(root, 'wiki/topics/zebra-only.md', '# Zebra Only\n\nJust a zebra here.\n');
    const snapshot = snapshotWith([
      artifact('wiki/topics/both.md', { title: 'Both' }),
      artifact('wiki/topics/zebra-only.md', { title: 'Zebra Only' }),
    ]);
    const index = await buildSearchIndex(snapshot, { workspaceRoot: root });
    const results = search(index, { q: 'zebra giraffe' });
    assert.equal(results.length, 1);
    assert.equal(results[0].path, 'wiki/topics/both.md');
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test('supports prefix matching on the first pass', async () => {
  const root = await makeWorkspace();
  try {
    await writeWorkspaceFile(root, 'wiki/topics/documentation.md', '# Documentation\n\nAll about documentation practices.\n');
    const snapshot = snapshotWith([artifact('wiki/topics/documentation.md', { title: 'Documentation' })]);
    const index = await buildSearchIndex(snapshot, { workspaceRoot: root });
    const results = search(index, { q: 'docu' });
    assert.equal(results.length, 1);
    assert.equal(results[0].path, 'wiki/topics/documentation.md');
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test('falls back to a fuzzy pass only when the first pass returns fewer than five results, first-pass results ranked first', async () => {
  const root = await makeWorkspace();
  try {
    await writeWorkspaceFile(root, 'wiki/topics/exact.md', '# Exact\n\nThis page says kangaroo clearly.\n');
    await writeWorkspaceFile(root, 'wiki/topics/typo.md', '# Typo\n\nThis page says kangaroe, a typo.\n');
    const snapshot = snapshotWith([
      artifact('wiki/topics/exact.md', { title: 'Exact' }),
      artifact('wiki/topics/typo.md', { title: 'Typo' }),
    ]);
    const index = await buildSearchIndex(snapshot, { workspaceRoot: root });
    const results = search(index, { q: 'kangaroo' });
    assert.equal(results.length, 2);
    assert.equal(results[0].path, 'wiki/topics/exact.md');
    assert.equal(results[1].path, 'wiki/topics/typo.md');
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test('never returns duplicate paths when a document matches both passes', async () => {
  const root = await makeWorkspace();
  try {
    await writeWorkspaceFile(root, 'wiki/topics/only.md', '# Only\n\nSolely about kestrel.\n');
    const snapshot = snapshotWith([artifact('wiki/topics/only.md', { title: 'Only' })]);
    const index = await buildSearchIndex(snapshot, { workspaceRoot: root });
    const results = search(index, { q: 'kestrel' });
    const paths = results.map((r) => r.path);
    assert.deepEqual(paths, [...new Set(paths)]);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test('filters by kind, applied before the limit', async () => {
  const root = await makeWorkspace();
  try {
    await writeWorkspaceFile(root, 'goals/quest.md', '# Quest\n\nA quest goal about dragons.\n');
    await writeWorkspaceFile(root, 'wiki/topics/dragons.md', '# Dragons\n\nA wiki topic about dragons.\n');
    const snapshot = snapshotWith([
      artifact('goals/quest.md', { kind: 'goal', title: 'Quest' }),
      artifact('wiki/topics/dragons.md', { kind: 'topic', title: 'Dragons' }),
    ]);
    const index = await buildSearchIndex(snapshot, { workspaceRoot: root });
    const all = search(index, { q: 'dragons' });
    assert.equal(all.length, 2);
    const filtered = search(index, { q: 'dragons', kind: 'goal' });
    assert.equal(filtered.length, 1);
    assert.equal(filtered[0].path, 'goals/quest.md');
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test('rejects a query shorter than 2 characters', async () => {
  const root = await makeWorkspace();
  try {
    const snapshot = snapshotWith([]);
    const index = await buildSearchIndex(snapshot, { workspaceRoot: root });
    assert.throws(() => search(index, { q: 'a' }), (error) => error.status === 400);
    assert.throws(() => search(index, { q: '' }), (error) => error.status === 400);
    assert.throws(() => search(index, { q: undefined }), (error) => error.status === 400);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test('defaults to a limit of 20 and never exceeds the maximum of 50', async () => {
  const root = await makeWorkspace();
  try {
    const artifacts = [];
    for (let i = 0; i < 60; i += 1) {
      const path = `wiki/topics/bulk-${String(i).padStart(2, '0')}.md`;
      await writeWorkspaceFile(root, path, `# Bulk ${i}\n\nEvery one of these mentions kraken.\n`);
      artifacts.push(artifact(path, { title: `Bulk ${i}` }));
    }
    const index = await buildSearchIndex(snapshotWith(artifacts), { workspaceRoot: root });
    assert.equal(search(index, { q: 'kraken' }).length, DEFAULT_LIMIT);
    assert.equal(search(index, { q: 'kraken', limit: 1000 }).length, MAX_LIMIT);
    assert.equal(search(index, { q: 'kraken', limit: 5 }).length, 5);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test('snippet is at most 180 characters, centered on the first match, plain text with no escaping', async () => {
  const root = await makeWorkspace();
  try {
    const filler = 'padding text '.repeat(20);
    await writeWorkspaceFile(
      root,
      'wiki/topics/snippet.md',
      `# Snippet\n\n${filler}narwhal <script>alert(1)</script> & more Q&A text ${filler}\n`,
    );
    const snapshot = snapshotWith([artifact('wiki/topics/snippet.md', { title: 'Snippet' })]);
    const index = await buildSearchIndex(snapshot, { workspaceRoot: root });
    const [result] = search(index, { q: 'narwhal' });
    assert.ok(result.snippet.length <= 180);
    assert.match(result.snippet, /narwhal/);
    assert.match(result.snippet, /<script>alert\(1\)<\/script>/);
    assert.match(result.snippet, /&/);
    assert.doesNotMatch(result.snippet, /&lt;|&amp;|&gt;/);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test('falls back to opening body text when the query only matches outside the body', async () => {
  const root = await makeWorkspace();
  try {
    await writeWorkspaceFile(root, 'wiki/topics/octopus.md', '# Octopus\n\nThis page opens with an unrelated sentence.\n');
    const snapshot = snapshotWith([artifact('wiki/topics/octopus.md', { title: 'Octopus' })]);
    const index = await buildSearchIndex(snapshot, { workspaceRoot: root });
    const [result] = search(index, { q: 'octopus' });
    assert.match(result.snippet, /This page opens with an unrelated sentence/);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});
