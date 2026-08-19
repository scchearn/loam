import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { request as httpRequest } from 'node:http';
import { connect } from 'node:net';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';

import { createServer } from '../server/server.mjs';

const BASE_CAPABILITIES = {
  wiki: { state: 'ready', required: true, reason: null, evidence: null },
  code_graph: { state: 'absent', required: false, reason: null, evidence: null },
  goals: { state: 'absent', required: false, reason: null, evidence: null },
  work: { state: 'absent', required: false, reason: null, evidence: null },
  checkpoints: { state: 'absent', required: false, reason: null, evidence: null },
  git: { state: 'ready', required: false, reason: null, evidence: null },
  qmd: { state: 'absent', required: false, reason: null, evidence: null },
  search_corpus: { state: 'ready', required: true, reason: null, evidence: null },
};

function baseSnapshot(root, artifacts = []) {
  return {
    profile: 'loam-view',
    schema_version: 1,
    generated_at: '2026-08-19T00:00:00+00:00',
    status: 'ready',
    posture: 'healthy',
    workspace: { root, name: 'workspace', platform: 'linux', git: { state: 'clean', branch: 'main', dirty: false, changed_count: 0 } },
    capabilities: BASE_CAPABILITIES,
    artifacts,
    relationships: [],
    events: [],
    metrics: {},
    signals: [],
    hints: [],
    probes: [],
  };
}

function sha256(content) {
  return createHash('sha256').update(content).digest('hex');
}

function artifactFor(path, content) {
  return {
    id: path,
    path,
    kind: 'wiki-index',
    title: 'Index',
    lifecycle_status: null,
    created_at: null,
    updated_at: null,
    captured_at: null,
    content_hash: sha256(content),
    bytes: Buffer.byteLength(content),
    attributes: {},
    parse_errors: [],
  };
}

async function makeWorkspace() {
  const root = await mkdtemp(join(tmpdir(), 'loam-view-server-'));
  await mkdir(join(root, 'wiki'), { recursive: true });
  const content = '# Index\n\nHello.\n';
  await writeFile(join(root, 'wiki', 'index.md'), content, 'utf8');
  return { root, content, path: 'wiki/index.md' };
}

function rawRequest(baseUrl, path, { method = 'GET', headers = {}, body } = {}) {
  return new Promise((resolvePromise, rejectPromise) => {
    const url = new URL(path, baseUrl);
    const req = httpRequest(url, { method, headers }, (res) => {
      const chunks = [];
      res.on('data', (chunk) => chunks.push(chunk));
      res.on('end', () => resolvePromise({
        status: res.statusCode,
        headers: res.headers,
        body: Buffer.concat(chunks).toString('utf8'),
      }));
    });
    req.on('error', rejectPromise);
    if (body !== undefined) req.write(body);
    req.end();
  });
}

/**
 * A literal request line, written straight to the socket.
 *
 * `rawRequest` builds its target with `new URL()`, which normalises `..` and
 * percent-encoded separators away before anything is sent — so the traversal
 * cases below would never reach the server through it. This sends exactly the
 * bytes given. (Technique from the T13 independent review.)
 */
function socketGet(baseUrl, requestTarget, host) {
  const { port, host: defaultHost } = new URL(baseUrl);
  return new Promise((resolvePromise) => {
    const socket = connect(Number(port), '127.0.0.1', () => {
      socket.write(`GET ${requestTarget} HTTP/1.1\r\nHost: ${host ?? defaultHost}\r\nConnection: close\r\n\r\n`);
    });
    const chunks = [];
    socket.on('data', (chunk) => chunks.push(chunk));
    socket.on('end', () => {
      const raw = Buffer.concat(chunks).toString('utf8');
      const [head, ...rest] = raw.split('\r\n\r\n');
      resolvePromise({ status: Number(head.split(' ')[1]), body: rest.join('\r\n\r\n') });
    });
    socket.on('error', () => resolvePromise({ status: 0, body: '' }));
  });
}

async function rawJson(baseUrl, path, options) {
  const res = await rawRequest(baseUrl, path, options);
  return { ...res, json: res.body ? JSON.parse(res.body) : undefined };
}

async function startServer(options) {
  const server = createServer(options);
  await new Promise((resolvePromise, rejectPromise) => {
    server.once('error', rejectPromise);
    server.listen(0, '127.0.0.1', resolvePromise);
  });
  const { port } = server.address();
  return { server, baseUrl: `http://127.0.0.1:${port}` };
}

async function stopServer(server) {
  await new Promise((resolvePromise) => server.close(resolvePromise));
}

async function withServer(options, fn) {
  const { server, baseUrl } = await startServer(options);
  try {
    await fn(baseUrl);
  } finally {
    await stopServer(server);
  }
}

test('createServer rejects an initial snapshot that fails schema validation', async () => {
  const { root } = await makeWorkspace();
  try {
    assert.throws(() => createServer({ workspaceRoot: root, initialSnapshot: { not: 'valid' } }));
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test('rejects a non-loopback Host header and accepts a loopback one', async () => {
  const { root } = await makeWorkspace();
  await withServer({ workspaceRoot: root, initialSnapshot: baseSnapshot(root) }, async (baseUrl) => {
    const bad = await rawJson(baseUrl, '/api/snapshot', { headers: { Host: 'evil.example.com' } });
    assert.equal(bad.status, 400);

    const port = new URL(baseUrl).port;
    const good = await rawJson(baseUrl, '/api/snapshot', { headers: { Host: `127.0.0.1:${port}` } });
    assert.equal(good.status, 200);
  });
  await rm(root, { recursive: true, force: true });
});

test('sends the exact CSP and security headers on API responses with no CORS and no-store', async () => {
  const { root } = await makeWorkspace();
  await withServer({ workspaceRoot: root, initialSnapshot: baseSnapshot(root) }, async (baseUrl) => {
    const res = await rawRequest(baseUrl, '/api/snapshot');
    assert.equal(res.status, 200);
    assert.equal(
      res.headers['content-security-policy'],
      "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self'; font-src 'self'; connect-src 'self'; object-src 'none'; frame-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'; worker-src 'none'",
    );
    assert.equal(res.headers['x-content-type-options'], 'nosniff');
    assert.equal(res.headers['referrer-policy'], 'no-referrer');
    assert.equal(res.headers['cross-origin-resource-policy'], 'same-origin');
    assert.equal(res.headers['cache-control'], 'no-store');
    assert.equal(res.headers['access-control-allow-origin'], undefined);
  });
  await rm(root, { recursive: true, force: true });
});

test('GET /api/snapshot returns the current snapshot', async () => {
  const { root } = await makeWorkspace();
  const snapshot = baseSnapshot(root);
  await withServer({ workspaceRoot: root, initialSnapshot: snapshot }, async (baseUrl) => {
    const res = await rawJson(baseUrl, '/api/snapshot');
    assert.equal(res.status, 200);
    assert.deepEqual(res.json, snapshot);
  });
  await rm(root, { recursive: true, force: true });
});

test('GET /api/search returns 503 when the search index is unavailable', async () => {
  const { root } = await makeWorkspace();
  await withServer({
    workspaceRoot: root,
    initialSnapshot: baseSnapshot(root),
    buildSearchIndex: async () => { throw new Error('index build exploded'); },
  }, async (baseUrl) => {
    const res = await rawJson(baseUrl, '/api/search?q=hello');
    assert.equal(res.status, 503);
  });
  await rm(root, { recursive: true, force: true });
});

test('GET /api/search returns deterministic results from the default index built at startup', async () => {
  const { root, path, content } = await makeWorkspace();
  const snapshot = baseSnapshot(root, [artifactFor(path, content)]);
  await withServer({ workspaceRoot: root, initialSnapshot: snapshot }, async (baseUrl) => {
    const res = await rawJson(baseUrl, '/api/search?q=hello');
    assert.equal(res.status, 200);
    assert.equal(res.json.results.length, 1);
    assert.equal(res.json.results[0].path, path);

    const short = await rawJson(baseUrl, '/api/search?q=h');
    assert.equal(short.status, 400);
  });
  await rm(root, { recursive: true, force: true });
});

test('unknown routes return 404', async () => {
  const { root } = await makeWorkspace();
  await withServer({ workspaceRoot: root, initialSnapshot: baseSnapshot(root) }, async (baseUrl) => {
    const res = await rawJson(baseUrl, '/nope');
    assert.equal(res.status, 404);
    const apiRes = await rawJson(baseUrl, '/api/nope');
    assert.equal(apiRes.status, 404);
  });
  await rm(root, { recursive: true, force: true });
});

test('GET /api/document reads an inventoried artifact fresh from disk', async () => {
  const { root, content, path } = await makeWorkspace();
  const snapshot = baseSnapshot(root, [artifactFor(path, content)]);
  await withServer({ workspaceRoot: root, initialSnapshot: snapshot }, async (baseUrl) => {
    const res = await rawJson(baseUrl, `/api/document?path=${encodeURIComponent(path)}`);
    assert.equal(res.status, 200);
    assert.equal(res.json.path, path);
    assert.equal(res.json.content, content);
    assert.equal(res.json.content_hash, sha256(content));
    assert.equal(res.json.snapshot_hash, sha256(content));
    assert.equal(res.json.changed_since_snapshot, false);

    // Editing the file after the snapshot must surface as changed, from a fresh read.
    const changed = `${content}more\n`;
    await writeFile(join(root, path), changed, 'utf8');
    const res2 = await rawJson(baseUrl, `/api/document?path=${encodeURIComponent(path)}`);
    assert.equal(res2.status, 200);
    assert.equal(res2.json.content, changed);
    assert.equal(res2.json.content_hash, sha256(changed));
    assert.equal(res2.json.snapshot_hash, sha256(content));
    assert.equal(res2.json.changed_since_snapshot, true);
  });
  await rm(root, { recursive: true, force: true });
});

test('GET /api/document requires a path', async () => {
  const { root } = await makeWorkspace();
  await withServer({ workspaceRoot: root, initialSnapshot: baseSnapshot(root) }, async (baseUrl) => {
    const res = await rawJson(baseUrl, '/api/document');
    assert.equal(res.status, 400);
  });
  await rm(root, { recursive: true, force: true });
});

test('GET /api/document 400s for a path outside the artifact inventory', async () => {
  const { root, path } = await makeWorkspace();
  const snapshot = baseSnapshot(root, [artifactFor(path, '# Index\n\nHello.\n')]);
  await withServer({ workspaceRoot: root, initialSnapshot: snapshot }, async (baseUrl) => {
    const res = await rawJson(baseUrl, '/api/document?path=wiki/not-inventoried.md');
    assert.equal(res.status, 400);
  });
  await rm(root, { recursive: true, force: true });
});

test('GET /api/document 400s an inventoried path that escapes the workspace root', async () => {
  const { root, path, content } = await makeWorkspace();
  const escapee = artifactFor('../outside.md', 'secret');
  const snapshot = baseSnapshot(root, [artifactFor(path, content), escapee]);
  await withServer({ workspaceRoot: root, initialSnapshot: snapshot }, async (baseUrl) => {
    const res = await rawJson(baseUrl, '/api/document?path=../outside.md');
    assert.equal(res.status, 400);
  });
  await rm(root, { recursive: true, force: true });
});

test('GET /api/document 404s an inventoried artifact whose file is missing on disk', async () => {
  const { root, path, content } = await makeWorkspace();
  const missing = artifactFor('wiki/missing.md', 'gone');
  const snapshot = baseSnapshot(root, [artifactFor(path, content), missing]);
  await withServer({ workspaceRoot: root, initialSnapshot: snapshot }, async (baseUrl) => {
    const res = await rawJson(baseUrl, '/api/document?path=wiki/missing.md');
    assert.equal(res.status, 404);
  });
  await rm(root, { recursive: true, force: true });
});

test('POST /api/refresh atomically swaps snapshot and search index on success', async () => {
  const { root, path, content } = await makeWorkspace();
  const initial = baseSnapshot(root, [artifactFor(path, content)]);
  const refreshed = baseSnapshot(root, [artifactFor(path, content), artifactFor('wiki/new.md', 'new')]);
  let indexed;
  await withServer({
    workspaceRoot: root,
    initialSnapshot: initial,
    refreshProducer: async () => refreshed,
    buildSearchIndex: async (snapshot) => { indexed = snapshot; return { size: snapshot.artifacts.length }; },
  }, async (baseUrl) => {
    const res = await rawJson(baseUrl, '/api/refresh', { method: 'POST' });
    assert.equal(res.status, 204);
    const after = await rawJson(baseUrl, '/api/snapshot');
    assert.deepEqual(after.json, refreshed);
    assert.deepEqual(indexed, refreshed);
  });
  await rm(root, { recursive: true, force: true });
});

test('POST /api/refresh rejects concurrent refreshes with 409 and only runs the producer once', async () => {
  const { root } = await makeWorkspace();
  const initial = baseSnapshot(root);
  let calls = 0;
  let releaseGate;
  const gate = new Promise((resolvePromise) => { releaseGate = resolvePromise; });
  let started;
  const startedPromise = new Promise((resolvePromise) => { started = resolvePromise; });
  await withServer({
    workspaceRoot: root,
    initialSnapshot: initial,
    refreshProducer: async () => {
      calls += 1;
      started();
      await gate;
      return initial;
    },
  }, async (baseUrl) => {
    const first = rawJson(baseUrl, '/api/refresh', { method: 'POST' });
    await startedPromise;
    const second = await rawJson(baseUrl, '/api/refresh', { method: 'POST' });
    assert.equal(second.status, 409);
    releaseGate();
    const firstResult = await first;
    assert.equal(firstResult.status, 204);
    assert.equal(calls, 1);
  });
  await rm(root, { recursive: true, force: true });
});

test('POST /api/refresh retains the prior snapshot atomically when the producer fails', async () => {
  const { root } = await makeWorkspace();
  const initial = baseSnapshot(root);
  await withServer({
    workspaceRoot: root,
    initialSnapshot: initial,
    refreshProducer: async () => { throw new Error('producer exploded'); },
  }, async (baseUrl) => {
    const res = await rawJson(baseUrl, '/api/refresh', { method: 'POST' });
    assert.ok(res.status >= 500 && res.status <= 504, `expected 500-504, got ${res.status}`);
    assert.ok(res.json?.error);
    const after = await rawJson(baseUrl, '/api/snapshot');
    assert.deepEqual(after.json, initial);
  });
  await rm(root, { recursive: true, force: true });
});

test('POST /api/refresh retains the prior snapshot when the producer output fails schema validation', async () => {
  const { root } = await makeWorkspace();
  const initial = baseSnapshot(root);
  await withServer({
    workspaceRoot: root,
    initialSnapshot: initial,
    refreshProducer: async () => ({ not: 'a valid snapshot' }),
  }, async (baseUrl) => {
    const res = await rawJson(baseUrl, '/api/refresh', { method: 'POST' });
    assert.ok(res.status >= 500 && res.status <= 504, `expected 500-504, got ${res.status}`);
    const after = await rawJson(baseUrl, '/api/snapshot');
    assert.deepEqual(after.json, initial);
  });
  await rm(root, { recursive: true, force: true });
});

test('POST /api/refresh retains the prior state when the search index build fails', async () => {
  const { root } = await makeWorkspace();
  const initial = baseSnapshot(root);
  const refreshed = baseSnapshot(root, [artifactFor('wiki/new.md', 'new')]);
  await withServer({
    workspaceRoot: root,
    initialSnapshot: initial,
    refreshProducer: async () => refreshed,
    buildSearchIndex: async () => { throw new Error('index exploded'); },
  }, async (baseUrl) => {
    const res = await rawJson(baseUrl, '/api/refresh', { method: 'POST' });
    assert.ok(res.status >= 500 && res.status <= 504, `expected 500-504, got ${res.status}`);
    const after = await rawJson(baseUrl, '/api/snapshot');
    assert.deepEqual(after.json, initial);
  });
  await rm(root, { recursive: true, force: true });
});

test('serves static files with immutable caching, and index.html with no-store', async () => {
  const { root } = await makeWorkspace();
  const publicRoot = await mkdtemp(join(tmpdir(), 'loam-view-public-'));
  await writeFile(join(publicRoot, 'index.html'), '<!doctype html><title>Loam View</title>', 'utf8');
  await mkdir(join(publicRoot, 'assets'), { recursive: true });
  await writeFile(join(publicRoot, 'assets', 'app.js'), 'export const x = 1;\n', 'utf8');

  await withServer({ workspaceRoot: root, publicRoot, initialSnapshot: baseSnapshot(root) }, async (baseUrl) => {
    const index = await rawRequest(baseUrl, '/');
    assert.equal(index.status, 200);
    assert.equal(index.headers['cache-control'], 'no-store');
    assert.match(index.body, /Loam View/);

    const asset = await rawRequest(baseUrl, '/assets/app.js');
    assert.equal(asset.status, 200);
    assert.equal(asset.headers['cache-control'], 'public, max-age=31536000, immutable');

    const missing = await rawRequest(baseUrl, '/assets/does-not-exist.js');
    assert.equal(missing.status, 404);

    const traversal = await rawRequest(baseUrl, '/../server.mjs');
    assert.ok([400, 404].includes(traversal.status));
  });
  await rm(root, { recursive: true, force: true });
  await rm(publicRoot, { recursive: true, force: true });
});

test('serves the pinned vendor modules under /vendor/ as their own static root', async () => {
  const { root } = await makeWorkspace();
  const publicRoot = await mkdtemp(join(tmpdir(), 'loam-view-public-'));
  const vendorRoot = await mkdtemp(join(tmpdir(), 'loam-view-vendor-'));
  await mkdir(join(vendorRoot, 'cytoscape'), { recursive: true });
  await writeFile(join(vendorRoot, 'cytoscape', 'cytoscape.esm.min.mjs'), 'export default 1;\n', 'utf8');
  await writeFile(join(publicRoot, 'index.html'), '<!doctype html><title>Loam View</title>', 'utf8');

  await withServer(
    { workspaceRoot: root, publicRoot, vendorRoot, initialSnapshot: baseSnapshot(root) },
    async (baseUrl) => {
      // Atlas imports this exact URL; without the route the view never boots.
      const module = await rawRequest(baseUrl, '/vendor/cytoscape/cytoscape.esm.min.mjs');
      assert.equal(module.status, 200);
      assert.equal(module.headers['content-type'], 'text/javascript; charset=utf-8');
      assert.match(module.body, /export default/);

      // The vendor prefix is its own root: a public/ file is not reachable
      // through it, and a missing module is a 404 rather than a fallback.
      const wrongRoot = await rawRequest(baseUrl, '/vendor/index.html');
      assert.equal(wrongRoot.status, 404);
      const missing = await rawRequest(baseUrl, '/vendor/cytoscape/nope.mjs');
      assert.equal(missing.status, 404);
    },
  );
  await rm(root, { recursive: true, force: true });
  await rm(publicRoot, { recursive: true, force: true });
  await rm(vendorRoot, { recursive: true, force: true });
});

test('serves the pinned vendor builds from the vendor root, not from public', async () => {
  const { root } = await makeWorkspace();
  await withServer({ workspaceRoot: root, initialSnapshot: baseSnapshot(root) }, async (baseUrl) => {
    const purify = await rawRequest(baseUrl, '/vendor/dompurify/purify.es.mjs');
    assert.equal(purify.status, 200);
    assert.equal(purify.headers['content-type'], 'text/javascript; charset=utf-8');
    assert.match(purify.body, /DOMPurify/);

    const traversal = await rawRequest(baseUrl, '/vendor/../server/server.mjs');
    assert.ok([400, 404].includes(traversal.status));
  });
  await rm(root, { recursive: true, force: true });
});

test('static and document routes refuse traversal that never passes through URL normalisation', async () => {
  const { root, path, content } = await makeWorkspace();
  const snapshot = baseSnapshot(root, [artifactFor(path, content)]);
  await withServer({ workspaceRoot: root, initialSnapshot: snapshot }, async (baseUrl) => {
    for (const target of [
      '/vendor/../server/server.mjs',
      '/vendor/..%2f..%2fserver%2fserver.mjs',
      '/vendor/%2e%2e/%2e%2e/server/server.mjs',
      '/vendor/..\\..\\server\\server.mjs',
      '/%2e%2e/%2e%2e/etc/passwd',
      '/../server/server.mjs',
    ]) {
      const res = await socketGet(baseUrl, target);
      assert.ok([400, 404].includes(res.status), `${target} answered ${res.status}`);
      assert.ok(!res.body.includes('createServer'), `${target} leaked server source`);
    }

    const vendored = await socketGet(baseUrl, '/vendor/dompurify/purify.es.mjs');
    assert.equal(vendored.status, 200, 'the vendor route still serves its own root');

    for (const query of [
      'path=../../etc/passwd',
      'path=%2e%2e%2f%2e%2e%2fetc%2fpasswd',
      'path=/etc/passwd',
      'path=wiki/../wiki/index.md',
    ]) {
      const res = await socketGet(baseUrl, `/api/document?${query}`);
      assert.equal(res.status, 400, `${query} answered ${res.status}`);
      assert.match(res.body, /not_inventoried|outside_root/);
    }

    const inventoried = await socketGet(baseUrl, `/api/document?path=${path}`);
    assert.equal(inventoried.status, 200);

    for (const host of ['evil.example.com', '127.0.0.1 evil.example.com', '127.0.0.1@evil.example.com']) {
      const res = await socketGet(baseUrl, '/api/snapshot', host);
      assert.equal(res.status, 400, `Host: ${host} was accepted`);
    }
  });
  await rm(root, { recursive: true, force: true });
});
