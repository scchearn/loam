import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { chmod, mkdir, mkdtemp, readFile, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { test } from 'node:test';

import { runtimeStorePath, writeLedger } from '../integration/ledger.mjs';

const hooksDir = fileURLToPath(new URL('../plugins/loam-adapter/hooks/', import.meta.url));
const integrationLoam = fileURLToPath(new URL('../integration/loam.mjs', import.meta.url));

async function shippedHooks() {
  return JSON.parse(await readFile(join(hooksDir, 'hooks.json'), 'utf8'));
}

function entries(hooks, event) {
  return (hooks.hooks[event] || []).flatMap((group) => group.hooks || []);
}

// (a) The SHIPPED hooks.json is what Claude/Codex actually load (#137, kula:
// hooks come from the marketplace SOURCE, not the staged cache). The three read
// surfaces are node shims; Stop carries both the ingestion boundary and the wake
// long-poll; the subagent hooks survive.
test('#137 the shipped hooks.json carries the three-surface shims and the full set', async () => {
  const hooks = await shippedHooks();
  for (const [event, file] of [['SessionStart', 'session-start.mjs'], ['UserPromptSubmit', 'user-prompt-submit.mjs']]) {
    const shims = entries(hooks, event);
    assert.equal(shims.length, 1, event);
    assert.equal(shims[0].type, 'command');
    assert.equal(shims[0].command, `node "\${CLAUDE_PLUGIN_ROOT}/hooks/${file}"`);
    assert.equal(shims[0].args, undefined, `${event} shim carries no args`);
  }
  // SessionStart must match a forked session, not only startup/resume (#137 doc-verify).
  assert.match(hooks.hooks.SessionStart[0].matcher, /(^|\|)fork(\||$)/);
  const stopJson = JSON.stringify(entries(hooks, 'Stop'));
  assert.match(stopJson, /stop\.mjs/, 'Stop keeps the ingestion boundary');
  assert.match(stopJson, /wake\.mjs/, 'Stop carries the wake long-poll');
  for (const shipped of ['SubagentStart', 'SubagentStop']) {
    assert.ok(Array.isArray(hooks.hooks[shipped]), `${shipped} present`);
  }
});

// (b) The kula blind spot: verify only ever checked JSON, never RAN the shim. Run
// the real session-start.mjs as a subprocess against a fixture ledger whose
// runtime is at a SPACED path; it must resolve that runtime, exec it with the
// `hook <harness> --event SessionStart ...` subcommand, and forward its envelope.
test('#137 the session-start shim resolves the ledger runtime and forwards its envelope', { skip: process.platform === 'win32' }, async () => {
  const base = await mkdtemp(join(tmpdir(), 'loam carried hooks ')); // a space on purpose
  const configDir = join(base, 'cfg');
  const globalRoot = join(base, 'home', '.agents', 'loam');
  await mkdir(globalRoot, { recursive: true });
  const version = '0.11.0';
  const target = 'x86_64-unknown-linux-gnu';
  const store_path = runtimeStorePath({ version, target, root: configDir });
  await mkdir(join(configDir, 'runtime', version, target), { recursive: true });
  // A fake runtime that echoes its argv as the "envelope" so the assertion sees
  // both that it ran and the subcommand it was given.
  await writeFile(store_path, '#!/bin/sh\nprintf \'ENVELOPE:%s\\n\' "$*"\n');
  await chmod(store_path, 0o755);
  await writeLedger({ channel: 'next', target: version, sha256: 'a'.repeat(64), store_path }, { root: configDir });

  const stdout = execFileSync(process.execPath, [join(hooksDir, 'session-start.mjs')], {
    input: JSON.stringify({ cwd: base, session_id: 's1' }),
    env: { ...process.env, LOAM_CONFIG_DIR: configDir, LOAM_HOME: globalRoot, LOAM_INTEGRATION_PATH: integrationLoam, PLUGIN_ROOT: '' },
    encoding: 'utf8',
  });
  assert.match(stdout, /^ENVELOPE:hook claude --event SessionStart\b/, 'the runtime ran with its subcommand from a spaced path');
});

// (c) When nothing resolves (no ledger, no install), the session-start shim
// soft-degrades to the repair hint additionalContext rather than a broken
// session — run the real shim end to end, not an injected seam.
test('#137 the session-start shim soft-degrades to the repair hint when no runtime resolves', async () => {
  const base = await mkdtemp(join(tmpdir(), 'loam-carried-degrade-'));
  const configDir = join(base, 'cfg'); // empty: no ledger
  const globalRoot = join(base, 'agents'); // empty: no install.json
  await mkdir(configDir, { recursive: true });
  await mkdir(globalRoot, { recursive: true });

  const stdout = execFileSync(process.execPath, [join(hooksDir, 'session-start.mjs')], {
    input: JSON.stringify({ cwd: base, session_id: 's1' }),
    env: { ...process.env, LOAM_CONFIG_DIR: configDir, LOAM_HOME: globalRoot, LOAM_INTEGRATION_PATH: integrationLoam, PLUGIN_ROOT: '' },
    encoding: 'utf8',
  });
  const parsed = JSON.parse(stdout);
  assert.equal(parsed.hookSpecificOutput.hookEventName, 'SessionStart');
  assert.match(parsed.hookSpecificOutput.additionalContext, /runtime failed to load[\s\S]*npx @scchearn\/loam install/);
});
