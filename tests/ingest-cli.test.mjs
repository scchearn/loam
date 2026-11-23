import assert from 'node:assert/strict';
import { mkdir, mkdtemp, readFile, realpath, rm, symlink, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';

import { main } from '../bin/loam.mjs';
import { parseArgs } from '../setup/args.mjs';

function sink() {
  let text = '';
  return { write(chunk) { text += chunk; return true; }, get text() { return text; } };
}

// Hermetic env: process.env minus LOAM_* / HCOM_*, config dir and home pinned.
async function fixture() {
  const root = await realpath(await mkdtemp(join(tmpdir(), 'loam-ingest-cli-')));
  const home = join(root, 'home');
  const configDir = join(root, 'config');
  const ws = join(root, 'ws');
  await mkdir(home, { recursive: true });
  await mkdir(ws, { recursive: true });
  const env = {};
  for (const [key, value] of Object.entries(process.env)) {
    if (!/^(LOAM_|HCOM_)/.test(key)) env[key] = value;
  }
  Object.assign(env, { HOME: home, USERPROFILE: home, LOAM_CONFIG_DIR: configDir });
  const configPath = join(configDir, 'config.json');
  const run = async (argv, cwd = ws) => {
    const out = sink();
    const err = sink();
    const code = await main(argv, out, err, { env, cwd, home });
    return { code, out: out.text, err: err.text };
  };
  const readConfig = async () => JSON.parse(await readFile(configPath, 'utf8'));
  const cleanup = () => rm(root, { recursive: true, force: true });
  return { root, home, configDir, configPath, ws, env, run, readConfig, cleanup };
}

test('parseArgs accepts ingest subcommands, path, and --json', () => {
  assert.deepEqual(parseArgs(['ingest', 'enable']), { command: 'ingest', action: 'enable', path: null, json: false, dryRun: false, yes: false, purge: false });
  assert.equal(parseArgs(['ingest', 'disable', '/x']).path, '/x');
  assert.equal(parseArgs(['ingest', 'status', '--json']).json, true);
  assert.throws(() => parseArgs(['ingest']), /enable\|disable\|status/);
  assert.throws(() => parseArgs(['ingest', 'bogus']), /enable\|disable\|status/);
  assert.throws(() => parseArgs(['ingest', 'enable', '--json']), /unknown option/);
  assert.throws(() => parseArgs(['ingest', 'enable', 'a', 'b']), /one path/);
});

test('enable with no path stores the canonical cwd', async () => {
  const f = await fixture();
  try {
    const link = join(f.root, 'link');
    await symlink(f.ws, link);
    const r = await f.run(['ingest', 'enable'], link);
    assert.equal(r.code, 0, r.err);
    assert.deepEqual((await f.readConfig()).background_ingest.workspaces, [f.ws]);
  } finally { await f.cleanup(); }
});

test('enable <path> stores absolute canonical form, idempotently', async () => {
  const f = await fixture();
  try {
    await mkdir(join(f.ws, 'sub'));
    assert.equal((await f.run(['ingest', 'enable', 'sub'])).code, 0);
    const again = await f.run(['ingest', 'enable', join(f.ws, 'sub')]);
    assert.equal(again.code, 0);
    assert.match(again.out, /already/);
    assert.deepEqual((await f.readConfig()).background_ingest.workspaces, [join(f.ws, 'sub')]);
  } finally { await f.cleanup(); }
});

test('disable removes the entry; absent is a no-op with a message', async () => {
  const f = await fixture();
  try {
    await f.run(['ingest', 'enable']);
    const gone = await f.run(['ingest', 'disable']);
    assert.equal(gone.code, 0);
    assert.deepEqual((await f.readConfig()).background_ingest.workspaces, []);
    const noop = await f.run(['ingest', 'disable']);
    assert.equal(noop.code, 0);
    assert.match(noop.out, /not enabled|no entry/i);
  } finally { await f.cleanup(); }
});

test('disable on a missing config does not create one', async () => {
  const f = await fixture();
  try {
    const r = await f.run(['ingest', 'disable']);
    assert.equal(r.code, 0);
    await assert.rejects(readFile(f.configPath, 'utf8'));
  } finally { await f.cleanup(); }
});

test('enable refuses the filesystem root and the home dir', async () => {
  const f = await fixture();
  try {
    const root = await f.run(['ingest', 'enable', '/']);
    assert.notEqual(root.code, 0);
    assert.match(root.err, /filesystem root/);
    const home = await f.run(['ingest', 'enable', '~']);
    assert.notEqual(home.code, 0);
    assert.match(home.err, /home/);
    await assert.rejects(readFile(f.configPath, 'utf8'));
  } finally { await f.cleanup(); }
});

test('a malformed existing config.json is refused and left untouched', async () => {
  const f = await fixture();
  try {
    await mkdir(f.configDir, { recursive: true });
    await writeFile(f.configPath, '{ not json');
    for (const verb of ['enable', 'disable']) {
      const r = await f.run(['ingest', verb]);
      assert.notEqual(r.code, 0);
      assert.match(r.err, /malformed/);
    }
    assert.equal(await readFile(f.configPath, 'utf8'), '{ not json');
  } finally { await f.cleanup(); }
});

test('other keys survive enable and disable', async () => {
  const f = await fixture();
  try {
    await mkdir(f.configDir, { recursive: true });
    const original = {
      org: 'acme', broker: { host: 'h', port: 8883 },
      background_ingest: { enabled: false, visibility: 'toast', timeout_seconds: 5, workspaces: ['/keep/me'] },
    };
    await writeFile(f.configPath, JSON.stringify(original));
    await f.run(['ingest', 'enable']);
    const added = await f.readConfig();
    assert.equal(added.org, 'acme');
    assert.deepEqual(added.broker, original.broker);
    assert.equal(added.background_ingest.visibility, 'toast');
    assert.equal(added.background_ingest.timeout_seconds, 5);
    assert.equal(added.background_ingest.enabled, false);
    assert.deepEqual(added.background_ingest.workspaces, ['/keep/me', f.ws]);
    await f.run(['ingest', 'disable']);
    const removed = await f.readConfig();
    assert.deepEqual(removed.background_ingest.workspaces, ['/keep/me']);
    assert.equal(removed.org, 'acme');
  } finally { await f.cleanup(); }
});

test('malformed sections and empty files', async () => {
  const f = await fixture();
  try {
    await mkdir(f.configDir, { recursive: true });
    for (const body of ['{"background_ingest":"oops","x":1}', '{"background_ingest":{"workspaces":"no"}}']) {
      await writeFile(f.configPath, body);
      for (const verb of ['enable', 'disable']) {
        const r = await f.run(['ingest', verb]);
        assert.notEqual(r.code, 0);
        assert.match(r.err, /malformed/);
      }
      assert.equal(await readFile(f.configPath, 'utf8'), body);
    }
    await writeFile(f.configPath, '  \n');
    const ok = await f.run(['ingest', 'enable']);
    assert.equal(ok.code, 0, ok.err);
    assert.deepEqual((await f.readConfig()).background_ingest.workspaces, [f.ws]);
  } finally { await f.cleanup(); }
});

test('enable warns when ingest stays off; disable names a covering entry', async () => {
  const f = await fixture();
  try {
    await mkdir(f.configDir, { recursive: true });
    await writeFile(f.configPath, JSON.stringify({ background_ingest: { enabled: false } }));
    const r = await f.run(['ingest', 'enable']);
    assert.equal(r.code, 0, r.err);
    assert.match(r.out, /warning: ingest is still off/);
    assert.match(r.out, /enabled_by: config_off/);
    const again = await f.run(['ingest', 'enable']);
    assert.match(again.out, /already enabled[\s\S]*warning/);
    await writeFile(f.configPath, JSON.stringify({ background_ingest: { workspaces: [f.ws] } }));
    await mkdir(join(f.ws, 'sub'));
    const d = await f.run(['ingest', 'disable'], join(f.ws, 'sub'));
    assert.equal(d.code, 0);
    assert.match(d.out, /covering allowlist entry/);
    assert.match(d.out, new RegExp(f.ws.replace(/[\\^$.*+?()[\]{}|]/g, '\\$&')));
  } finally { await f.cleanup(); }
});

test('enable refuses an ancestor of home', async () => {
  const f = await fixture();
  try {
    const r = await f.run(['ingest', 'enable', f.root]);
    assert.notEqual(r.code, 0);
    assert.match(r.err, /home/);
    await assert.rejects(readFile(f.configPath, 'utf8'));
  } finally { await f.cleanup(); }
});

test('status honours LOAM_HOME for the legacy kill switch', async () => {
  const f = await fixture();
  try {
    const lh = join(f.root, 'lh');
    await mkdir(lh);
    await writeFile(join(lh, 'config.json'), JSON.stringify({ background_ingest: { enabled: false } }));
    await mkdir(f.configDir, { recursive: true });
    await writeFile(f.configPath, JSON.stringify({ background_ingest: { workspaces: [f.ws] } }));
    f.env.LOAM_HOME = lh;
    const r = await f.run(['ingest', 'status', '--json']);
    assert.equal(JSON.parse(r.out).enabled, false);
  } finally { await f.cleanup(); }
});

test('status reports enablement as text and --json', async () => {
  const f = await fixture();
  try {
    const off = await f.run(['ingest', 'status']);
    assert.equal(off.code, 0, off.err);
    assert.match(off.out, /enabled: false/);
    assert.match(off.out, /enabled_by: default_off/);
    await f.run(['ingest', 'enable']);
    const on = await f.run(['ingest', 'status', '--json']);
    const body = JSON.parse(on.out);
    assert.equal(body.enabled, true);
    assert.equal(body.enabled_by, 'allowlist');
    assert.equal(body.config_path, f.configPath);
    assert.equal(body.config_error, null);
    assert.deepEqual(body.ignored_entries, []);
    const text = await f.run(['ingest', 'status']);
    assert.match(text.out, /config_path:/);
    assert.match(text.out, /config_error:/);
    assert.match(text.out, /ignored_entries:/);
  } finally { await f.cleanup(); }
});

test('status surfaces config_error and ignored entries for a bad file', async () => {
  const f = await fixture();
  try {
    await mkdir(f.configDir, { recursive: true });
    await writeFile(f.configPath, JSON.stringify({ background_ingest: { workspaces: ['relative'] } }));
    const r = await f.run(['ingest', 'status', '--json']);
    assert.deepEqual(JSON.parse(r.out).ignored_entries, ['relative']);
    await writeFile(f.configPath, '{ bad');
    const bad = await f.run(['ingest', 'status', '--json']);
    assert.equal(bad.code, 0);
    assert.ok(JSON.parse(bad.out).config_error);
  } finally { await f.cleanup(); }
});

test('status [path] inspects another path', async () => {
  const f = await fixture();
  try {
    await f.run(['ingest', 'enable']);
    const other = join(f.root, 'other');
    await mkdir(other);
    const r = await f.run(['ingest', 'status', other, '--json']);
    assert.equal(JSON.parse(r.out).enabled, false);
  } finally { await f.cleanup(); }
});

test('ingest --help prints help', async () => {
  const out = sink();
  const code = await main(['ingest', '--help'], out, sink());
  assert.equal(code, 0);
  assert.match(out.text, /ingest enable\|disable\|status/);
});
