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

// A SECOND Stop hook beside stop.mjs: Claude and Codex run every matching Stop
// hook, so the ingestion boundary (stop.mjs) stays a fast one-shot while this one
// long-polls the wake socket. On a frame it returns {decision:"block", reason};
// on timeout / no runtime / no session it returns {} (allow-stop).
export async function handleWake(payload = {}, env = process.env, options = {}) {
  return pollWake(payload, { harness: marketplaceHarness(env), env, fallback: {}, ...options });
}

if (process.argv[1] && resolve(process.argv[1]) === resolve(fileURLToPath(import.meta.url))) {
  process.stdout.write(`${JSON.stringify(await handleWake(await readPayload()))}\n`);
}
