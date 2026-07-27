import { createHash } from 'node:crypto';
import { createReadStream } from 'node:fs';
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

function errorCode(error) {
  return ['ENOENT', 'EACCES', 'EPERM', 'EISDIR'].includes(error?.code) ? error.code : 'OTHER';
}

function frame(value) {
  const bytes = Buffer.from(String(value), 'utf8');
  if (bytes.length > 0xffffffff) throw new FingerprintError('fingerprint_unavailable', 'fingerprint field is too large');
  const prefix = Buffer.alloc(4);
  prefix.writeUInt32BE(bytes.length, 0);
  return Buffer.concat([prefix, bytes]);
}

function reasonByte(reason) {
  if (reason === 'new') return 0x01;
  if (reason === 'stale') return 0x02;
  throw new FingerprintError('fingerprint_unavailable', 'unknown actionable diff reason');
}

function validatePath(workspace, value) {
  if (typeof value !== 'string') throw new FingerprintError('fingerprint_unavailable', 'actionable path must be a string');
  const path = String(value).replaceAll('\\', '/');
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

async function hashBytes(path, deadline) {
  const hash = createHash('sha256');
  await new Promise((resolvePromise, reject) => {
    const stream = createReadStream(path);
    const abort = () => {
      stream.destroy();
      reject(new FingerprintError('fingerprint_unavailable', 'fingerprint deadline exceeded'));
    };
    const timer = setTimeout(abort, Math.max(1, deadline - Date.now()));
    stream.on('data', (chunk) => hash.update(chunk));
    stream.once('error', (error) => {
      clearTimeout(timer);
      reject(error);
    });
    stream.once('end', () => {
      clearTimeout(timer);
      resolvePromise();
    });
  });
  return hash.digest();
}

function contentResult(tag, value) {
  return Buffer.concat([Buffer.from([tag]), value]);
}

async function entryResult(entry, workspace, deadline) {
  const location = validatePath(workspace, entry.path);
  let before;
  try {
    before = await stat(location.absolute);
  } catch (error) {
    return { complete: false, size: 0, bytes: contentResult(0x02, frame(errorCode(error))) };
  }
  if (Date.now() >= deadline) {
    return { complete: false, size: before.size, bytes: Buffer.from([0x05]) };
  }
  let digest;
  try {
    digest = await hashBytes(location.absolute, deadline);
  } catch (error) {
    if (error instanceof FingerprintError) return { complete: false, size: before.size, bytes: Buffer.from([0x05]) };
    return { complete: false, size: before.size, bytes: contentResult(0x03, frame(errorCode(error))) };
  }
  if (Date.now() >= deadline) return { complete: false, size: before.size, bytes: Buffer.from([0x05]) };
  let after;
  try {
    after = await stat(location.absolute);
  } catch {
    return { complete: false, size: -1, bytes: Buffer.from([0x04]) };
  }
  const changed = before.size !== after.size
    || before.mtimeMs !== after.mtimeMs
    || (before.dev !== undefined && after.dev !== undefined && before.dev !== after.dev)
    || (before.ino !== undefined && after.ino !== undefined && before.ino !== after.ino);
  if (changed) return { complete: false, size: -1, bytes: Buffer.from([0x04]) };
  const size = Buffer.alloc(8);
  size.writeBigUInt64BE(BigInt(before.size), 0);
  return { complete: true, size: before.size, bytes: contentResult(0x01, Buffer.concat([size, digest])) };
}

export async function fingerprintActionable({
  workspace,
  entries = [],
  exclusionsPath,
  deadlineMs = 20000,
} = {}) {
  const started = Date.now();
  const exclusions = await readFile(exclusionsPath).catch((error) => {
    throw new FingerprintError('exclusions_unavailable', error.message);
  });
  const exclusionsDigest = createHash('sha256').update(exclusions).digest();
  const actionable = entries.filter((entry) => entry && (entry.reason === 'new' || entry.reason === 'stale'));
  const validated = actionable.map((entry) => ({ ...entry, ...validatePath(workspace, entry.path) }));
  validated.sort((a, b) => Buffer.compare(Buffer.from(a.path, 'utf8'), Buffer.from(b.path, 'utf8')));
  const names = new Set();
  for (const entry of validated) {
    if (names.has(entry.path)) throw new FingerprintError('fingerprint_unavailable', 'duplicate actionable path');
    names.add(entry.path);
  }
  const header = Buffer.concat([
    Buffer.from(ALGORITHM, 'utf8'),
    exclusionsDigest,
    (() => { const b = Buffer.alloc(4); b.writeUInt32BE(validated.length, 0); return b; })(),
  ]);
  if (validated.length === 0) {
    const serialized = Buffer.concat([header, Buffer.from([0x00])]);
    return { fingerprint: createHash('sha256').update(serialized).digest('hex'), complete: true, count: 0, serialized };
  }
  const records = [];
  let complete = true;
  for (const entry of validated) {
    if (Date.now() - started >= deadlineMs) {
      complete = false;
      records.push(Buffer.concat([frame(entry.path), Buffer.from([reasonByte(entry.reason)]), frame(entry.mtime), Buffer.from([0x05])]));
      continue;
    }
    const result = await entryResult(entry, workspace, started + deadlineMs);
    complete &&= result.complete;
    records.push(Buffer.concat([
      frame(entry.path),
      Buffer.from([reasonByte(entry.reason)]),
      frame(entry.mtime),
      result.bytes,
    ]));
  }
  const serialized = Buffer.concat([header, Buffer.from([0x01]), ...records]);
  return {
    fingerprint: createHash('sha256').update(serialized).digest('hex'),
    complete,
    count: validated.length,
    serialized,
  };
}
