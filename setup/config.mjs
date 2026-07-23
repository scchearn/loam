import { copyFile, readFile } from 'node:fs/promises';
import { randomUUID } from 'node:crypto';
import { resolve } from 'node:path';

import { writeAtomicFile } from './atomic.mjs';

export class ConfigError extends Error {
  constructor(message) {
    super(message);
    this.name = 'ConfigError';
  }
}

export function dedupe(values, key = (value) => value) {
  const seen = new Set();
  return values.filter((value) => {
    const identity = key(value);
    if (seen.has(identity)) return false;
    seen.add(identity);
    return true;
  });
}

function dedupeJson(value) {
  if (Array.isArray(value)) {
    const nested = value.map(dedupeJson);
    return dedupe(nested, (entry) => {
      if (entry && typeof entry === 'object') return entry.command || JSON.stringify(entry);
      return entry;
    });
  }
  if (value && typeof value === 'object') {
    return Object.fromEntries(Object.entries(value).map(([key, entry]) => [key, dedupeJson(entry)]));
  }
  return value;
}

function isPolicyOwned(config) {
  return config.managed === true || config.policyOwned === true || typeof config.managedBy === 'string';
}

export async function mergeJsonConfig({ filePath, update, policyOwned = false } = {}) {
  const destination = resolve(filePath);
  let current = {};
  let existed = false;
  try {
    current = JSON.parse(await readFile(destination, 'utf8'));
    existed = true;
  } catch (error) {
    if (error?.code !== 'ENOENT') throw new ConfigError(`malformed JSON: ${destination}`);
  }
  if (!current || Array.isArray(current) || typeof current !== 'object') {
    throw new ConfigError(`malformed JSON: ${destination}`);
  }
  if (policyOwned || isPolicyOwned(current)) throw new ConfigError(`policy-owned config: ${destination}`);
  const next = dedupeJson(typeof update === 'function' ? update(current) : { ...current, ...update });
  let backupPath = null;
  if (existed) {
    backupPath = `${destination}.backup-${randomUUID()}`;
    await copyFile(destination, backupPath);
  }
  try {
    await writeAtomicFile(destination, `${JSON.stringify(next, null, 2)}\n`);
  } catch (error) {
    throw new ConfigError(`config write failed: ${error instanceof Error ? error.message : String(error)}`);
  }
  return { config: next, backupPath };
}
