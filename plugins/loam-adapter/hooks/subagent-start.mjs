#!/usr/bin/env node

import { handleMarketplaceSubagentStart } from '../adapter.mjs';
import { fileURLToPath } from 'node:url';
import { resolve } from 'node:path';

async function readPayload() {
  let input = '';
  process.stdin.setEncoding('utf8');
  for await (const chunk of process.stdin) input += chunk;
  try { return input.trim() ? JSON.parse(input) : {}; }
  catch { return {}; }
}

export function marketplaceHarness(payload = {}) {
  return payload.agent_type === 'loam_ingestor'
    && typeof payload.turn_id === 'string' && payload.turn_id.length > 0
    ? 'codex' : 'claude';
}

export async function handleSubagentStart(payload = {}, env = process.env, options = {}) {
  return handleMarketplaceSubagentStart(payload, {
    ...options,
    env,
    harness: marketplaceHarness(payload),
  });
}

if (process.argv[1] && resolve(process.argv[1]) === resolve(fileURLToPath(import.meta.url))) {
  process.stdout.write(`${JSON.stringify(await handleSubagentStart(await readPayload()))}\n`);
}
