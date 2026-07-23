import { readFile, stat } from 'node:fs/promises';
import { isAbsolute, join, resolve } from 'node:path';

import { assertInside, resolveSkillsRoot } from './paths.mjs';

const SEMVER = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/;
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

  if (metadata.schema_version !== 1) throw new Error('unsupported install metadata schema');
  const runtimeVersion = requireString(metadata.runtime_version, 'runtime_version');
  if (!SEMVER.test(runtimeVersion)) throw new Error('install metadata runtime_version is invalid');
  const target = requireString(metadata.target, 'target');
  const runtimeSha256 = requireString(metadata.runtime_sha256, 'runtime_sha256');
  if (!SHA256.test(runtimeSha256)) throw new Error('install metadata runtime_sha256 is invalid');
  const runtimePath = containedAbsolutePath(root, requireString(metadata.runtime_path, 'runtime_path'), 'runtime_path');
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

  return {
    ...metadata,
    plugin_version: requireString(metadata.plugin_version, 'plugin_version'),
    runtime_version: runtimeVersion,
    target,
    runtime_sha256: runtimeSha256.toLowerCase(),
    runtime_path: runtimePath,
    adapter_root: adapterRoot,
    integration_path: integrationPath,
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

export async function readRequiredVersion({ skillsRoot, home, env } = {}) {
  const root = resolve(skillsRoot || resolveSkillsRoot({ home, env }));
  const file = join(root, 'loam-using', 'scripts', 'CLI_VERSION');
  const version = (await readFile(file, 'utf8')).trim();
  if (!SEMVER.test(version)) throw new Error(`invalid CLI_VERSION at ${file}`);
  return version;
}

export async function readSkillContent({ skillsRoot, home, env } = {}) {
  const root = resolve(skillsRoot || resolveSkillsRoot({ home, env }));
  const file = join(root, 'loam-using', 'SKILL.md');
  await stat(file);
  return readFile(file, 'utf8');
}
