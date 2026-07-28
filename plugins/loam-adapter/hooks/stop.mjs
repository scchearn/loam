#!/usr/bin/env node

import { handleMarketplaceStop } from '../adapter.mjs';
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

export function marketplaceHarness(env = process.env) {
  return env.PLUGIN_ROOT ? 'codex' : 'claude';
}

export async function handleStop(payload = {}, env = process.env, options = {}) {
  return handleMarketplaceStop(payload, {
    ...options,
    env,
    harness: marketplaceHarness(env),
  });
}

if (process.argv[1] && resolve(process.argv[1]) === resolve(fileURLToPath(import.meta.url))) {
  process.stdout.write(`${JSON.stringify(await handleStop(await readPayload()))}\n`);
}
