import { fileURLToPath } from 'node:url';
import { basename, resolve } from 'node:path';

import { loadSkillInventory } from './inventory.mjs';
import { readRequiredVersion, readSkillContent } from '../integration/metadata.mjs';
import { runSkills } from './process.mjs';

const defaultPackageRoot = fileURLToPath(new URL('..', import.meta.url));

// Read-back of a large global inventory across many agent roots can outrun the
// default 120s command timeout on Windows (issue #50). The list is read-only,
// so a generous ceiling is safe; scoping by --agent is what actually keeps it fast.
export const SKILLS_LIST_TIMEOUT_MS = 600_000;

// loam harness id -> Skills CLI agent id (as accepted by `skills list --agent`).
const HARNESS_SKILL_AGENTS = {
  claude: 'claude-code',
  codex: 'codex',
  cursor: 'cursor',
  opencode: 'opencode',
};

// The agents to scope verification to. Loam installs with `--agent *`, so its
// skills are linked under every detected agent; listing just the harnesses Loam
// models confirms presence without scanning all 60+ agent roots. Falls back to
// a single known agent so verification never reverts to a full unfiltered scan.
export function skillsAgentsFor(harnesses = {}) {
  const agents = Object.entries(harnesses)
    .filter(([, harness]) => harness && harness.state !== 'absent')
    .map(([id]) => HARNESS_SKILL_AGENTS[id])
    .filter(Boolean);
  return agents.length ? [...new Set(agents)] : ['claude-code'];
}

function parseJsonOutput(stdout) {
  const text = String(stdout || '').trim();
  try {
    return JSON.parse(text);
  } catch {
    const start = Math.min(...[text.indexOf('{'), text.indexOf('[')].filter((value) => value >= 0));
    if (!Number.isFinite(start)) throw new Error('Skills CLI list output is not JSON');
    return JSON.parse(text.slice(start));
  }
}

export function normalizeSkillList(stdout) {
  const raw = typeof stdout === 'string' ? parseJsonOutput(stdout) : stdout;
  const entries = Array.isArray(raw) ? raw : raw?.skills || raw?.installed || raw?.data || [];
  if (!Array.isArray(entries)) throw new Error('Skills CLI list output has no skills array');
  return {
    raw,
    source: typeof raw?.source === 'string' ? raw.source : '',
    entries: entries.filter((entry) => entry && typeof entry === 'object'),
  };
}

export function skillEntryAliases(entry) {
  const values = [entry.name, entry.skill, entry.id, entry.slug, entry.directory, entry.path];
  if (typeof entry.path === 'string') values.push(basename(entry.path));
  return [...new Set(values.filter((value) => typeof value === 'string' && value))];
}

export function skillEntrySource(entry, listSource = '') {
  return [entry.source, entry.repository, entry.repo, entry.url, listSource]
    .find((value) => typeof value === 'string' && value) || '';
}

export async function listSkills({ global = false, cwd, runner, agents, timeoutMs = SKILLS_LIST_TIMEOUT_MS } = {}) {
  const args = ['list', '--json'];
  if (global) args.push('--global');
  if (agents?.length) args.push('--agent', ...agents);
  const result = await runSkills(args, { cwd, runner, timeoutMs });
  if (!result.ok) return { ...result, entries: [], source: '' };
  try {
    return { ...result, ...normalizeSkillList(result.stdout) };
  } catch (error) {
    return { ...result, ok: false, category: 'invalid_list', entries: [], source: '', stderr: error.message };
  }
}

export async function verifyGlobalSkills({
  packageRoot = defaultPackageRoot,
  skillsRoot,
  runner,
  cwd,
  agents,
} = {}) {
  const inventory = await loadSkillInventory({ packageRoot });
  let requiredVersion = '';
  let skillContent = '';
  let localError = '';
  try {
    requiredVersion = await readRequiredVersion({ skillsRoot });
    skillContent = await readSkillContent({ skillsRoot });
  } catch (error) {
    localError = error instanceof Error ? error.message : String(error);
  }
  const listed = await listSkills({ global: true, cwd, runner, agents });
  if (!listed.ok) {
    return { ready: false, category: listed.category || 'skills_list_failed', detail: listed.stderr || localError, inventory };
  }

  const aliases = new Set(listed.entries.flatMap(skillEntryAliases));
  const missing = inventory.skills.filter(
    (skill) => !skill.aliases.some((alias) => aliases.has(alias)),
  );
  const hasSource = listed.entries.some((entry) => skillEntrySource(entry, listed.source).includes('scchearn/loam'));
  if (missing.length || !hasSource || localError) {
    return {
      ready: false,
      category: missing.length || localError ? 'skills_missing' : 'skills_source_missing',
      missing: missing.map((skill) => skill.frontmatterName),
      source: hasSource,
      requiredVersion,
      detail: localError,
      inventory,
    };
  }
  return { ready: true, changed: false, requiredVersion, inventory, listed };
}

export async function ensureGlobalSkills(options = {}) {
  const current = await verifyGlobalSkills(options);
  if (current.ready && !options.refresh) return current;

  const added = await runSkills(
    ['add', 'scchearn/loam', '--global', '--agent', '*', '--yes'],
    { cwd: options.cwd, runner: options.runner },
  );
  if (!added.ok) {
    return { ...current, ready: false, changed: false, category: 'skills_install_failed', detail: added.stderr };
  }
  const verified = await verifyGlobalSkills(options);
  return { ...verified, changed: true };
}
