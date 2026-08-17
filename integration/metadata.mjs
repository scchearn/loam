import { readFile, stat } from 'node:fs/promises';
import { isAbsolute, join, resolve } from 'node:path';

import { assertInside, resolveSkillsRoot } from './paths.mjs';

// Core semver with optional semver 2.0.0 prerelease (`-` plus dot-separated
// identifiers). Build metadata (`+...`) stays rejected: npm refuses it and
// `+` in tag-derived URLs is unsafe. Numeric prerelease identifiers must not
// have leading zeros.
const SEMVER = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-(?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*)(?:\.(?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*))*)?$/;
const SHA256 = /^[a-f0-9]{64}$/i;

function requireString(value, label) {
  if (typeof value !== 'string' || !value) throw new Error(`install metadata ${label} is missing`);
  return value;
}

function containedAbsolutePath(root, value, label) {
  if (!isAbsolute(value)) throw new Error(`install metadata ${label} must be absolute`);
  return assertInside(root, value, `install metadata ${label}`);
}

export function validateInstallMetadata(globalRoot, metadata) {
  const root = resolve(globalRoot);
  if (!metadata || typeof metadata !== 'object' || Array.isArray(metadata)) {
    throw new Error('install metadata must be an object');
  }

  // Schema 2 drops the runtime_* fields — the config-dir ledger is the runtime
  // authority. Schema 1 is tolerated during the migration window: its runtime_*
  // fields are read only by the T6 migration (from the raw file), never here,
  // and are validated only if present so garbage still fails loudly.
  if (metadata.schema_version !== 1 && metadata.schema_version !== 2) {
    throw new Error('unsupported install metadata schema');
  }
  const target = requireString(metadata.target, 'target');
  const adapterRoot = containedAbsolutePath(root, requireString(metadata.adapter_root, 'adapter_root'), 'adapter_root');
  const integrationPath = containedAbsolutePath(
    root,
    requireString(metadata.integration_path, 'integration_path'),
    'integration_path',
  );
  if (metadata.skills_scope !== 'global') throw new Error('install metadata skills_scope must be global');
  if (typeof metadata.skills_source !== 'string' || !metadata.skills_source) {
    throw new Error('install metadata skills_source is missing');
  }
  if (!Array.isArray(metadata.configured_harnesses)) {
    throw new Error('install metadata configured_harnesses is invalid');
  }
  if (metadata.runtime_version !== undefined
    && (typeof metadata.runtime_version !== 'string' || !SEMVER.test(metadata.runtime_version))) {
    throw new Error('install metadata runtime_version is invalid');
  }
  if (metadata.runtime_sha256 !== undefined
    && (typeof metadata.runtime_sha256 !== 'string' || !SHA256.test(metadata.runtime_sha256))) {
    throw new Error('install metadata runtime_sha256 is invalid');
  }

  return {
    ...metadata,
    plugin_version: requireString(metadata.plugin_version, 'plugin_version'),
    target,
    adapter_root: adapterRoot,
    integration_path: integrationPath,
    ...(metadata.runtime_sha256 !== undefined ? { runtime_sha256: metadata.runtime_sha256.toLowerCase() } : {}),
  };
}

export async function readInstallMetadata(globalRoot) {
  let metadata;
  try {
    metadata = JSON.parse(await readFile(join(resolve(globalRoot), 'install.json'), 'utf8'));
  } catch (error) {
    throw new Error(`invalid install metadata: ${error instanceof Error ? error.message : String(error)}`);
  }
  return validateInstallMetadata(globalRoot, metadata);
}

export async function readSkillContent({ skillsRoot, home, env } = {}) {
  const root = resolve(skillsRoot || resolveSkillsRoot({ home, env }));
  const file = join(root, 'loam-using', 'SKILL.md');
  await stat(file);
  return readFile(file, 'utf8');
}
