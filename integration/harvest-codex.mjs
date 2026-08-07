import { readdir, stat } from 'node:fs/promises';
import { homedir } from 'node:os';
import { join, resolve } from 'node:path';

import { HarvestError } from './harvest-store.mjs';
import { readTail } from './harvest-store.mjs';
import { measureStore, registerHarvestBackend } from './harvest-store.mjs';

registerHarvestBackend('codex', {
  locateStore: ({ sessionId, env }) => locateCodexStore({ sessionId, env }),
  measure: ({ store }) => measureStore(store.path),
  readWindow: async ({ store, state, config }) => parseCodexStore({ path: store.path, sessionId: state.session_id, lines: (await readTail(store.path, state.cursor?.value || 0, { maxBytes: config.max_window_bytes })).lines }),
});

const INJECTED_PREFIXES = ['<environment_context>', '<permissions', '# AGENTS.md'];
const ERROR_RE = /(?:\b(?:error|failed|fail(?:ure)?|exception)\b|exit code\s*[1-9])/i;

function isInjected(text) {
  const trimmed = String(text ?? '').trimStart();
  return INJECTED_PREFIXES.some((prefix) => trimmed.startsWith(prefix));
}

function extractText(content) {
  if (typeof content === 'string') return content;
  if (Array.isArray(content)) {
    return content
      .map((part) => {
        if (typeof part?.text === 'string') return part.text;
        return '';
      })
      .join('\n');
  }
  return '';
}

async function findRollouts(sessionsRoot, sessionId) {
  const results = [];
  async function walk(directory) {
    let entries;
    try { entries = await readdir(directory, { withFileTypes: true }); } catch { return; }
    for (const entry of entries) {
      const path = join(directory, entry.name);
      if (entry.isDirectory()) {
        if (entry.name === 'node_modules' || entry.name.startsWith('.')) continue;
        await walk(path);
      } else if (entry.isFile() && entry.name.includes(`-${sessionId}.jsonl`)) {
        try {
          const info = await stat(path);
          results.push({ path, mtime: info.mtimeMs });
        } catch {}
      }
    }
  }
  await walk(resolve(sessionsRoot));
  return results;
}

export async function locateCodexStore({ sessionId, env = process.env }) {
  const codexHome = env.CODEX_HOME || join(homedir(), '.codex');
  const sessionsRoot = join(resolve(codexHome), 'sessions');
  const found = await findRollouts(sessionsRoot, sessionId);
  if (!found.length) throw new HarvestError('store_missing', `no Codex rollout found for session ${sessionId}`);
  found.sort((a, b) => b.mtime - a.mtime);
  return { path: found[0].path, kind: 'jsonl' };
}

function normaliseOutput(output) {
  if (typeof output === 'string') {
    const trimmed = output.trim();
    if (trimmed.startsWith('[')) {
      try {
        const parsed = JSON.parse(trimmed);
        if (Array.isArray(parsed)) {
          return parsed
            .map((part) => (typeof part?.text === 'string' ? part.text : ''))
            .join('\n');
        }
      } catch {}
    }
    return output;
  }
  if (Array.isArray(output)) {
    return output
      .map((part) => (typeof part?.text === 'string' ? part.text : ''))
      .join('\n');
  }
  if (output && typeof output === 'object') {
    for (const key of ['text', 'content', 'output']) {
      if (typeof output[key] === 'string') return output[key];
    }
  }
  return '';
}

function isErrorFromOutput(output) {
  const text = String(output ?? '');
  const firstLine = text.split(/\r?\n/, 1)[0] || '';
  if (/^exit code:\s*[1-9]/i.test(firstLine.trim())) return true;
  return ERROR_RE.test(text.slice(0, 200));
}

export async function parseCodexStore({ path, sessionId, lines } = {}) {
  const storeLines = lines || (await readTail(path, 0, { maxBytes: 512 * 1024 * 1024 })).lines;
  const records = [];
  const assistantSeen = new Set();
  let skipped = 0;
  for (const line of storeLines) {
    let record;
    try { record = JSON.parse(line.text); } catch { skipped += 1; continue; }
    if (!record || typeof record !== 'object') { skipped += 1; continue; }
    const recordSession = record.session_id ?? record.sessionId;
    if (recordSession && recordSession !== sessionId) continue;
    const type = record.type;
    const payload = record.payload || record;
    if (type === 'event_msg' || type === 'response_item' || type === 'user_message') {
      const innerType = payload?.type;
      const message = payload?.message;
      const role = message?.role;
      if (innerType === 'message' && role === 'user' || innerType === 'user_message'
        || (type === 'event_msg' && role === 'user' && !innerType)) {
        const text = extractText(message?.content ?? payload?.content);
        if (text.trim() && !isInjected(text)) {
          records.push({ cursor: line.offset, kind: 'user', session_id: sessionId, text: text.trim(), timestamp: record.timestamp ?? '' });
        }
        continue;
      }
      if (innerType === 'agent_message') {
        const text = extractText(payload?.content);
        const key = `agent:${text}`;
        if (!assistantSeen.has(key)) {
          assistantSeen.add(key);
          records.push({ cursor: line.offset, kind: 'assistant', session_id: sessionId, text, timestamp: record.timestamp ?? '' });
        }
        continue;
      }
      if (innerType === 'message' && role === 'assistant') {
        const text = extractText(message?.content ?? payload?.content);
        const key = `assistant:${text}`;
        if (!assistantSeen.has(key)) {
          assistantSeen.add(key);
          records.push({ cursor: line.offset, kind: 'assistant', session_id: sessionId, text, timestamp: record.timestamp ?? '' });
        }
        continue;
      }
      if (innerType === 'function_call') {
        let input = {};
        const raw = payload?.arguments;
        if (typeof raw === 'string') {
          try { input = JSON.parse(raw); } catch { input = { raw }; }
        } else if (raw && typeof raw === 'object') {
          input = raw;
        }
        records.push({
          cursor: line.offset, kind: 'tool_use', session_id: sessionId,
          tool_use_id: String(payload?.call_id ?? ''),
          name: String(payload?.name ?? ''),
          input,
          timestamp: record.timestamp ?? '',
        });
        continue;
      }
      if (innerType === 'function_call_output') {
        records.push({
          cursor: line.offset, kind: 'tool_result', session_id: sessionId,
          tool_use_id: String(payload?.call_id ?? ''),
          is_error: isErrorFromOutput(payload?.output),
          content: normaliseOutput(payload?.output),
          timestamp: record.timestamp ?? '',
        });
        continue;
      }
    }
  }
  return { records, skipped };
}
