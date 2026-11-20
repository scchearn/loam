// Hermetic git fixture: a realpath'd temp repo with its own empty global config,
// no system config and no autocrlf, so results never depend on the developer's git setup.
import { execFileSync } from 'node:child_process';
import { chmod, mkdir, mkdtemp, realpath, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';

const SEED = {
  'wiki/index.md': '# Index\n',
  'wiki/log.md': '# Log\n',
  'wiki/topics/a.md': '# A\n',
  'src/a.js': 'export const a = 1;\n',
};

export async function createGitWorkspace({ identity = true, seed = true } = {}) {
  const base = await realpath(await mkdtemp(join(tmpdir(), 'loam-gitws-')));
  const root = join(base, 'repo');
  const home = join(base, 'home');
  const globalConfig = join(base, 'gitconfig');
  await mkdir(root, { recursive: true });
  await mkdir(home, { recursive: true });
  await writeFile(globalConfig, '');
  const env = {
    GIT_CONFIG_GLOBAL: globalConfig,
    GIT_CONFIG_NOSYSTEM: '1',
    HOME: home,
    USERPROFILE: home,
    GIT_TERMINAL_PROMPT: '0',
  };
  const fullEnv = () => {
    const merged = { ...process.env, ...env };
    for (const key of ['GIT_AUTHOR_NAME', 'GIT_AUTHOR_EMAIL', 'GIT_COMMITTER_NAME', 'GIT_COMMITTER_EMAIL', 'EMAIL', 'GIT_DIR', 'GIT_WORK_TREE', 'GIT_INDEX_FILE']) delete merged[key];
    return merged;
  };
  const run = (args, { cwd = root, input } = {}) =>
    execFileSync('git', args, { cwd, env: fullEnv(), input, encoding: 'utf8', windowsHide: true, stdio: ['pipe', 'pipe', 'pipe'] });

  run(['init', '-q']);
  run(['symbolic-ref', 'HEAD', 'refs/heads/main']);
  run(['config', 'core.autocrlf', 'false']);
  if (identity) {
    run(['config', 'user.name', 'Loam Test']);
    run(['config', 'user.email', 'loam@example.test']);
  }
  const write = async (rel, text) => {
    const target = join(root, rel);
    await mkdir(dirname(target), { recursive: true });
    await writeFile(target, text);
  };
  if (seed) {
    for (const [rel, text] of Object.entries(SEED)) await write(rel, text);
    run(['add', '-A']);
    run(['-c', 'user.name=Seed', '-c', 'user.email=seed@example.test', 'commit', '-q', '-m', 'seed']);
  }
  const extraDirs = [];
  return {
    root,
    wiki: join(root, 'wiki'),
    env: fullEnv(),
    git: (args, options) => run(args, options),
    write,
    commitCount: () => Number(run(['rev-list', '--count', 'HEAD']).trim()),
    head: () => run(['rev-parse', 'HEAD']).trim(),
    async addHook(name, body) {
      const file = join(root, '.git', 'hooks', name);
      await mkdir(dirname(file), { recursive: true });
      await writeFile(file, `#!/bin/sh\n${body}\n`);
      await chmod(file, 0o755);
      return file;
    },
    async tempDir(name) {
      const dir = join(base, name);
      await mkdir(dir, { recursive: true });
      extraDirs.push(dir);
      return dir;
    },
    async cleanup() {
      await rm(base, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
    },
  };
}
