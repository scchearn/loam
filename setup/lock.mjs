import { hostname } from 'node:os';
import { mkdir, open, readFile, stat, unlink } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { randomUUID } from 'node:crypto';

export class SetupLockError extends Error {
  constructor(message) {
    super(message);
    this.name = 'SetupLockError';
    this.code = 'SETUP_LOCKED';
  }
}

function defaultIsProcessAlive(pid) {
  if (!Number.isInteger(pid) || pid <= 0) return false;
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    return error?.code === 'EPERM';
  }
}

function sleep(milliseconds) {
  return new Promise((resolvePromise) => setTimeout(resolvePromise, milliseconds));
}

async function readMetadata(lockPath) {
  try {
    return JSON.parse(await readFile(lockPath, 'utf8'));
  } catch {
    return null;
  }
}

async function tryCreate(lockPath) {
  const token = randomUUID();
  const metadata = {
    pid: process.pid,
    host: hostname(),
    started_at: new Date().toISOString(),
    token,
  };
  let handle;
  try {
    handle = await open(lockPath, 'wx', 0o600);
    await handle.writeFile(`${JSON.stringify(metadata)}\n`, 'utf8');
    await handle.close();
    return metadata;
  } catch (error) {
    await handle?.close().catch(() => {});
    if (error?.code === 'EEXIST') return null;
    throw error;
  }
}

export async function acquireSetupLock({
  globalRoot,
  timeoutMs = 30_000,
  pollMs = 100,
  staleMs = 10 * 60_000,
  isProcessAlive = defaultIsProcessAlive,
  sleepFn = sleep,
  now = () => Date.now(),
} = {}) {
  const root = resolve(globalRoot);
  const lockPath = join(root, 'setup.lock');
  await mkdir(root, { recursive: true, mode: 0o700 });
  const deadline = now() + timeoutMs;

  while (true) {
    const metadata = await tryCreate(lockPath);
    if (metadata) {
      let released = false;
      return {
        path: lockPath,
        metadata,
        async release() {
          if (released) return;
          released = true;
          const current = await readMetadata(lockPath);
          if (current?.token === metadata.token) await unlink(lockPath).catch(() => {});
        },
      };
    }

    const existing = await readMetadata(lockPath);
    let stale = false;
    try {
      const startedAt = existing?.started_at ? Date.parse(existing.started_at) : Number.NaN;
      const timestamp = Number.isFinite(startedAt) ? startedAt : (await stat(lockPath)).mtimeMs;
      const fileAge = now() - timestamp;
      stale = fileAge > staleMs && !(await isProcessAlive(existing?.pid));
    } catch {
      stale = false;
    }
    if (stale) {
      await unlink(lockPath).catch(() => {});
      continue;
    }
    if (now() >= deadline) throw new SetupLockError(`setup lock is held: ${lockPath}`);
    await sleepFn(pollMs);
  }
}

export async function withSetupLock(options, callback) {
  const lock = await acquireSetupLock(options);
  try {
    return await callback(lock);
  } finally {
    await lock.release();
  }
}
