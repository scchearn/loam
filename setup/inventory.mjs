import { readFile, stat } from 'node:fs/promises';
import { basename, dirname, isAbsolute, join, relative, resolve } from 'node:path';

const semver = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/;

function removeQuotes(value) {
  return value.replace(/^['"]|['"]$/g, '');
}

function parseFrontmatter(content, sourcePath) {
  const match = content.match(/^---\r?\n([\s\S]*?)\r?\n---(?:\r?\n|$)/);
  if (!match) throw new Error(`missing skill frontmatter: ${sourcePath}`);

  let name = '';
  let version = '';
  let inMetadata = false;
  for (const line of match[1].split(/\r?\n/)) {
    if (/^metadata:\s*$/.test(line)) {
      inMetadata = true;
      continue;
    }
    if (inMetadata && /^[^\s]/.test(line)) inMetadata = false;
    if (line.startsWith('name:')) name = removeQuotes(line.slice(5).trim());
    if (inMetadata && /^\s+version:/.test(line)) {
      version = removeQuotes(line.replace(/^\s+version:\s*/, '').trim());
    }
  }

  if (!name) throw new Error(`missing skill frontmatter name: ${sourcePath}`);
  if (!semver.test(version)) throw new Error(`invalid skill metadata.version at ${sourcePath}`);
  return { name, version };
}

function assertInside(root, candidate, label) {
  const relativePath = relative(root, candidate);
  if (relativePath.startsWith('..') || isAbsolute(relativePath)) {
    throw new Error(`${label} escapes package root: ${candidate}`);
  }
}

async function resolveSkillFile(packageRoot, declaredPath) {
  const declared = resolve(packageRoot, declaredPath);
  assertInside(packageRoot, declared, 'skill path');
  const info = await stat(declared);
  const skillDir = info.isDirectory() ? declared : dirname(declared);
  const skillPath = info.isDirectory() ? join(declared, 'SKILL.md') : declared;
  assertInside(packageRoot, skillPath, 'skill file');
  return { skillDir, skillPath };
}

export async function loadSkillInventory({ packageRoot = resolve(new URL('..', import.meta.url).pathname) } = {}) {
  const root = resolve(packageRoot);
  const manifestPath = join(root, '.claude-plugin', 'plugin.json');
  const manifest = JSON.parse(await readFile(manifestPath, 'utf8'));
  if (!Array.isArray(manifest.skills) || manifest.skills.length === 0) {
    throw new Error(`plugin skill inventory is empty: ${manifestPath}`);
  }

  const skills = [];
  const frontmatterNames = new Set();
  const directoryNames = new Set();
  for (const declaredPath of manifest.skills) {
    const { skillDir, skillPath } = await resolveSkillFile(root, declaredPath);
    const frontmatter = parseFrontmatter(await readFile(skillPath, 'utf8'), skillPath);
    const directoryName = basename(skillDir);
    if (frontmatterNames.has(frontmatter.name)) {
      throw new Error(`duplicate skill frontmatter name: ${frontmatter.name}`);
    }
    if (directoryNames.has(directoryName)) {
      throw new Error(`duplicate skill directory name: ${directoryName}`);
    }
    frontmatterNames.add(frontmatter.name);
    directoryNames.add(directoryName);
    skills.push({
      aliases: [frontmatter.name, directoryName],
      directoryName,
      frontmatterName: frontmatter.name,
      metadataVersion: frontmatter.version,
      path: skillPath,
      sourcePath: relative(root, skillPath),
    });
  }

  skills.sort((left, right) => left.sourcePath.localeCompare(right.sourcePath));
  return { packageRoot: root, skills };
}
