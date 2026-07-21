/**
 * loam plugin for OpenCode.ai
 *
 * Injects loam::using bootstrap context via first-user-message transform.
 * Skill content is read from the npx skills install path (single source of
 * truth). No config hook — OpenCode discovers ~/.agents/skills/ natively.
 *
 * At session start:
 * - Checks if the local git clone is behind origin/main (update notice)
 * - Runs loamstate.sh and injects a workspace state summary (wiki, checkpoints,
 *   drift, signals)
 *
 * Both checks fail silently — core injection works even if bash/git/loamstate
 * are absent (e.g. native Windows without Git Bash).
 *
 * Uses <LOAM_IMPORTANT> wrapper and "You have loam" dedup marker to avoid
 * collision with superpowers.
 */

import path from 'path';
import fs from 'fs';
import os from 'os';
import { execSync } from 'child_process';

// Simple frontmatter extraction (avoid dependency on skills-core for bootstrap)
const extractAndStripFrontmatter = (content) => {
  const match = content.match(/^---\n([\s\S]*?)\n---\n([\s\S]*)$/);
  if (!match) return { frontmatter: {}, content };

  const frontmatterStr = match[1];
  const body = match[2];
  const frontmatter = {};

  for (const line of frontmatterStr.split('\n')) {
    const colonIdx = line.indexOf(':');
    if (colonIdx > 0) {
      const key = line.slice(0, colonIdx).trim();
      const value = line.slice(colonIdx + 1).trim().replace(/^["']|["']$/g, '');
      frontmatter[key] = value;
    }
  }

  return { frontmatter, content: body };
};

// Find loam::using SKILL.md: project-scoped first, then global.
// Returns absolute path or null.
const findSkillPath = () => {
  const candidates = [
    path.join(process.cwd(), '.agents/skills/loam-using/SKILL.md'),
    path.join(os.homedir(), '.agents/skills/loam-using/SKILL.md'),
  ];
  for (const p of candidates) {
    if (fs.existsSync(p)) return p;
  }
  return null;
};

// Find the platform-appropriate loamstate wrapper in the npx skills install
// path: .ps1 on Windows, .sh elsewhere. Project scope wins over global.
// Returns absolute path or null.
const findLoamstatePath = () => {
  const wrapper =
    process.platform === 'win32' ? 'loamstate.ps1' : 'loamstate.sh';
  const candidates = [
    path.join(process.cwd(), '.agents/skills/loam-using/scripts', wrapper),
    path.join(os.homedir(), '.agents/skills/loam-using/scripts', wrapper),
  ];
  for (const p of candidates) {
    if (fs.existsSync(p)) return p;
  }
  return null;
};

// Check if the local git clone is behind origin/main.
// Returns update notice string or empty string.
// Fails silently on any error (offline, not a git repo, timeout).
const checkForUpdate = (pluginRoot) => {
  try {
    const localHead = execSync('git rev-parse HEAD', {
      cwd: pluginRoot,
      timeout: 5000,
      encoding: 'utf8',
    }).trim();
    const remoteHead = execSync('git ls-remote -h origin main', {
      cwd: pluginRoot,
      timeout: 5000,
      encoding: 'utf8',
    }).trim().split('\t')[0];
    if (localHead !== remoteHead) {
      return ` **Update available — run: cd ${pluginRoot} && git pull**`;
    }
  } catch {}
  return '';
};

// Run the platform state wrapper and format a compact workspace state block.
// Returns formatted string or empty string on any failure.
// On Windows the wrapper runs under in-box Windows PowerShell 5.1, so state is
// no longer skipped just because bash is absent.
const getWorkspaceState = () => {
  const loamstatePath = findLoamstatePath();
  if (!loamstatePath) return '';

  const isWindows = process.platform === 'win32';
  if (!isWindows) {
    try {
      execSync('bash --version', { timeout: 2000, encoding: 'utf8', stdio: 'pipe' });
    } catch {
      return '';
    }
  }

  const command = isWindows
    ? `powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "${loamstatePath}" --fast "${process.cwd()}"`
    : `bash "${loamstatePath}" --fast "${process.cwd()}"`;

  let stdout;
  try {
    stdout = execSync(command, {
      timeout: 5000,
      encoding: 'utf8',
      stdio: ['pipe', 'pipe', 'pipe'],
    });
  } catch {
    return '';
  }

  let state;
  try {
    state = JSON.parse(stdout);
  } catch {
    return '';
  }

  const lines = [`Workspace: ${process.cwd()} · Probe: loamstate --fast`];

  // Wiki line
  if (state.exists && state.wiki_root) {
    const parts = [`Wiki: ${state.wiki_root}`];
    parts.push(state.qmd_ready ? 'qmd: ready' : 'qmd: not installed');
    if (state.qmd_ready && state.collection) parts.push(`collection: ${state.collection}`);
    lines.push(parts.join(' · '));
  } else {
    lines.push('Wiki: none');
  }

  // Checkpoints line
  if (state.checkpoint_count > 0 && state.latest_checkpoint) {
    const cp = state.latest_checkpoint;
    lines.push(`Checkpoints: ${state.checkpoint_count} (latest: "${cp.title}" — ${cp.captured_at})`);
  }

  // Drift line
  if (state.drift_count != null && state.drift_count > 0) {
    lines.push(`Code graph drift: ${state.drift_count}`);
  }

  // Signals (hints)
  if (state.hints && state.hints.length > 0) {
    lines.push('');
    lines.push('Signals:');
    for (const h of state.hints) {
      const evidence = Object.entries(h.evidence || {})
        .map(([key, value]) => `${key}: ${typeof value === 'string' ? value : JSON.stringify(value)}`)
        .join(', ');
      const evidencePart = evidence ? ` (${evidence})` : '';
      const cmd = h.command ? ` → ${h.command}` : '';
      lines.push(`- [loam:hint] ${h.kind} — ${h.message}${evidencePart}${cmd}`);
    }
  }

  return `\n## Workspace state\n\n${lines.join('\n')}\n`;
};

export const LoamPlugin = async ({ client, directory }) => {
  // Plugin root = clone root (where .git lives), two levels up from loam.js
  const pluginRoot = path.resolve(import.meta.dirname, '../..');

  // Helper to generate bootstrap content
  const getBootstrapContent = () => {
    const skillPath = findSkillPath();
    if (!skillPath) {
      console.error('[loam] loam::using not found — run: npx skills add scchearn/loam');
      return null;
    }

    const fullContent = fs.readFileSync(skillPath, 'utf8');
    const { content } = extractAndStripFrontmatter(fullContent);

    // Read plugin version from package.json (the installed plugin version)
    let version = '';
    try {
      const pkgPath = path.join(pluginRoot, 'package.json');
      const pkg = JSON.parse(fs.readFileSync(pkgPath, 'utf8'));
      version = pkg.version || '';
    } catch {}

    // Check if the local clone is behind origin/main (OpenCode-only update check)
    const updateNotice = checkForUpdate(pluginRoot);

    // Get workspace state from loamstate.sh (skips silently if bash/loamstate absent)
    const workspaceState = getWorkspaceState();

    const toolMapping = `**Tool Mapping for OpenCode:**
When skills reference tools you don't have, substitute OpenCode equivalents:
- \`TodoWrite\` → \`todowrite\`
- \`Task\` tool with subagents → Use OpenCode's subagent system (@mention)
- \`Skill\` tool → OpenCode's native \`skill\` tool
- \`Read\`, \`Write\`, \`Edit\`, \`Bash\` → Your native tools

Use OpenCode's native \`skill\` tool to list and load skills.`;

    return `<LOAM_IMPORTANT>
You have loam${version ? ` (v${version})` : ''}.${updateNotice}

**IMPORTANT: The loam::using skill content is included below. It is ALREADY LOADED - you are currently following it. Do NOT use the skill tool to load 'loam::using' again - that would be redundant.**

${content}
${workspaceState}
${toolMapping}
</LOAM_IMPORTANT>`;
  };

  return {
    // Inject bootstrap into the first user message of each session.
    // Using a user message instead of a system message avoids:
    //   1. Token bloat from system messages repeated every turn
    //   2. Multiple system messages breaking Qwen and other models
    'experimental.chat.messages.transform': async (_input, output) => {
      if (!output.messages.length) return;
      const firstUser = output.messages.find(m => m.info.role === 'user');
      if (!firstUser || !firstUser.parts.length) return;
      // Only inject once — dedup on loam's own marker, not superpowers'
      if (firstUser.parts.some(p => p.type === 'text' && p.text.includes('You have loam'))) return;
      const bootstrap = getBootstrapContent();
      if (!bootstrap) return;
      const ref = firstUser.parts[0];
      firstUser.parts.unshift({ ...ref, type: 'text', text: bootstrap });
    }
  };
};
