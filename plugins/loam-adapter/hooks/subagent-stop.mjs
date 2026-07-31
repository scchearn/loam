#!/usr/bin/env node

import { handleMarketplaceSubagentStop } from '../adapter.mjs';
import { fileURLToPath } from 'node:url';
import { resolve } from 'node:path';

async function readPayload() {
  let input = '';
  process.stdin.setEncoding('utf8');
  for await (const chunk of process.stdin) input += chunk;
  try { return input.trim() ? JSON.parse(input) : {}; }
  catch { return {}; }
}

export function marketplaceHarness(env = process.env) {
  return env.PLUGIN_ROOT ? 'codex' : 'claude';
}

export async function handleSubagentStop(payload = {}, env = process.env, options = {}) {
  return handleMarketplaceSubagentStop(payload, {
    ...options,
    env,
    harness: marketplaceHarness(env),
  });
}

if (process.argv[1] && resolve(process.argv[1]) === resolve(fileURLToPath(import.meta.url))) {
  process.stdout.write(`${JSON.stringify(await handleSubagentStop(await readPayload()))}\n`);
}
