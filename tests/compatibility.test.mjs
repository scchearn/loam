import assert from 'node:assert/strict';
import { execFile, spawn } from 'node:child_process';
import { cp, mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { promisify } from 'node:util';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { test } from 'node:test';

import { assertPackageAssets } from '../setup/package-check.mjs';

const execFileAsync = promisify(execFile);
const packageRoot = fileURLToPath(new URL('..', import.meta.url));
const loaderPath = join(packageRoot, '.opencode', 'plugins', 'loam.js');
const hookPath = join(packageRoot, 'hooks', 'session-start.mjs');
const marketplaceRoot = join(packageRoot, 'plugins', 'loam-adapter');
const marketplaceHookPath = join(marketplaceRoot, 'hooks', 'session-start.mjs');
const marketplaceStopPath = join(marketplaceRoot, 'hooks', 'stop.mjs');
const marketplaceSubagentStartPath = join(marketplaceRoot, 'hooks', 'subagent-start.mjs');
const marketplaceSubagentStopPath = join(marketplaceRoot, 'hooks', 'subagent-stop.mjs');
const codexStopPath = join(packageRoot, 'adapters', 'codex-stop.mjs');
const ingestWorkerPath = join(packageRoot, 'adapters', 'ingest-worker.mjs');
const npmCommand = process.platform === 'win32' ? 'npm.cmd' : 'npm';

async function runHook(env, payload = {}, path = hookPath) {
  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath, [path], {
      cwd: packageRoot,
      env: { ...process.env, ...env },
    });
    let stdout = '';
    let stderr = '';
    child.stdout.on('data', (chunk) => { stdout += chunk; });
    child.stderr.on('data', (chunk) => { stderr += chunk; });
    child.once('error', reject);
    child.once('close', (code) => resolve({ code, stdout, stderr }));
    child.stdin.end(JSON.stringify(payload));
  });
}

test('legacy OpenCode entry delegates to the shared adapter without startup polling', async () => {
  const source = await readFile(loaderPath, 'utf8');
  assert.match(source, /adapters[\\/]opencode\.mjs/);
  assert.doesNotMatch(source, /git ls-remote|loamstate\.(sh|ps1)|findSkillPath/);

  const plugin = await import(pathToFileURL(loaderPath).href);
  assert.equal(typeof plugin.LoamPlugin, 'function');
  assert.equal(typeof plugin.default, 'function');
});

test('missing adapter in an existing clone returns setup recovery instead of a loader error', async () => {
  const clone = await mkdtemp(join(tmpdir(), 'loam-legacy-clone-'));
  const cloneLoader = join(clone, '.opencode', 'plugins', 'loam.js');
  await mkdir(dirname(cloneLoader), { recursive: true });
  await cp(loaderPath, cloneLoader);

  try {
    const loaded = await import(`${pathToFileURL(cloneLoader).href}?fixture=${Date.now()}`);
    const plugin = await loaded.LoamPlugin({ directory: clone });
    const output = { messages: [{ info: { role: 'user' }, parts: [{ type: 'text', text: 'hello' }] }] };
    await plugin['experimental.chat.messages.transform']({}, output);
    assert.match(output.messages[0].parts[0].text, /npx @scchearn\/loam setup/);
  } finally {
    await rm(clone, { recursive: true, force: true });
  }
});

test('packed tarball contains a loadable adapter through the preserved main entry', async () => {
  const destination = await mkdtemp(join(tmpdir(), 'loam-pack-'));
  const extracted = join(destination, 'extracted');
  await mkdir(extracted);
  try {
    const { stdout } = await execFileAsync(npmCommand, [
      'pack',
      '--ignore-scripts',
      '--silent',
      '--pack-destination',
      destination,
    ], { cwd: packageRoot, shell: process.platform === 'win32' });
    const tarball = join(destination, stdout.trim().split(/\r?\n/).at(-1));
    await execFileAsync('tar', ['-xzf', tarball], { cwd: extracted });
    const packedRoot = join(extracted, 'package');
    const packedLoader = join(packedRoot, '.opencode', 'plugins', 'loam.js');
    const loaded = await import(`${pathToFileURL(packedLoader).href}?fixture=${Date.now()}`);
    const plugin = await loaded.LoamPlugin({ directory: packedRoot });
    assert.equal(typeof plugin['experimental.chat.messages.transform'], 'function');
  } finally {
    await rm(destination, { recursive: true, force: true });
  }
});

test('publication guard rejects a package fixture missing the shared integration', async () => {
  const fixture = await mkdtemp(join(tmpdir(), 'loam-package-fixture-'));
  try {
    const excluded = ['/node_modules/', '/.git/', '/cli/', '/plans/', '/specs/', '/target/', '/tests/'];
    await cp(packageRoot, fixture, {
      recursive: true,
      filter: (source) => !excluded.some((part) => source.includes(part)),
    });
    await rm(join(fixture, 'integration'), { recursive: true, force: true });
    await assert.rejects(
      () => assertPackageAssets({ packageRoot: fixture }),
      /package asset is missing: integration/,
    );
  } finally {
    await rm(fixture, { recursive: true, force: true });
  }
});

test('packaged session hook emits valid Claude, Cursor, and default envelopes', async () => {
  for (const [env, field] of [
    [{ CLAUDE_PLUGIN_ROOT: packageRoot }, 'hookSpecificOutput'],
    [{ CURSOR_PLUGIN_ROOT: packageRoot }, 'additional_context'],
    [{ COPILOT_CLI: '1' }, 'additionalContext'],
  ]) {
    const result = await runHook(env, { cwd: join(packageRoot, 'workspace') });
    assert.equal(result.code, 0, result.stderr);
    const parsed = JSON.parse(result.stdout);
    assert.ok(parsed[field]);
    const context = field === 'hookSpecificOutput' ? parsed[field].additionalContext : parsed[field];
    assert.match(context, /<LOAM_IMPORTANT>/);
    assert.match(context, /npx @scchearn\/loam setup/);
  }
});

test('thin marketplace adapter contains no skills and emits Claude and Codex envelopes', async () => {
  const claude = JSON.parse(await readFile(join(marketplaceRoot, '.claude-plugin', 'plugin.json'), 'utf8'));
  const codex = JSON.parse(await readFile(join(marketplaceRoot, '.codex-plugin', 'plugin.json'), 'utf8'));
  const adapter = await import(pathToFileURL(join(marketplaceRoot, 'adapter.mjs')).href);

  assert.equal('skills' in claude, false);
  assert.equal('skills' in codex, false);
  assert.equal('hooks' in claude, false);
  assert.equal('hooks' in codex, false);

  const calls = [];
  const codexAdapter = adapter.createMarketplaceAdapter({
    harness: 'codex',
    getContext: async (input) => {
      calls.push(input);
      return 'codex context';
    },
  });
  assert.deepEqual(await codexAdapter({ cwd: '/workspace' }), {
    hookSpecificOutput: {
      hookEventName: 'SessionStart',
      additionalContext: 'codex context',
    },
  });
  assert.equal(calls[0].harness, 'codex');

  for (const env of [
    { CLAUDE_PLUGIN_ROOT: marketplaceRoot },
    { PLUGIN_ROOT: marketplaceRoot, CLAUDE_PLUGIN_ROOT: marketplaceRoot },
  ]) {
    const result = await runHook(env, { cwd: join(packageRoot, 'workspace') }, marketplaceHookPath);
    assert.equal(result.code, 0, result.stderr);
    const parsed = JSON.parse(result.stdout);
    assert.equal(parsed.hookSpecificOutput.hookEventName, 'SessionStart');
    assert.match(parsed.hookSpecificOutput.additionalContext, /<LOAM_IMPORTANT>/);
    assert.match(parsed.hookSpecificOutput.additionalContext, /npx @scchearn\/loam setup/);
  }
});

test('Codex marketplace adapter falls back to the legacy Claude integration harness', async () => {
  const fixture = await mkdtemp(join(tmpdir(), 'loam-codex-legacy-integration-'));
  const integrationPath = join(fixture, 'loam.mjs');
  await writeFile(integrationPath, `
export async function runIntegration(argv, { output }) {
  const harness = argv[argv.indexOf('--harness') + 1];
  if (harness === 'codex') throw new Error('unsupported harness: codex');
  output.write('<LOAM_IMPORTANT>legacy integration</LOAM_IMPORTANT>');
  return 0;
}
`);
  try {
    const adapter = await import(`${pathToFileURL(join(marketplaceRoot, 'adapter.mjs')).href}?legacy=${Date.now()}`);
    const output = await adapter.createMarketplaceAdapter({ harness: 'codex', integrationPath })({ cwd: '/workspace' });
    assert.equal(output.hookSpecificOutput.additionalContext, '<LOAM_IMPORTANT>legacy integration</LOAM_IMPORTANT>');
  } finally {
    await rm(fixture, { recursive: true, force: true });
  }
});

test('plugin manifests point at the packaged Node hook entry', async () => {
  const claude = JSON.parse(await readFile(join(packageRoot, '.claude-plugin', 'plugin.json'), 'utf8'));
  const cursor = JSON.parse(await readFile(join(packageRoot, '.cursor-plugin', 'plugin.json'), 'utf8'));
  const claudeHooks = JSON.parse(await readFile(join(packageRoot, 'hooks', 'hooks.json'), 'utf8'));
  const cursorHooks = JSON.parse(await readFile(join(packageRoot, 'hooks', 'hooks-cursor.json'), 'utf8'));

  assert.equal(claude.hooks, './hooks/hooks.json');
  assert.equal(cursor.hooks, './hooks/hooks-cursor.json');
  const claudeSessionStart = claudeHooks.hooks.SessionStart[0].hooks[0];
  assert.equal(claudeSessionStart.command, 'node');
  assert.match(claudeSessionStart.args[0], /session-start\.mjs/);
  assert.match(cursorHooks.hooks.sessionStart[0].command, /session-start\.mjs/);
});

test('marketplace plugin owns SessionStart and Stop for Claude and Codex', async () => {
  const hooks = JSON.parse(await readFile(join(marketplaceRoot, 'hooks', 'hooks.json'), 'utf8'));
  assert.match(hooks.hooks.SessionStart[0].hooks[0].command, /session-start\.mjs/);
  assert.match(hooks.hooks.Stop[0].hooks[0].command, /stop\.mjs/);

  const stop = await import(pathToFileURL(marketplaceStopPath).href);
  const calls = [];
  const run = { id: 8 };
  const workspace = resolve('/workspace');
  const loadHooks = async () => ({
    resolveGlobalRoot: () => '/global',
    beginHookRun: async (input) => { calls.push(['begin', input]); return run; },
    finishHookRun: async (input) => calls.push(['finish', input]),
  });
  const loadIngest = async () => ({
    resolveGlobalRoot: () => '/global',
    resolveSkillsRoot: () => '/skills',
    dispatchBoundary: async (input) => {
      calls.push(['ingest', input]);
      return { action: 'skip', reason: 'nothing_to_do', detail: 'no changes' };
    },
  });

  assert.deepEqual(await stop.handleStop(
    { cwd: workspace, session_id: 'session' },
    { PLUGIN_ROOT: marketplaceRoot },
    { loadHooks, loadIngest },
  ), {});
  assert.deepEqual(calls.map(([kind]) => kind), ['begin', 'ingest', 'finish']);
  assert.deepEqual(calls[0][1], {
    globalRoot: '/global', harness: 'codex', hook: 'stop', workspace, sessionId: 'session',
  });
  assert.equal(calls[1][1].harness, 'codex');
  assert.equal(calls[1][1].globalRoot, '/global');
  assert.equal(calls[1][1].skillsRoot, '/skills');
  assert.equal(calls[1][1].hookRunId, 8);
  assert.deepEqual(calls[2][1], {
    run,
    status: 'succeeded',
    action: 'skip',
    reason: 'nothing_to_do',
    detail: 'no changes',
  });

  calls.length = 0;
  assert.deepEqual(await stop.handleStop(
    { cwd: workspace, session_id: 'failed-session' },
    { PLUGIN_ROOT: marketplaceRoot },
    {
      loadHooks,
      loadIngest: async () => ({
        resolveGlobalRoot: () => '/global',
        resolveSkillsRoot: () => '/skills',
        dispatchBoundary: async () => { throw new Error('gate failed'); },
      }),
    },
  ), {});
  assert.deepEqual(calls.map(([kind]) => kind), ['begin', 'finish']);
  assert.equal(calls[1][1].status, 'failed');
  assert.match(calls[1][1].detail, /gate failed/);

  calls.length = 0;
  assert.deepEqual(await stop.handleStop(
    { cwd: workspace },
    { PLUGIN_ROOT: marketplaceRoot },
    { loadHooks: async () => { throw new Error('old integration'); }, loadIngest },
  ), {});
  assert.deepEqual(calls.map(([kind]) => kind), ['ingest']);
});

test('shared SubagentStart and SubagentStop hooks match both separators but only Codex loam_ingestor acts', async () => {
  const hooks = JSON.parse(await readFile(join(marketplaceRoot, 'hooks', 'hooks.json'), 'utf8'));
  assert.match(hooks.hooks.SubagentStart[0].matcher, /loam_ingestor/u);
  assert.match(hooks.hooks.SubagentStart[0].matcher, /loam:ingestor/u);
  assert.equal(hooks.hooks.SubagentStop[0].matcher, hooks.hooks.SubagentStart[0].matcher);
  assert.match(hooks.hooks.SubagentStart[0].hooks[0].command, /subagent-start\.mjs/u);
  assert.match(hooks.hooks.SubagentStop[0].hooks[0].command, /subagent-stop\.mjs/u);

  const start = await import(`${pathToFileURL(marketplaceSubagentStartPath).href}?subagent=${Date.now()}`);
  const stop = await import(`${pathToFileURL(marketplaceSubagentStopPath).href}?subagent=${Date.now()}`);
  const unavailable = { loadIngest: async () => { throw new Error('must stay inert'); } };
  for (const [payload, env] of [
    [{ cwd: '/workspace', agent_id: 'agent-1', agent_type: 'loam:ingestor' }, { CLAUDE_PLUGIN_ROOT: marketplaceRoot }],
    [{ cwd: '/workspace', turn_id: 'turn-1', agent_id: 'agent-1', agent_type: 'foreign' }, { PLUGIN_ROOT: marketplaceRoot }],
    [{ cwd: '/workspace', agent_id: 'agent-1', agent_type: 'loam_ingestor' }, { CLAUDE_PLUGIN_ROOT: marketplaceRoot }],
    [{ cwd: '/workspace', agent_id: 'agent-1', agent_type: 'loam_ingestor' }, { PLUGIN_ROOT: marketplaceRoot }],
  ]) {
    assert.deepEqual(await start.handleSubagentStart(payload, env, unavailable), {});
    assert.deepEqual(await stop.handleSubagentStop(payload, env, unavailable), {});
  }
});

test('Codex SubagentStart injects resolved preparation-first context and SubagentStop finalizes by agent_id', async () => {
  const start = await import(`${pathToFileURL(marketplaceSubagentStartPath).href}?start=${Date.now()}`);
  const stop = await import(`${pathToFileURL(marketplaceSubagentStopPath).href}?stop=${Date.now()}`);
  const globalRoot = resolve('/global');
  const workspace = resolve('/workspace');
  const integrationPath = join(globalRoot, 'integration', 'loam.mjs');
  const adapterPath = join(globalRoot, 'plugins', '0.9.10');
  const workerPath = join(adapterPath, 'ingest-worker.mjs');
  const calls = [];
  const loadHooks = async () => ({
    startHookWorker: async (input) => calls.push(['worker-start', input]),
    finishHookWorker: async (input) => calls.push(['worker-finish', input]),
  });
  const loadIngest = async () => ({
    resolveGlobalRoot: () => globalRoot,
    bindNativeAgent: async (input) => {
      calls.push(['bind', input]);
      return {
        status: 'bound', owns_claim: true, hook_run_id: 17,
        workspace, agent_id: 'agent-17',
        integration_path: integrationPath,
        adapter_path: adapterPath,
        worker_path: workerPath,
      };
    },
    finalizeNativeAgentRun: async (input) => {
      calls.push(['finalize', input]);
      return { reason: 'ok', owns_claim: true, hook_run_id: 17, workspace };
    },
  });
  const env = { CLAUDE_PLUGIN_ROOT: marketplaceRoot, LOAM_INGEST_GLOBAL_ROOT: globalRoot };
  const payload = { cwd: workspace, turn_id: 'turn-17', agent_id: 'agent-17', agent_type: 'loam_ingestor', last_assistant_message: 'I definitely succeeded' };
  const response = await start.handleSubagentStart(payload, env, { loadHooks, loadIngest });
  assert.equal(response.hookSpecificOutput.hookEventName, 'SubagentStart');
  const context = response.hookSpecificOutput.additionalContext;
  assert.match(context, /Agent identity: agent-17/u);
  assert.ok(context.includes(`Workspace: ${workspace}`));
  assert.ok(context.includes(`Installed integration: ${integrationPath}`));
  assert.ok(context.includes(`Installed adapter: ${adapterPath}`));
  assert.match(context, /first action must be exactly this preparation command/iu);
  assert.ok(context.includes(workerPath));
  assert.match(context, /--native-prepare.*--agent-id.*agent-17/u);
  assert.ok(context.indexOf('--native-prepare') < context.indexOf('loam::ingesting-codebase'));
  assert.match(context, /action.*skip.*stop immediately/iu);
  assert.deepEqual(calls.map(([kind]) => kind), ['bind', 'worker-start']);

  calls.length = 0;
  assert.deepEqual(await stop.handleSubagentStop(payload, env, { loadHooks, loadIngest }), {});
  assert.deepEqual(calls.map(([kind]) => kind), ['finalize', 'worker-finish']);
  assert.deepEqual(calls[0][1], { globalRoot, workspace, agentId: 'agent-17', env });
  assert.equal(JSON.stringify(calls[0][1]).includes('definitely succeeded'), false, 'assistant text is not completion evidence');
  assert.equal(calls[1][1].reason, 'ok');
  assert.equal(calls[1][1].run.id, 17);
});

test('loam_ingestor native preparation command routes through the installed worker without launching a model', async () => {
  const worker = await import(`${pathToFileURL(ingestWorkerPath).href}?native-prepare=${Date.now()}`);
  let received;
  const result = await worker.main({
    nativePrepare: true,
    globalRoot: '/global',
    workspace: '/workspace',
    agentId: 'agent-17',
    skillsRoot: '/skills',
    env: {},
    prepareNativeAgentRun: async (input) => { received = input; return { action: 'skip', reason: 'busy' }; },
    runWorker: async () => { throw new Error('native preparation must not launch a model'); },
  });
  assert.deepEqual(result, { action: 'skip', reason: 'busy' });
  assert.deepEqual(received, {
    globalRoot: '/global', workspace: '/workspace', agentId: 'agent-17', skillsRoot: '/skills', env: {},
  });
});

test('packaged SubagentStart and SubagentStop handlers emit one inert JSON response on missing intent', async () => {
  const home = await mkdtemp(join(tmpdir(), 'loam-subagent-output-'));
  for (const path of [marketplaceSubagentStartPath, marketplaceSubagentStopPath]) {
    const result = await runHook({
      HOME: home, USERPROFILE: home, LOAM_HOME: join(home, '.agents', 'loam'), PLUGIN_ROOT: marketplaceRoot,
    }, { cwd: home, turn_id: 'turn-missing', agent_id: 'agent-missing', agent_type: 'loam_ingestor' }, path);
    assert.equal(result.code, 0, result.stderr);
    assert.equal(result.stdout, '{}\n');
    assert.equal(result.stderr, '');
  }
});

test('Codex systemMessage reports only immediate toast dispatch failures', async () => {
  const direct = await import(`${pathToFileURL(codexStopPath).href}?toast-failure=${Date.now()}`);
  const input = { cwd: '/workspace', session_id: 'codex-session' };
  const loadIngest = ({ visibility = 'toast', outcome, failure } = {}) => async () => ({
    resolveGlobalRoot: () => '/global',
    resolveSkillsRoot: () => '/skills',
    readIngestConfig: async () => ({ visibility }),
    dispatchBoundary: async () => {
      if (failure) throw failure;
      return outcome;
    },
  });
  const runDirect = (options) => direct.main({
    input,
    env: {},
    errorOutput: { write: () => {} },
    loadIngest: loadIngest(options),
  });

  assert.match((await runDirect({ outcome: { action: 'skip', reason: 'unavailable' } })).systemMessage, /could not start/u);
  assert.match((await runDirect({ failure: new Error('gate failed') })).systemMessage, /could not start/u);
  assert.deepEqual(await runDirect({ outcome: { action: 'spawn_worker' } }), {});
  assert.deepEqual(await runDirect({ outcome: { action: 'skip', reason: 'nothing_to_do' } }), {});
  assert.deepEqual(await runDirect({ visibility: 'silent', outcome: { action: 'skip', reason: 'unavailable' } }), {});
  assert.deepEqual(await runDirect({ visibility: 'native', outcome: { action: 'skip', reason: 'unavailable' } }), {});
});

test('Codex marketplace Stop matches direct one-shot warning semantics', async () => {
  const stop = await import(`${pathToFileURL(marketplaceStopPath).href}?codex-toast=${Date.now()}`);
  const options = (visibility, dispatchBoundary) => ({
    loadHooks: async () => { throw new Error('logging unavailable'); },
    loadIngest: async () => ({
      resolveGlobalRoot: () => '/global',
      resolveSkillsRoot: () => '/skills',
      readIngestConfig: async () => ({ visibility }),
      dispatchBoundary,
    }),
  });
  const input = { cwd: '/workspace', session_id: 'codex-session' };
  const env = { PLUGIN_ROOT: marketplaceRoot };

  const reported = await stop.handleStop(
    input,
    env,
    options('toast', async () => ({ action: 'skip', reason: 'unavailable' })),
  );
  assert.match(reported.systemMessage, /could not start/u);
  assert.deepEqual(await stop.handleStop(
    input,
    env,
    options('toast', async () => ({ action: 'spawn_worker' })),
  ), {});
  assert.deepEqual(await stop.handleStop(
    input,
    env,
    options('native', async () => { throw new Error('native failure'); }),
  ), {});
});

test('Codex native Stop returns one spawn_agent continuation with identical direct and marketplace logging semantics', async () => {
  const direct = await import(`${pathToFileURL(codexStopPath).href}?native=${Date.now()}`);
  const stop = await import(`${pathToFileURL(marketplaceStopPath).href}?native=${Date.now()}`);
  const run = { id: 91 };
  const finishCalls = [];
  const continuation = {
    action: 'spawn_worker',
    workspace: '/workspace',
    native_continuation: {
      decision: 'block',
      reason: 'Call spawn_agent exactly once using the loam_ingestor agent profile, then finish this continuation immediately.',
    },
  };
  const loadIngest = async () => ({
    resolveGlobalRoot: () => '/global',
    resolveSkillsRoot: () => '/skills',
    readIngestConfig: async () => ({ visibility: 'native' }),
    dispatchBoundary: async () => continuation,
  });
  const input = { cwd: '/workspace', session_id: 'native-session', stop_hook_active: false };

  const directResponse = await direct.main({ input, env: {}, loadIngest });
  const marketplaceResponse = await stop.handleStop(input, { PLUGIN_ROOT: marketplaceRoot }, {
    loadIngest,
    loadHooks: async () => ({
      resolveGlobalRoot: () => '/global',
      beginHookRun: async () => run,
      finishHookRun: async (call) => finishCalls.push(call),
    }),
  });

  assert.deepEqual(marketplaceResponse, directResponse);
  assert.equal(directResponse.decision, 'block');
  assert.equal((directResponse.reason.match(/spawn_agent/gu) || []).length, 1);
  assert.match(directResponse.reason, /loam_ingestor/u);
  assert.match(directResponse.reason, /finish (?:this )?continuation immediately/iu);
  assert.deepEqual(finishCalls, [{ run, status: 'succeeded', action: 'spawn_worker' }]);

  finishCalls.length = 0;
  const fallbackLoad = async () => ({
    resolveGlobalRoot: () => '/global',
    resolveSkillsRoot: () => '/skills',
    readIngestConfig: async () => ({ visibility: 'native' }),
    dispatchBoundary: async () => ({ action: 'spawn_worker', workspace: '/workspace', native_fallback: true }),
  });
  assert.deepEqual(await stop.handleStop(
    { ...input, stop_hook_active: true },
    { PLUGIN_ROOT: marketplaceRoot },
    {
      loadIngest: fallbackLoad,
      loadHooks: async () => ({
        resolveGlobalRoot: () => '/global',
        beginHookRun: async () => run,
        finishHookRun: async (call) => finishCalls.push(call),
      }),
    },
  ), {});
  assert.deepEqual(finishCalls, [{ run, status: 'succeeded', action: 'spawn_worker' }]);

  const boundLoad = async () => ({
    resolveGlobalRoot: () => '/global',
    resolveSkillsRoot: () => '/skills',
    readIngestConfig: async () => ({ visibility: 'native' }),
    dispatchBoundary: async () => ({ action: 'skip', reason: 'busy', workspace: '/workspace' }),
  });
  assert.deepEqual(await direct.main({ input: { ...input, stop_hook_active: true }, env: {}, loadIngest: boundLoad }), {});
  assert.deepEqual(await stop.handleStop(
    { ...input, stop_hook_active: true },
    { PLUGIN_ROOT: marketplaceRoot },
    { loadIngest: boundLoad, loadHooks: async () => { throw new Error('logging unavailable'); } },
  ), {});
});

test('marketplace Stop writes exact hook JSON when logging is unavailable', async () => {
  const home = await mkdtemp(join(tmpdir(), 'loam-stop-output-'));
  const result = await runHook({
    HOME: home,
    USERPROFILE: home,
    LOAM_HOME: join(home, '.agents', 'loam'),
    PLUGIN_ROOT: marketplaceRoot,
  }, { cwd: home }, marketplaceStopPath);

  assert.equal(result.code, 0, result.stderr);
  assert.equal(result.stdout, '{}\n');
  assert.equal(result.stderr, '');
});

test('Claude marketplace Stop forwards agent_type without making a background-session registration claim', async () => {
  const stop = await import(`${pathToFileURL(marketplaceStopPath).href}?claude-visibility=${Date.now()}`);
  let forwarded;
  const response = await stop.handleStop(
    { cwd: '/workspace', session_id: 'claude-session', agent_type: 'loam:ingestor' },
    { CLAUDE_PLUGIN_ROOT: marketplaceRoot },
    {
      loadHooks: async () => { throw new Error('logging unavailable'); },
      loadIngest: async () => ({
        resolveGlobalRoot: () => '/global',
        resolveSkillsRoot: () => '/skills',
        dispatchBoundary: async (input) => {
          forwarded = input;
          return { action: 'spawn_worker', workspace: '/workspace' };
        },
      }),
    },
  );

  assert.deepEqual(response, {});
  assert.equal('systemMessage' in response, false);
  assert.equal(forwarded.payload.agent_type, 'loam:ingestor');
});
