import { readFile } from 'node:fs/promises';
import nodePath, { isAbsolute, join, resolve } from 'node:path';

// These live in the integration tree, not setup/: the published integration is
// staged standalone, so it must not import ../setup/* (that tree is not copied
// alongside it). See integration/config-store.mjs.
import { configRoot, publishJson, SEMVER } from './config-store.mjs';

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

// Containment and identity are decided with `path.relative`, not a string
// prefix: on win32 it compares case-insensitively (the filesystem does, and a
// config dir can be spelled `C:\Users\Alice` or `c:\users\alice` across
// runs) and reports another drive or UNC share as an absolute result. The
// path flavour is a parameter so tests can exercise win32 rules on any host.
// The root itself is not "inside" itself: store paths are always below it.
export function isInsideRoot(root, target, p = nodePath) {
  const rel = p.relative(p.resolve(root), p.resolve(target));
  return Boolean(rel) && !rel.startsWith('..') && !p.isAbsolute(rel);
}

// True when two paths name the same location (case-insensitive on win32).
export function samePath(a, b, p = nodePath) {
  return p.relative(p.resolve(a), p.resolve(b)) === '';
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
  if (root && !isInsideRoot(root, store_path)) {
    throw new Error('ledger store_path is outside the config dir');
  }
  return { schema_version, channel, target, sha256: sha256.toLowerCase(), store_path };
}

// The absolute runtime binary path for injected hooks and internal resolvers:
// the ledger's store_path (config-dir authority), falling back to a schema-1
// install.json runtime_path while a machine is pending migration (T6). `null`
// when neither resolves. Node and Rust resolve the same config-dir store.
export async function resolveRuntimePath({ globalRoot, ...options } = {}) {
  const ledger = await readLedger(options);
  if (ledger?.store_path) return ledger.store_path;
  if (globalRoot) {
    try {
      const raw = JSON.parse(await readFile(join(resolve(globalRoot), 'install.json'), 'utf8'));
      if (typeof raw.runtime_path === 'string' && raw.runtime_path) return raw.runtime_path;
    } catch {
      // No readable install.json — nothing to fall back to.
    }
  }
  return null;
}

// Raw ledger text; `null` when it does not exist (or no config dir). I/O errors
// other than ENOENT propagate.
async function readLedgerText(options) {
  const path = ledgerPath(options);
  if (!path) return null;
  try {
    return await readFile(path, 'utf8');
  } catch (error) {
    if (error?.code === 'ENOENT') return null;
    throw error;
  }
}

// Read + validate the ledger; `null` when it does not exist (or no config dir).
export async function readLedger(options = {}) {
  const raw = await readLedgerText(options);
  return raw === null ? null : validateLedger(JSON.parse(raw), options);
}

// Install/update-only read. A present-but-invalid ledger (unparseable, or
// failing validation — e.g. an out-of-root `store_path`) is reported as
// `invalid` with the reason and `ledger: null`, so the transaction that is about
// to rewrite the ledger can replace it. The unvalidated content is never
// returned: `ledger` is the validated object or `null`. Readiness and hook
// paths keep using the strict `readLedger`.
export async function readLedgerLenient(options = {}) {
  const raw = await readLedgerText(options);
  if (raw === null) return { ledger: null, invalid: null };
  try {
    return { ledger: validateLedger(JSON.parse(raw), options), invalid: null };
  } catch (error) {
    return { ledger: null, invalid: error.message };
  }
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
