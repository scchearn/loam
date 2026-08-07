import { spawn } from 'node:child_process';

import { processDescriptor, terminateChild } from '../integration/ingest-process.mjs';
import { safeDetail } from '../integration/runtime.mjs';

export const SKILLS_CLI_VERSION = '1.5.20';

export function npxCommand() {
  return process.platform === 'win32' ? 'npx.cmd' : 'npx';
}

function timeoutResult(timeoutMs) {
  return {
    ok: false,
    code: null,
    signal: 'SIGTERM',
    stdout: '',
    stderr: `command timed out after ${timeoutMs}ms`,
    category: 'timeout',
  };
}

export function runCommand({
  command,
  args = [],
  cwd,
  env = process.env,
  timeoutMs = 120_000,
  runner,
} = {}) {
  const request = { command, args: [...args], cwd, env, shell: false, timeoutMs };
  const task = runner ? () => runner(request) : () => spawnCommand(request);
  return new Promise((resolvePromise) => {
    let settled = false;
    const timer = setTimeout(() => {
      if (settled) return;
      settled = true;
      resolvePromise(timeoutResult(timeoutMs));
    }, timeoutMs);
    Promise.resolve()
      .then(task)
      .then((result) => {
        if (settled) return;
        settled = true;
        clearTimeout(timer);
        const code = result?.code ?? 1;
        resolvePromise({
          ok: code === 0,
          code,
          signal: result?.signal ?? null,
          stdout: String(result?.stdout ?? ''),
          stderr: safeDetail(result?.stderr),
          ...(result?.category ? { category: result.category } : {}),
        });
      })
      .catch((error) => {
        if (settled) return;
        settled = true;
        clearTimeout(timer);
        resolvePromise({
          ok: false,
          code: null,
          signal: null,
          stdout: '',
          stderr: safeDetail(error instanceof Error ? error.message : error),
          category: 'process_error',
        });
      });
  });
}

function killTree(child) {
  // ponytail: on Windows the child is cmd.exe/npx.cmd and child.kill() leaves the
  // node/skills descendants orphaned; taskkill /T (via terminateChild) tears down the
  // whole tree. On unix child.kill() is the proven path — terminateChild's group-kill
  // assumes a detached child, which spawnCommand is not. (issue #50)
  if (process.platform === 'win32') return terminateChild(child, { platform: 'win32' });
  child.kill();
  return Promise.resolve();
}

function spawnCommand({ command, args, cwd, env, timeoutMs }) {
  return new Promise((resolvePromise) => {
    const descriptor = processDescriptor({ command, args, env });
    const child = spawn(descriptor.executable, descriptor.args, {
      cwd,
      env: descriptor.env || env,
      shell: false,
      windowsHide: true,
      windowsVerbatimArguments: descriptor.windowsVerbatimArguments === true,
    });
    let stdout = '';
    let stderr = '';
    let settled = false;
    const finish = (code, signal, category) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      resolvePromise({ code, signal, stdout, stderr, ...(category ? { category } : {}) });
    };
    const timer = setTimeout(() => {
      killTree(child).finally(() => finish(null, 'SIGTERM', 'timeout'));
    }, timeoutMs);
    child.stdout?.on('data', (chunk) => {
      if (stdout.length < 1_048_576) stdout += chunk.toString();
    });
    child.stderr?.on('data', (chunk) => {
      if (stderr.length < 1_048_576) stderr += chunk.toString();
    });
    child.once('error', (error) => {
      stderr += error.message;
      finish(null, null, 'process_error');
    });
    child.once('close', (code, signal) => finish(code, signal));
  });
}

export function runSkills(args, options = {}) {
  return runCommand({
    ...options,
    command: options.command || npxCommand(),
    args: ['--yes', '--package', `skills@${SKILLS_CLI_VERSION}`, 'skills', ...args],
  });
}
