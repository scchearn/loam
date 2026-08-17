import { homedir } from 'node:os';
import { readFile } from 'node:fs/promises';
import { isAbsolute, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import net from 'node:net';
import { spawn } from 'node:child_process';

const CODEX_START_FAILURE_MESSAGE = 'Loam background ingestion could not start. Run npx @scchearn/loam install to repair the installation.';

function stopResponse({ harness, visibility, outcome, failure }) {
  if (harness === 'codex' && visibility === 'native' && outcome?.native_continuation) return outcome.native_continuation;
  return harness === 'codex' && visibility === 'toast' && (failure || outcome?.reason === 'unavailable')
    ? { systemMessage: CODEX_START_FAILURE_MESSAGE }
    : {};
}

async function defaultIntegrationPath() {
  if (process.env.LOAM_INTEGRATION_PATH) return process.env.LOAM_INTEGRATION_PATH;
  const fallback = join(homedir(), '.agents', 'loam', 'integration', 'loam.mjs');
  try {
    const metadata = JSON.parse(await readFile(join(homedir(), '.agents', 'loam', 'install.json'), 'utf8'));
    return typeof metadata.integration_path === 'string' ? metadata.integration_path : fallback;
  } catch {
    return fallback;
  }
}

async function defaultIngestModules({ integrationPath } = {}) {
  integrationPath ||= await defaultIntegrationPath();
  const root = new URL('./', pathToFileURL(integrationPath));
  const [paths, ingest] = await Promise.all([
    import(new URL('paths.mjs', root).href),
    import(new URL('ingest.mjs', root).href),
  ]);
  return { ...paths, ...ingest };
}

async function defaultHookModules({ integrationPath } = {}) {
  integrationPath ||= await defaultIntegrationPath();
  const root = new URL('./', pathToFileURL(integrationPath));
  const [paths, hooks] = await Promise.all([
    import(new URL('paths.mjs', root).href),
    import(new URL('hooks.mjs', root).href),
  ]);
  return { ...paths, ...hooks };
}

export function workspaceFromPayload(payload = {}, fallback = process.cwd()) {
  const value = payload.cwd || payload.workspaceRoot || payload.workspace?.root || payload.session?.cwd || fallback;
  return resolve(value);
}

async function defaultHarvestModules({ integrationPath } = {}) {
  integrationPath ||= await defaultIntegrationPath();
  const root = new URL('./', pathToFileURL(integrationPath));
  try {
    const [paths, harvest] = await Promise.all([
      import(new URL('paths.mjs', root).href),
      import(new URL('harvest.mjs', root).href),
    ]);
    return { ...paths, ...harvest };
  } catch {
    return null;
  }
}

export async function handleMarketplaceStop(payload = {}, {
  harness = 'claude',
  env = process.env,
  loadHooks = defaultHookModules,
  loadIngest = defaultIngestModules,
  loadHarvest = defaultHarvestModules,
} = {}) {
  const workspace = workspaceFromPayload(payload);
  let hookRun = null;
  let finishHookRun;
  try {
    const hooks = await loadHooks();
    finishHookRun = hooks.finishHookRun;
    hookRun = await hooks.beginHookRun?.({
      globalRoot: hooks.resolveGlobalRoot({ env }),
      harness,
      hook: 'stop',
      workspace,
      sessionId: typeof payload?.session_id === 'string' ? payload.session_id : undefined,
    });
  } catch {}

  let failure;
  let outcome;
  let harvestOutcome;
  let visibility = 'silent';
  try {
    const { resolveGlobalRoot, resolveSkillsRoot, readIngestConfig, dispatchBoundary } = await loadIngest();
    if (!dispatchBoundary) throw new Error('Loam ingestion integration is unavailable');
    const globalRoot = env.LOAM_INGEST_GLOBAL_ROOT || resolveGlobalRoot({ env });
    if (harness === 'codex') visibility = (await readIngestConfig?.(globalRoot, env))?.visibility || 'silent';
    const dispatchPayload = {
      session_id: typeof payload?.session_id === 'string' ? payload.session_id : undefined,
      cwd: typeof payload?.cwd === 'string' ? payload.cwd : undefined,
      stop_hook_active: payload?.stop_hook_active === true,
      agent_type: typeof payload?.agent_type === 'string' ? payload.agent_type : undefined,
    };
    outcome = await dispatchBoundary({
      harness,
      payload: dispatchPayload,
      globalRoot,
      skillsRoot: env.LOAM_INGEST_SKILLS_ROOT || resolveSkillsRoot({ env }),
      hookRunId: hookRun?.id,
      env,
      // S1: defer the physical worker spawn so it runs after finishHookRun, so
      // the worker's worker-start sees the finished, action-set parent.
      deferSpawn: true,
    });
    const harvest = await loadHarvest();
    if (harvest?.harvestTick) {
      try {
        harvestOutcome = await harvest.harvestTick({
          harness,
          payload: dispatchPayload,
          globalRoot,
          env,
          hookRunId: hookRun?.id,
        });
      } catch {}
    }
  } catch (error) {
    failure = error;
  }

  if (hookRun && finishHookRun) {
    try {
      let finishArgs;
      if (failure) {
        finishArgs = { status: 'failed', detail: failure instanceof Error ? failure.message : String(failure) };
      } else if (harness === 'codex' && outcome?.native_continuation) {
        // The Codex native path requested a parent-model spawn without spawning
        // one itself: record the distinct continued lane and its typed event.
        finishArgs = {
          status: 'continued',
          action: 'request_worker',
          events: [{ event: 'codex_native', phase: 'continuation', outcome: 'returned', visibility: 'native' }],
        };
      } else if (harvestOutcome?.action === 'spawn_worker') {
        finishArgs = { status: 'succeeded', action: 'spawn_worker', reason: 'harvest_dispatched' };
      } else {
        finishArgs = {
          status: 'succeeded',
          action: outcome?.action,
          ...(outcome?.reason !== undefined ? { reason: outcome.reason } : {}),
          ...(outcome?.detail !== undefined ? { detail: outcome.detail } : {}),
          ...(harness === 'claude' && outcome?.recursion
            ? { events: [{ event: 'claude_recursion_guard', outcome: 'refused', agent_type: 'loam:ingestor' }] }
            : {}),
        };
      }
      await finishHookRun({ run: hookRun, ...finishArgs });
    } catch {}
  }
  // Spawn the detached worker only now that hook-finish has returned (S1).
  if (typeof outcome?.spawn === 'function') {
    try { await outcome.spawn(); } catch {}
  }
  return stopResponse({ harness, visibility, outcome, failure });
}

function shellArg(value, platform = process.platform) {
  const text = String(value);
  return platform === 'win32'
    ? `'${text.replaceAll("'", "''")}'`
    : `'${text.replaceAll("'", `'"'"'`)}'`;
}

function nativeAgentContext(bound, { globalRoot, platform = process.platform } = {}) {
  const command = [
    process.execPath,
    bound.worker_path,
    '--native-prepare',
    '--global-root', globalRoot,
    '--workspace', bound.workspace,
    '--agent-id', bound.agent_id,
  ].map((value) => shellArg(value, platform)).join(' ');
  return [
    'You are the Codex loam_ingestor subagent for this pending Loam run.',
    `Workspace: ${bound.workspace}`,
    `Agent identity: ${bound.agent_id}`,
    `Installed integration: ${bound.integration_path}`,
    `Installed adapter: ${bound.adapter_path}`,
    'Your first action must be exactly this preparation command:',
    command,
    'Parse its one-line JSON output. If action is "skip", stop immediately without invoking any ingestion skill.',
    `Only if action is "run", invoke loam::ingesting-codebase exactly once for ${bound.workspace}.`,
    'Do not spawn or delegate to any other agent.',
  ].join('\n');
}

function nativeHookRun(result, globalRoot) {
  return Number.isSafeInteger(result?.hook_run_id) && result.hook_run_id > 0
    ? { id: result.hook_run_id, globalRoot, workspace: result.workspace }
    : null;
}

export async function handleMarketplaceSubagentStart(payload = {}, {
  harness = 'claude',
  env = process.env,
  loadHooks = defaultHookModules,
  loadIngest = defaultIngestModules,
  platform = process.platform,
} = {}) {
  if (harness !== 'codex' || payload?.agent_type !== 'loam_ingestor') return {};
  try {
    const ingest = await loadIngest();
    const globalRoot = env.LOAM_INGEST_GLOBAL_ROOT || ingest.resolveGlobalRoot({ env });
    const bound = await ingest.bindNativeAgent({
      globalRoot,
      workspace: workspaceFromPayload(payload),
      agentId: payload.agent_id,
    });
    if (bound?.status !== 'bound' && bound?.status !== 'late') return {};
    const run = bound.owns_claim ? nativeHookRun(bound, globalRoot) : null;
    if (run) {
      // The observed native subagent is the external worker-start proof; the
      // native command inserts it and the transition together, all-or-nothing.
      try {
        await (await loadHooks()).startHookWorker?.({
          run,
          sessionId: bound.agent_id,
          origin: 'external',
          events: [{ event: 'subagent', phase: 'start', outcome: 'observed', agent_type: 'loam_ingestor', session_id: bound.agent_id }],
        });
      } catch {}
    }
    return {
      hookSpecificOutput: {
        hookEventName: 'SubagentStart',
        additionalContext: nativeAgentContext(bound, { globalRoot, platform }),
      },
    };
  } catch {
    return {};
  }
}

export async function handleMarketplaceSubagentStop(payload = {}, {
  harness = 'claude',
  env = process.env,
  loadHooks = defaultHookModules,
  loadIngest = defaultIngestModules,
} = {}) {
  if (harness !== 'codex' || payload?.agent_type !== 'loam_ingestor') return {};
  try {
    const ingest = await loadIngest();
    const globalRoot = env.LOAM_INGEST_GLOBAL_ROOT || ingest.resolveGlobalRoot({ env });
    const result = await ingest.finalizeNativeAgentRun({
      globalRoot,
      workspace: workspaceFromPayload(payload),
      agentId: payload.agent_id,
      env,
    });
    const run = result?.owns_claim ? nativeHookRun(result, globalRoot) : null;
    if (run) {
      const stopOutcome = result.reason === 'ok' ? 'succeeded'
        : result.reason === 'unavailable' ? 'failed' : 'skipped';
      try {
        await (await loadHooks()).finishHookWorker?.({
          run,
          reason: result.reason,
          origin: 'external',
          sessionId: payload.agent_id,
          // The native run's preparation/finalization (buffered across the
          // prepare/stop boundary) flush before the subagent/stop proof.
          events: [
            ...(Array.isArray(result.events) ? result.events : []),
            { event: 'subagent', phase: 'stop', outcome: stopOutcome, agent_type: 'loam_ingestor', session_id: payload.agent_id },
          ],
        });
      } catch {}
    }
  } catch {}
  return {};
}

// ---------------------------------------------------------------------------
// Harness-native federation lane (harness-native-wake): the shipped marketplace
// plugin carries SessionStart render+register, UserPromptSubmit per-turn drain,
// and a Stop-hook long-poll wake — so a machine that installed the plugin from
// the marketplace ALONE (no `npx @scchearn/loam install` staging native hooks)
// still gets the full OpenCode-parity federation surface. Every rendered body
// comes from the runtime's own `hook <harness>` renderer (the same three-surfaces
// path OpenCode uses); this adapter only spawns it and wraps the wake body in the
// documented block-decision. No hcom, no PTY, no message content in any log.
// ---------------------------------------------------------------------------

const WAKE_KIND = 'loam-wake';
// Soft-degrade text when no runtime resolves (a marketplace-only machine with no
// staged install): the SessionStart surface shows a repair hint; the per-turn and
// wake surfaces stay silent rather than spam. Mirrors the OpenCode UNAVAILABLE hint.
const UNAVAILABLE_HINT = 'You have loam.\nLoam is unavailable. Run: npx @scchearn/loam install';
// The Stop poller holds the session at most this long, then returns allow-stop.
// The harness hook `timeout` is the hard ceiling; this bound keeps a killed
// connector or a quiet idle from living forever as a node process. Named consts,
// tuned here rather than hardcoded inline. Ceiling: keep hooks.json Stop timeout
// (seconds) at or above STOP_WAKE_BUDGET_MS/1000.
const STOP_WAKE_BUDGET_MS = 4 * 60 * 60 * 1000;
// Re-arm cadence: every window the poller re-registers the wake_ref so a connector
// restart mid-idle re-establishes it (#112 self-heal), and — because the poller's
// notify port is ephemeral — a stale persisted ref pruned on a failed wake is
// replaced on the next cycle (connector-self-healing seam 4's designed behavior).
const STOP_WAKE_RENEW_MS = 4 * 60 * 1000;

/**
 * Resolve the private runtime binary and global root the same way stop.mjs
 * resolves the integration (install.json is the single ladder root; LOAM_HOME
 * overrides it). Returns null when nothing is installed — the caller degrades
 * softly. This is deliberately NOT a second resolution ladder: it reads the same
 * install.json the ingestion lane already reads, taking `runtime_path` from it.
 */
async function resolveRuntimePaths(env = process.env) {
  try {
    const globalRoot = env.LOAM_HOME && isAbsolute(env.LOAM_HOME)
      ? resolve(env.LOAM_HOME)
      : join(homedir(), '.agents', 'loam');
    const install = JSON.parse(await readFile(join(globalRoot, 'install.json'), 'utf8'));
    const runtimePath = typeof install.runtime_path === 'string' ? install.runtime_path : null;
    return runtimePath ? { runtimePath, globalRoot } : null;
  } catch {
    return null;
  }
}

/**
 * Spawn the runtime, feed a bounded stdin frame, collect stdout. `ok` honors the
 * exit code — a nonzero runtime exit (e.g. an inject-contract refusal) is a
 * failure, not a success with empty output — so the register/drain breadcrumbs
 * can name the exit. A spawn error or timeout resolves ok:false, never throws.
 */
function spawnRuntime(runtimePath, args, input = '{}', timeoutMs = 5000) {
  return new Promise((settle) => {
    let child;
    try {
      child = spawn(runtimePath, args, { stdio: ['pipe', 'pipe', 'ignore'], windowsHide: true });
    } catch {
      settle({ ok: false, code: null, stdout: '' });
      return;
    }
    let stdout = '';
    let done = false;
    const finish = (result) => { if (done) return; done = true; clearTimeout(timer); settle(result); };
    const timer = setTimeout(() => { try { child.kill(); } catch {} finish({ ok: false, code: null, stdout: '' }); }, timeoutMs);
    child.stdout.setEncoding('utf8');
    child.stdout.on('data', (chunk) => { if (stdout.length < 1_048_576) stdout += chunk; });
    child.once('error', () => finish({ ok: false, code: null, stdout: '' }));
    child.once('close', (code) => finish({ ok: code === 0, code, stdout: stdout.trim() }));
    child.stdin.on('error', () => {});
    child.stdin.end(input === undefined ? '{}' : String(input));
  });
}

/**
 * Render one hook surface through the runtime's own `hook <harness>` command —
 * the SAME renderer OpenCode drives, so bodies are never formatted here. For
 * SessionStart/UserPromptSubmit the runtime emits the harness-native envelope
 * (and, on SessionStart, registers the session); for the Wake drain `--body`
 * emits the bare three-surfaces body the poller wraps in a block-decision.
 * `unavailable:true` marks a machine with no staged runtime.
 */
async function defaultRender({ harness, event, workspace, sessionId, bodyOnly = false, payload, env = process.env, timeoutMs = 5000 } = {}) {
  const paths = await resolveRuntimePaths(env);
  if (!paths) return { unavailable: true, ok: false, code: null, stdout: '' };
  const args = buildRenderArgs({ harness, event, workspace, sessionId, bodyOnly });
  const result = await spawnRuntime(paths.runtimePath, args, JSON.stringify(payload ?? {}), timeoutMs);
  return { ...result, unavailable: false };
}

/**
 * Argv for `loam hook <harness> --event <event> [--workspace] [--session-id]
 * [--body]`. The mailbox events (Wake, per-turn) REQUIRE `--session-id` or the
 * runtime refuses the drain and renders empty; the Wake drain adds `--body` to
 * get the bare three-surfaces body the poller wraps. Pinned by a contract test so
 * the shape can't drift.
 */
export function buildRenderArgs({ harness, event, workspace = null, sessionId = null, bodyOnly = false }) {
  const args = ['hook', harness, '--event', event];
  if (workspace) args.push('--workspace', workspace);
  if (sessionId) args.push('--session-id', sessionId);
  if (bodyOnly) args.push('--body');
  return args;
}

/**
 * Argv for `federation inject <register|drop> <workspace> --global-root ...
 * --session-id ... [--wake-ref ...]`. Workspace is POSITIONAL — `--workspace` is
 * rejected as an unknown flag (exit 64) — and only register carries the wake_ref.
 * Mirrors the OpenCode plugin's builder; pinned by a contract test.
 */
export function buildInjectArgs({ action, workspace, globalRoot, sessionId, wakeRef = null }) {
  const args = ['federation', 'inject', action, workspace, '--global-root', globalRoot, '--session-id', sessionId];
  if (action === 'register' && wakeRef) args.push('--wake-ref', wakeRef);
  return args;
}

/**
 * The soft-degrade output for a machine with no runtime. SessionStart shows the
 * repair hint; the per-turn surface stays silent (empty additionalContext) so a
 * broken install never spams every prompt. Codex consumes the bare body on
 * stdout (its runtime envelope is the plain body); the additionalContext harnesses
 * get the documented hookSpecificOutput shape.
 */
function unavailableResponse(harness, event) {
  const hint = event === 'SessionStart' ? UNAVAILABLE_HINT : '';
  if (harness === 'codex') return hint;
  return JSON.stringify({ hookSpecificOutput: { hookEventName: event, additionalContext: hint } });
}

/**
 * SessionStart: forward the runtime's rendered envelope verbatim (it already
 * carries the workflow baseline + Federation section and registers the session).
 * A missing runtime or a failed/empty render degrades to the repair hint rather
 * than a broken session.
 */
export async function handleMarketplaceSessionStart(payload = {}, {
  harness = 'claude', env = process.env, render = defaultRender,
} = {}) {
  const workspace = workspaceFromPayload(payload);
  const sessionId = typeof payload?.session_id === 'string' ? payload.session_id : undefined;
  const result = await render({ harness, event: 'SessionStart', workspace, sessionId, payload, env });
  if (result.unavailable || !result.ok || !result.stdout) return unavailableResponse(harness, 'SessionStart');
  return result.stdout;
}

/**
 * UserPromptSubmit: forward the runtime's per-turn drain envelope verbatim. An
 * unavailable/failed render is a silent no-op (empty envelope), never the repair
 * hint — the drain already returns a valid empty envelope when nothing is new.
 */
export async function handleMarketplaceUserPromptSubmit(payload = {}, {
  harness = 'claude', env = process.env, render = defaultRender,
} = {}) {
  const workspace = workspaceFromPayload(payload);
  const sessionId = typeof payload?.session_id === 'string' ? payload.session_id : undefined;
  const result = await render({ harness, event: 'UserPromptSubmit', workspace, sessionId, payload, env });
  if (result.unavailable || !result.ok || !result.stdout) return unavailableResponse(harness, 'UserPromptSubmit');
  return result.stdout;
}

/**
 * The in-hook wake listener: a localhost notify socket the connector's wake
 * fanout (`wake_one`) connects to, exactly the notify-tcp scheme OpenCode's
 * plugin listener uses. `next(timeoutMs)` resolves true when a `loam-wake` frame
 * has arrived since the last check, false on the bounded timeout — the shape the
 * wait-and-renew loop needs. Only the topic-derived hint is ever surfaced to the
 * log, never sender content.
 */
export async function startWakeListener({ log = null } = {}) {
  let fired = false;
  let notify = null;
  const server = net.createServer((socket) => {
    let frame = '';
    socket.setEncoding('utf8');
    socket.on('data', (chunk) => {
      frame += chunk;
      if (frame.includes(`"kind":"${WAKE_KIND}"`)) {
        const hint = (frame.match(/"hint":"([^"]*)"/) || [])[1] || '';
        void log?.('wake frame', { hint });
        socket.end();
        fired = true;
        if (notify) { const fire = notify; notify = null; fire(); }
      }
    });
    socket.on('error', () => {});
  });
  await new Promise((resolveListen, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', resolveListen);
  });
  const address = server.address();
  const port = typeof address === 'object' && address ? address.port : 0;
  return {
    wakeRef: `notify-tcp://127.0.0.1:${port}`,
    port,
    next: (timeoutMs) => new Promise((resolveNext) => {
      if (fired) { fired = false; return resolveNext(true); }
      const timer = setTimeout(() => { notify = null; resolveNext(false); }, timeoutMs);
      notify = () => { clearTimeout(timer); fired = false; resolveNext(true); };
    }),
    close: () => new Promise((resolveClose) => server.close(() => resolveClose())),
  };
}

/**
 * Register or drop the poller's wake_ref through `federation inject`. Workspace
 * is POSITIONAL (the CLI rejects `--workspace` as unknown, exit 64); register
 * carries the ref, drop omits it. Idempotent on the connector side (seam 4's
 * upsert), so re-registering each cycle is safe.
 */
async function defaultInject(action, { harness, payload, env = process.env, wakeRef = null, paths = null, timeoutMs = 5000 } = {}) {
  paths ||= await resolveRuntimePaths(env);
  if (!paths) return { ok: false, code: null };
  const workspace = workspaceFromPayload(payload);
  const sessionId = typeof payload?.session_id === 'string' ? payload.session_id : '';
  const args = buildInjectArgs({ action, workspace, globalRoot: paths.globalRoot, sessionId, wakeRef });
  const result = await spawnRuntime(paths.runtimePath, args, '{}', timeoutMs);
  return { ok: result.ok, code: result.code };
}

/**
 * The Stop-hook long-poll wake (Scenario 3). Owns a notify-tcp wake_ref for the
 * life of the poll, blocks on it with a bounded, renewable wait, and answers a
 * wake by draining via the Wake path and returning the documented block-decision
 * — `{decision:"block", reason:<rendered body>}`, identical for Claude and Codex
 * (verified against codex-rs stop.command.output.schema.json). Every guarantee is
 * structural:
 *  - block ONLY with a newly admitted frame in hand (a non-empty drain), so
 *    consecutive blocks are consecutive deliveries, never a stop_hook_active loop;
 *  - every wait is bounded and the registration renewable, so a killed connector
 *    or expired ref degrades to allow-stop (the `fallback`), never a hung session;
 *  - no session id or no runtime => immediate fallback (can't register a drain).
 */
export async function pollWake(payload = {}, {
  harness = 'claude',
  env = process.env,
  fallback = {},
  render = defaultRender,
  inject = defaultInject,
  listen = startWakeListener,
  resolvePaths = resolveRuntimePaths,
  budgetMs = STOP_WAKE_BUDGET_MS,
  renewMs = STOP_WAKE_RENEW_MS,
  now = () => Date.now(),
  log = null,
} = {}) {
  const sessionId = typeof payload?.session_id === 'string' ? payload.session_id : null;
  if (!sessionId) return fallback;
  // No runtime installed (a machine that never ran `npx install`): a wake is
  // impossible, so don't even open a socket — degrade to allow-stop.
  const paths = await resolvePaths(env);
  if (!paths) return fallback;

  let listener;
  try { listener = await listen({ log }); }
  catch { return fallback; }

  const workspace = workspaceFromPayload(payload);
  // Logged for observability only; it needs no gate — the newly-admitted-frame
  // rule above is the real loop guard.
  void log?.('wake poll', { port: listener.port ?? null, active: payload?.stop_hook_active === true });
  try {
    let armed = await inject('register', { harness, payload, env, wakeRef: listener.wakeRef, paths });
    await log?.('wake register', { ok: Boolean(armed?.ok), exit: armed?.code ?? null });
    // Connector unreachable at the first arm: no wake is possible this idle, so
    // degrade to allow-stop rather than block a session on a dead connector.
    if (!armed?.ok) return fallback;

    const deadline = now() + budgetMs;
    while (now() < deadline) {
      const waitMs = Math.min(renewMs, deadline - now());
      const fired = await listener.next(waitMs);
      if (fired) {
        const drain = await render({ harness, event: 'Wake', workspace, sessionId, bodyOnly: true, payload, env });
        const body = !drain.unavailable && drain.ok ? drain.stdout : '';
        if (body) {
          await log?.('wake block', { bytes: body.length });
          return { decision: 'block', reason: body };
        }
        // Empty drain: a wake that raced a per-turn drain (mailbox already
        // consumed) — not a delivery, so keep waiting rather than block on nothing.
        await log?.('wake drain', { outcome: 'empty' });
        continue;
      }
      // Renew window elapsed with no frame: re-arm. A failed re-arm means the
      // connector died mid-poll -> degrade to allow-stop.
      armed = await inject('register', { harness, payload, env, wakeRef: listener.wakeRef, paths });
      if (!armed?.ok) { await log?.('wake renew', { ok: false, exit: armed?.code ?? null }); return fallback; }
    }
    await log?.('wake timeout', {});
    return fallback;
  } finally {
    await inject('drop', { harness, payload, env, paths }).catch(() => {});
    await listener.close().catch(() => {});
  }
}
