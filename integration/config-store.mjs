import { chmod, mkdir, rename, rm, writeFile } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { randomUUID } from 'node:crypto';

// Self-contained config-dir + atomic-write utilities for the integration tree.
//
// The published integration is STAGED standalone (`stageIntegration` copies only
// `integration/*.mjs` into `<globalRoot>/integration/<version>/`), so nothing in
// `integration/` may import from `../setup/` — that tree does not exist next to
// the staged copy, and the import would fail to resolve on a real install (the
// "published adapters load only the staged integration" contract). The ledger
// modules need `configRoot`, `SEMVER`, and an atomic JSON writer, so they live
// here rather than in `setup/`.
//
// These MIRROR their setup/ originals — `setup/profile.mjs` `configRoot`,
// `setup/constants.mjs` `SEMVER`, `setup/atomic.mjs` `publishJson` — and the
// Rust runtime mirrors `configRoot` too (`cli/src/provisioning.rs`). Keep the
// copies in sync; the config-dir ladder is a deliberately triplicated contract.

// The config-dir root: LOAM_CONFIG_DIR -> platform config dir -> ~/.config/loam.
// `null` when no basis resolves. Byte-identical to setup/profile.mjs configRoot.
export function configRoot({
  env = process.env,
  home = env.HOME || env.USERPROFILE,
  platform = process.platform,
} = {}) {
  const explicit = env.LOAM_CONFIG_DIR?.trim();
  if (explicit) return resolve(explicit);

  if (platform === 'darwin' && home) {
    return join(home, 'Library', 'Application Support', 'loam');
  }
  if (platform === 'win32' && env.APPDATA) {
    return resolve(env.APPDATA, 'loam');
  }
  if (env.XDG_CONFIG_HOME?.trim()) return resolve(env.XDG_CONFIG_HOME, 'loam');
  if (home) return join(home, '.config', 'loam');
  return null;
}

// Core semver with optional 2.0.0 prerelease; build metadata rejected. Mirrors
// setup/constants.mjs SEMVER.
export const SEMVER = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-(?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*)(?:\.(?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*))*)?$/;

async function setMode(path, mode) {
  await chmod(path, mode).catch((error) => {
    if (process.platform !== 'win32') throw error;
  });
}

// Atomic file write via a temp file + rename. Mirrors setup/atomic.mjs.
async function writeAtomicFile(filePath, contents, { mode = 0o600 } = {}) {
  const destination = resolve(filePath);
  await mkdir(dirname(destination), { recursive: true, mode: 0o700 });
  const temporary = `${destination}.${randomUUID()}.tmp`;
  try {
    await writeFile(temporary, contents, { encoding: 'utf8', mode });
    await setMode(temporary, mode);
    await rename(temporary, destination);
  } catch (error) {
    await rm(temporary, { force: true }).catch(() => {});
    throw error;
  }
  return destination;
}

// Atomic 0600 JSON publish — the ledger's commit primitive. Mirrors
// setup/atomic.mjs publishJson.
export async function publishJson({ filePath, value }) {
  return writeAtomicFile(filePath, `${JSON.stringify(value, null, 2)}\n`, { mode: 0o600 });
}
