#!/usr/bin/env node

import { createClaudeAdapter, handleClaudeHook } from '../adapters/claude-session-start.mjs';
import { handleCursorHook } from '../adapters/cursor-session-start.mjs';
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

export async function handleSessionStart(payload = {}, env = process.env) {
  if (env.CURSOR_PLUGIN_ROOT) return handleCursorHook(payload);
  if (env.CLAUDE_PLUGIN_ROOT && !env.COPILOT_CLI) return handleClaudeHook(payload);

  const claude = await createClaudeAdapter()(payload);
  return { additionalContext: claude.hookSpecificOutput.additionalContext };
}

if (process.argv[1] && resolve(process.argv[1]) === resolve(fileURLToPath(import.meta.url))) {
  process.stdout.write(`${JSON.stringify(await handleSessionStart(await readPayload()))}\n`);
}
