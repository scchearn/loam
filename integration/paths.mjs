import { homedir } from 'node:os';
import { isAbsolute, join, relative, resolve } from 'node:path';

export const SUPPORTED_TARGETS = Object.freeze([
  'x86_64-apple-darwin',
  'aarch64-apple-darwin',
  'x86_64-pc-windows-msvc',
  'x86_64-unknown-linux-musl',
  'aarch64-unknown-linux-musl',
]);

export function assertInside(root, candidate, label = 'path') {
  const resolvedRoot = resolve(root);
  const resolvedCandidate = resolve(candidate);
  const relativePath = relative(resolvedRoot, resolvedCandidate);
  if (relativePath.startsWith('..') || isAbsolute(relativePath)) {
    throw new Error(`${label} escapes global Loam root: ${candidate}`);
  }
  return resolvedCandidate;
}

export function detectTarget({
  platform = process.platform,
  arch = process.arch,
  override = process.env.LOAM_TARGET,
} = {}) {
  if (override) {
    if (!SUPPORTED_TARGETS.includes(override)) {
      throw new Error(`unsupported runtime target: ${override}`);
    }
    return override;
  }

  const target = {
    darwin: { x64: 'x86_64-apple-darwin', arm64: 'aarch64-apple-darwin' },
    linux: { x64: 'x86_64-unknown-linux-musl', arm64: 'aarch64-unknown-linux-musl' },
    win32: { x64: 'x86_64-pc-windows-msvc' },
  }[platform]?.[arch];
  if (!target) throw new Error(`unsupported runtime target: ${platform}/${arch}`);
  return target;
}

export function resolveGlobalRoot({ home = homedir(), env = process.env, integrationPath } = {}) {
  if (env.LOAM_HOME) {
    if (!isAbsolute(env.LOAM_HOME)) throw new Error('LOAM_HOME must be absolute');
    return resolve(env.LOAM_HOME);
  }
  if (integrationPath) return resolve(integrationPath, '..', '..');
  return resolve(home, '.agents', 'loam');
}

export function resolveSkillsRoot({ home = homedir(), env = process.env } = {}) {
  if (env.LOAM_SKILLS_ROOT) {
    if (!isAbsolute(env.LOAM_SKILLS_ROOT)) throw new Error('LOAM_SKILLS_ROOT must be absolute');
    return resolve(env.LOAM_SKILLS_ROOT);
  }
  return resolve(home, '.agents', 'skills');
}

export function runtimePath(globalRoot, version, target, { platform = process.platform } = {}) {
  const executable = platform === 'win32' ? 'loam.exe' : 'loam';
  return join(globalRoot, 'bin', version, target, executable);
}
