/**
 * loam plugin for OpenCode.ai
 *
 * Injects loam::using bootstrap context via first-user-message transform.
 * Skill content is read from the npx skills install path (single source of
 * truth). No config hook — OpenCode discovers ~/.agents/skills/ natively.
 *
 * Uses <LOAM_IMPORTANT> wrapper and "You have loam" dedup marker to avoid
 * collision with superpowers.
 */

import path from 'path';
import fs from 'fs';
import os from 'os';

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

export const LoamPlugin = async ({ client, directory }) => {
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
      const pkgPath = path.resolve(__dirname, '../../package.json');
      const pkg = JSON.parse(fs.readFileSync(pkgPath, 'utf8'));
      version = pkg.version || '';
    } catch {}

    const toolMapping = `**Tool Mapping for OpenCode:**
When skills reference tools you don't have, substitute OpenCode equivalents:
- \`TodoWrite\` → \`todowrite\`
- \`Task\` tool with subagents → Use OpenCode's subagent system (@mention)
- \`Skill\` tool → OpenCode's native \`skill\` tool
- \`Read\`, \`Write\`, \`Edit\`, \`Bash\` → Your native tools

Use OpenCode's native \`skill\` tool to list and load skills.`;

    return `<LOAM_IMPORTANT>
You have loam${version ? ` (v${version})` : ''}.

**IMPORTANT: The loam::using skill content is included below. It is ALREADY LOADED - you are currently following it. Do NOT use the skill tool to load 'loam::using' again - that would be redundant.**

${content}

${toolMapping}
</LOAM_IMPORTANT>`;
  };

  return {
    // Inject bootstrap into the first user message of each session.
    // Using a user message instead of a system message avoids:
    //   1. Token bloat from system messages repeated every turn
    //   2. Multiple system messages breaking Qwen and other models
    'experimental.chat.messages.transform': async (_input, output) => {
      const bootstrap = getBootstrapContent();
      if (!bootstrap || !output.messages.length) return;
      const firstUser = output.messages.find(m => m.info.role === 'user');
      if (!firstUser || !firstUser.parts.length) return;
      // Only inject once — dedup on loam's own marker, not superpowers'
      if (firstUser.parts.some(p => p.type === 'text' && p.text.includes('You have loam'))) return;
      const ref = firstUser.parts[0];
      firstUser.parts.unshift({ ...ref, type: 'text', text: bootstrap });
    }
  };
};