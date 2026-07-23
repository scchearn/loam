const CONTROL_CHARS = /[\u0000-\u0008\u000B\u000C\u000E-\u001F\u007F]/g;

export function cleanText(value, limit = 4096) {
  const cleaned = String(value ?? '').replace(CONTROL_CHARS, '�');
  return cleaned.length > limit ? `${cleaned.slice(0, limit - 1)}…` : cleaned;
}

export function stripFrontmatter(content) {
  return String(content ?? '').replace(/^---\r?\n[\s\S]*?\r?\n---(?:\r?\n|$)/, '').trim();
}

export function formatNativeRuntimeCommand(runtimePath, platform = process.platform) {
  const value = cleanText(runtimePath, 4096);
  if (!value) return '';
  if (platform === 'win32') return `& '${value.replaceAll("'", "''")}'`;
  return `'${value.replaceAll("'", "'\\''")}'`;
}

function formatWorkspaceState(state, workspace) {
  const lines = [`Workspace: ${cleanText(workspace)}`];
  if (state.exists && state.wiki_root) {
    const wiki = [`Wiki: ${cleanText(state.wiki_root)}`];
    wiki.push(state.qmd_ready ? 'qmd: ready' : 'qmd: not installed');
    if (state.qmd_ready && state.collection) wiki.push(`collection: ${cleanText(state.collection)}`);
    lines.push(wiki.join(' · '));
  } else {
    lines.push('Wiki: none');
  }
  if (state.checkpoint_count > 0 && state.latest_checkpoint) {
    lines.push(
      `Checkpoints: ${state.checkpoint_count} (latest: "${cleanText(state.latest_checkpoint.title)}" — ${cleanText(state.latest_checkpoint.captured_at)})`,
    );
  }
  if (state.drift_count != null && state.drift_count > 0) lines.push(`Code graph drift: ${state.drift_count}`);
  if (Array.isArray(state.hints) && state.hints.length > 0) {
    lines.push('', 'Signals:');
    for (const hint of state.hints) {
      const evidence = Object.entries(hint.evidence || {})
        .map(([key, value]) => `${key}: ${cleanText(typeof value === 'string' ? value : JSON.stringify(value))}`)
        .join(', ');
      const evidencePart = evidence ? ` (${evidence})` : '';
      const command = hint.command ? ` → ${cleanText(hint.command)}` : '';
      lines.push(`- [loam:hint] ${cleanText(hint.kind)} — ${cleanText(hint.message)}${evidencePart}${command}`);
    }
  }
  return `## Workspace state\n\n${lines.join('\n')}`;
}

export function formatContext({
  skillContent = '',
  pluginVersion = '',
  runtimePath,
  platform = process.platform,
  workspace = '',
  state,
  unavailable,
} = {}) {
  const version = pluginVersion ? ` (v${cleanText(pluginVersion, 128)})` : '';
  const body = stripFrontmatter(skillContent);
  const command = runtimePath ? `Native runtime command: ${formatNativeRuntimeCommand(runtimePath, platform)}` : '';
  const content = [
    `You have loam${version}.`,
    body,
    command,
    state ? formatWorkspaceState(state, workspace) : '',
    unavailable
      ? `Loam is unavailable: ${cleanText(unavailable.message || unavailable.detail || unavailable.category)}\nNo workspace state was generated.\nRecovery: npx @scchearn/loam setup`
      : '',
  ]
    .filter(Boolean)
    .join('\n\n');
  return `<LOAM_IMPORTANT>\n\n${content}\n\n</LOAM_IMPORTANT>`;
}
