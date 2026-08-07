import { open } from 'node:fs/promises';
import { stat } from 'node:fs/promises';

export class HarvestError extends Error {
  constructor(reason, detail = '') {
    super(detail || reason);
    this.name = 'HarvestError';
    this.reason = reason;
  }
}

export async function measureStore(path) {
  try {
    const info = await stat(path);
    return { present: true, size: info.size, mtime_ms: info.mtimeMs };
  } catch (error) {
    if (error?.code === 'ENOENT') return { present: false, size: 0, mtime_ms: 0 };
    throw new HarvestError('store_unreadable', `cannot stat store: ${String(error?.message || error)}`);
  }
}

export async function readTail(path, offset, { maxBytes = 262144 } = {}) {
  const handle = await open(path, 'r');
  try {
    const start = Number(offset) || 0;
    const info = await handle.stat();
    const length = Math.min(info.size - start, maxBytes);
    if (length <= 0) return { lines: [], nextOffset: start, bytesRead: 0 };
    const buffer = Buffer.alloc(length);
    const { bytesRead } = await handle.read(buffer, 0, length, start);
    const text = new TextDecoder('utf-8', { fatal: false }).decode(buffer.subarray(0, bytesRead));
    const lines = [];
    let lineStart = 0;
    let nextOffset = start;
    for (let index = 0; index < text.length; index += 1) {
      if (text[index] === '\n') {
        const content = text.slice(lineStart, index);
        lines.push({ text: content, offset: start + lineStart, endOffset: start + index });
        nextOffset = start + index + 1;
        lineStart = index + 1;
      }
    }
    return { lines, nextOffset, bytesRead };
  } finally {
    await handle.close();
  }
}

export async function parseLines(lines, parse) {
  const parsed = [];
  let skipped = 0;
  for (const line of lines) {
    try {
      const value = parse(line);
      if (value === null || value === undefined) skipped += 1;
      else parsed.push(value);
    } catch {
      skipped += 1;
    }
  }
  return { parsed, skipped };
}

export function detectRotation(state, measured) {
  const cursorValue = state?.cursor?.value;
  if (!measured.present) return { rotated: true, reset: 0 };
  if (Number.isFinite(cursorValue) && measured.size < cursorValue) return { rotated: true, reset: 0 };
  if (Number.isFinite(cursorValue) && measured.size === cursorValue) return { rotated: false, delta: 0 };
  return { rotated: false, delta: Math.max(0, measured.size - (cursorValue || 0)) };
}

const BACKENDS = new Map();

export function registerHarvestBackend(harness, backend) {
  BACKENDS.set(harness, backend);
}

export function getHarvestBackend(harness) {
  return BACKENDS.get(harness) || null;
}
