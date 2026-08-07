import { basename } from 'node:path';

const TOOL_NAME_ALIASES = Object.freeze({
  run_shell_command: 'Bash', shell: 'Bash', bash: 'Bash',
  read_file: 'Read', read: 'Read',
  edit_file: 'Edit', apply_patch: 'Edit', replace: 'Edit',
});

export function normalizeToolName(name) {
  return TOOL_NAME_ALIASES[name] || String(name ?? 'unknown');
}

function truncatedOutput(value, maxBytes) {
  const text = String(value ?? '');
  if (Buffer.byteLength(text, 'utf8') <= maxBytes) return text;
  let slice = text.slice(0, maxBytes);
  while (Buffer.byteLength(slice, 'utf8') > maxBytes) slice = slice.slice(0, -1);
  return slice;
}

function pushFile(files, path, cap = 5) {
  const base = basename(String(path ?? ''));
  if (!base || files.includes(base)) return files;
  const next = [...files, base];
  return next.length > cap ? next.slice(0, cap) : next;
}

export function setToolError(tools, { at }) {
  for (const tool of tools) {
    if (tool.is_error === undefined || tool.is_error === null) tool.is_error = false;
    if (at === 'call_time') tool.error_fidelity = 'call_time';
  }
}

export function groupExchanges(records, { maxTurns, maxBytes, toolOutputBytes = 1000 }) {
  const exchanges = [];
  let current = null;
  let boundaryCursor = null;
  let bytes = 0;
  let truncated = false;
  let lastCursor = null;

  const flush = () => {
    if (!current) return;
    const fidelity = current.tools.some((tool) => tool.error_fidelity === 'call_time' && tool.output === null)
      ? 'call_time' : 'result_time';
    setToolError(current.tools, { at: fidelity });
    delete current.tool_use_ids;
    exchanges.push(current);
    current = null;
  };

  const openTurn = (record) => {
    if (current) flush();
    current = {
      position: record.cursor,
      user: String(record.text ?? ''),
      action: '',
      files: [],
      timestamp: record.timestamp ?? '',
      tools: [],
      edits: [],
      errors: [],
      ended_on_error: false,
      assistant: [],
    };
    bytes = Buffer.byteLength(current.user, 'utf8');
  };

  for (const record of records) {
    lastCursor = record.cursor;
    if (record.kind === 'user') {
      openTurn(record);
      continue;
    }
    if (!current) continue;
    if (record.kind === 'assistant') {
      if (record.text) {
        current.assistant.push(String(record.text));
        bytes += Buffer.byteLength(String(record.text), 'utf8');
      }
      continue;
    }
    if (record.kind === 'tool_use') {
      const input = record.input || {};
      const file = typeof input.file_path === 'string' ? input.file_path
        : typeof input.file === 'string' ? input.file : null;
      current.tools.push({
        name: normalizeToolName(record.name),
        is_error: false,
        file,
        command: typeof input.command === 'string' ? input.command : null,
        output: null,
        tool_use_id: record.tool_use_id,
        error_fidelity: 'call_time',
      });
      current.tool_use_ids ||= new Set();
      current.tool_use_ids.add(record.tool_use_id);
      if (file) current.files = pushFile(current.files, file);
      bytes += 128;
      continue;
    }
    if (record.kind === 'tool_result') {
      const tool = current.tools.find((entry) => entry.tool_use_id === record.tool_use_id);
      if (!tool) continue;
      tool.is_error = record.is_error === true;
      tool.output = truncatedOutput(record.content, toolOutputBytes);
      delete tool.error_fidelity;
      if (record.is_error === true) current.ended_on_error = true;
      continue;
    }
  }

  if (current && bytes > maxBytes) {
    truncated = true;
    boundaryCursor = lastCursor;
    flush();
  } else if (current) {
    boundaryCursor = current.position;
  } else if (exchanges.length) {
    boundaryCursor = exchanges[exchanges.length - 1].position;
  } else {
    boundaryCursor = 0;
  }

  return {
    exchanges: exchanges.slice(0, maxTurns),
    boundaryCursor,
    truncated,
  };
}

export function renderWindow({ harness, sessionId, workspace, exchanges, windowStart, windowEnd }) {
  const lines = [];
  lines.push('---');
  lines.push(`harness: ${harness}`);
  lines.push(`session_id: ${sessionId}`);
  lines.push(`workspace: ${workspace}`);
  lines.push(`turns: ${exchanges.length}`);
  lines.push(`window_start: "${windowStart}"`);
  lines.push(`window_end: "${windowEnd}"`);
  lines.push('---');
  exchanges.forEach((exchange, index) => {
    lines.push('');
    lines.push(`## Turn ${index + 1}`);
    lines.push('');
    lines.push('### User');
    lines.push(exchange.user ?? '');
    for (const text of exchange.assistant || []) {
      lines.push('');
      lines.push('### Assistant');
      lines.push(text);
    }
    for (const tool of exchange.tools || []) {
      const parts = [`tool: ${tool.name}`, `ok: ${tool.is_error !== true}`];
      if (tool.file) parts.push(`file: ${tool.file}`);
      if (tool.command) parts.push(`cmd: ${tool.command}`);
      lines.push(`- ${parts.join(' | ')}`);
      if (tool.output !== null && tool.output !== undefined) lines.push(`  output: ${tool.output}`);
    }
  });
  return `${lines.join('\n')}\n`;
}

export async function writeWindow(path, rendered, atomic) {
  await atomic(path, rendered);
}
