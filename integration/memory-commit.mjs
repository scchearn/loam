// Commits exactly the memory paths a background run changed, on the checked-out branch.
//
// Safety properties: never stage a path the run did not write, never touch unrelated index
// entries (add + `commit --only` on an explicit literal pathspec list), never run or bypass
// hooks or signing (skip with a typed reason instead), never push, never force, never retry,
// and never throw: memory writes are already on disk and must not be blocked or rolled back.
import { spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { readFile, stat } from 'node:fs/promises';
import { isAbsolute, join, resolve } from 'node:path';

const GIT_TIMEOUT_MS = 30_000;
const MAX_STDOUT = 16 * 1024 * 1024;
// Every hook that `git add` plus `git commit --only` can fire. Any one present means we skip: the system never runs user hooks.
const HOOKS = ['pre-commit', 'prepare-commit-msg', 'commit-msg', 'post-commit', 'post-index-change', 'reference-transaction'];
const KINDS = new Set(['harvest', 'ingest']);
const MID_OPERATION = ['MERGE_HEAD', 'rebase-merge', 'rebase-apply', 'CHERRY_PICK_HEAD', 'REVERT_HEAD', 'BISECT_LOG'];

export async function readMemoryCommitConfig(globalRoot, env = process.env) {
  let section = {};
  if (globalRoot) {
    try {
      const file = JSON.parse(await readFile(join(resolve(globalRoot), 'config.json'), 'utf8'));
      if (file && typeof file.memory_commits === 'object' && file.memory_commits) section = file.memory_commits;
    } catch { /* missing or malformed config means defaults */ }
  }
  const enabled = env.LOAM_MEMORY_COMMITS === '0' ? false : env.LOAM_MEMORY_COMMITS === '1' ? true : section.enabled !== false;
  return { enabled };
}

// Repository-redirecting variables (set when running inside a git hook) must never steer these commands.
const REPO_ENV = ['GIT_DIR', 'GIT_WORK_TREE', 'GIT_INDEX_FILE', 'GIT_PREFIX', 'GIT_COMMON_DIR', 'GIT_OBJECT_DIRECTORY', 'GIT_ALTERNATE_OBJECT_DIRECTORIES', 'GIT_NAMESPACE'];
function cleanEnv(env) {
  const copy = { ...env };
  for (const key of REPO_ENV) delete copy[key];
  return copy;
}

// Runs git without a shell. Never rejects: resolves { code, stdout, stderr, timedOut, error }.
export function runGit(args, { cwd, env = {}, input, timeoutMs = GIT_TIMEOUT_MS } = {}) {
  return new Promise((resolvePromise) => {
    let child;
    let stdout = Buffer.alloc(0);
    let overflow = false;
    let stderr = '';
    let timedOut = false;
    let settled = false;
    let timer;
    const finish = (result) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      resolvePromise({ stdout: stdout.toString('utf8'), stderr, timedOut, overflow, ...result });
    };
    try {
      child = spawn('git', args, {
        cwd,
        env: { ...cleanEnv({ ...process.env, ...env }), GIT_TERMINAL_PROMPT: '0', GIT_EDITOR: 'true', LC_ALL: 'C', LANGUAGE: 'C' },
        windowsHide: true,
        stdio: ['pipe', 'pipe', 'pipe'],
      });
    } catch (error) { finish({ code: null, error }); return; }
    timer = setTimeout(() => { timedOut = true; child.kill('SIGKILL'); }, timeoutMs);
    child.on('error', (error) => finish({ code: null, error }));
    child.stdout.on('data', (chunk) => {
      if (stdout.length + chunk.length > MAX_STDOUT) overflow = true;
      if (stdout.length >= MAX_STDOUT) return;
      stdout = Buffer.concat([stdout, chunk]).subarray(0, MAX_STDOUT);
    });
    child.stderr.on('data', (chunk) => { if (stderr.length < 65_536) stderr += chunk.toString('utf8'); });
    child.on('close', (code) => finish({ code }));
    child.stdin.on('error', () => {});
    child.stdin.end(input ?? '');
  });
}

const unavailable = (result) => result.error?.code === 'ENOENT' || result.error?.code === 'EACCES';
const failureReason = (result) => {
  if (result.timedOut) return 'timeout';
  if (unavailable(result)) return 'git_unavailable';
  return /index\.lock/.test(result.stderr || '') ? 'locked' : 'commit_failed';
};

async function sha256Of(path) {
  try { return createHash('sha256').update(await readFile(path)).digest('hex'); }
  catch (error) { return error?.code === 'ENOENT' ? 'deleted' : `unreadable:${error?.code || 'error'}`; }
}

async function readStatus(git, { toplevel, prefix, env }) {
  const result = await git(
    ['--literal-pathspecs', '-c', 'core.quotepath=off', 'status', '--porcelain=v1', '-z', '--untracked-files=all', '--no-renames', '--ignore-submodules', '--', prefix || '.'],
    { cwd: toplevel, env: { ...env, GIT_OPTIONAL_LOCKS: '0' } },
  );
  if (result.timedOut) return { skip: 'timeout' };
  if (unavailable(result)) return { skip: 'git_unavailable' };
  if (result.code !== 0 || result.overflow) return { skip: 'commit_failed' };
  const entries = {};
  for (const record of result.stdout.split('\0')) {
    if (record.length < 4) continue;
    const xy = record.slice(0, 2);
    if (xy === '!!') continue;
    const path = record.slice(3);
    entries[path] = { xy, sha256: await sha256Of(join(toplevel, path)) };
  }
  // Ignored untracked files never appear in status. Track them separately (best effort, cheap stat
  // signature) so a run that only wrote ignored files reports `untracked`, not `no_changes`.
  const ignored = {};
  const listed = await git(
    ['--literal-pathspecs', '-c', 'core.quotepath=off', 'ls-files', '-o', '-i', '--exclude-standard', '-z', '--', prefix || '.'],
    { cwd: toplevel, env: { ...env, GIT_OPTIONAL_LOCKS: '0' } },
  );
  if (listed.code === 0 && !listed.overflow && !listed.timedOut) {
    for (const path of listed.stdout.split('\0')) {
      if (!path) continue;
      try { const info = await stat(join(toplevel, path)); ignored[path] = `${info.size}:${info.mtimeMs}`; }
      catch { ignored[path] = 'unreadable'; }
    }
  }
  return { entries, ignored };
}

export async function snapshotMemory({ wikiRoot, globalRoot, config, env = {}, git = runGit } = {}) {
  try {
    const settings = config || await readMemoryCommitConfig(globalRoot, { ...process.env, ...env });
    if (!settings.enabled) return { enabled: false };
    try { if (!(await stat(wikiRoot)).isDirectory()) return { skip: 'not_a_repo' }; } catch { return { skip: 'not_a_repo' }; }
    const top = await git(['rev-parse', '--show-toplevel', '--show-prefix'], { cwd: wikiRoot, env });
    if (unavailable(top)) return { skip: 'git_unavailable' };
    if (top.timedOut) return { skip: 'timeout' };
    if (top.code !== 0) return { skip: 'not_a_repo' };
    const lines = top.stdout.split('\n').map((line) => line.replace(/\r$/, ''));
    const toplevel = lines[0];
    const prefix = lines[1] || '';
    if (!toplevel) return { skip: 'not_a_repo' };
    if (prefix) {
      const ignored = await git(['--literal-pathspecs', 'check-ignore', '-q', '--', prefix], { cwd: toplevel, env });
      if (ignored.code === 0) {
        const tracked = await git(['--literal-pathspecs', 'ls-files', '-z', '--', prefix], { cwd: toplevel, env });
        if (tracked.code === 0 && !tracked.stdout) return { skip: 'untracked' };
      }
    }
    const status = await readStatus(git, { toplevel, prefix, env });
    if (status.skip) return { skip: status.skip };
    return { enabled: true, toplevel, prefix, entries: status.entries, ignored: status.ignored };
  } catch { return { skip: 'commit_failed' }; }
}

async function exists(path, { executable = false } = {}) {
  try {
    const info = await stat(path);
    if (executable && process.platform !== 'win32') return info.isFile() && (info.mode & 0o111) !== 0;
    return executable ? info.isFile() : true;
  } catch { return false; }
}

async function gitPaths(git, { toplevel, env }, names) {
  const result = await git(['rev-parse', ...names.flatMap((name) => ['--git-path', name])], { cwd: toplevel, env });
  if (result.code !== 0) return null;
  const lines = result.stdout.split('\n').map((line) => line.replace(/\r$/, '')).filter(Boolean);
  if (lines.length !== names.length) return null;
  return Object.fromEntries(names.map((name, index) => [name, isAbsolute(lines[index]) ? lines[index] : resolve(toplevel, lines[index])]));
}

// Ask git the way commit does: env and config are honoured, hostname autodetection is not.
async function hasIdentity(git, { toplevel, env }) {
  for (const name of ['GIT_AUTHOR_IDENT', 'GIT_COMMITTER_IDENT']) {
    const result = await git(['-c', 'user.useConfigOnly=true', 'var', name], { cwd: toplevel, env });
    if (result.code !== 0 || !result.stdout.trim()) return false;
  }
  return true;
}

const skipped = (reason) => ({ memory_commit: `commit_skipped:${reason}` });

export async function commitMemoryWrites({ snapshot, kind, summary, env = {}, git = runGit } = {}) {
  try {
    if (!snapshot || snapshot.enabled === false) return { memory_commit: 'disabled' };
    if (snapshot.skip) return skipped(snapshot.skip);
    if (!KINDS.has(kind)) return skipped('commit_failed');
    const { toplevel, prefix } = snapshot;
    const status = await readStatus(git, { toplevel, prefix, env });
    if (status.skip) return skipped(status.skip);

    // Written set: any path whose state or content differs from the snapshot. A path that was already
    // dirty and that the run changed is committed in full, including the human's earlier edits.
    const changed = (now, then) => Object.keys(now || {}).filter((path) => {
      const before = then?.[path];
      const after = now[path];
      return before === undefined || JSON.stringify(before) !== JSON.stringify(after);
    });
    const written = changed(status.entries, snapshot.entries);
    if (written.length === 0) return skipped(changed(status.ignored, snapshot.ignored).length > 0 ? 'untracked' : 'no_changes');
    written.sort();

    const paths = await gitPaths(git, { toplevel, env }, [...MID_OPERATION, 'index.lock', ...HOOKS.map((hook) => `hooks/${hook}`)]);
    if (!paths) return skipped('commit_failed');
    for (const name of MID_OPERATION) if (await exists(paths[name])) return skipped('mid_operation');
    if (await exists(paths['index.lock'])) return skipped('locked');
    const head = await git(['symbolic-ref', '-q', 'HEAD'], { cwd: toplevel, env });
    if (head.timedOut) return skipped('timeout');
    if (head.code !== 0) return skipped('detached_head');
    if (!(await hasIdentity(git, { toplevel, env }))) return skipped('no_identity');
    for (const hook of HOOKS) if (await exists(paths[`hooks/${hook}`], { executable: true })) return skipped('hooks_present');
    const signing = await git(['config', '--type=bool', '--get', 'commit.gpgsign'], { cwd: toplevel, env });
    if (signing.code === 0 && signing.stdout.trim() === 'true') return skipped('signing_required');

    const input = written.join('\0') + '\0';
    const staged = await git(['--literal-pathspecs', '-c', 'core.quotepath=off', 'ls-files', '-s', '-z', '--', prefix || '.'], { cwd: toplevel, env });
    const writtenSet = new Set(written);
    const priorIndex = staged.code === 0 && !staged.overflow
      ? staged.stdout.split('\0').filter((record) => record && writtenSet.has(record.slice(record.indexOf('\t') + 1))).map((record) => `${record}\0`).join('')
      : '';
    // `summary` may be a function of the written set, for subjects that report the file count.
    const text = typeof summary === 'function' ? summary(written) : summary;
    const message = `loam(${kind}): ${String(text ?? '').replace(/\s+/g, ' ').trim()}`.trimEnd();
    const added = await git(['--literal-pathspecs', 'add', '-A', '--pathspec-from-file=-', '--pathspec-file-nul'], { cwd: toplevel, env, input });
    if (added.code !== 0) return skipped(failureReason(added));
    const committed = await git(
      ['--literal-pathspecs', 'commit', '--only', '-q', '-m', message, '--pathspec-from-file=-', '--pathspec-file-nul'],
      { cwd: toplevel, env, input },
    );
    if (committed.code !== 0) {
      // Best effort: do not leave the run's paths staged by our add. Never fail on this.
      // then put back whatever the human had staged for those paths.
      await git(['--literal-pathspecs', 'reset', '-q', '--pathspec-from-file=-', '--pathspec-file-nul'], { cwd: toplevel, env, input }).catch(() => {});
      if (priorIndex) await git(['update-index', '-z', '--index-info'], { cwd: toplevel, env, input: priorIndex }).catch(() => {});
      return skipped(failureReason(committed));
    }
    const hash = await git(['rev-parse', 'HEAD'], { cwd: toplevel, env });
    return { memory_commit: 'committed', memory_commit_hash: hash.stdout.trim(), memory_commit_files: written };
  } catch { return skipped('commit_failed'); }
}
