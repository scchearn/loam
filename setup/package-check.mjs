import { readdir, readFile, stat } from 'node:fs/promises';
import { isAbsolute, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const defaultRoot = fileURLToPath(new URL('..', import.meta.url));

function assertInside(root, candidate, label) {
  const relativePath = relative(root, candidate);
  if (relativePath.startsWith('..') || isAbsolute(relativePath)) {
    throw new Error(`${label} escapes package root: ${candidate}`);
  }
}

async function hasFile(directory) {
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const entryPath = join(directory, entry.name);
    if (entry.isFile()) return true;
    if (entry.isDirectory() && (await hasFile(entryPath))) return true;
  }
  return false;
}

async function assertAsset(root, declaredPath, label = 'package asset') {
  const assetPath = resolve(root, declaredPath);
  assertInside(root, assetPath, label);

  let info;
  try {
    info = await stat(assetPath);
  } catch {
    throw new Error(`${label} is missing: ${declaredPath}`);
  }
  if (info.isDirectory() && !(await hasFile(assetPath))) {
    throw new Error(`${label} is missing: ${declaredPath}`);
  }
}

export async function assertPackageAssets({ packageRoot = defaultRoot } = {}) {
  const root = resolve(packageRoot);
  const packageJson = JSON.parse(await readFile(join(root, 'package.json'), 'utf8'));

  if (typeof packageJson.main !== 'string' || !packageJson.main) {
    throw new Error('package main asset is missing');
  }
  await assertAsset(root, packageJson.main, 'package main asset');

  if (!Array.isArray(packageJson.files)) {
    throw new Error('package files list is missing');
  }
  for (const declaredPath of packageJson.files) {
    if (typeof declaredPath !== 'string' || !declaredPath) {
      throw new Error('package files list contains an invalid asset');
    }
    await assertAsset(root, declaredPath);
  }

  for (const [name, declaredPath] of Object.entries(packageJson.bin || {})) {
    await assertAsset(root, declaredPath, `package bin asset ${name}`);
  }

  return packageJson;
}

if (process.argv[1] && resolve(process.argv[1]) === resolve(fileURLToPath(import.meta.url))) {
  await assertPackageAssets();
}
