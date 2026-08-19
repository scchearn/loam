#!/usr/bin/env node

import { handleMarketplaceUserPromptSubmit } from '../adapter.mjs';
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

export async function handleUserPromptSubmit(payload = {}, env = process.env, options = {}) {
  return handleMarketplaceUserPromptSubmit(payload, { ...options, env, harness: marketplaceHarness(env) });
}

// The runtime already produced the per-turn envelope (or a valid empty one);
// write it verbatim.
if (process.argv[1] && resolve(process.argv[1]) === resolve(fileURLToPath(import.meta.url))) {
  process.stdout.write(`${await handleUserPromptSubmit(await readPayload())}\n`);
}
