import assert from 'node:assert/strict';
import { copyFile, mkdir, readFile, readdir, rm, writeFile } from 'node:fs/promises';
import { mkdtemp, realpath } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

import {
  harvestPublicReason, harvestStatePath, pruneHarvestSessions,
  readHarvestConfig, readHarvestState, writeHarvestState,
} from '../integration/harvest-state.mjs';
import { groupExchanges, normalizeToolName, renderWindow, setToolError, writeWindow } from '../integration/harvest-window.mjs';
import { HarvestError, detectRotation, measureStore, parseLines, readTail } from '../integration/harvest-store.mjs';
import { chmod } from 'node:fs/promises';
import { delimiter } from 'node:path';
import { launchModel, PROMPT as INGEST_PROMPT } from '../integration/ingest.mjs';
import { parseClaudeStore, locateClaudeStore } from '../integration/harvest-claude.mjs';
import { parseCodexStore, locateCodexStore } from '../integration/harvest-codex.mjs';
import { readOpenCodeWindow, locateOpenCodeStore, openCodeMeasure } from '../integration/harvest-opencode.mjs';
import { harvestRecursion, harvestTick, startHarvestWorker, runHarvest } from '../integration/harvest.mjs';
import { harvestLastRunPath, readHarvestState as readHarvestStateFile } from '../integration/harvest-state.mjs';
import { acquireLease, releaseLease } from '../integration/ingest.mjs';

// mkdtemp under os.tmpdir() can land behind a symlink (macOS: /var/folders -> /private/var/folders);
// canonicalWorkspace() in the implementation resolves through realpath, so tests must start from an
// already-resolved temp dir or writes and reads disagree on the hashed state path.
async function mkdtempCanonical(prefix) {
  return realpath(await mkdtemp(join(tmpdir(), prefix)));
}

function userRecord(cursor, text, sessionId = 's', timestamp = '2026-08-07T08:00:00Z') {
  return { cursor, kind: 'user', session_id: sessionId, text, timestamp };
}
function assistantRecord(cursor, text, timestamp = '2026-08-07T08:00:01Z') {
  return { cursor, kind: 'assistant', text, timestamp };
}
function toolUseRecord(cursor, toolUseId, name, input = {}) {
  return { cursor, kind: 'tool_use', session_id: 's', tool_use_id: toolUseId, name, input };
}
function toolResultRecord(cursor, toolUseId, isError = false, content = 'ok') {
  return { cursor, kind: 'tool_result', session_id: 's', tool_use_id: toolUseId, is_error: isError, content };
}

const FIXTURES = new URL('./fixtures/harvest/', import.meta.url);

async function fixture(name) {
  return readFile(new URL(name, FIXTURES), 'utf8');
}

test('window_prompt_contract: fixture headers, turn anchoring, tool-output cap, and prompt references', async () => {
  const window = await fixture('window-sample.md');
  const prompt = await fixture('prompt-contract.txt');

  const header = window.match(/^---\n([\s\S]*?)\n---/)?.[1] ?? '';
  const fields = Object.fromEntries(
    header.split('\n').map((line) => {
      const match = line.match(/^([a-z_]+):\s*(.+)$/);
      return match ? [match[1], match[2]] : null;
    }).filter(Boolean),
  );
  for (const key of ['harness', 'session_id', 'workspace', 'turns', 'window_start', 'window_end']) {
    assert.ok(key in fields, `window fixture must declare ${key}`);
  }
  assert.equal(fields.harness, 'claude');
  assert.ok(Number(fields.turns) > 0, 'turns must be a positive integer');

  const turnBlocks = [...window.matchAll(/^## Turn (\d+)$/gm)].map((match) => Number(match[1]));
  assert.ok(turnBlocks.length >= 1, 'window must contain at least one turn block');
  assert.deepEqual(turnBlocks, [...Array(turnBlocks.length).keys()].map((i) => i + 1), 'turns must be numbered 1..N in order');

  const body = window.split(/^---$/m).slice(2).join('\n');
  for (const turn of turnBlocks) {
    const block = body.match(new RegExp(`## Turn ${turn}\\n([\\s\\S]*?)(?=\\n## Turn |$)`))?.[1] ?? '';
    assert.ok(/### User\n/.test(block), `turn ${turn} must be user-anchored`);
  }

  for (const line of body.split('\n')) {
    const output = line.match(/^ {2}output: (.*)$/)?.[1];
    if (output) assert.ok(output.length <= 1000, `tool output must not exceed 1000 bytes: ${output.length}`);
  }

  const windowPathMentions = prompt.split('Window file:').length - 1;
  assert.equal(windowPathMentions, 1, 'prompt must name the window path exactly once');
  const skillMentions = prompt.match(/loam::learning-from-session/g)?.length ?? 0;
  assert.equal(skillMentions, 1, 'prompt must name the routing skill exactly once');
  assert.ok(prompt.includes('/home/user/projects/example-app'), 'prompt must name the workspace');
});

test('harvest_config_and_state: absent config defaults on; opt-outs disable and write nothing', async () => {
  const root = await mkdtempCanonical('loam-harvest-config-');
  const globalRoot = join(root, 'global');
  await mkdir(globalRoot, { recursive: true });
  const workspace = join(root, 'workspace');
  await mkdir(workspace, { recursive: true });

  const absent = await readHarvestConfig(globalRoot, {});
  assert.equal(absent.enabled, true, 'absent background_harvest section must default enabled: true');
  assert.equal(absent.threshold_turns, 8);
  assert.equal(absent.threshold_conversation_bytes, 16384);
  assert.equal(absent.min_interval_seconds, 900);
  assert.equal(absent.tool_output_bytes, 1000);

  await writeFile(join(globalRoot, 'config.json'), JSON.stringify({ background_harvest: { enabled: false } }));
  const disabled = await readHarvestConfig(globalRoot, {});
  assert.equal(disabled.enabled, false);

  const envOff = await readHarvestConfig(globalRoot, { LOAM_HARVEST_BACKGROUND: '0' });
  assert.equal(envOff.enabled, false);
  const envOn = await readHarvestConfig(globalRoot, { LOAM_HARVEST_BACKGROUND: '1' });
  assert.equal(envOn.enabled, true);

  const statePath = harvestStatePath(globalRoot, workspace, 'session-a');
  const sepPattern = process.platform === 'win32' ? '\\\\' : '/';
  assert.match(statePath, new RegExp(`run${sepPattern}[0-9a-f]{16}${sepPattern}harvest${sepPattern}[0-9a-f]{16}\\.json$`));
  await writeHarvestState(globalRoot, workspace, 'session-a', { session_id: 'session-a', harness: 'claude', workspace });
  assert.ok(!statePath.includes('..'), 'state path must stay inside the global root');
  const listed = await readdir(join(globalRoot, 'run'));
  assert.equal(listed.length, 1, 'state writes must live under one runRoot');

  const disabledGlobal = await mkdtempCanonical('loam-harvest-off-');
  await readHarvestConfig(disabledGlobal, { LOAM_HARVEST_BACKGROUND: '0' });
  const offRun = join(disabledGlobal, 'run');
  await assert.rejects(() => readdir(offRun), { code: 'ENOENT' }, 'an opted-out install must create no runRoot');
});

test('harvest_config_and_state: numeric env overrides clamp, state round-trips, unknown schema refused', async () => {
  const root = await mkdtempCanonical('loam-harvest-state-');
  const globalRoot = join(root, 'global');
  const workspace = join(root, 'workspace');
  await mkdir(workspace, { recursive: true });

  const clamped = await readHarvestConfig(globalRoot, {
    LOAM_HARVEST_THRESHOLD_TURNS: 'nan',
    LOAM_HARVEST_THRESHOLD_BYTES: '-5',
    LOAM_HARVEST_MIN_INTERVAL: '42',
  });
  assert.equal(clamped.threshold_turns, 8, 'non-numeric env must fall back');
  assert.equal(clamped.threshold_conversation_bytes, 16384, 'negative env must fall back');
  assert.equal(clamped.min_interval_seconds, 42);

  const state = {
    schema: 1, session_id: 'session-b', harness: 'codex', workspace,
    store: { path: '/tmp/store.jsonl', kind: 'jsonl', size: 100, mtime_ms: 1 },
    cursor: { kind: 'bytes', value: 42, updated_at: 2 },
  };
  await writeHarvestState(globalRoot, workspace, 'session-b', state);
  const read = await readHarvestState(globalRoot, workspace, 'session-b');
  assert.deepEqual(read, state);

  await writeFile(harvestStatePath(globalRoot, workspace, 'session-b'), JSON.stringify({ schema: 2 }) + '\n');
  await assert.rejects(() => writeHarvestState(globalRoot, workspace, 'session-b', state), /schema/);
});

test('harvest_config_and_state: pruning is bounded by count and age and tolerates missing files', async () => {
  const root = await mkdtempCanonical('loam-harvest-prune-');
  const globalRoot = join(root, 'global');
  const workspace = join(root, 'workspace');
  await mkdir(workspace, { recursive: true });
  const old = Date.now() - 20 * 24 * 3600 * 1000;
  const first = harvestStatePath(globalRoot, workspace, 'sess-0');
  for (let i = 0; i < 5; i += 1) {
    const path = harvestStatePath(globalRoot, workspace, `sess-${i}`);
    await writeHarvestState(globalRoot, workspace, `sess-${i}`, { schema: 1, session_id: `sess-${i}`, workspace });
    const { utimes } = await import('node:fs/promises');
    await utimes(path, new Date(old + i), new Date(old + i));
  }
  await pruneHarvestSessions(globalRoot, workspace, { retain_sessions: 3, retain_session_days: 14 });
  const remaining = await readdir(join(first, '..'));
  assert.equal(remaining.length, 3, 'retain_sessions bounds the file count');
  await assert.doesNotReject(() => pruneHarvestSessions(globalRoot, 'no-such-workspace', { retain_sessions: 3, retain_session_days: 14 }));
});

test('harvest_config_and_state: every taxonomy reason maps to exactly one public category', () => {
  const expected = {
    disabled: 'disabled', recursion: 'disabled',
    lease_held: 'busy', orphan_live: 'busy', orphan_unknown: 'busy',
    debounced: 'too_soon', backoff: 'too_soon',
    wiki_missing: 'nothing_to_do', below_threshold: 'nothing_to_do',
    nothing_durable: 'nothing_to_do', foreign_workspace: 'nothing_to_do',
    store_missing: 'unavailable', store_unreadable: 'unavailable',
    session_unknown: 'unavailable', sqlite_unavailable: 'unavailable', schema_unknown: 'unavailable',
    ok: 'ok',
  };
  for (const [reason, category] of Object.entries(expected)) {
    assert.equal(harvestPublicReason(reason), category, `${reason} must map to ${category}`);
  }
  assert.equal(harvestPublicReason('anything-else'), 'unavailable');
});

test('harvest_window: grouping is user-anchored, the trailing incomplete turn is excluded, and its cursor returned', () => {
  const records = [
    userRecord(0, 'first user turn'),
    assistantRecord(1, 'first assistant reply'),
    userRecord(2, 'second user turn'),
    assistantRecord(3, 'second assistant reply'),
    assistantRecord(4, 'trailing assistant without a following user'),
  ];
  const { exchanges, boundaryCursor, truncated } = groupExchanges(records, { maxTurns: 40, maxBytes: 262144 });
  assert.equal(exchanges.length, 1, 'only turns confirmed by a following user record emit');
  assert.equal(exchanges[0].user, 'first user turn');
  assert.equal(boundaryCursor, 2, 'cursor is the first record of the trailing incomplete turn');
  assert.equal(truncated, false);
});

test('harvest_window: maxBytes fallback emits message granularity and flags truncated', () => {
  const records = [
    userRecord(0, 'a user turn'),
    assistantRecord(1, 'a long assistant reply that never sees its next user turn before the byte cap'),
  ];
  const { exchanges, boundaryCursor, truncated } = groupExchanges(records, { maxTurns: 40, maxBytes: 64 });
  assert.equal(truncated, true, 'byte cap without a turn boundary must fall back to message granularity');
  assert.equal(exchanges.length, 1);
  assert.equal(boundaryCursor, 1, 'cursor is the last complete record in the fallback');
});

test('harvest_window: tool output truncates on a UTF-8 character boundary', () => {
  const wide = '界'.repeat(600);
  const records = [
    userRecord(0, 'u'),
    toolUseRecord(1, 'tu-1', 'Bash', { command: 'echo hi' }),
    toolResultRecord(2, 'tu-1', false, wide),
    userRecord(3, 'next'),
  ];
  const { exchanges } = groupExchanges(records, { maxTurns: 40, maxBytes: 262144, toolOutputBytes: 1000 });
  const tool = exchanges[0].tools[0];
  assert.ok(Buffer.byteLength(tool.output, 'utf8') <= 1000, 'truncation must respect the byte budget');
  assert.equal(Buffer.from(tool.output, 'utf8').toString('utf8'), tool.output, 'must not split a multi-byte character');
});

test('harvest_window: files dedupe and cap at 5, and names normalise to the common vocabulary', () => {
  const records = [
    userRecord(0, 'u'),
    toolUseRecord(1, 't1', 'run_shell_command', { command: 'ls' }),
    toolUseRecord(2, 't2', 'read_file', { file_path: 'a.md' }),
    toolUseRecord(3, 't3', 'edit_file', { file_path: 'a.md' }),
    toolUseRecord(4, 't4', 'apply_patch', { file_path: 'b.md' }),
    toolUseRecord(5, 't5', 'weird_thing', {}),
    userRecord(6, 'next'),
  ];
  const { exchanges } = groupExchanges(records, { maxTurns: 40, maxBytes: 262144 });
  assert.equal(normalizeToolName('run_shell_command'), 'Bash');
  assert.equal(normalizeToolName('shell'), 'Bash');
  assert.equal(normalizeToolName('bash'), 'Bash');
  assert.equal(normalizeToolName('read_file'), 'Read');
  assert.equal(normalizeToolName('read'), 'Read');
  assert.equal(normalizeToolName('edit_file'), 'Edit');
  assert.equal(normalizeToolName('replace'), 'Edit');
  assert.equal(normalizeToolName('weird_thing'), 'weird_thing');
  const names = exchanges[0].tools.map((t) => t.name);
  assert.deepEqual(names, ['Bash', 'Read', 'Edit', 'Edit', 'weird_thing']);
  assert.equal(exchanges[0].files.length, 2, 'files dedupe basenames');
  assert.ok(exchanges[0].files.length <= 5);
});

test('harvest_window: setToolError marks from result when available, else call-time fidelity', () => {
  const tools = [{ name: 'Bash', is_error: false }];
  setToolError(tools, { at: 'result_time' });
  assert.equal(tools[0].is_error, false);
  const callTime = [{ name: 'Bash' }];
  setToolError(callTime, { at: 'call_time' });
  assert.equal(callTime[0].is_error, false);
  assert.equal(callTime[0].error_fidelity, 'call_time');
});

test('harvest_window: renderWindow matches the T1 fixture format byte-for-byte', async () => {
  const fixtureText = await fixture('window-sample.md');
  const exchanges = [
    {
      position: 0, user: 'how is zole doing?', action: '', files: ['plans/loam-mqtt-envelope.md'],
      timestamp: '2026-07-30T09:55:37Z',
      assistant: ['`zole` is alive and on T9 — but there\'s a problem worth acting on now.'],
      tools: [
        { name: 'Bash', is_error: false, file: null, command: 'grep -n "^### T"', output: '48:### T1' },
      ], edits: [], errors: [], ended_on_error: false,
    },
  ];
  const rendered = renderWindow({
    harness: 'claude',
    sessionId: '68c34863-ff3b-4b4a-ad92-e243a5644eeb',
    workspace: '/home/user/projects/example-app',
    exchanges,
    windowStart: '2026-07-30T09:55:37Z',
    windowEnd: '2026-07-30T11:42:00Z',
  });
  assert.ok(rendered.startsWith('---\nharness: claude\nsession_id: 68c34863-ff3b-4b4a-ad92-e243a5644eeb\nworkspace: /home/user/projects/example-app\nturns: 1\nwindow_start: "2026-07-30T09:55:37Z"\nwindow_end: "2026-07-30T11:42:00Z"\n---\n'), 'header must match the T1 fixture header shape');
  assert.match(rendered, /\n## Turn 1\n\n### User\nhow is zole doing\?\n\n### Assistant\n`zole` is alive and on T9/, 'turn blocks must be user-anchored with Assistant sections');
  assert.match(rendered, /- tool: Bash \| ok: true \| cmd: grep -n "\^### T"\n  output: 48:### T1/, 'tool lines must carry name, ok, command, and output');
  assert.match(fixtureText, /^## Turn 1$.*^### User$/ms, 'the T1 window fixture itself must follow the same shape');
  assert.equal(rendered.split('## Turn ').length - 1, 1);
});

test('harvest_store: reads resume from the offset and never touch earlier bytes', async () => {
  const root = await mkdtempCanonical('loam-harvest-store-');
  const path = join(root, 'store.jsonl');
  const prefix = '{"kind":"unparseable-prefix"}\n';
  const tail = '{"cursor":1}\n{"cursor":2}\n';
  await writeFile(path, prefix + tail);
  const measured = await measureStore(path);
  assert.equal(measured.present, true);
  assert.equal(measured.size, prefix.length + tail.length);
  const { lines, nextOffset } = await readTail(path, prefix.length, { maxBytes: 1024 });
  assert.equal(lines.length, 2, 'only bytes after the offset are read');
  assert.equal(lines[0].text, '{"cursor":1}');
  assert.equal(nextOffset, prefix.length + tail.length);
  const { parsed } = await parseLines(lines, (line) => JSON.parse(line.text));
  assert.equal(parsed.length, 2);
});

test('harvest_store: malformed middle lines are skipped without aborting', async () => {
  const root = await mkdtempCanonical('loam-harvest-store-');
  const path = join(root, 'store.jsonl');
  await writeFile(path, '{"cursor":1}\n{broken json\n{"cursor":3}\n');
  const { lines } = await readTail(path, 0, { maxBytes: 1024 });
  const { parsed, skipped } = await parseLines(lines, (line) => { try { return JSON.parse(line.text); } catch { return null; } });
  assert.equal(parsed.length, 2);
  assert.equal(skipped, 1);
});

test('harvest_store: a truncated trailing line is not consumed', async () => {
  const root = await mkdtempCanonical('loam-harvest-store-');
  const path = join(root, 'store.jsonl');
  const content = '{"cursor":1}\n{"cursor":2}';
  await writeFile(path, content);
  const { lines, nextOffset } = await readTail(path, 0, { maxBytes: 1024 });
  assert.equal(lines.length, 1, 'the trailing line without a newline must be dropped');
  assert.equal(lines[0].text, '{"cursor":1}');
  assert.ok(nextOffset < content.length, 'nextOffset stops before the fragment');
});

test('harvest_store: invalid UTF-8 does not throw', async () => {
  const root = await mkdtempCanonical('loam-harvest-store-');
  const path = join(root, 'store.jsonl');
  await writeFile(path, Buffer.from('{"cursor":1}\n\xff\xfe\n{"cursor":2}\n'));
  const { lines } = await readTail(path, 0, { maxBytes: 1024 });
  assert.equal(lines.length, 3);
});

test('harvest_store: a file shorter than the cursor reports rotation', async () => {
  const state = { cursor: { value: 500 }, store: { path: '/tmp/whatever.jsonl' } };
  const measured = { present: true, size: 100, mtime_ms: 1 };
  const rotated = detectRotation(state, measured);
  assert.equal(rotated.rotated, true);
  assert.equal(rotated.reset, 0);
  const same = detectRotation(state, { present: true, size: 500, mtime_ms: 1 });
  assert.equal(same.rotated, false);
  assert.equal(same.delta, 0);
  const gone = detectRotation(state, { present: false, size: 0, mtime_ms: 1 });
  assert.equal(gone.rotated, true);
});

test('harvest_store: HarvestError carries the taxonomy reason', () => {
  const error = new HarvestError('store_missing');
  assert.equal(error.reason, 'store_missing');
  assert.equal(harvestPublicReason(error.reason), 'unavailable');
});

test('harvest_launcher: parameterized launchModel carries the agent id and harvest prompt on Claude bg, ingest argv unchanged', async () => {
  const bin = await mkdtempCanonical('loam-harvest-launch-');
  const workspace = await mkdtempCanonical('loam-harvest-launch-ws-');
  const command = join(bin, process.platform === 'win32' ? 'claude.cmd' : 'claude');
  const script = process.platform === 'win32' ? join(bin, 'claude-shim.cjs') : command;
  const calls = join(bin, 'calls.jsonl');
  const state = join(bin, 'agent.json');
  await writeFile(script, `#!/usr/bin/env node
const fs = require('node:fs');
const args = process.argv.slice(2);
fs.appendFileSync(process.env.LOAM_TEST_CALLS, JSON.stringify(args) + '\\n');
if (args[0] === '--help') { process.stdout.write('--bg'); process.exit(0); }
if (args[0] === '--bg') {
  fs.writeFileSync(process.env.LOAM_TEST_AGENT, JSON.stringify([{ name: args[args.indexOf('--name') + 1], id: 'agent-1', queries: 0 }]));
  process.stdout.write('backgrounded');
  process.exit(0);
}
if (args[0] === 'agents') {
  process.stdout.write(JSON.stringify([{ name: JSON.parse(fs.readFileSync(process.env.LOAM_TEST_AGENT, 'utf8'))[0].name, id: 'agent-1', status: 'done' }]));
  process.exit(0);
}
process.exit(0);
`);
  if (process.platform === 'win32') {
    await writeFile(command, `@echo off\r\n"${process.execPath}" "${script}" %*\r\n`);
  } else {
    await chmod(command, 0o700);
  }
  const env = {
    ...process.env,
    PATH: [bin, process.env.PATH || ''].filter(Boolean).join(delimiter),
    LOAM_TEST_CALLS: calls,
    LOAM_TEST_AGENT: state,
  };
  const root = await mkdtempCanonical('loam-harvest-launch-root-');
  const lease = {
    schema: 1, lease_id: 'lease-1', workspace, harness: 'claude',
    launch_mode: 'claude_bg', planned_identity: { name: 'loam-harvest-1' },
  };
  await writeFile(join(root, 'lease.json'), JSON.stringify(lease) + '\n');

  const harvestResult = await launchModel({
    launchMode: 'claude_bg', workspace, env, timeoutMs: 10000,
    lease, openCodeSession: null, root,
    prompt: 'HARVEST PROMPT: review /tmp/window.md via loam::learning-from-session',
    agentId: 'loam:harvester',
  });
  assert.equal(harvestResult.category, null);

  const ingestResult = await launchModel({
    launchMode: 'claude_bg', workspace, env, timeoutMs: 10000,
    lease, openCodeSession: null, root,
  });
  assert.equal(ingestResult.category, null);

  const argv = (await readFile(calls, 'utf8')).trim().split('\n').map(JSON.parse);
  const bgRuns = argv.filter((args) => args[0] === '--bg');
  assert.equal(bgRuns.length, 2);
  const [harvestBg, ingestBg] = bgRuns;
  assert.equal(harvestBg[harvestBg.indexOf('--agent') + 1], 'loam:harvester', 'harvest must use the harvester agent');
  assert.match(harvestBg.at(-1), /HARVEST PROMPT: review \/tmp\/window\.md via loam::learning-from-session/);
  assert.match(harvestBg.at(-1), /loam::learning-from-session/);
  assert.equal(harvestBg.at(-2), '--');
  assert.equal(ingestBg[ingestBg.indexOf('--agent') + 1], 'loam:ingestor', 'ingest default must keep its agent id');
});

test('harvest_launcher: ingest defaults remain byte-identical to today', async () => {
  assert.ok(INGEST_PROMPT.startsWith('Run the existing loam::ingesting-codebase skill'), 'ingest prompt default must be today\'s constant');
  const root = await mkdtempCanonical('loam-harvest-launch-ingest-');
  const bin = await mkdtempCanonical('loam-harvest-launch-bin-');
  const workspace = await mkdtempCanonical('loam-harvest-launch-ws2-');
  const command = join(bin, process.platform === 'win32' ? 'claude.cmd' : 'claude');
  const script = process.platform === 'win32' ? join(bin, 'claude-shim.cjs') : command;
  const calls = join(bin, 'calls.jsonl');
  await writeFile(script, `#!/usr/bin/env node
const fs = require('node:fs');
const args = process.argv.slice(2);
fs.appendFileSync(process.env.LOAM_TEST_CALLS, JSON.stringify(args) + '\\n');
if (args[0] === '--help') { process.stdout.write('--bg'); process.exit(0); }
if (args[0] === '--bg') { process.stdout.write('backgrounded'); process.exit(0); }
if (args[0] === 'agents') { process.stdout.write('[]'); process.exit(0); }
process.exit(0);
`);
  if (process.platform === 'win32') {
    await writeFile(command, `@echo off\r\n"${process.execPath}" "${script}" %*\r\n`);
  } else {
    await chmod(command, 0o700);
  }
  const env = {
    ...process.env,
    PATH: [bin, process.env.PATH || ''].filter(Boolean).join(delimiter),
    LOAM_TEST_CALLS: calls,
  };
  const lease = { schema: 1, lease_id: 'lease-2', workspace, harness: 'claude', launch_mode: 'claude_bg', planned_identity: { name: 'loam-ingest-1' } };
  await writeFile(join(root, 'lease.json'), JSON.stringify(lease) + '\n');
  const result = await launchModel({ launchMode: 'claude_bg', workspace, env, timeoutMs: 10000, lease, openCodeSession: null, root });
  assert.equal(result.category, null);
  const argv = (await readFile(calls, 'utf8')).trim().split('\n').map(JSON.parse);
  const bg = argv.filter((args) => args[0] === '--bg').at(-1);
  assert.equal(bg[bg.indexOf('--agent') + 1], 'loam:ingestor');
  assert.equal(bg.at(-1), `${INGEST_PROMPT} Workspace: ${workspace}`);
});

test('harvest_claude: genuine user text opens turns; tool-result carriers attach; meta/compact/sidechain excluded; foreign results dropped', async () => {
  const root = await mkdtempCanonical('loam-harvest-claude-');
  const sessionStore = join(root, 'session.jsonl');
  await mkdir(join(root, 'projects', 'proj-a'), { recursive: true });
  await writeFile(join(root, 'projects', 'proj-a', 'sess-1.jsonl'), await readFile(new URL('claude/session-store.jsonl', FIXTURES)));

  const located = await locateClaudeStore({ sessionId: 'sess-1', env: { CLAUDE_CONFIG_DIR: root } });
  assert.equal(located.kind, 'jsonl');
  assert.ok(located.path.endsWith('sess-1.jsonl'));

  const { records, skipped } = await parseClaudeStore({ path: located.path, sessionId: 'sess-1' });
  assert.equal(skipped, 1, 'the malformed tail line must be skipped, not abort');
  const users = records.filter((r) => r.kind === 'user');
  assert.deepEqual(users.map((u) => u.text), [
    'first genuine user turn with string content',
    'second user turn via text block',
    'third user turn with plain text',
    'trailing user turn without completion',
  ], 'only records with genuine text open turns; the tool-result carriers never open one');
  assert.equal(records.filter((r) => r.kind === 'tool_result').length, 2, 'only the two sess-1 tool results pass; the foreign CCCC result is dropped');
  assert.equal(records.filter((r) => r.kind === 'assistant').length, 2, 'assistant text records only (a-1, a-4); tool-use-only records never emit assistant text');
  assert.equal(records.some((r) => String(r.text || '').includes('compaction')), false, 'isCompactSummary excluded');
  assert.equal(records.some((r) => String(r.text || '').includes('subagent')), false, 'isSidechain excluded');
  assert.equal(records.some((r) => String(r.text || '').includes('meta record')), false, 'isMeta excluded');

  const { exchanges } = groupExchanges(records, { maxTurns: 40, maxBytes: 262144 });
  assert.equal(exchanges.length, 3, 'the three turns confirmed by a following user record emit; the trailing turn is excluded');
  const tool = exchanges[0].tools.find((t) => t.name === 'Bash');
  assert.equal(tool.is_error, false);
  assert.equal(tool.output, 'hello\n');
  assert.equal(exchanges[0].files.length, 0, 'the Edit tool result never arrives, so no file is admitted');
});

test('harvest_claude: a cursor placed mid-file excludes everything before it', async () => {
  const root = await mkdtempCanonical('loam-harvest-claude-cursor-');
  const store = join(root, 'store.jsonl');
  await writeFile(store, await readFile(new URL('claude/session-store.jsonl', FIXTURES)));
  const bytes = Buffer.byteLength('{"type":"user","sessionId":"sess-1","session_id":"sess-1","cwd":"/workspace","timestamp":"2026-08-07T08:00:00Z","message":{"role":"user","content":"first genuine user turn with string content"},"uuid":"u-1"}\n', 'utf8');
  const tail = await readTail(store, bytes, { maxBytes: 262144 });
  const { records } = await parseClaudeStore({ path: store, sessionId: 'sess-1', lines: tail.lines });
  assert.ok(!records.some((r) => String(r.text || '').includes('first genuine')), 'the pre-cursor turn never reaches the parse');
  assert.ok(records.some((r) => String(r.text || '').includes('second user turn')), 'post-cursor material is parsed');
});

test('harvest_launcher: harvester agent frontmatter declares the expected tool set', async () => {
  const agent = await readFile(new URL('../plugins/loam-adapter/agents/harvester.md', import.meta.url), 'utf8');
  assert.match(agent, /^name: harvester$/m);
  assert.match(agent, /^tools: Read, Glob, Grep, Write, Edit, Bash, Skill$/m);
  assert.match(agent, /^model: haiku$/m);
  assert.match(agent, /loam::learning-from-session/);
  assert.match(agent, /Never spawn or delegate to another agent/);
});

test('harvest_codex: duplicated assistant text appears once; injected turns excluded; both output shapes yield the same text', async () => {
  const root = await mkdtempCanonical('loam-harvest-codex-');
  const sessions = join(root, 'sessions', 'rollout-1');
  await mkdir(sessions, { recursive: true });
  await writeFile(join(sessions, 'rollout-001-sess-c.jsonl'), await readFile(new URL('codex/rollout.jsonl', FIXTURES)));

  const located = await locateCodexStore({ sessionId: 'sess-c', env: { CODEX_HOME: root } });
  assert.ok(located.path.endsWith('rollout-001-sess-c.jsonl'));
  assert.equal(located.kind, 'jsonl');

  const { records, skipped } = await parseCodexStore({ path: located.path, sessionId: 'sess-c' });
  assert.equal(skipped, 1, 'malformed trailing record skipped');
  const users = records.filter((r) => r.kind === 'user');
  assert.deepEqual(users.map((u) => u.text), [
    'first real user question',
    'second real user question after injected turns',
    'trailing user turn without completion',
  ], 'injected <environment_context>/<permissions>/# AGENTS.md turns never open an exchange');
  const assistantTexts = records.filter((r) => r.kind === 'assistant').map((r) => r.text);
  assert.equal(assistantTexts.filter((t) => t.includes('assistant answer in response_item')).length, 1, 'duplicated assistant text appears exactly once');
  const tools = records.filter((r) => r.kind === 'tool_use');
  assert.equal(tools.length, 3);
  const readTool = tools.find((t) => t.name === 'read_file');
  assert.deepEqual(readTool.input, { file_path: '/workspace/README.md' }, 'arguments JSON string parsed defensively');

  const { exchanges } = groupExchanges(records, { maxTurns: 40, maxBytes: 262144 });
  assert.equal(exchanges.length, 2, 'the two turns confirmed by a following user record emit; the trailing turn is excluded');
  assert.equal(exchanges[0].tools.length, 2, 'first exchange carries call_1 and call_2');
  const readExchangeTool = exchanges[0].tools.find((t) => t.name === 'Read');
  assert.equal(readExchangeTool.output, '# README\n', 'both function_call_output shapes normalise to the same string');
  const failedTool = exchanges[1].tools.find((t) => t.name === 'Bash' && t.command === 'false');
  assert.equal(failedTool.is_error, true, 'Exit code: 1 marks the error');
});

test('harvest_codex: newest rollout wins when several exist', async () => {
  const root = await mkdtempCanonical('loam-harvest-codex-newest-');
  const sessions = join(root, 'sessions');
  await mkdir(join(sessions, 'r1'), { recursive: true });
  await mkdir(join(sessions, 'r2'), { recursive: true });
  await writeFile(join(sessions, 'r1', 'rollout-001-sess-c.jsonl'), '{"a":1}\n');
  await new Promise((resolve) => setTimeout(resolve, 20));
  await writeFile(join(sessions, 'r2', 'rollout-001-sess-c.jsonl'), '{"a":2}\n');
  const located = await locateCodexStore({ sessionId: 'sess-c', env: { CODEX_HOME: root } });
  assert.ok(located.path.includes('r2'), 'newest mtime wins');
});

test('harvest_opencode: foreign sessions never appear; synthetic parts excluded; apply_patch recovers its file; directory mismatch aborts', async () => {
  const fixtureDb = new URL('opencode/fixture.db', FIXTURES);
  const dataHome = await mkdtempCanonical('loam-harvest-opencode-locate-');
  await mkdir(join(dataHome, 'opencode'), { recursive: true });
  await copyFile(fileURLToPath(fixtureDb), join(dataHome, 'opencode', 'opencode.db'));
  const located = await locateOpenCodeStore({ env: { XDG_DATA_HOME: dataHome } });
  assert.equal(located.kind, 'sqlite');
  assert.equal(located.path, join(dataHome, 'opencode', 'opencode.db'), 'resolves under XDG_DATA_HOME, not the host default');

  const { records } = await readOpenCodeWindow({ store: fileURLToPath(fixtureDb), sessionId: 'sess-oc-a', workspace: '/workspace/a', rowid: 0 });
  assert.ok(records.some((r) => String(r.text || '').includes('first user question in workspace A')), 'workspace A material is read');
  assert.ok(!records.some((r) => String(r.text || '').includes('FOREIGN')), 'workspace B material never appears');
  assert.ok(!records.some((r) => String(r.text || '').includes('synthetic')), 'synthetic parts excluded');
  const toolRecords = records.filter((r) => r.kind === 'tool_use');
  assert.equal(toolRecords.length, 3);
  const patch = toolRecords.find((t) => t.name === 'apply_patch');
  assert.equal(patch.file, '/workspace/a/src/app.js', 'apply_patch recovers filePath from state.metadata.files');
  const errorTool = toolRecords.find((t) => t.name === 'webfetch');
  assert.equal(errorTool.is_error, true);

  await assert.rejects(
    () => readOpenCodeWindow({ store: fileURLToPath(fixtureDb), sessionId: 'sess-oc-a', workspace: '/workspace/WRONG', rowid: 0 }),
    (error) => error.reason === 'foreign_workspace',
    'a directory mismatch must abort before any content read',
  );
});

test('harvest_opencode: absent node:sqlite yields sqlite_unavailable rather than a crash', async () => {
  const fixtureDb = new URL('opencode/fixture.db', FIXTURES);
  const { readOpenCodeWindow } = await import('../integration/harvest-opencode.mjs');
  const error = await readOpenCodeWindow({
    store: fileURLToPath(fixtureDb), sessionId: 'sess-oc-a', workspace: '/workspace/a', rowid: 0,
    sqliteLoader: async () => { throw new Error('node:sqlite not available'); },
  }).then(() => null, (caught) => caught);
  assert.ok(error, 'must throw');
  assert.equal(error.reason, 'sqlite_unavailable');
});

test('harvest_opencode: measure is filesystem-only and tolerant', async () => {
  const root = await mkdtempCanonical('loam-harvest-opencode-measure-');
  const dbPath = join(root, 'opencode.db');
  await writeFile(dbPath, 'x');
  await writeFile(join(root, 'opencode.db-wal'), 'y');
  const measured = await openCodeMeasure({ store: { path: dbPath } });
  assert.equal(measured.present, true);
  assert.ok(measured.size >= 1);
  const missing = await openCodeMeasure({ store: { path: join(root, 'nope.db') } });
  assert.equal(missing.present, false);
});

test('harvest_opencode: parts are batched — one part query for the whole window, not one per message', async () => {
  const fixtureDb = new URL('opencode/fixture.db', FIXTURES);
  let partQueryCount = 0;
  let messageQueryCount = 0;
  const countingLoader = async () => {
    const sqlite = await import('node:sqlite');
    const RealDatabaseSync = sqlite.DatabaseSync;
    return {
      DatabaseSync: class CountingDatabaseSync extends RealDatabaseSync {
        prepare(sql) {
          if (sql.includes('FROM part') && sql.includes('IN (')) partQueryCount += 1;
          if (sql.includes('FROM message')) messageQueryCount += 1;
          return super.prepare(sql);
        }
      },
    };
  };
  const { readOpenCodeWindow } = await import('../integration/harvest-opencode.mjs');
  await readOpenCodeWindow({
    store: fileURLToPath(fixtureDb), sessionId: 'sess-oc-a', workspace: '/workspace/a', rowid: 0,
    sqliteLoader: countingLoader,
  });
  assert.equal(partQueryCount, 1, 'parts must be fetched in one batched query');
  assert.equal(messageQueryCount, 1, 'messages must be fetched in one query');
});

test('harvest_opencode: a symlinked workspace resolves to the realpath before the cross-check', async () => {
  const root = await mkdtempCanonical('loam-harvest-opencode-symlink-');
  const realDir = join(root, 'real-workspace');
  await mkdir(realDir, { recursive: true });
  const linkDir = join(root, 'link-workspace');
  const { symlink, copyFile } = await import('node:fs/promises');
  await symlink(realDir, linkDir, 'dir');
  const fixtureDb = new URL('opencode/fixture.db', FIXTURES);
  const dbCopy = join(root, 'fixture.db');
  await copyFile(fileURLToPath(fixtureDb), dbCopy);
  const sqlite = await import('node:sqlite');
  const db = new sqlite.DatabaseSync(dbCopy);
  db.prepare('UPDATE session SET directory = ? WHERE id = ?').run(realDir, 'sess-oc-a');
  db.close();
  const { readOpenCodeWindow } = await import('../integration/harvest-opencode.mjs');
  const ok = await readOpenCodeWindow({
    store: dbCopy, sessionId: 'sess-oc-a', workspace: linkDir, rowid: 0,
  });
  assert.ok(Array.isArray(ok.records), 'a symlink resolving to the session directory must pass');
  const foreign = join(root, 'unrelated');
  await mkdir(foreign, { recursive: true });
  await assert.rejects(
    () => readOpenCodeWindow({ store: dbCopy, sessionId: 'sess-oc-a', workspace: foreign, rowid: 0 }),
    (error) => error.reason === 'foreign_workspace',
  );
});

test('harvest_tick: recursion refusal covers every Loam worker marker on every harness', () => {
  const envMarkers = [
    { LOAM_HARVEST_WORKER: '1' }, { LOAM_HARVEST_CHILD: '1' },
    { LOAM_INGEST_WORKER: '1' }, { LOAM_INGEST_CHILD: '1' },
  ];
  for (const env of envMarkers) {
    for (const harness of ['claude', 'codex', 'opencode']) {
      assert.equal(harvestRecursion({}, env), true, `${harness} must refuse on ${JSON.stringify(env)}`);
    }
  }
  const payloadMarkers = [
    { stop_hook_active: true }, { loam_ingest_child: true }, { loam_harvest_child: true },
    { child_session: true }, { agent_type: 'loam:ingestor' }, { agent_type: 'loam:harvester' },
  ];
  for (const payload of payloadMarkers) {
    for (const harness of ['claude', 'codex', 'opencode']) {
      assert.equal(harvestRecursion(payload, {}), true, `${harness} must refuse on payload ${JSON.stringify(payload)}`);
    }
  }
  assert.equal(harvestRecursion({}, {}), false, 'a plain turn end is not recursion');
});

test('harvest_tick: opt-out means no spawn and no file written, for all three harnesses', async () => {
  const root = await mkdtempCanonical('loam-harvest-tick-off-');
  const globalRoot = join(root, 'global');
  await mkdir(globalRoot, { recursive: true });
  const workspace = join(root, 'workspace');
  await mkdir(workspace, { recursive: true });
  let spawns = 0;
  for (const harness of ['claude', 'codex', 'opencode']) {
    const result = await harvestTick({
      harness, payload: { cwd: workspace, session_id: `sess-${harness}` },
      globalRoot, env: { LOAM_HARVEST_BACKGROUND: '0' }, now: 1000,
      startWorker: async () => { spawns += 1; },
      measureStore: async () => ({ present: true, size: 999999, mtime_ms: 1 }),
    });
    assert.equal(result.action, 'skip', `${harness} opt-out must skip`);
    assert.equal(result.reason, 'disabled');
  }
  assert.equal(spawns, 0, 'no spawn on opt-out');
  await assert.rejects(() => readdir(join(globalRoot, 'run')), { code: 'ENOENT' }, 'an opted-out tick writes nothing under runRoot');
});

test('harvest_tick: a sub-threshold delta writes only observed state and spawns nothing', async () => {
  const root = await mkdtempCanonical('loam-harvest-tick-thresh-');
  const globalRoot = join(root, 'global');
  const workspace = join(root, 'workspace');
  await mkdir(workspace, { recursive: true });
  let spawns = 0;
  const result = await harvestTick({
    harness: 'claude', payload: { cwd: workspace, session_id: 'sess-t' },
    globalRoot, env: {}, now: 1000, storePath: join(root, 'store.jsonl'),
    startWorker: async () => { spawns += 1; },
    measureStore: async () => ({ present: true, size: 10, mtime_ms: 1 }),
  });
  assert.equal(result.action, 'skip');
  assert.equal(result.reason, 'below_threshold');
  assert.equal(spawns, 0);
  const { readHarvestState } = await import('../integration/harvest-state.mjs');
  const state = await readHarvestState(globalRoot, workspace, 'sess-t');
  assert.ok(state.observed, 'the observed block is persisted');
  assert.equal(state.observed.store_size, 10);
});

test('harvest_tick: makes zero runtime/model/network calls; returns before the worker resolves', async () => {
  const root = await mkdtempCanonical('loam-harvest-tick-clean-');
  const globalRoot = join(root, 'global');
  const workspace = join(root, 'workspace');
  await mkdir(workspace, { recursive: true });
  let resolved = false;
  let tickReturned = false;
  let calls = 0;
  // Deterministic, not a wall-clock race: this promise only resolves when the
  // test calls resolveSpawn() below, which happens after asserting tickReturned.
  // A fixed setTimeout race is flaky under slow/loaded CI I/O.
  let resolveSpawn;
  const spawnPromise = new Promise((r) => { resolveSpawn = r; }).then(() => {
    resolved = true;
    return { child: { pid: 1 } };
  });
  const result = await harvestTick({
    harness: 'opencode', payload: { cwd: workspace, session_id: 'sess-clean' },
    globalRoot, env: {}, now: 1000, storePath: join(root, 'store.jsonl'),
    startWorker: () => { calls += 1; return spawnPromise; },
    measureStore: async () => ({ present: true, size: 9999999, mtime_ms: 1 }),
  });
  tickReturned = true;
  assert.equal(result.action, 'spawn_worker');
  assert.equal(calls, 1);
  assert.equal(resolved, false, 'the tick must not await the spawned worker');
  assert.equal(tickReturned, true);
  resolveSpawn();
  await spawnPromise;
});

test('harvest_tick: a harvest-agent identity is refused on each harness', async () => {
  const root = await mkdtempCanonical('loam-harvest-tick-agent-');
  const globalRoot = join(root, 'global');
  const workspace = join(root, 'workspace');
  await mkdir(workspace, { recursive: true });
  let spawns = 0;
  for (const harness of ['claude', 'codex', 'opencode']) {
    const result = await harvestTick({
      harness, payload: { cwd: workspace, session_id: `sess-a-${harness}`, agent_type: 'loam:harvester' },
      globalRoot, env: {}, now: 1000,
      startWorker: async () => { spawns += 1; },
      measureStore: async () => ({ present: true, size: 999999, mtime_ms: 1 }),
    });
    assert.equal(result.action, 'skip', `${harness} must refuse a harvest agent turn end`);
  }
  assert.equal(spawns, 0);
});

test('harvest_tick: one hundred simulated ticks on an opted-out workspace leave runRoot empty', async () => {
  const root = await mkdtempCanonical('loam-harvest-tick-100-');
  const globalRoot = join(root, 'global');
  const workspace = join(root, 'workspace');
  await mkdir(workspace, { recursive: true });
  for (let i = 0; i < 100; i += 1) {
    const result = await harvestTick({
      harness: 'claude', payload: { cwd: workspace, session_id: `sess-${i % 3}` },
      globalRoot, env: { LOAM_HARVEST_BACKGROUND: '0' }, now: i * 1000,
      startWorker: async () => {},
      measureStore: async () => ({ present: true, size: 999999, mtime_ms: 1 }),
    });
    assert.equal(result.reason, 'disabled');
  }
  await assert.rejects(() => readdir(join(globalRoot, 'run')), { code: 'ENOENT' });
});

test('harvest_run: a held lease records busy with an unchanged cursor and zero model calls', async () => {
  const root = await mkdtempCanonical('loam-harvest-run-held-');
  const globalRoot = join(root, 'global');
  const workspace = join(root, 'workspace');
  await mkdir(workspace, { recursive: true });
  const statePath = join(globalRoot, 'run');
  let modelCalls = 0;
  const result = await runHarvest({
    harness: 'claude', workspace, sessionId: 'sess-held', globalRoot,
    env: {}, probeFullState: async () => ({ ready: true, state: { wiki_root: join(workspace, 'wiki'), exists: true } }),
    backend: { readWindow: async () => ({ records: [] }), locateStore: async () => ({ path: join(workspace, 'store.jsonl'), kind: 'jsonl' }) },
    launch: async () => { modelCalls += 1; return { category: null, completion: Promise.resolve({ code: 0 }) }; },
    acquireLease: async () => ({ status: 'held' }),
    releaseLease: async () => {},
    readWindow: async () => ({ records: [], boundaryCursor: 0 }),
  });
  assert.equal(result.reason, 'busy');
  assert.equal(modelCalls, 0);
  assert.equal(result.cursorChanged, false);
});

test('harvest_run: a wiki-less fixture records wiki_missing and launches nothing', async () => {
  const root = await mkdtempCanonical('loam-harvest-run-wiki-');
  const globalRoot = join(root, 'global');
  const workspace = join(root, 'workspace');
  await mkdir(workspace, { recursive: true });
  let launches = 0;
  const result = await runHarvest({
    harness: 'claude', workspace, sessionId: 'sess-wiki', globalRoot,
    env: {}, probeFullState: async () => ({ ready: true, state: { wiki_root: '', exists: false } }),
    backend: { readWindow: async () => ({ records: [] }), locateStore: async () => ({ path: join(workspace, 'store.jsonl'), kind: 'jsonl' }) },
    launch: async () => { launches += 1; return { category: null, completion: Promise.resolve({ code: 0 }) }; },
    readWindow: async () => ({ records: [], boundaryCursor: 0 }),
  });
  assert.equal(result.reason, 'wiki_missing');
  assert.equal(launches, 0);
});

test('harvest_run: a sub-threshold window launches nothing and leaves the cursor unchanged', async () => {
  const root = await mkdtempCanonical('loam-harvest-run-sub-');
  const globalRoot = join(root, 'global');
  const workspace = join(root, 'workspace');
  await mkdir(workspace, { recursive: true });
  let launches = 0;
  const result = await runHarvest({
    harness: 'claude', workspace, sessionId: 'sess-sub', globalRoot,
    env: {}, probeFullState: async () => ({ ready: true, state: { wiki_root: join(workspace, 'wiki'), exists: true } }),
    backend: { readWindow: async () => ({ records: [] }), locateStore: async () => ({ path: join(workspace, 'store.jsonl'), kind: 'jsonl' }) },
    launch: async () => { launches += 1; return { category: null, completion: Promise.resolve({ code: 0 }) }; },
    readWindow: async () => ({ records: [], boundaryCursor: 0 }),
  });
  assert.equal(result.reason, 'below_threshold');
  assert.equal(launches, 0);
  assert.equal(result.cursorChanged, false);
});

test('harvest_run: a crossing window launches exactly one agent over records after the cursor only', async () => {
  const root = await mkdtempCanonical('loam-harvest-run-cross-');
  const globalRoot = join(root, 'global');
  const workspace = join(root, 'workspace');
  await mkdir(workspace, { recursive: true });
  const storePath = join(workspace, 'store.jsonl');
  await writeFile(storePath, 'pre-cursor\npost-cursor-1\npost-cursor-2\n');
  await mkdir(globalRoot, { recursive: true });
  await writeFile(join(globalRoot, 'config.json'), JSON.stringify({
    background_harvest: { threshold_turns: 1, threshold_conversation_bytes: 1 },
  }));
  let launches = 0;
  const result = await runHarvest({
    harness: 'claude', workspace, sessionId: 'sess-cross', globalRoot,
    env: {}, probeFullState: async () => ({ ready: true, state: { wiki_root: join(workspace, 'wiki'), exists: true } }),
    backend: {
      locateStore: async () => ({ path: storePath, kind: 'jsonl' }),
      readWindow: async ({ store, state, config }) => {
        const { readTail } = await import('../integration/harvest-store.mjs');
        const tail = await readTail(store.path, state.cursor?.value || 0, { maxBytes: config.max_window_bytes });
        return { records: tail.lines.map((line, index) => ({ cursor: line.offset, kind: index === 0 ? 'assistant' : 'user', session_id: 'sess-cross', text: line.text, timestamp: '' })) };
      },
    },
    launch: async ({ window }) => { launches += 1; return { category: null, completion: Promise.resolve({ code: 0 }) }; },
    readWindow: async () => ({ records: [], boundaryCursor: 0 }),
  });
  assert.equal(launches, 1);
  assert.equal(result.cursorChanged, true);
  assert.equal(result.boundaryCursor > 0, true);
});

test('harvest_run: a zero-admission run advances the cursor, writes no wiki artifact, and records success', async () => {
  const root = await mkdtempCanonical('loam-harvest-run-zero-');
  const globalRoot = join(root, 'global');
  const workspace = join(root, 'workspace');
  await mkdir(workspace, { recursive: true });
  const storePath = join(workspace, 'store.jsonl');
  await writeFile(storePath, 'line-1\nline-2\nline-3\n');
  await mkdir(globalRoot, { recursive: true });
  await writeFile(join(globalRoot, 'config.json'), JSON.stringify({
    background_harvest: { threshold_turns: 1, threshold_conversation_bytes: 1 },
  }));
  let launches = 0;
  const result = await runHarvest({
    harness: 'claude', workspace, sessionId: 'sess-zero', globalRoot,
    env: {}, probeFullState: async () => ({ ready: true, state: { wiki_root: join(workspace, 'wiki'), exists: true } }),
    backend: {
      locateStore: async () => ({ path: storePath, kind: 'jsonl' }),
      readWindow: async ({ store, state, config }) => {
        const { readTail } = await import('../integration/harvest-store.mjs');
        const tail = await readTail(store.path, state.cursor?.value || 0, { maxBytes: config.max_window_bytes });
        return { records: tail.lines.map((line, index) => ({ cursor: line.offset, kind: index === 0 ? 'assistant' : 'user', session_id: 'sess-zero', text: line.text, timestamp: '' })), boundaryCursor: tail.lines.length ? tail.lines[tail.lines.length - 1].endOffset : 0 };
      },
    },
    launch: async () => { launches += 1; return { category: null, completion: Promise.resolve({ code: 0 }) }; },
    readWindow: async () => ({ records: [], boundaryCursor: 0 }),
  });
  assert.equal(launches, 1);
  assert.equal(result.reason, 'ok');
  assert.equal(result.cursorChanged, true);
  const state = await readHarvestState(globalRoot, workspace, 'sess-zero');
  assert.ok(state.cursor.value > 0, 'cursor advances on a zero-admission run');
});

test('harvest_run: an interrupted run leaves the cursor unchanged so the same material is reconsidered', async () => {
  const root = await mkdtempCanonical('loam-harvest-run-int-');
  const globalRoot = join(root, 'global');
  const workspace = join(root, 'workspace');
  await mkdir(workspace, { recursive: true });
  const storePath = join(workspace, 'store.jsonl');
  await writeFile(storePath, 'user-1\nassistant-1\nuser-2\n');
  await mkdir(globalRoot, { recursive: true });
  await writeFile(join(globalRoot, 'config.json'), JSON.stringify({
    background_harvest: { threshold_turns: 1, threshold_conversation_bytes: 1 },
  }));
  const result = await runHarvest({
    harness: 'claude', workspace, sessionId: 'sess-int', globalRoot,
    env: {}, probeFullState: async () => ({ ready: true, state: { wiki_root: join(workspace, 'wiki'), exists: true } }),
    backend: {
      locateStore: async () => ({ path: storePath, kind: 'jsonl' }),
      readWindow: async ({ store, state, config }) => {
        const { readTail } = await import('../integration/harvest-store.mjs');
        const tail = await readTail(store.path, state.cursor?.value || 0, { maxBytes: config.max_window_bytes });
        return { records: tail.lines.map((line, index) => ({ cursor: line.offset, kind: index === 1 ? 'assistant' : 'user', session_id: 'sess-int', text: line.text, timestamp: '' })) };
      },
    },
    launch: async () => ({ category: 'orphan_unknown', completion: Promise.resolve({ code: 0 }) }),
    readWindow: async () => ({ records: [], boundaryCursor: 0 }),
  });
  assert.equal(result.reason, 'busy');
  assert.equal(result.cursorChanged, false, 'a non-completing run never advances the cursor');
});

test('harvest_run: a store shorter than its cursor resets the cursor and continues', async () => {
  const root = await mkdtempCanonical('loam-harvest-run-rotate-');
  const globalRoot = join(root, 'global');
  const workspace = join(root, 'workspace');
  await mkdir(workspace, { recursive: true });
  await mkdir(globalRoot, { recursive: true });
  await writeFile(join(globalRoot, 'config.json'), JSON.stringify({
    background_harvest: { threshold_turns: 1, threshold_conversation_bytes: 1 },
  }));
  const storePath = join(workspace, 'store.jsonl');
  await writeFile(storePath, 'user-1\nassistant-1\nuser-2\n');
  const statePath = harvestStatePath(globalRoot, workspace, 'sess-rot');
  const dir = join(statePath, '..');
  await mkdir(dir, { recursive: true });
  await writeFile(statePath, JSON.stringify({
    schema: 1, session_id: 'sess-rot', harness: 'claude', workspace,
    cursor: { kind: 'bytes', value: 5000, updated_at: 1 },
  }) + '\n');
  let launches = 0;
  const result = await runHarvest({
    harness: 'claude', workspace, sessionId: 'sess-rot', globalRoot,
    env: {}, probeFullState: async () => ({ ready: true, state: { wiki_root: join(workspace, 'wiki'), exists: true } }),
    backend: {
      locateStore: async () => ({ path: storePath, kind: 'jsonl' }),
      readWindow: async ({ store, state, config }) => {
        const { readTail } = await import('../integration/harvest-store.mjs');
        const tail = await readTail(store.path, state.cursor?.value || 0, { maxBytes: config.max_window_bytes });
        return { records: tail.lines.map((line, index) => ({ cursor: line.offset, kind: index === 1 ? 'assistant' : 'user', session_id: 'sess-rot', text: line.text, timestamp: '' })) };
      },
    },
    launch: async () => { launches += 1; return { category: null, completion: Promise.resolve({ code: 0 }) }; },
    readWindow: async () => ({ records: [], boundaryCursor: 0 }),
  });
  assert.equal(launches, 1, 'the rotated store is read from the reset cursor and still launches');
  const state = await readHarvestState(globalRoot, workspace, 'sess-rot');
  assert.equal(state.rotations, 1, 'the rotation is recorded');
});

test('harvest_run: two sessions in one workspace serialize on the shared lease without lost or duplicated material', async () => {
  const root = await mkdtempCanonical('loam-harvest-run-cc-');
  const globalRoot = join(root, 'global');
  const workspace = join(root, 'workspace');
  await mkdir(workspace, { recursive: true });
  await mkdir(globalRoot, { recursive: true });
  await writeFile(join(globalRoot, 'config.json'), JSON.stringify({
    background_harvest: { threshold_turns: 1, threshold_conversation_bytes: 1 },
  }));
  const storeA = join(workspace, 'a.jsonl');
  const storeB = join(workspace, 'b.jsonl');
  await writeFile(storeA, 'user-a\nassistant-a\nuser-a2\n');
  await writeFile(storeB, 'user-b\nassistant-b\nuser-b2\n');
  let launches = 0;
  // sess-a's launch blocks on a gate we control, so sess-b's attempt is
  // guaranteed to happen while sess-a still holds the lease. Racing two
  // Promise.all-started runs against each other's incidental I/O timing is
  // flaky under slow/loaded CI (both can legitimately run back-to-back with
  // no actual overlap), so this test enforces the overlap explicitly instead.
  let releaseA;
  const gateA = new Promise((r) => { releaseA = r; });
  let enteredA;
  const enteredAPromise = new Promise((r) => { enteredA = r; });
  const runOne = (sessionId, storePath, { blocks = false } = {}) => runHarvest({
    harness: 'claude', workspace, sessionId, globalRoot,
    env: {}, probeFullState: async () => ({ ready: true, state: { wiki_root: join(workspace, 'wiki'), exists: true } }),
    backend: {
      locateStore: async () => ({ path: storePath, kind: 'jsonl' }),
      readWindow: async ({ store, state, config }) => {
        const { readTail } = await import('../integration/harvest-store.mjs');
        const tail = await readTail(store.path, state.cursor?.value || 0, { maxBytes: config.max_window_bytes });
        return { records: tail.lines.map((line, index) => ({ cursor: line.offset, kind: index === 1 ? 'assistant' : 'user', session_id: sessionId, text: line.text, timestamp: '' })) };
      },
    },
    launch: async () => {
      launches += 1;
      if (blocks) { enteredA(); await gateA; }
      return { category: null, completion: Promise.resolve({ code: 0 }) };
    },
    readWindow: async () => ({ records: [], boundaryCursor: 0 }),
  });
  const runningA = runOne('sess-a', storeA, { blocks: true });
  await enteredAPromise;
  const busy = await runOne('sess-b', storeB);
  assert.equal(busy.reason, 'busy', 'the other session records busy while the lease is held');
  assert.equal(busy.cursorChanged, false, 'the busy session leaves its cursor unchanged, so its material is reconsidered');
  releaseA();
  const ran = await runningA;
  assert.equal(launches, 1, 'concurrent same-workspace runs serialize: exactly one holds the shared lease');
  assert.equal(ran.reason, 'ok', 'the lease-holding session harvests');
  assert.equal(ran.cursorChanged, true, 'the lease-holding session advances its cursor');

  const stateA = await readHarvestState(globalRoot, workspace, 'sess-a');
  const stateB = await readHarvestState(globalRoot, workspace, 'sess-b');
  assert.ok(stateA?.cursor?.value > 0, 'the harvested session advanced its cursor');
  assert.equal(stateB?.cursor?.value ?? null, null, 'the busy session never advanced (no material lost)');
});

test('harvest_run: the detached worker parses its argv and reports through hook bookkeeping', async () => {
  const { main } = await import('../adapters/harvest-worker.mjs');
  const calls = [];
  const result = await main({
    harness: 'claude', workspace: '/workspace', sessionId: 'sess-w', hookRunId: 7,
    globalRoot: '/global',
    runHarvest: async (input) => {
      calls.push(input);
      return { reason: 'wiki_missing' };
    },
    startHookWorker: async (input) => calls.push(['start', input]),
    finishHookWorker: async (input) => calls.push(['finish', input]),
  });
  assert.equal(result.reason, 'wiki_missing');
  assert.deepEqual(calls, [
    ['start', { run: { id: 7, globalRoot: '/global', workspace: '/workspace' } }],
    {
      harness: 'claude', workspace: '/workspace', sessionId: 'sess-w', globalRoot: '/global',
      skillsRoot: undefined, env: process.env,
    },
    ['finish', { run: { id: 7, globalRoot: '/global', workspace: '/workspace' }, reason: 'wiki_missing' }],
  ]);
});

test('harvest_status: reports enabled state, per-session cursors, wiki cache, last run, and lease state, content-free', async () => {
  const root = await mkdtempCanonical('loam-harvest-status-');
  const globalRoot = join(root, 'global');
  const workspace = join(root, 'workspace');
  await mkdir(workspace, { recursive: true });
  await mkdir(globalRoot, { recursive: true });
  await writeHarvestState(globalRoot, workspace, 'sess-s1', {
    schema: 1, session_id: 'sess-s1', harness: 'claude', workspace,
    cursor: { kind: 'bytes', value: 1234, updated_at: 5 },
    wiki: { present: true, root: join(workspace, 'wiki'), checked_at: 6 },
  });
  const { harvestStatus } = await import('../integration/harvest.mjs');
  const status = await harvestStatus({ globalRoot, workspace, env: {} });
  assert.equal(status.enabled, true);
  assert.equal(status.sessions.length, 1);
  assert.equal(status.sessions[0].session_id, 'sess-s1');
  assert.equal(status.sessions[0].cursor.value, 1234);
  assert.equal(status.sessions[0].wiki.present, true);
  assert.equal(status.lease_state, 'dead');
  assert.equal(status.queue.root.includes('harvest'), true);
  assert.ok(!JSON.stringify(status).includes('conversation'), 'status must be content-free');
});
