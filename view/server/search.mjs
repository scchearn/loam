import { readFile } from 'node:fs/promises';
import { join } from 'node:path';

import { assertInside } from '../../integration/paths.mjs';
import MiniSearch from '../vendor/minisearch/index.js';

export const MIN_QUERY_LENGTH = 2;
export const DEFAULT_LIMIT = 20;
export const MAX_LIMIT = 50;

const SNIPPET_LENGTH = 180;
const FUZZY_FLOOR = 5;
const EXCLUDED_PATHS = new Set(['wiki/SCHEMA.md']);

function frontmatterBlock(raw) {
  if (!raw.startsWith('---\n') && !raw.startsWith('---\r\n')) return null;
  const end = raw.indexOf('\n---', 4);
  if (end === -1) return null;
  return { yaml: raw.slice(4, end), bodyStart: end + 4 };
}

function unquote(value) {
  return value.trim().replace(/^['"]|['"]$/g, '');
}

function parseTags(yaml) {
  const lines = yaml.split(/\r?\n/);
  for (let i = 0; i < lines.length; i += 1) {
    const match = /^tags:\s*(.*)$/.exec(lines[i]);
    if (!match) continue;
    const inline = match[1].trim();
    if (inline) {
      return inline.replace(/^\[|\]$/g, '').split(',').map(unquote).filter(Boolean);
    }
    const tags = [];
    for (let j = i + 1; j < lines.length; j += 1) {
      const item = /^\s*-\s*(.+)$/.exec(lines[j]);
      if (!item) break;
      tags.push(unquote(item[1]));
    }
    return tags;
  }
  return [];
}

function extractHeadings(markdown) {
  const headings = [];
  for (const line of markdown.split(/\r?\n/)) {
    const match = /^#{1,6}\s+(.+)$/.exec(line.trim());
    if (match) headings.push(match[1].trim());
  }
  return headings.join(' ');
}

// Strip enough Markdown syntax to leave a readable plain-text body for
// indexing and snippets; not a full renderer, just noise removal.
function normalizeBody(markdown) {
  return markdown
    .replace(/```[\s\S]*?```/g, ' ')
    .replace(/`[^`]+`/g, ' ')
    .replace(/!\[[^\]]*\]\([^)]*\)/g, ' ')
    .replace(/\[\[([^\]|#]+)(?:[|#][^\]]*)?\]\]/g, '$1')
    .replace(/\[([^\]]*)\]\([^)]*\)/g, '$1')
    .replace(/^#{1,6}\s+/gm, '')
    .replace(/^>\s?/gm, '')
    .replace(/^-{3,}\s*$/gm, ' ')
    .replace(/\s+/g, ' ')
    .trim();
}

async function loadDocument(workspaceRoot, artifact) {
  const absolute = assertInside(workspaceRoot, join(workspaceRoot, artifact.path), 'search index path');
  const raw = await readFile(absolute, 'utf8');
  const front = frontmatterBlock(raw);
  const tags = front ? parseTags(front.yaml) : [];
  const markdown = front ? raw.slice(front.bodyStart).replace(/^\r?\n/, '') : raw;
  return {
    path: artifact.path,
    kind: artifact.kind,
    title: artifact.title || artifact.path,
    headings: extractHeadings(markdown),
    tags: tags.join(' '),
    body: normalizeBody(markdown),
  };
}

/**
 * Build the Search index from a validated snapshot: corpus is the snapshot's
 * artifact inventory minus wiki/SCHEMA.md, deduplicated by canonical path.
 * Each document is a fresh disk read, so the index always reflects current
 * content at rebuild time.
 */
export async function buildSearchIndex(snapshot, { workspaceRoot }) {
  const corpus = [];
  const seen = new Set();
  for (const artifact of snapshot.artifacts) {
    if (EXCLUDED_PATHS.has(artifact.path) || seen.has(artifact.path)) continue;
    seen.add(artifact.path);
    corpus.push(artifact);
  }

  const docs = new Map();
  // ponytail: sequential reads keep this simple; a workspace's wiki/spec/plan
  // corpus is small, not worth a concurrency pool.
  for (const artifact of corpus) {
    try {
      const doc = await loadDocument(workspaceRoot, artifact);
      docs.set(doc.path, doc);
    } catch {
      // Unreadable or escaped file: drop from the search corpus rather than fail the whole index.
    }
  }

  const mini = new MiniSearch({
    idField: 'path',
    fields: ['title', 'headings', 'path', 'tags', 'body'],
  });
  mini.addAll(Array.from(docs.values()));

  return { mini, docs };
}

function clampLimit(rawLimit) {
  const n = Number(rawLimit);
  if (!Number.isFinite(n) || n <= 0) return DEFAULT_LIMIT;
  return Math.min(Math.floor(n), MAX_LIMIT);
}

function makeSnippet(body, terms) {
  const lowerBody = body.toLowerCase();
  let position = -1;
  for (const term of terms) {
    const idx = lowerBody.indexOf(term.toLowerCase());
    if (idx !== -1 && (position === -1 || idx < position)) position = idx;
  }
  if (position === -1) return body.slice(0, SNIPPET_LENGTH).trim();
  const half = Math.floor(SNIPPET_LENGTH / 2);
  const start = Math.max(0, position - half);
  return body.slice(start, start + SNIPPET_LENGTH).trim();
}

function byScoreThenPath(a, b) {
  return b.score - a.score || a.id.localeCompare(b.id);
}

/**
 * Run one deterministic search against a built index. AND + prefix first;
 * if that yields fewer than five results, a fuzzy 0.2 pass fills in,
 * appended after (never reordering) the first-pass results.
 */
export function search(index, { q, kind, limit } = {}) {
  const query = (q ?? '').trim();
  if (query.length < MIN_QUERY_LENGTH) {
    throw Object.assign(new Error('query must be at least 2 characters'), { status: 400 });
  }
  const effectiveLimit = clampLimit(limit);
  const filter = kind ? (result) => index.docs.get(result.id)?.kind === kind : undefined;
  const searchOptions = { combineWith: 'AND', prefix: true, filter };

  const exact = index.mini.search(query, { ...searchOptions, fuzzy: false });
  const exactPaths = new Set(exact.map((result) => result.id));
  const fuzzyExtra = exact.length < FUZZY_FLOOR
    ? index.mini.search(query, { ...searchOptions, fuzzy: 0.2 }).filter((result) => !exactPaths.has(result.id))
    : [];

  const merged = [...exact.sort(byScoreThenPath), ...fuzzyExtra.sort(byScoreThenPath)].slice(0, effectiveLimit);

  return merged.map((result) => {
    const doc = index.docs.get(result.id);
    return {
      path: result.id,
      kind: doc.kind,
      title: doc.title,
      snippet: makeSnippet(doc.body, result.terms),
    };
  });
}
