import { createHash } from 'node:crypto';
import { readFile, stat } from 'node:fs/promises';
import { isAbsolute, relative, resolve } from 'node:path';

const ALGORITHM = 'loam-actionable-fingerprint/2';
const MAX_PATH_BYTES = 1024 * 1024;

export class FingerprintError extends Error {
  constructor(reason, message) {
    super(message);
    this.name = 'FingerprintError';
    this.reason = reason;
  }
}

function validatePath(workspace, value) {
  if (typeof value !== 'string') throw new FingerprintError('fingerprint_unavailable', 'actionable path must be a string');
  const path = value.replaceAll('\\', '/');
  const bytes = Buffer.from(path, 'utf8');
  if (!path || bytes.length > MAX_PATH_BYTES || isAbsolute(path) || /^[A-Za-z]:\//u.test(path)
    || path.startsWith('//') || /[\u0000-\u001f\u007f]/u.test(path)
    || path.split('/').some((part) => part === '' || part === '.' || part === '..')) {
    throw new FingerprintError('fingerprint_unavailable', 'invalid actionable path');
  }
  const absolute = resolve(workspace, path);
  const outside = relative(resolve(workspace), absolute);
  if (!outside || outside.startsWith('..') || isAbsolute(outside)) {
    throw new FingerprintError('fingerprint_unavailable', 'actionable path escapes workspace');
  }
  return { path, absolute };
}

function errorCode(error) {
  return ['ENOENT', 'EACCES', 'EPERM', 'EISDIR'].includes(error?.code) ? error.code : 'OTHER';
}

async function entryResult(entry, deadline) {
  const base = { path: entry.path, reason: entry.reason, mtime: entry.mtime };
  if (Date.now() >= deadline) return { entry: { ...base, error: 'deadline' }, complete: false };
  try {
    const info = await stat(entry.absolute);
    const contents = await readFile(entry.absolute, { signal: AbortSignal.timeout(Math.max(1, deadline - Date.now())) });
    return {
      entry: { ...base, size: info.size, digest: createHash('sha256').update(contents).digest('hex') },
      complete: true,
    };
  } catch (error) {
    return {
      entry: { ...base, error: error?.name === 'AbortError' ? 'deadline' : errorCode(error) },
      complete: false,
    };
  }
}

export async function fingerprintActionable({ workspace, entries = [], exclusionsPath, deadlineMs = 20000 } = {}) {
  const deadline = Date.now() + deadlineMs;
  const exclusions = await readFile(exclusionsPath).catch((error) => {
    throw new FingerprintError('exclusions_unavailable', error.message);
  });
  const exclusionsDigest = createHash('sha256').update(exclusions).digest('hex');
  const actionable = entries.filter((entry) => entry && (entry.reason === 'new' || entry.reason === 'stale'));
  const validated = actionable.map((entry) => ({ ...entry, ...validatePath(workspace, entry.path) }));
  validated.sort((a, b) => Buffer.compare(Buffer.from(a.path, 'utf8'), Buffer.from(b.path, 'utf8')));
  const names = new Set();
  for (const entry of validated) {
    if (names.has(entry.path)) throw new FingerprintError('fingerprint_unavailable', 'duplicate actionable path');
    names.add(entry.path);
  }
  const payload = { algorithm: ALGORITHM, exclusions: exclusionsDigest, entries: [] };
  let complete = true;
  for (const entry of validated) {
    const result = await entryResult(entry, deadline);
    complete &&= result.complete;
    payload.entries.push(result.entry);
  }
  return {
    fingerprint: createHash('sha256').update(JSON.stringify(payload)).digest('hex'),
    complete,
    count: validated.length,
  };
}
