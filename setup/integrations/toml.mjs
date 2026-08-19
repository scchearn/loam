// Minimal TOML-safe editor for Codex's `~/.codex/config.toml`, scoped to
// `[mcp_servers.<name>]` tables. The JSON merge helper cannot be used for TOML,
// and loam already hand-parses this file; this adds the smallest safe writer for
// MCP registration that preserves every unrelated table, key, and comment.
//
// Only two operations are needed — upsert one server table, remove one server
// table — so this is a line-region editor, not a full TOML parser. It never
// touches bytes outside the target `[mcp_servers.<name>]` region.

// A TOML string for a value. Prefer a literal (single-quote) string so Windows
// paths with backslashes need no escaping; fall back to a basic string only when
// the value itself contains a single quote or newline.
export function tomlString(value) {
  const text = String(value);
  if (!text.includes("'") && !text.includes('\n')) return `'${text}'`;
  return `"${text.replace(/\\/g, '\\\\').replace(/"/g, '\\"').replace(/\n/g, '\\n')}"`;
}

function tomlArray(values) {
  return `[${values.map((value) => tomlString(value)).join(', ')}]`;
}

// True when a line opens a table that belongs to the named server: the table
// itself `[mcp_servers.<name>]` or any sub-table `[mcp_servers.<name>.xxx]`.
// Accepts a bare or quoted name segment.
function opensServerTable(line, name) {
  const m = line.match(/^\s*\[\s*mcp_servers\s*\.\s*("?)([^".\]]+)\1\s*(\].*|\..*)$/);
  if (!m) return false;
  return m[2] === name;
}

// True when a line opens ANY table header `[...]` (top-level or dotted).
function opensAnyTable(line) {
  return /^\s*\[/.test(line);
}

export function hasMcpServerTable(text, name) {
  return text.split('\n').some((line) => opensServerTable(line, name));
}

// Remove the `[mcp_servers.<name>]` table and every one of its sub-tables,
// including the body lines under each, up to (but not including) the next
// unrelated table header. Leaves all other content byte-identical.
export function removeMcpServerTable(text, name) {
  const lines = text.split('\n');
  const kept = [];
  let dropping = false;
  for (const line of lines) {
    if (opensAnyTable(line)) {
      dropping = opensServerTable(line, name);
      if (dropping) continue;
    }
    if (dropping) continue;
    kept.push(line);
  }
  // Collapse a run of blank lines left behind to at most one, and trim trailing
  // blanks so repeated add/remove cycles do not accumulate whitespace.
  const collapsed = [];
  for (const line of kept) {
    if (line.trim() === '' && collapsed.length && collapsed[collapsed.length - 1].trim() === '') continue;
    collapsed.push(line);
  }
  while (collapsed.length && collapsed[collapsed.length - 1].trim() === '') collapsed.pop();
  return collapsed.length ? `${collapsed.join('\n')}\n` : '';
}

// Render a `[mcp_servers.<name>]` table body from a descriptor.
// Remote:  url = '<url>'
// Local:   command = '<abs>'   args = ['mcp', ...]
function renderServerTable(name, descriptor) {
  const rows = [`[mcp_servers.${name}]`];
  if (descriptor.transport === 'remote') {
    rows.push(`url = ${tomlString(descriptor.url)}`);
  } else {
    rows.push(`command = ${tomlString(descriptor.command)}`);
    rows.push(`args = ${tomlArray(descriptor.args || [])}`);
  }
  return rows.join('\n');
}

// Insert or replace the named server table. Replacing removes the old region
// first (preserving everything else), then appends the fresh table. Idempotent.
export function upsertMcpServerTable(text, name, descriptor) {
  const base = hasMcpServerTable(text, name) ? removeMcpServerTable(text, name) : text;
  const table = renderServerTable(name, descriptor);
  if (!base || base.trim() === '') return `${table}\n`;
  const separator = base.endsWith('\n') ? '\n' : '\n\n';
  return `${base}${separator}${table}\n`;
}

// Self-check: run `node setup/integrations/toml.mjs`.
if (import.meta.url === `file://${process.argv[1]}`) {
  const assert = (await import('node:assert/strict')).default;
  const original = '# header comment\n[other]\nkeep = true\n\n[mcp_servers.existing]\nurl = \'https://x\'\n';
  const added = upsertMcpServerTable(original, 'grep', { transport: 'remote', url: 'https://mcp.grep.app' });
  assert.ok(added.includes('[mcp_servers.grep]'));
  assert.ok(added.includes("url = 'https://mcp.grep.app'"));
  assert.ok(added.includes('# header comment') && added.includes('keep = true'), 'unrelated content preserved');
  assert.ok(added.includes('[mcp_servers.existing]'), 'other server table preserved');

  const local = upsertMcpServerTable(original, 'qmd', { transport: 'local', command: '/opt/loam/qmd', args: ['mcp'] });
  assert.ok(local.includes("command = '/opt/loam/qmd'") && local.includes("args = ['mcp']"));

  const removed = removeMcpServerTable(added, 'grep');
  assert.ok(!removed.includes('[mcp_servers.grep]'));
  assert.ok(removed.includes('[mcp_servers.existing]') && removed.includes('keep = true'), 'removal is surgical');

  // Windows path uses a literal string (no backslash escaping needed).
  const win = upsertMcpServerTable('', 'qmd', { transport: 'local', command: 'C:\\loam\\qmd.cmd', args: ['mcp'] });
  assert.ok(win.includes("command = 'C:\\loam\\qmd.cmd'"), 'windows path is a literal TOML string');

  // Idempotent upsert does not duplicate the table.
  const twice = upsertMcpServerTable(added, 'grep', { transport: 'remote', url: 'https://mcp.grep.app' });
  assert.equal((twice.match(/\[mcp_servers\.grep\]/g) || []).length, 1, 'upsert is idempotent');
  console.log('ok: toml editor self-check passed');
}
