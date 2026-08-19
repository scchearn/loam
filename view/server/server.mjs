import { createHash } from 'node:crypto';
import { readFile, stat } from 'node:fs/promises';
import { createServer as createHttpServer } from 'node:http';
import { extname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

import { assertInside } from '../../integration/paths.mjs';
import { safeDetail } from '../../integration/runtime.mjs';
import { validateSnapshot } from './validate-snapshot.mjs';

const CSP = [
  "default-src 'self'",
  "script-src 'self'",
  "style-src 'self'",
  "img-src 'self'",
  "font-src 'self'",
  "connect-src 'self'",
  "object-src 'none'",
  "frame-src 'none'",
  "base-uri 'none'",
  "form-action 'none'",
  "frame-ancestors 'none'",
  "worker-src 'none'",
].join('; ');

const SECURITY_HEADERS = {
  'Content-Security-Policy': CSP,
  'X-Content-Type-Options': 'nosniff',
  'Referrer-Policy': 'no-referrer',
  'Cross-Origin-Resource-Policy': 'same-origin',
};

const LOOPBACK_HOSTNAMES = new Set(['127.0.0.1', 'localhost', '::1', '[::1]']);

const CONTENT_TYPES = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.mjs': 'text/javascript; charset=utf-8',
  '.css': 'text/css; charset=utf-8',
  '.json': 'application/json; charset=utf-8',
  '.svg': 'image/svg+xml',
  '.woff2': 'font/woff2',
  '.map': 'application/json; charset=utf-8',
};

function isLoopbackHost(hostHeader) {
  if (!hostHeader) return false;
  try {
    return LOOPBACK_HOSTNAMES.has(new URL(`http://${hostHeader}`).hostname.toLowerCase());
  } catch {
    return false;
  }
}

function sendJson(res, status, body) {
  if (res.writableEnded) return;
  const payload = Buffer.from(JSON.stringify(body));
  res.writeHead(status, {
    ...SECURITY_HEADERS,
    'Content-Type': 'application/json; charset=utf-8',
    'Cache-Control': 'no-store',
    'Content-Length': payload.length,
  });
  res.end(payload);
}

function sendEmpty(res, status) {
  if (res.writableEnded) return;
  res.writeHead(status, { ...SECURITY_HEADERS, 'Cache-Control': 'no-store' });
  res.end();
}

// ponytail: any thrown error not tagged with a 500-504 status collapses to a
// generic 500 rather than leaking arbitrary status codes to the client.
function errorStatus(error) {
  const status = Number(error?.status);
  return Number.isInteger(status) && status >= 500 && status <= 504 ? status : 500;
}

/**
 * Local read-only Loam View HTTP server per the spec's Runtime API and
 * failure contract. Binds nowhere by itself — callers `.listen(0, '127.0.0.1')`.
 */
export function createServer({
  workspaceRoot,
  publicRoot = fileURLToPath(new URL('../public/', import.meta.url)),
  initialSnapshot,
  refreshProducer,
  buildSearchIndex = async () => null,
  stderr = process.stderr,
} = {}) {
  if (!workspaceRoot) throw new Error('createServer requires workspaceRoot');
  const initialCheck = validateSnapshot(initialSnapshot);
  if (!initialCheck.valid) {
    throw new Error(`initial snapshot failed schema validation: ${initialCheck.errors.join('; ')}`);
  }

  const state = { snapshot: initialSnapshot, searchIndex: null };
  let refreshInFlight = null;

  function findArtifact(path) {
    return state.snapshot.artifacts.find((entry) => entry.path === path);
  }

  async function handleSnapshot(req, res) {
    sendJson(res, 200, state.snapshot);
  }

  async function handleRefresh(req, res) {
    if (refreshInFlight) {
      sendJson(res, 409, { error: 'refresh_in_progress' });
      return;
    }
    refreshInFlight = (async () => {
      let raw;
      try {
        raw = await refreshProducer({ workspaceRoot });
      } catch (error) {
        sendJson(res, errorStatus(error), { error: 'refresh_failed', message: safeDetail(error?.message ?? error) });
        return;
      }
      const check = validateSnapshot(raw);
      if (!check.valid) {
        sendJson(res, 500, { error: 'refresh_invalid_schema', message: safeDetail(check.errors.join('; ')) });
        return;
      }
      let index;
      try {
        index = await buildSearchIndex(raw);
      } catch (error) {
        sendJson(res, errorStatus(error), { error: 'refresh_index_failed', message: safeDetail(error?.message ?? error) });
        return;
      }
      // Swap snapshot and search index together: neither is visible until both succeeded.
      state.snapshot = raw;
      state.searchIndex = index;
      sendEmpty(res, 204);
    })();
    try {
      await refreshInFlight;
    } finally {
      refreshInFlight = null;
    }
  }

  async function handleDocument(req, res, url) {
    const path = url.searchParams.get('path');
    if (!path) return sendJson(res, 400, { error: 'missing_path' });
    const artifact = findArtifact(path);
    if (!artifact) return sendJson(res, 400, { error: 'not_inventoried' });

    let absolute;
    try {
      absolute = assertInside(workspaceRoot, join(workspaceRoot, path), 'document path');
    } catch {
      return sendJson(res, 400, { error: 'outside_root' });
    }

    let content;
    try {
      content = await readFile(absolute);
    } catch (error) {
      if (error?.code === 'ENOENT') return sendJson(res, 404, { error: 'not_found' });
      if (error?.code === 'EACCES' || error?.code === 'EPERM') return sendJson(res, 403, { error: 'forbidden' });
      return sendJson(res, 500, { error: 'read_failed', message: safeDetail(error?.message ?? error) });
    }

    const contentHash = createHash('sha256').update(content).digest('hex');
    sendJson(res, 200, {
      path,
      content: content.toString('utf8'),
      content_hash: contentHash,
      snapshot_hash: artifact.content_hash,
      changed_since_snapshot: contentHash !== artifact.content_hash,
    });
  }

  async function handleSearch(req, res) {
    // ponytail: real Search index ships in T10; until then every query is
    // honestly unavailable rather than faked.
    sendJson(res, 503, { error: 'search_unavailable' });
  }

  async function handleStatic(req, res, pathname) {
    const relative = pathname === '/' ? '/index.html' : pathname;
    let target;
    try {
      target = assertInside(publicRoot, join(publicRoot, decodeURIComponent(relative)), 'static path');
    } catch {
      return sendJson(res, 400, { error: 'invalid_path' });
    }

    let info;
    try {
      info = await stat(target);
    } catch {
      return sendJson(res, 404, { error: 'not_found' });
    }
    if (!info.isFile()) return sendJson(res, 404, { error: 'not_found' });

    const body = await readFile(target);
    const isIndex = relative === '/index.html';
    res.writeHead(200, {
      ...SECURITY_HEADERS,
      'Content-Type': CONTENT_TYPES[extname(target)] || 'application/octet-stream',
      'Cache-Control': isIndex ? 'no-store' : 'public, max-age=31536000, immutable',
      'Content-Length': body.length,
    });
    res.end(body);
  }

  const server = createHttpServer((req, res) => {
    const startedAt = Date.now();
    const url = new URL(req.url, 'http://internal');
    res.once('finish', () => {
      // Route, status, and duration only — never document bodies or search queries.
      stderr.write(`${req.method} ${url.pathname} ${res.statusCode} ${Date.now() - startedAt}ms\n`);
    });

    if (!isLoopbackHost(req.headers.host)) {
      sendJson(res, 400, { error: 'invalid_host' });
      return;
    }

    Promise.resolve()
      .then(() => {
        if (url.pathname === '/api/snapshot' && req.method === 'GET') return handleSnapshot(req, res);
        if (url.pathname === '/api/refresh' && req.method === 'POST') return handleRefresh(req, res);
        if (url.pathname === '/api/document' && req.method === 'GET') return handleDocument(req, res, url);
        if (url.pathname === '/api/search' && req.method === 'GET') return handleSearch(req, res);
        if (url.pathname.startsWith('/api/')) return sendJson(res, 404, { error: 'not_found' });
        if (req.method === 'GET' || req.method === 'HEAD') return handleStatic(req, res, url.pathname);
        return sendJson(res, 404, { error: 'not_found' });
      })
      .catch((error) => {
        sendJson(res, 500, { error: 'internal_error', message: safeDetail(error?.message ?? error) });
      });
  });

  return server;
}
