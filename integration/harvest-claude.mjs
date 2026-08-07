import { readdir } from 'node:fs/promises';
import { homedir } from 'node:os';
import { join, resolve } from 'node:path';

import { HarvestError } from './harvest-store.mjs';
import { readTail } from './harvest-store.mjs';
import { measureStore, registerHarvestBackend } from './harvest-store.mjs';

registerHarvestBackend('claude', {
  locateStore: ({ sessionId, env }) => locateClaudeStore({ sessionId, env }),
  measure: ({ store }) => measureStore(store.path),
  readWindow: async ({ store, state, config }) => parseClaudeStore({ path: store.path, sessionId: state.session_id, lines: (await readTail(store.path, state.cursor?.value || 0, { maxBytes: config.max_window_bytes })).lines }),
});

const SKIP_TYPES = new Set(['summary', 'system', 'result', 'progress', 'file-history-snapshot', 'saved_hook_context']);
const ERROR_RE = /(?:\b(?:error|failed|fail(?:ure)?|exception)\b|exit code\s*[1-9])/i;

function parseContentBlock(block) {
  if (!block || typeof block !== 'object') return null;
  const type = block.type;
  if (type === 'text') return { kind: 'text', text: String(block.text ?? '') };
  if (type === 'tool_use') {
    return {
      kind: 'tool_use',
      tool_use_id: String(block.id ?? ''),
      name: String(block.name ?? ''),
      input: block.input ?? {},
    };
  }
  if (type === 'tool_result') {
    return {
      kind: 'tool_result',
      tool_use_id: String(block.tool_use_id ?? ''),
      is_error: block.is_error === true,
      content: typeof block.content === 'string' ? block.content
        : Array.isArray(block.content)
          ? block.content.map((part) => typeof part?.text === 'string' ? part.text : '').join('\n')
          : '',
    };
  }
  return null;
}

function isErrorFromResult(result) {
  if (result.is_error === true) return true;
  const head = String(result.content ?? '').slice(0, 500);
  return ERROR_RE.test(head);
}

export async function locateClaudeStore({ sessionId, env = process.env }) {
  const configDir = env.CLAUDE_CONFIG_DIR || join(homedir(), '.claude');
  const projects = join(resolve(configDir), 'projects');
  let entries;
  try { entries = await readdir(projects, { withFileTypes: true }); } catch {
    throw new HarvestError('store_missing', `no Claude projects directory at ${projects}`);
  }
  for (const entry of entries) {
    if (!entry.isDirectory()) continue;
    const candidate = join(projects, entry.name, `${sessionId}.jsonl`);
    try {
      const { stat } = await import('node:fs/promises');
      await stat(candidate);
      return { path: candidate, kind: 'jsonl' };
    } catch {}
  }
  throw new HarvestError('store_missing', `no Claude store found for session ${sessionId}`);
}

export async function parseClaudeStore({ path, sessionId, lines } = {}) {
  const storeLines = lines || (await readTail(path, 0, { maxBytes: 512 * 1024 * 1024 })).lines;
  const records = [];
  let skipped = 0;
  for (const line of storeLines) {
    let record;
    try { record = JSON.parse(line.text); } catch { skipped += 1; continue; }
    if (!record || typeof record !== 'object') { skipped += 1; continue; }
    const type = record.type;
    if (SKIP_TYPES.has(type)) continue;
    if (record.isMeta === true || record.isCompactSummary === true || record.isSidechain === true) continue;
    const recordSession = record.sessionId ?? record.session_id;
    if (type === 'user') {
      const content = record.message?.content;
      const text = typeof content === 'string' ? content : '';
      if (typeof content === 'string') {
        if (text.trim()) records.push({ cursor: line.offset, kind: 'user', session_id: recordSession, text, timestamp: record.timestamp ?? '' });
        continue;
      }
      if (Array.isArray(content)) {
        let hasText = false;
        let textParts = [];
        const results = [];
        for (const block of content) {
          const parsed = parseContentBlock(block);
          if (!parsed) continue;
          if (parsed.kind === 'text') { hasText = true; textParts.push(parsed.text); }
          else if (parsed.kind === 'tool_result') {
            results.push(parsed);
          }
        }
        if (hasText && textParts.some((t) => t.trim())) {
          records.push({
            cursor: line.offset, kind: 'user', session_id: recordSession,
            text: textParts.join('\n'), timestamp: record.timestamp ?? '',
          });
        }
        for (const result of results) {
          if (recordSession === sessionId) {
            records.push({
              cursor: line.offset, kind: 'tool_result', session_id: recordSession,
              tool_use_id: result.tool_use_id, is_error: result.is_error === true,
              content: result.content, timestamp: record.timestamp ?? '',
            });
          }
        }
        continue;
      }
      continue;
    }
    if (type === 'assistant') {
      const content = record.message?.content;
      if (!Array.isArray(content)) continue;
      const textParts = [];
      const toolUses = [];
      for (const block of content) {
        const parsed = parseContentBlock(block);
        if (!parsed) continue;
        if (parsed.kind === 'text' && parsed.text.trim()) textParts.push(parsed.text);
        else if (parsed.kind === 'tool_use') toolUses.push(parsed);
      }
      if (textParts.length) {
        records.push({
          cursor: line.offset, kind: 'assistant', session_id: recordSession,
          text: textParts.join('\n'), timestamp: record.timestamp ?? '',
        });
      }
      for (const toolUse of toolUses) {
        records.push({
          cursor: line.offset, kind: 'tool_use', session_id: recordSession,
          tool_use_id: toolUse.tool_use_id, name: toolUse.name, input: toolUse.input,
          timestamp: record.timestamp ?? '',
        });
      }
      continue;
    }
  }
  return { records, skipped };
}

export function claudeResultError(result) {
  return isErrorFromResult(result);
}
