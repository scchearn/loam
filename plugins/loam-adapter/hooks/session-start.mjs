#!/usr/bin/env node

import { handleMarketplaceSessionStart } from '../adapter.mjs';
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

export async function handleSessionStart(payload = {}, env = process.env, options = {}) {
  return handleMarketplaceSessionStart(payload, { ...options, env, harness: marketplaceHarness(env) });
}

// The runtime already produced the harness-native envelope; write it verbatim.
if (process.argv[1] && resolve(process.argv[1]) === resolve(fileURLToPath(import.meta.url))) {
  process.stdout.write(`${await handleSessionStart(await readPayload())}\n`);
}
