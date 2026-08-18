import assert from 'node:assert/strict';
import { execSync } from 'node:child_process';
import { chmod, mkdtemp, readFile, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { test } from 'node:test';

import { renderPluginHooks } from '../setup/harnesses.mjs';

const packageRoot = fileURLToPath(new URL('..', import.meta.url));
const NATIVE_EVENTS = ['SessionStart', 'UserPromptSubmit', 'PreToolUse', 'PostToolUse'];

async function shippedHooks() {
  return JSON.parse(await readFile(join(packageRoot, 'plugins', 'loam-adapter', 'hooks', 'hooks.json'), 'utf8'));
}

function nativeEntries(rendered) {
  return NATIVE_EVENTS.flatMap((event) => (rendered.hooks[event] || []).flatMap((group) => group.hooks || []));
}

// (a) Claude/Codex take `command` as ONE shell string and ignore `args`/`async`;
// an args-array entry ran the bare binary with no subcommand (#133). Every
// native entry must be a single-string command carrying no `args`/`async`.
test('#133 native plugin entries are single-string commands with no args/async', async () => {
  const rendered = renderPluginHooks('/opt/loam/bin/loam', 'claude', await shippedHooks());
  const entries = nativeEntries(rendered);
  assert.equal(entries.length, NATIVE_EVENTS.length);
  for (const entry of entries) {
    assert.equal(entry.type, 'command');
    assert.equal(typeof entry.command, 'string');
    assert.equal(entry.args, undefined, 'no args field Claude would drop');
    assert.equal(entry.async, undefined, 'no async field Claude does not honor');
    assert.ok(entry.command.includes(' hook claude --event '), entry.command);
  }
});

// (b) The rewrite must stay a SUPERSET of the shipped hooks: a prior wholesale
// replace dropped SubagentStart/SubagentStop and forked Stop's timeout (#132).
test('#132 rendered hooks are a superset of the shipped hooks, verbatim', async () => {
  const shipped = await shippedHooks();
  const rendered = renderPluginHooks('/opt/loam/bin/loam', 'claude', shipped);
  for (const event of Object.keys(shipped.hooks)) {
    assert.ok(rendered.hooks[event], `shipped event ${event} survives the rewrite`);
  }
  // Shipped-owned events pass through untouched (Stop keeps its 90s budget).
  assert.deepEqual(rendered.hooks.SubagentStart, shipped.hooks.SubagentStart);
  assert.deepEqual(rendered.hooks.SubagentStop, shipped.hooks.SubagentStop);
  assert.deepEqual(rendered.hooks.Stop, shipped.hooks.Stop);
  assert.equal(rendered.hooks.Stop[0].hooks[0].timeout, 90, 'one authoritative Stop timeout');
});

// (c) The runtime path is double-quoted so a darwin store under "Application
// Support" survives the shell split — the reason someone reached for args[].
test('#133 the rendered command quotes a runtime path containing spaces', async () => {
  const spaced = '/Users/x/Library/Application Support/loam/runtime/bin/loam';
  const rendered = renderPluginHooks(spaced, 'codex', await shippedHooks());
  const [entry] = nativeEntries(rendered);
  // The renderer resolve()s the path, so build the expected the same way — on
  // windows resolve() drive-prefixes and backslashes it; a literal POSIX string
  // would only match on POSIX.
  assert.equal(entry.command, `"${resolve(spaced)}" hook codex --event SessionStart`);
});

// (d) The verify blind spot kula found: verify only checked its own JSON. Prove
// the rendered command STRING actually executes the runtime with the subcommand
// — through a real shell, against a runtime at a spaced path. Pre-fix (bare
// command, args dropped) the runtime would see no args and this fails.
// POSIX-only: the fake runtime is a `#!/bin/sh` script made executable via
// chmod, neither of which is meaningful on windows; the quoting/schema the
// exec proves is covered cross-platform by tests (a)-(c) above.
test('#133 the rendered command runs the runtime with its subcommand through a shell', { skip: process.platform === 'win32' }, async () => {
  const dir = await mkdtemp(join(tmpdir(), 'loam hook schema ')); // a space in the path on purpose
  const fakeRuntime = join(dir, 'loam');
  // Echo argv so the assertion can see the subcommand actually arrived.
  await writeFile(fakeRuntime, '#!/bin/sh\nprintf \'RAN:%s\\n\' "$*"\n');
  await chmod(fakeRuntime, 0o755);

  const rendered = renderPluginHooks(fakeRuntime, 'claude', await shippedHooks());
  const [entry] = nativeEntries(rendered);
  const stdout = execSync(entry.command, { encoding: 'utf8' });
  assert.equal(stdout.trim(), 'RAN:hook claude --event SessionStart', 'the runtime ran with its full subcommand');
  assert.notEqual(stdout.trim(), 'RAN:', 'a bare-command exec (the #133 bug) would produce empty args');
});
