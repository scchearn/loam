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
const marketplaceRoot = join(packageRoot, 'plugins', 'loam-adapter');
const marketplaceStopPath = join(marketplaceRoot, 'hooks', 'stop.mjs');
const marketplaceSubagentStartPath = join(marketplaceRoot, 'hooks', 'subagent-start.mjs');
const marketplaceSubagentStopPath = join(marketplaceRoot, 'hooks', 'subagent-stop.mjs');
const codexStopPath = join(packageRoot, 'adapters', 'codex-stop.mjs');
const ingestWorkerPath = join(packageRoot, 'adapters', 'ingest-worker.mjs');
const npmCommand = process.platform === 'win32' ? 'npm.cmd' : 'npm';

async function runHook(env, payload = {}, path = marketplaceStopPath) {
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
    const output = { system: [] };
    await plugin['experimental.chat.system.transform']({ sessionID: 's' }, output);
    assert.match(output.system[0], /npx @scchearn\/loam install/);
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
    assert.equal(typeof plugin['experimental.chat.system.transform'], 'function');
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

test('thin marketplace adapter carries no skills and the harness-native federation surface', async () => {
  const claude = JSON.parse(await readFile(join(marketplaceRoot, '.claude-plugin', 'plugin.json'), 'utf8'));
  const codex = JSON.parse(await readFile(join(marketplaceRoot, '.codex-plugin', 'plugin.json'), 'utf8'));
  const adapter = await import(pathToFileURL(join(marketplaceRoot, 'adapter.mjs')).href);

  assert.equal('skills' in claude, false);
  assert.equal('skills' in codex, false);
  // Claude finds `hooks/hooks.json` by convention; Codex names it explicitly.
  // Either way the file setup rewrites is the same one.
  assert.equal('hooks' in claude, false);
  assert.equal(codex.hooks, './hooks/hooks.json');

  // harness-native-wake: the shipped plugin now carries the federation surface
  // itself (SessionStart render+register, per-turn drain, Stop-hook wake) so a
  // marketplace-only install — no `npx install` staging native hooks — still
  // gets it. The bodies still come only from the runtime's `hook <harness>`
  // renderer; the adapter just spawns it and wraps the wake body.
  assert.equal(typeof adapter.handleMarketplaceStop, 'function');
  assert.equal(typeof adapter.handleMarketplaceSessionStart, 'function');
  assert.equal(typeof adapter.handleMarketplaceUserPromptSubmit, 'function');
  assert.equal(typeof adapter.pollWake, 'function');
  for (const retired of ['createMarketplaceAdapter', 'createClaudeAdapter', 'handleMarketplaceHook', 'handleClaudeHook']) {
    assert.equal(retired in adapter, false, `${retired} must be retired`);
  }
});

test('plugin manifests carry skills only and register no Node hook entry', async () => {
  const claude = JSON.parse(await readFile(join(packageRoot, '.claude-plugin', 'plugin.json'), 'utf8'));
  const cursor = JSON.parse(await readFile(join(packageRoot, '.cursor-plugin', 'plugin.json'), 'utf8'));

  // Hook registration moved to setup, the only party that knows the version-
  // and target-qualified private runtime path.
  assert.equal('hooks' in claude, false);
  assert.equal('hooks' in cursor, false);
  assert.ok(Array.isArray(claude.skills) && claude.skills.length > 0);
  await assert.rejects(() => readFile(join(packageRoot, 'hooks', 'hooks.json'), 'utf8'));
});

test('marketplace plugin ships the harness-native federation hooks plus the ingestion Stop', async () => {
  const hooks = JSON.parse(await readFile(join(marketplaceRoot, 'hooks', 'hooks.json'), 'utf8'));
  // harness-native-wake: the shipped plugin carries the three federation hooks
  // itself as self-resolving node shims (the runtime path is unknown at
  // marketplace-install time, so a native baked-path entry is impossible here).
  assert.match(hooks.hooks.SessionStart[0].hooks[0].command, /session-start\.mjs/);
  assert.match(hooks.hooks.UserPromptSubmit[0].hooks[0].command, /user-prompt-submit\.mjs/);
  // Two Stop entries: the fast ingestion boundary and the long-poll wake. Claude
  // and Codex run every matching Stop hook, so they coexist without either
  // suppressing the other.
  assert.match(hooks.hooks.Stop[0].hooks[0].command, /stop\.mjs/);
  const wakeEntry = hooks.hooks.Stop[1].hooks[0];
  assert.match(wakeEntry.command, /wake\.mjs/);
  // wake-async (#141): on Claude the wake runs asyncRewake — off the visible Stop
  // pipeline, waking on exit 2 — so an idle session shows no held "running stop
  // hooks" spinner. The timeout is the deliberate 1h idle-wake window / harness
  // ceiling, not the old 14520s dev-era arming value.
  assert.equal(wakeEntry.asyncRewake, true, 'the wake entry runs asyncRewake, not a synchronous hold');
  // The harness ceiling is STRICTLY above the internal 1h wake budget (3600s) so
  // the poller's own deadline fires first and drops its wake_ref before the reaper.
  assert.equal(wakeEntry.timeout, 3660, 'the harness timeout sits a margin above the 1h budget');
  assert.equal(typeof wakeEntry.rewakeMessage, 'string');
  assert.equal(typeof wakeEntry.rewakeSummary, 'string');

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
  assert.equal(calls[1][1].origin, 'external');
  assert.equal(calls[1][1].sessionId, 'agent-17');
  assert.deepEqual(calls[1][1].events, [
    { event: 'subagent', phase: 'start', outcome: 'observed', agent_type: 'loam_ingestor', session_id: 'agent-17' },
  ]);

  calls.length = 0;
  assert.deepEqual(await stop.handleSubagentStop(payload, env, { loadHooks, loadIngest }), {});
  assert.deepEqual(calls.map(([kind]) => kind), ['finalize', 'worker-finish']);
  assert.deepEqual(calls[0][1], { globalRoot, workspace, agentId: 'agent-17', env });
  assert.equal(JSON.stringify(calls[0][1]).includes('definitely succeeded'), false, 'assistant text is not completion evidence');
  assert.equal(calls[1][1].reason, 'ok');
  assert.equal(calls[1][1].run.id, 17);
  assert.equal(calls[1][1].origin, 'external');
  assert.equal(calls[1][1].sessionId, 'agent-17');
  assert.deepEqual(calls[1][1].events, [
    { event: 'subagent', phase: 'stop', outcome: 'succeeded', agent_type: 'loam_ingestor', session_id: 'agent-17' },
  ]);
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
  assert.deepEqual(finishCalls, [{
    run,
    status: 'continued',
    action: 'request_worker',
    events: [{ event: 'codex_native', phase: 'continuation', outcome: 'returned', visibility: 'native' }],
  }]);

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

test('Claude marketplace Stop records claude_recursion_guard when a loam:ingestor session is refused', async () => {
  const stop = await import(`${pathToFileURL(marketplaceStopPath).href}?claude-recursion=${Date.now()}`);
  const run = { id: 77 };
  const finishCalls = [];
  const response = await stop.handleStop(
    { cwd: '/workspace', session_id: 'claude-session', agent_type: 'loam:ingestor' },
    { CLAUDE_PLUGIN_ROOT: marketplaceRoot },
    {
      loadHooks: async () => ({
        resolveGlobalRoot: () => '/global',
        beginHookRun: async () => run,
        finishHookRun: async (call) => finishCalls.push(call),
      }),
      loadIngest: async () => ({
        resolveGlobalRoot: () => '/global',
        resolveSkillsRoot: () => '/skills',
        dispatchBoundary: async () => ({ action: 'skip', reason: 'disabled', recursion: true, workspace: '/workspace' }),
      }),
    },
  );

  assert.deepEqual(response, {});
  assert.deepEqual(finishCalls, [{
    run,
    status: 'succeeded',
    action: 'skip',
    reason: 'disabled',
    events: [{ event: 'claude_recursion_guard', outcome: 'refused', agent_type: 'loam:ingestor' }],
  }]);
});

test('marketplace Stop spawns the detached worker only after hook-finish returns', async () => {
  const stop = await import(`${pathToFileURL(marketplaceStopPath).href}?defer-spawn=${Date.now()}`);
  const order = [];
  const run = { id: 55 };
  await stop.handleStop(
    { cwd: '/workspace', session_id: 's' },
    { CLAUDE_PLUGIN_ROOT: marketplaceRoot },
    {
      loadHooks: async () => ({
        resolveGlobalRoot: () => '/global',
        beginHookRun: async () => run,
        finishHookRun: async () => { order.push('finish'); },
      }),
      loadIngest: async () => ({
        resolveGlobalRoot: () => '/global',
        resolveSkillsRoot: () => '/skills',
        dispatchBoundary: async (input) => {
          assert.equal(input.deferSpawn, true);
          return { action: 'spawn_worker', workspace: '/workspace', spawn: async () => { order.push('spawn'); } };
        },
      }),
    },
  );

  assert.deepEqual(order, ['finish', 'spawn']);
});
