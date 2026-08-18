#!/usr/bin/env node

import { pollWake } from '../adapter.mjs';
import { marketplaceHarness } from './stop.mjs';
import { fileURLToPath } from 'node:url';
import { resolve } from 'node:path';

async function readPayload() {
  let input = '';
  process.stdin.setEncoding('utf8');
  for await (const chunk of process.stdin) input += chunk;
  try {
    return input.trim() ? JSON.parse(input) : {};
  } catch {
    return {};
  }
}

// Headless Claude (`claude -p`, the Agent SDK path) sets entrypoint=sdk-cli and
// never idles waiting for a human, so arming the wake window would just hold the
// Stop hook until its timeout and hang the invocation (#140). Probe-verified on
// this harness: an interactive terminal is entrypoint `cli`, `claude -p` is
// `sdk-cli`. Only Claude carries this env; Codex is gated out by the harness check.
export function isHeadlessClaude(env = process.env) {
  return env.CLAUDE_CODE_ENTRYPOINT === 'sdk-cli';
}

// A SECOND Stop hook beside stop.mjs: Claude and Codex run every matching Stop
// hook, so the ingestion boundary (stop.mjs) stays a fast one-shot while this one
// waits on the wake socket. It resolves the harness-agnostic core result: a frame
// gives {decision:"block", reason}, timeout / no runtime / no session / headless
// gives {} (allow-stop). The CLI entry below translates that per harness.
export async function handleWake(payload = {}, env = process.env, options = {}) {
  const harness = marketplaceHarness(env);
  // No human to wake and a held Stop hangs the invocation — don't even open a socket.
  if (harness === 'claude' && isHeadlessClaude(env)) return {};
  return pollWake(payload, { harness, env, fallback: {}, ...options });
}

// Translate the harness-agnostic core result into per-harness process output.
//  - Claude (asyncRewake, registered in hooks.json): the hook runs off the visible
//    pipeline; a frame wakes the idle session by writing the body to stderr and
//    exiting 2, otherwise exit 0 (allow-stop) with nothing on stdout. No decision
//    JSON — that channel is the synchronous Codex path only.
//  - Codex: the synchronous stop.command block-decision contract (verified against
//    codex-rs stop.command.output.schema.json): body on stdout, exit 0.
export function renderWakeOutput(result, harness) {
  if (harness === 'claude') {
    if (result && result.decision === 'block' && typeof result.reason === 'string' && result.reason) {
      return { exitCode: 2, stdout: '', stderr: result.reason };
    }
    return { exitCode: 0, stdout: '', stderr: '' };
  }
  return { exitCode: 0, stdout: `${JSON.stringify(result ?? {})}\n`, stderr: '' };
}

if (process.argv[1] && resolve(process.argv[1]) === resolve(fileURLToPath(import.meta.url))) {
  const env = process.env;
  const result = await handleWake(await readPayload(), env);
  const { exitCode, stdout, stderr } = renderWakeOutput(result, marketplaceHarness(env));
  // Set exitCode and write, then let the process exit naturally so both streams flush.
  process.exitCode = exitCode;
  if (stdout) process.stdout.write(stdout);
  if (stderr) process.stderr.write(stderr);
}
