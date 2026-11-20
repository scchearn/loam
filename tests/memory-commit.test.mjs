import assert from 'node:assert/strict';
import { mkdir, mkdtemp, readFile, realpath, rm, writeFile, appendFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';

import { commitMemoryWrites, readMemoryCommitConfig, snapshotMemory } from '../integration/memory-commit.mjs';
import { createGitWorkspace } from './fixtures/git-workspace.mjs';

async function withWorkspace(options, body) {
  if (typeof options === 'function') { body = options; options = {}; }
  const ws = await createGitWorkspace(options);
  try { await body(ws); } finally { await ws.cleanup(); }
}

const run = (ws, kind = 'harvest') => async () => {
  const snapshot = await snapshotMemory({ wikiRoot: ws.wiki, env: ws.env });
  return { snapshot, commit: (summary = 'test run') => commitMemoryWrites({ snapshot, kind, summary, env: ws.env }) };
};

test('memory_commit: commits exactly the written set and leaves an unrelated staged file staged', async () => {
  await withWorkspace(async (ws) => {
    const { commit } = await run(ws)();
    await ws.write('src/staged.js', 'export const staged = 1;\n');
    ws.git(['add', 'src/staged.js']);
    await ws.write('wiki/topics/new.md', '# New\n');
    await appendFile(join(ws.wiki, 'index.md'), '- [[new]]\n');
    const before = ws.commitCount();
    const result = await commit('2 turns');
    assert.equal(result.memory_commit, 'committed');
    assert.deepEqual([...result.memory_commit_files].sort(), ['wiki/index.md', 'wiki/topics/new.md']);
    assert.equal(ws.commitCount(), before + 1);
    assert.equal(result.memory_commit_hash, ws.head());
    assert.equal(ws.git(['log', '-1', '--format=%s']).trim(), 'loam(harvest): 2 turns');
    assert.deepEqual(ws.git(['diff', '--cached', '--name-only']).trim().split('\n'), ['src/staged.js']);
    assert.deepEqual(ws.git(['show', '--name-only', '--format=', 'HEAD']).trim().split('\n').sort(), ['wiki/index.md', 'wiki/topics/new.md']);
  });
});

test('memory_commit: a written path containing pathspec magic (wiki/topics/[draft].md) is committed literally', async () => {
  await withWorkspace(async (ws) => {
    const { commit } = await run(ws)();
    await ws.write('wiki/topics/[draft].md', '# Draft\n');
    await ws.write('wiki/topics/d.md', '# Decoy\n');
    const result = await commit();
    assert.equal(result.memory_commit, 'committed');
    assert.deepEqual(result.memory_commit_files, ['wiki/topics/[draft].md', 'wiki/topics/d.md'].sort());
    assert.equal(ws.git(['status', '--porcelain']).trim(), '');
  });
});

test('memory_commit: pre-existing dirty files unchanged by the run are not committed', async () => {
  await withWorkspace(async (ws) => {
    await appendFile(join(ws.wiki, 'topics', 'a.md'), 'human edit\n');
    await ws.write('src/a.js', 'export const a = 2;\n');
    const { commit } = await run(ws)();
    await ws.write('wiki/topics/b.md', '# B\n');
    const result = await commit();
    assert.equal(result.memory_commit, 'committed');
    assert.deepEqual(result.memory_commit_files, ['wiki/topics/b.md']);
    assert.match(ws.git(['status', '--porcelain']), / M wiki\/topics\/a\.md/);
    assert.match(ws.git(['status', '--porcelain']), / M src\/a\.js/);
  });
});

test('memory_commit: a run that changes nothing records commit_skipped:no_changes', async () => {
  await withWorkspace(async (ws) => {
    await appendFile(join(ws.wiki, 'log.md'), 'dirty before\n');
    const { commit } = await run(ws)();
    const before = ws.commitCount();
    assert.equal((await commit()).memory_commit, 'commit_skipped:no_changes');
    assert.equal(ws.commitCount(), before);
  });
});

test('memory_commit: a pre-dirty wiki/log.md the run also appended to is committed in full', async () => {
  await withWorkspace(async (ws) => {
    await appendFile(join(ws.wiki, 'log.md'), 'human line\n');
    const { commit } = await run(ws)();
    await appendFile(join(ws.wiki, 'log.md'), 'run line\n');
    const result = await commit();
    assert.equal(result.memory_commit, 'committed');
    assert.deepEqual(result.memory_commit_files, ['wiki/log.md']);
    assert.equal(ws.git(['show', 'HEAD:wiki/log.md']), '# Log\nhuman line\nrun line\n');
    assert.equal(ws.git(['status', '--porcelain']).trim(), '');
  });
});

test('memory_commit: deletions and archive moves are committed', async () => {
  await withWorkspace(async (ws) => {
    const { commit } = await run(ws)();
    await rm(join(ws.wiki, 'topics', 'a.md'));
    await ws.write('wiki/archive/a.md', '# A\n');
    const result = await commit('archive');
    assert.equal(result.memory_commit, 'committed');
    assert.deepEqual(result.memory_commit_files, ['wiki/archive/a.md', 'wiki/topics/a.md']);
    assert.equal(ws.git(['status', '--porcelain']).trim(), '');
    assert.equal(ws.git(['ls-tree', '-r', '--name-only', 'HEAD']).includes('wiki/topics/a.md'), false);
  });
});

test('memory_commit: excluded wiki records commit_skipped:untracked and never runs add or commit', async () => {
  await withWorkspace({ seed: false }, async (ws) => {
    await ws.write('src/a.js', 'x\n');
    ws.git(['add', '-A']);
    ws.git(['-c', 'user.name=S', '-c', 'user.email=s@e.test', 'commit', '-q', '-m', 'seed']);
    await ws.write('.git/info/exclude', 'wiki/\n');
    await ws.write('wiki/index.md', '# I\n');
    const calls = [];
    const git = async (args, options) => {
      calls.push(args);
      const { runGit } = await import('../integration/memory-commit.mjs');
      return runGit(args, options);
    };
    const snapshot = await snapshotMemory({ wikiRoot: ws.wiki, env: ws.env, git });
    await appendFile(join(ws.wiki, 'index.md'), 'more\n');
    const result = await commitMemoryWrites({ snapshot, kind: 'ingest', summary: 's', env: ws.env, git });
    assert.equal(result.memory_commit, 'commit_skipped:untracked');
    assert.equal(calls.some((args) => args.includes('add') || args.includes('commit')), false);
    assert.equal(ws.commitCount(), 1);
  });
});

async function skipCase(ws, expected, arrange, { mutateFirst = true } = {}) {
  const { commit } = await run(ws)();
  if (mutateFirst) await appendFile(join(ws.wiki, 'index.md'), 'run\n');
  await arrange?.();
  const before = ws.commitCount?.();
  const result = await commit();
  assert.equal(result.memory_commit, `commit_skipped:${expected}`, JSON.stringify(result));
  return before;
}

test('memory_commit: merge, rebase, cherry-pick, index lock, missing identity, detached HEAD and non-repo each record a typed skip with zero new commits', async () => {
  for (const [expected, arrange] of [
    ['mid_operation', (ws) => ws.write('.git/MERGE_HEAD', `${ws.head()}\n`)],
    ['mid_operation', (ws) => mkdir(join(ws.root, '.git', 'rebase-merge'), { recursive: true })],
    ['mid_operation', (ws) => mkdir(join(ws.root, '.git', 'rebase-apply'), { recursive: true })],
    ['mid_operation', (ws) => ws.write('.git/CHERRY_PICK_HEAD', `${ws.head()}\n`)],
    ['mid_operation', (ws) => ws.write('.git/REVERT_HEAD', `${ws.head()}\n`)],
    ['locked', (ws) => ws.write('.git/index.lock', '')],
    ['detached_head', (ws) => ws.git(['checkout', '-q', '--detach'])],
  ]) {
    await withWorkspace(async (ws) => {
      const before = ws.commitCount();
      await skipCase(ws, expected, () => arrange(ws));
      await rm(join(ws.root, '.git', 'index.lock'), { force: true });
      assert.equal(ws.commitCount(), before);
    });
  }
  await withWorkspace({ identity: false }, async (ws) => {
    const before = ws.commitCount();
    await skipCase(ws, 'no_identity');
    assert.equal(ws.commitCount(), before);
  });
  const plain = await realpath(await mkdtemp(join(tmpdir(), 'loam-norepo-')));
  try {
    await mkdir(join(plain, 'wiki'));
    await writeFile(join(plain, 'wiki', 'index.md'), '#\n');
    const env = { GIT_CEILING_DIRECTORIES: tmpdir() };
    const snapshot = await snapshotMemory({ wikiRoot: join(plain, 'wiki'), env });
    assert.deepEqual(snapshot, { skip: 'not_a_repo' });
    const result = await commitMemoryWrites({ snapshot, kind: 'harvest', summary: 's', env });
    assert.equal(result.memory_commit, 'commit_skipped:not_a_repo');
  } finally { await rm(plain, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 }); }
});

test('memory_commit: a missing git binary records commit_skipped:git_unavailable', { skip: process.platform === 'win32' }, async () => {
  await withWorkspace(async (ws) => {
    const snapshot = await snapshotMemory({ wikiRoot: ws.wiki, env: { ...ws.env, PATH: '' } });
    assert.deepEqual(snapshot, { skip: 'git_unavailable' });
  });
});

test('memory_commit: an active pre-commit hook records commit_skipped:hooks_present and commit.gpgsign=true records commit_skipped:signing_required, each with zero commits', async () => {
  if (process.platform !== 'win32') {
    await withWorkspace(async (ws) => {
      await ws.addHook('pre-commit', 'exit 0');
      const before = ws.commitCount();
      await skipCase(ws, 'hooks_present');
      assert.equal(ws.commitCount(), before);
    });
    await withWorkspace(async (ws) => {
      await ws.addHook('commit-msg', 'exit 0');
      await skipCase(ws, 'hooks_present');
    });
    await withWorkspace(async (ws) => {
      await ws.addHook('pre-commit.sample', 'exit 1');
      const { commit } = await run(ws)();
      await appendFile(join(ws.wiki, 'index.md'), 'x\n');
      assert.equal((await commit()).memory_commit, 'committed');
    });
  }
  await withWorkspace(async (ws) => {
    ws.git(['config', 'commit.gpgsign', 'true']);
    const before = ws.commitCount();
    await skipCase(ws, 'signing_required');
    assert.equal(ws.commitCount(), before);
  });
});

test('memory_commit: linked worktree commit lands only on that worktree branch', async () => {
  await withWorkspace(async (ws) => {
    const dir = await ws.tempDir('linked');
    const tree = join(dir, 'tree');
    ws.git(['worktree', 'add', '-q', '-b', 'feat', tree]);
    const wikiRoot = join(tree, 'wiki');
    const snapshot = await snapshotMemory({ wikiRoot, env: ws.env });
    await writeFile(join(wikiRoot, 'topics', 'w.md'), '# W\n');
    const mainBefore = ws.git(['rev-parse', 'main']).trim();
    const result = await commitMemoryWrites({ snapshot, kind: 'ingest', summary: 'linked', env: ws.env });
    assert.equal(result.memory_commit, 'committed', JSON.stringify(result));
    assert.equal(ws.git(['rev-parse', 'main']).trim(), mainBefore);
    assert.equal(ws.git(['rev-parse', 'feat']).trim(), result.memory_commit_hash);
  });
});

test('memory_commit: a failing commit reports commit_skipped:commit_failed and does not leave the written paths staged', async () => {
  await withWorkspace(async (ws) => {
    const { commit } = await run(ws)();
    await ws.write('wiki/topics/n.md', '# N\n');
    const calls = [];
    const { runGit } = await import('../integration/memory-commit.mjs');
    const snapshot = await snapshotMemory({ wikiRoot: ws.wiki, env: ws.env });
    await ws.write('wiki/topics/n2.md', '# N2\n');
    const git = async (args, options) => {
      calls.push(args);
      if (args.includes('commit')) return { code: 1, stdout: '', stderr: 'fatal: boom', timedOut: false };
      return runGit(args, options);
    };
    const result = await commitMemoryWrites({ snapshot, kind: 'harvest', summary: 's', env: ws.env, git });
    assert.equal(result.memory_commit, 'commit_skipped:commit_failed');
    assert.equal(ws.git(['diff', '--cached', '--name-only']).trim(), '');
    void commit;
  });
});

test('memory_commit: git timeouts and index.lock stderr map to timeout and locked', async () => {
  await withWorkspace(async (ws) => {
    const { runGit } = await import('../integration/memory-commit.mjs');
    for (const [fake, expected] of [
      [{ code: null, stdout: '', stderr: '', timedOut: true }, 'timeout'],
      [{ code: 128, stdout: '', stderr: "fatal: Unable to create '/x/.git/index.lock': File exists.", timedOut: false }, 'locked'],
    ]) {
      const snapshot = await snapshotMemory({ wikiRoot: ws.wiki, env: ws.env });
      await ws.write(`wiki/topics/${expected}.md`, 'x\n');
      const git = async (args, options) => (args.includes('add') ? fake : runGit(args, options));
      const result = await commitMemoryWrites({ snapshot, kind: 'harvest', summary: 's', env: ws.env, git });
      assert.equal(result.memory_commit, `commit_skipped:${expected}`);
      ws.git(['reset', '-q']);
    }
  });
});

test('memory_commit: disabled by config or LOAM_MEMORY_COMMITS=0 spawns no git and leaves .git/index byte-identical', async () => {
  await withWorkspace(async (ws) => {
    const globalRoot = await ws.tempDir('global');
    await writeFile(join(globalRoot, 'config.json'), JSON.stringify({ memory_commits: { enabled: false } }));
    const indexBefore = await readFile(join(ws.root, '.git', 'index'));
    let spawned = 0;
    const git = async () => { spawned += 1; return { code: 0, stdout: '', stderr: '' }; };
    for (const [root, env] of [[globalRoot, ws.env], [undefined, { ...ws.env, LOAM_MEMORY_COMMITS: '0' }]]) {
      const snapshot = await snapshotMemory({ wikiRoot: ws.wiki, globalRoot: root, env, git });
      assert.deepEqual(snapshot, { enabled: false });
      await appendFile(join(ws.wiki, 'index.md'), 'x\n');
      const result = await commitMemoryWrites({ snapshot, kind: 'harvest', summary: 's', env, git });
      assert.deepEqual(result, { memory_commit: 'disabled' });
    }
    assert.equal(spawned, 0);
    assert.deepEqual(await readFile(join(ws.root, '.git', 'index')), indexBefore);
  });
});

test('memory_commit: readMemoryCommitConfig precedence: env override, then config, then default on', async () => {
  const root = await realpath(await mkdtemp(join(tmpdir(), 'loam-mc-config-')));
  try {
    assert.deepEqual(await readMemoryCommitConfig(root, {}), { enabled: true });
    await writeFile(join(root, 'config.json'), JSON.stringify({ memory_commits: { enabled: false } }));
    assert.equal((await readMemoryCommitConfig(root, {})).enabled, false);
    assert.equal((await readMemoryCommitConfig(root, { LOAM_MEMORY_COMMITS: '1' })).enabled, true);
    await writeFile(join(root, 'config.json'), JSON.stringify({ memory_commits: { enabled: true } }));
    assert.equal((await readMemoryCommitConfig(root, { LOAM_MEMORY_COMMITS: '0' })).enabled, false);
    await writeFile(join(root, 'config.json'), '{not json');
    assert.equal((await readMemoryCommitConfig(root, {})).enabled, true);
  } finally { await rm(root, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 }); }
});
