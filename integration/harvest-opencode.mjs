import { stat, realpath } from 'node:fs/promises';
import { homedir } from 'node:os';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

import { HarvestError } from './harvest-store.mjs';
import { registerHarvestBackend } from './harvest-store.mjs';

registerHarvestBackend('opencode', {
  locateStore: ({ env }) => locateOpenCodeStore({ env }),
  measure: ({ store }) => openCodeMeasure({ store }),
  readWindow: ({ store, state, workspace, config }) => readOpenCodeWindow({
    store: store.path, sessionId: state.session_id, workspace, rowid: state.cursor?.value || 0,
  }),
});

const SYNTHETIC_EXCLUDED = true;

export async function locateOpenCodeStore({ env = process.env } = {}) {
  const dataHome = env.XDG_DATA_HOME || join(homedir(), '.local', 'share');
  const candidates = [
    join(resolve(dataHome), 'opencode', 'opencode.db'),
    join(homedir(), '.local', 'share', 'opencode', 'opencode.db'),
  ];
  for (const candidate of [...new Set(candidates)]) {
    try {
      const info = await stat(candidate);
      if (info.isFile()) return { path: candidate, kind: 'sqlite' };
    } catch {}
  }
  throw new HarvestError('store_missing', 'no OpenCode database found');
}

export async function openCodeMeasure({ store, env = process.env }) {
  const path = store?.path;
  if (!path) return { present: false, size: 0, mtime_ms: 0 };
  let total = 0;
  let mtime = 0;
  for (const candidate of [path, `${path}-wal`]) {
    try {
      const info = await stat(candidate);
      total += info.size;
      mtime = Math.max(mtime, info.mtimeMs);
    } catch {}
  }
  return { present: total > 0, size: total, mtime_ms: mtime };
}

async function openSqlite(sqliteLoader, storePath) {
  let sqlite;
  try {
    sqlite = sqliteLoader ? await sqliteLoader() : await import('node:sqlite');
  } catch {
    throw new HarvestError('sqlite_unavailable', 'node:sqlite is unavailable on this Node runtime');
  }
  try {
    return new sqlite.DatabaseSync(storePath, { readOnly: true });
  } catch (error) {
    throw new HarvestError('store_unreadable', `cannot open sqlite database: ${String(error?.message || error)}`);
  }
}

function partData(part) {
  try { return typeof part?.data === 'string' ? JSON.parse(part.data) : part?.data; } catch { return null; }
}

export async function readOpenCodeWindow({ store, sessionId, workspace, rowid = 0, sqliteLoader }) {
  const dbPath = typeof store === 'string' ? store : store?.path;
  const db = await openSqlite(sqliteLoader, dbPath);
  try {
    const directory = db.prepare('SELECT directory FROM session WHERE id = ?').get(sessionId);
    if (!directory) throw new HarvestError('session_unknown', `no OpenCode session ${sessionId}`);
    let sessionDir = directory.directory;
    try { sessionDir = await realpath(sessionDir); } catch { sessionDir = resolve(sessionDir); }
    let workspaceResolved = workspace;
    try { workspaceResolved = await realpath(workspace); } catch { workspaceResolved = resolve(workspace); }
    if (sessionDir !== workspaceResolved) {
      throw new HarvestError('foreign_workspace', `session ${sessionId} belongs to ${sessionDir}, not ${workspaceResolved}`);
    }

    const messages = db.prepare(
      'SELECT id, session_id, data, rowid FROM message WHERE session_id = ? AND rowid > ? ORDER BY rowid',
    ).all(sessionId, rowid);
    const records = [];
    if (messages.length) {
      const messageIds = messages.map((message) => message.id);
      const placeholders = messageIds.map(() => '?').join(',');
      const parts = db.prepare(
        `SELECT data, message_id FROM part WHERE session_id = ? AND message_id IN (${placeholders}) ORDER BY rowid`,
      ).all(sessionId, ...messageIds);
      const partsByMessage = new Map();
      for (const part of parts) {
        const list = partsByMessage.get(part.message_id) || [];
        list.push(part);
        partsByMessage.set(part.message_id, list);
      }
      for (const message of messages) {
        const data = partData(message);
        if (!data || typeof data !== 'object') continue;
        const role = data.role;
        if (role !== 'user' && role !== 'assistant') continue;
        const parsedParts = (partsByMessage.get(message.id) || []).map(partData).filter(Boolean);
        if (role === 'user') {
          const texts = parsedParts
            .filter((part) => part.type === 'text' && part.synthetic !== true && typeof part.text === 'string' && part.text.trim())
            .map((part) => part.text);
          if (texts.length) {
            records.push({
              cursor: message.rowid, kind: 'user', session_id: sessionId,
              text: texts.join('\n'), timestamp: String(message.time_created ?? ''),
            });
          }
          continue;
        }
        for (const part of parsedParts) {
          if (part.type === 'text' && part.synthetic !== true && typeof part.text === 'string' && part.text.trim()) {
            records.push({
              cursor: message.rowid, kind: 'assistant', session_id: sessionId,
              text: part.text, timestamp: String(message.time_created ?? ''),
            });
          } else if (part.type === 'tool') {
            const state = part.state || {};
            const input = state.input || {};
            const metadata = state.metadata || {};
            let file = null;
            if (typeof input.filePath === 'string') file = input.filePath;
            else if (Array.isArray(metadata.files) && metadata.files[0]?.filePath) file = metadata.files[0].filePath;
            records.push({
              cursor: message.rowid, kind: 'tool_use', session_id: sessionId,
              tool_use_id: String(part.callID ?? ''),
              name: String(part.tool ?? ''),
              input,
              is_error: state.status === 'error',
              output: typeof state.output === 'string' ? state.output : state.error ?? '',
              file,
            });
          }
        }
      }
    }
    return { records, nextRowid: messages.length ? messages[messages.length - 1].rowid : rowid };
  } finally {
    try { db.close(); } catch {}
  }
}
