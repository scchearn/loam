import { chmod, copyFile, mkdir, mkdtemp, rename, rm, stat, writeFile } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { randomUUID } from 'node:crypto';

async function setMode(path, mode) {
  await chmod(path, mode).catch((error) => {
    if (process.platform !== 'win32') throw error;
  });
}

export async function createStagingDirectory(globalRoot, { prefix = 'setup' } = {}) {
  const root = resolve(globalRoot);
  const stagingRoot = join(root, 'staging');
  await mkdir(stagingRoot, { recursive: true, mode: 0o700 });
  const stagingPath = await mkdtemp(join(stagingRoot, `${prefix}-`));
  await setMode(stagingPath, 0o700);
  return stagingPath;
}

export async function cleanupStaging(stagingPath) {
  await rm(stagingPath, { recursive: true, force: true });
}

export async function writeAtomicFile(filePath, contents, { mode = 0o600 } = {}) {
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

export async function publishAtomic({ stagedPath, destination, mode = 0o700, keepBackup = true } = {}) {
  const source = resolve(stagedPath);
  const target = resolve(destination);
  await mkdir(dirname(target), { recursive: true, mode: 0o700 });
  await setMode(source, mode);

  const backup = `${target}.backup-${randomUUID()}`;
  let hadExisting = false;
  try {
    await stat(target);
    hadExisting = true;
    await rename(target, backup);
  } catch (error) {
    if (error?.code !== 'ENOENT') throw error;
  }

  try {
    await rename(source, target);
    if (hadExisting && !keepBackup) await rm(backup, { force: true });
    return { destination: target, backupPath: hadExisting ? backup : null };
  } catch (error) {
    await rm(target, { force: true }).catch(() => {});
    if (hadExisting) await rename(backup, target).catch(() => {});
    throw error;
  }
}

export async function publishJson({ filePath, value }) {
  return writeAtomicFile(filePath, `${JSON.stringify(value, null, 2)}\n`, { mode: 0o600 });
}

export async function backupFile(filePath, destination) {
  await mkdir(dirname(resolve(destination)), { recursive: true, mode: 0o700 });
  await copyFile(resolve(filePath), resolve(destination));
  return resolve(destination);
}
