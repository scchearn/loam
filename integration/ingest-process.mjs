import { execFile as nodeExecFile, spawn } from 'node:child_process';
import { statSync } from 'node:fs';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir, uptime } from 'node:os';
import { basename, extname, isAbsolute, join, win32 } from 'node:path';

function windowsEnv(env = process.env) {
  const result = {};
  for (const [key, value] of Object.entries(env)) {
    const existing = Object.keys(result).find((candidate) => candidate.toLowerCase() === key.toLowerCase());
    result[existing || key] = value;
  }
  return result;
}

export function normalizeEnvironment(env = process.env, platform = process.platform) {
  return platform === 'win32' ? windowsEnv(env) : { ...env };
}

function isRegularFile(path) {
  try { return statSync(path).isFile(); } catch { return false; }
}

function cmdToken(value) {
  const text = String(value);
  if (text.includes('%')) throw new Error('Windows batch arguments cannot contain percent expansion');
  return `"${text.replaceAll('"', '""')}"`;
}

function runExecFile(command, args, options = {}) {
  return new Promise((resolvePromise) => {
    nodeExecFile(command, args, options, (error, stdout, stderr) => resolvePromise({
      code: error ? (typeof error.code === 'number' ? error.code : null) : 0,
      stdout: String(stdout || ''), stderr: String(stderr || ''), error: error || null,
    }));
  });
}

export function resolveExecutable(command, { platform = process.platform, env = process.env } = {}) {
  const source = String(command);
  const pathApi = platform === 'win32' ? win32 : undefined;
  const absolute = pathApi ? pathApi.isAbsolute(source) : isAbsolute(source);
  if (absolute) {
    if (!isRegularFile(source)) throw new Error(`executable is not a file: ${source}`);
    return source;
  }
  const entries = String(env.PATH || '').split(platform === 'win32' ? ';' : ':').filter(Boolean);
  const extensions = platform === 'win32'
    ? String(env.PATHEXT || '.COM;.EXE;.BAT;.CMD').split(';').filter(Boolean)
    : [''];
  for (const directory of entries) {
    const base = pathApi ? pathApi.join(directory, source) : join(directory, source);
    const candidates = pathApi && extname(source) ? [base] : extensions.map((extension) => base + extension);
    for (const candidate of candidates) if (isRegularFile(candidate)) return candidate;
  }
  throw new Error(`executable not found on PATH: ${source}`);
}

export function processDescriptor({
  command,
  args = [],
  platform = process.platform,
  env = process.env,
} = {}) {
  const executable = resolveExecutable(command, { platform, env });
  const windowsBatch = platform === 'win32' && ['.cmd', '.bat'].includes(extname(executable).toLowerCase());
  if (!windowsBatch) {
    return { kind: command === process.execPath || command === 'node' || /^node(?:\.exe)?$/iu.test(basename(executable)) ? 'node' : 'direct', executable, args: [...args], shell: false };
  }
  const commandEnvironment = normalizeEnvironment(env, platform);
  const comspec = commandEnvironment.ComSpec || commandEnvironment.COMSPEC || commandEnvironment.comspec
    || join(commandEnvironment.SystemRoot || commandEnvironment.systemroot || 'C:\\\\Windows', 'System32', 'cmd.exe');
  if (!isAbsolute(comspec) && !win32.isAbsolute(comspec)) throw new Error('ComSpec must be absolute');
  if (!isRegularFile(comspec)) throw new Error('ComSpec is not a file');
  return {
    kind: 'cmd',
    executable: comspec,
    args: ['/d', '/s', '/c', `"${[executable, ...args].map(cmdToken).join(' ')}"`],
    shell: false,
    env: commandEnvironment,
    windowsVerbatimArguments: true,
  };
}

export function startTracked({
  command,
  args = [],
  cwd,
  env = process.env,
  platform = process.platform,
  input,
  timeoutMs = 900000,
  detached = false,
  windowsHide = true,
  captureOutput = true,
} = {}) {
  const descriptor = processDescriptor({ command, args, platform, env });
  let resolveCompletion;
  const completion = new Promise((resolvePromise) => { resolveCompletion = resolvePromise; });
  const child = spawn(descriptor.executable, descriptor.args, {
    cwd,
    env: descriptor.env || normalizeEnvironment(env, platform),
    shell: false,
    windowsVerbatimArguments: descriptor.windowsVerbatimArguments === true,
    detached,
    windowsHide,
    stdio: captureOutput ? ['pipe', 'pipe', 'pipe'] : [input === undefined ? 'ignore' : 'pipe', 'ignore', 'ignore'],
  });
  if (detached) child.unref();
  const MAX_OUTPUT = 64 * 1024;
  let stdout = '';
  let stderr = '';
  const append = (current, chunk) => current.length >= MAX_OUTPUT
    ? current : current + chunk.toString().slice(0, MAX_OUTPUT - current.length);
  let settled = false;
  const finish = (result) => {
    if (settled) return;
    settled = true;
    clearTimeout(timer);
    resolveCompletion({ ...result, stdout, stderr, child, descriptor });
  };
  const timer = setTimeout(() => {
    terminateChild(child, { platform }).finally(() => finish({ code: null, signal: 'SIGTERM', category: 'timeout' }));
  }, timeoutMs);
  child.stdout?.on('data', (chunk) => { stdout = append(stdout, chunk); });
  child.stderr?.on('data', (chunk) => { stderr = append(stderr, chunk); });
  child.once('error', (error) => finish({ code: null, signal: null, category: 'runtime_error', error }));
  child.once('close', (code, signal) => finish({ code, signal, category: null }));
  if (input !== undefined) child.stdin?.end(String(input));
  else child.stdin?.end();
  return {
    child,
    descriptor,
    completion,
    closeStdin: () => child.stdin?.end(),
    writeStdin: (value) => child.stdin?.write(String(value)),
  };
}

export function spawnDetached(options = {}) {
  const descriptor = processDescriptor(options);
  const child = spawn(descriptor.executable, descriptor.args, {
    cwd: options.cwd,
    env: descriptor.env || normalizeEnvironment(options.env || process.env, options.platform || process.platform),
    shell: false,
    windowsVerbatimArguments: descriptor.windowsVerbatimArguments === true,
    detached: true,
    windowsHide: options.windowsHide !== false,
    stdio: 'ignore',
  });
  child.unref();
  return { child, descriptor };
}

export async function terminateChild(child, { platform = process.platform, graceMs = 2000 } = {}) {
  if (!child?.pid) return;
  if (platform === 'win32') {
    const ok = await new Promise((done) => {
      const taskkill = process.env.SystemRoot
        ? join(process.env.SystemRoot, 'System32', 'taskkill.exe') : 'taskkill.exe';
      nodeExecFile(taskkill, ['/pid', String(child.pid), '/T', '/F'], { windowsHide: true, shell: false }, (error) => done(!error));
    });
    return ok;
  }
  const signal = (name) => {
    try { process.kill(-child.pid, name); return true; }
    catch (error) {
      if (error?.code === 'ESRCH') return true;
      try { return child.kill(name); } catch { return false; }
    }
  };
  if (!signal('SIGTERM')) return false;
  await new Promise((done) => setTimeout(done, graceMs));
  return signal('SIGKILL');
}

export async function bootIdentity(platform = process.platform) {
  if (platform === 'linux') {
    try { return (await readFile('/proc/sys/kernel/random/boot_id', 'utf8')).trim(); } catch {}
  }
  return String(Math.round((Date.now() / 1000 - uptime()) / 60));
}

export async function processStartIdentity(pid, platform = process.platform) {
  if (platform === 'linux') {
    try {
      const text = await readFile('/proc/' + String(pid) + '/stat', 'utf8');
      const close = text.lastIndexOf(')');
      if (close < 0) return null;
      const fields = text.slice(close + 2).trim().split(/\s+/u);
      return fields[19] || null;
    } catch { return null; }
  }
  if (platform === 'darwin') {
    const result = await runExecFile('ps', ['-p', String(pid), '-o', 'lstart='], { timeout: 3000 });
    return result.code === 0 ? result.stdout.trim() || null : null;
  }
  if (platform === 'win32' && Number.isInteger(Number(pid))) {
    let directory;
    try {
      directory = await mkdtemp(join(tmpdir(), 'loam-process-'));
      const script = join(directory, 'process-start.ps1');
      await writeFile(script, '$processId = [int]$env:LOAM_PROCESS_PID\n(Get-Process -Id $processId).StartTime.ToUniversalTime().Ticks\n', { mode: 0o600 });
      const result = await runExecFile('powershell.exe', [
        '-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', script,
      ], { timeout: 3000, env: { ...process.env, LOAM_PROCESS_PID: String(Number(pid)) } });
      return result.code === 0 ? result.stdout.trim() || null : null;
    } finally {
      if (directory) await rm(directory, { recursive: true, force: true });
    }
  }
  return null;
}

export async function childIdentity(pid, { platform = process.platform } = {}) {
  return { pid, boot_id: await bootIdentity(platform), process_start: await processStartIdentity(pid, platform), captured_at: new Date().toISOString() };
}

export async function classifyChild(identity, { platform = process.platform } = {}) {
  if (!identity?.pid || !identity.boot_id) return 'unknown';
  let alive = true;
  try { process.kill(Number(identity.pid), 0); } catch (error) {
    if (error?.code === 'ESRCH') alive = false;
    else if (error?.code !== 'EPERM') return 'unknown';
  }
  if (!alive) return 'dead';
  if (identity.boot_id !== await bootIdentity(platform)) return 'dead';
  if (identity.process_start) {
    const current = await processStartIdentity(identity.pid, platform);
    if (!current) return 'unknown';
    if (current !== identity.process_start) return 'dead';
  } else {
    return 'unknown';
  }
  return 'live';
}

export async function execFile(command, args = [], options = {}) {
  try {
    return await startTracked({
      command,
      args,
      cwd: options.cwd,
      env: options.env || process.env,
      platform: options.platform || process.platform,
      timeoutMs: options.timeout || options.timeoutMs || 30000,
    }).completion;
  } catch (error) {
    return { code: null, stdout: '', stderr: '', error, category: 'runtime_error' };
  }
}
