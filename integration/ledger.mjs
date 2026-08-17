import { readFile } from 'node:fs/promises';
import { isAbsolute, join, resolve, sep } from 'node:path';

import { publishJson } from '../setup/atomic.mjs';
import { configRoot } from '../setup/profile.mjs';
import { SEMVER } from '../setup/constants.mjs';

// Durable config-dir runtime ledger. It records how the runtime target was
// selected and what is installed — `{ schema_version, channel, target, sha256,
// store_path }` — and is the sole authority at readiness (compared against the
// runtime's own self-report). `channel` is provenance only (nix manifest.json
// `originalUrl` precedent): it says how the target was chosen, never an input
// to version comparison, which is string-equality + sha only. The ledger lives
// under the config dir so it survives an uninstall-preserve; never under the
// volatile `~/.agents/loam`. See plans/runtime-channel-ledger.md.

const SHA256 = /^[a-f0-9]{64}$/i;
const CHANNELS = new Set(['next', 'latest', 'pinned']);
export const LEDGER_SCHEMA_VERSION = 1;

// The config root, honouring an explicit `root` override (used to thread one
// already-resolved config dir through install/write) over the env-derived
// ladder. `null` when nothing resolves.
function resolveConfigRoot(options) {
  if ('root' in options) return options.root ? resolve(options.root) : null;
  return configRoot(options);
}

// Config-dir root of the versioned runtime store. `null` when no config basis
// resolves — callers must handle that and never write to a null path.
export function runtimeStoreRoot(options = {}) {
  const root = resolveConfigRoot(options);
  return root ? join(root, 'runtime') : null;
}

export function ledgerPath(options = {}) {
  const store = runtimeStoreRoot(options);
  return store ? join(store, 'ledger.json') : null;
}

// Absolute path of the runtime binary inside the config-dir store:
// `<configRoot>/runtime/<version>/<target>/loam[.exe]`. The ledger's
// `store_path` points here; Node and Rust must resolve it identically (T11).
export function runtimeStorePath({ version, target, platform = process.platform, ...options } = {}) {
  const store = runtimeStoreRoot(options);
  if (!store) return null;
  const executable = platform === 'win32' ? 'loam.exe' : 'loam';
  return join(store, version, target, executable);
}

// Validate and normalize a ledger object. Throws on any malformed field. When a
// config root resolves, `store_path` must be an absolute path inside it.
export function validateLedger(value, options = {}) {
  if (!value || typeof value !== 'object' || Array.isArray(value)) {
    throw new Error('ledger is not an object');
  }
  const { schema_version, channel, target, sha256, store_path } = value;
  if (schema_version !== LEDGER_SCHEMA_VERSION) {
    throw new Error(`ledger schema_version must be ${LEDGER_SCHEMA_VERSION}`);
  }
  if (!CHANNELS.has(channel)) throw new Error(`ledger channel is invalid: ${channel}`);
  if (typeof target !== 'string' || !SEMVER.test(target)) {
    throw new Error(`ledger target is not semver: ${target}`);
  }
  if (typeof sha256 !== 'string' || !SHA256.test(sha256)) {
    throw new Error('ledger sha256 is invalid');
  }
  if (typeof store_path !== 'string' || !store_path || !isAbsolute(store_path)) {
    throw new Error('ledger store_path must be an absolute path');
  }
  const root = resolveConfigRoot(options);
  if (root && !(resolve(store_path) + sep).startsWith(resolve(root) + sep)) {
    throw new Error('ledger store_path is outside the config dir');
  }
  return { schema_version, channel, target, sha256: sha256.toLowerCase(), store_path };
}

// Read + validate the ledger; `null` when it does not exist (or no config dir).
export async function readLedger(options = {}) {
  const path = ledgerPath(options);
  if (!path) return null;
  let raw;
  try {
    raw = await readFile(path, 'utf8');
  } catch (error) {
    if (error?.code === 'ENOENT') return null;
    throw error;
  }
  return validateLedger(JSON.parse(raw), options);
}

// Atomically write the ledger (0600 via publishJson; the runtime/ dir is 0700).
// This write is the commit point of the install/update transaction (T7).
export async function writeLedger(ledger, options = {}) {
  const path = ledgerPath(options);
  if (!path) throw new Error('cannot resolve a config dir for the runtime ledger');
  const normalized = validateLedger({ schema_version: LEDGER_SCHEMA_VERSION, ...ledger }, options);
  await publishJson({ filePath: path, value: normalized });
  return { path, ledger: normalized };
}
