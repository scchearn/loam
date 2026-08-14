import { readFile, mkdir, rm } from 'node:fs/promises';
import { dirname, join } from 'node:path';

import { writeAtomicFile } from '../atomic.mjs';
import { mergeJsonConfig } from '../config.mjs';
import { hasMcpServerTable, removeMcpServerTable, upsertMcpServerTable } from './toml.mjs';

// Per-harness MCP registration, verified against real config from each harness
// (schemas confirmed against public source, 2026-08):
//   claude  ~/.claude.json              mcpServers.<name>  remote {type:http,url} / local {type:stdio,command,args}
//   cursor  ~/.cursor/mcp.json          mcpServers.<name>  (same shape as claude)
//   opencode ~/.config/opencode/opencode.json  mcp.<name>  remote {type:remote,url,enabled} / local {type:local,command:[abs,...],enabled}
//   codex   ~/.codex/config.toml        [mcp_servers.<name>]  remote url= / local command=+args=
//
// A descriptor is transport-neutral: { transport:'remote', url } or
// { transport:'local', command:'<abs>', args:[...] }. Each harness renders it to
// its own shape. Node/npx is guaranteed but PATH is not, so `command` is always
// an absolute path (the catalog resolves it), never a bare binary name.

export const MCP_HARNESSES = ['claude', 'codex', 'opencode', 'cursor'];

const JSON_TARGETS = {
  claude: { path: (home) => join(home, '.claude.json'), key: 'mcpServers' },
  cursor: { path: (home) => join(home, '.cursor', 'mcp.json'), key: 'mcpServers' },
  opencode: { path: (home) => join(home, '.config', 'opencode', 'opencode.json'), key: 'mcp' },
};

export function mcpTarget(harness, home) {
  if (harness === 'codex') return { format: 'toml', path: join(home, '.codex', 'config.toml'), key: 'mcp_servers' };
  const target = JSON_TARGETS[harness];
  if (!target) throw new Error(`unknown MCP harness: ${harness}`);
  return { format: 'json', path: target.path(home), key: target.key };
}

// The harness-specific entry object for a JSON harness.
export function renderJsonEntry(harness, descriptor) {
  if (harness === 'opencode') {
    return descriptor.transport === 'remote'
      ? { type: 'remote', url: descriptor.url, enabled: true }
      : { type: 'local', command: [descriptor.command, ...(descriptor.args || [])], enabled: true };
  }
  // claude + cursor share the mcpServers shape.
  return descriptor.transport === 'remote'
    ? { type: 'http', url: descriptor.url }
    : { type: 'stdio', command: descriptor.command, args: descriptor.args || [] };
}

async function readJson(path) {
  try {
    return { existed: true, value: JSON.parse(await readFile(path, 'utf8')) };
  } catch (error) {
    if (error?.code === 'ENOENT') return { existed: false, value: {} };
    throw error;
  }
}

async function readText(path) {
  try {
    return { existed: true, value: await readFile(path, 'utf8') };
  } catch (error) {
    if (error?.code === 'ENOENT') return { existed: false, value: '' };
    throw error;
  }
}

// Is an MCP entry named `name` already present in this harness config? Used for
// idempotency and ownership: a present entry that loam did not record is
// user-owned and must be left untouched.
export async function detectMcpEntry({ harness, home, name }) {
  const target = mcpTarget(harness, home);
  if (target.format === 'toml') {
    const { existed, value } = await readText(target.path);
    return { present: existed && hasMcpServerTable(value, name), path: target.path };
  }
  const { value } = await readJson(target.path);
  const bucket = value?.[target.key];
  return { present: Boolean(bucket && Object.prototype.hasOwnProperty.call(bucket, name)), path: target.path, entry: bucket?.[name] };
}

// Register the MCP entry for a harness. Idempotent and non-clobbering: caller is
// responsible for the ownership check (skip when a user-owned entry exists); this
// writes loam's entry. Returns { registered, backupPath?, path }.
export async function registerMcpEntry({ harness, home, name, descriptor }) {
  const target = mcpTarget(harness, home);
  if (target.format === 'toml') {
    const { value } = await readText(target.path);
    await mkdir(dirname(target.path), { recursive: true });
    await writeAtomicFile(target.path, upsertMcpServerTable(value, name, descriptor));
    return { registered: true, path: target.path };
  }
  await mkdir(dirname(target.path), { recursive: true });
  const entry = renderJsonEntry(harness, descriptor);
  const result = await mergeJsonConfig({
    filePath: target.path,
    update: (current) => ({
      ...current,
      [target.key]: { ...(current[target.key] || {}), [name]: entry },
    }),
  });
  return { registered: true, backupPath: result.backupPath, path: target.path };
}

// Remove loam's MCP entry from a harness config. Preserves all unrelated config.
// Returns { removed } — false when there was nothing to remove.
export async function deregisterMcpEntry({ harness, home, name }) {
  const target = mcpTarget(harness, home);
  if (target.format === 'toml') {
    const { existed, value } = await readText(target.path);
    if (!existed || !hasMcpServerTable(value, name)) return { removed: false, path: target.path };
    const next = removeMcpServerTable(value, name);
    if (next === '') await rm(target.path, { force: true });
    else await writeAtomicFile(target.path, next);
    return { removed: true, path: target.path };
  }
  const { existed, value } = await readJson(target.path);
  if (!existed) return { removed: false, path: target.path };
  const bucket = value?.[target.key];
  if (!bucket || !Object.prototype.hasOwnProperty.call(bucket, name)) return { removed: false, path: target.path };
  await mergeJsonConfig({
    filePath: target.path,
    update: (current) => {
      const next = { ...current, [target.key]: { ...(current[target.key] || {}) } };
      delete next[target.key][name];
      return next;
    },
  });
  return { removed: true, path: target.path };
}
