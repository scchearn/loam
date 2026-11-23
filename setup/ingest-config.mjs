import { readFile } from 'node:fs/promises';
import { homedir } from 'node:os';
import { isAbsolute, join, parse, relative, resolve } from 'node:path';

import { canonicalWorkspace, ingestStatus } from '../integration/ingest.mjs';
import { configRoot, publishJson } from '../integration/config-store.mjs';
import { resolveGlobalRoot } from '../integration/paths.mjs';
import { EXIT_CODES } from './constants.mjs';
import { UsageError } from './errors.mjs';

// `loam ingest enable|disable|status [path]`: edits `background_ingest.workspaces`
// in the config-dir config.json (the file Rust's read_config also parses), so every
// write is atomic via publishJson and a malformed file is refused, never replaced.

function expandHome(value, home) {
  if (value === '~') return home;
  return /^~[\\/]/u.test(value) ? join(home, value.slice(2)) : value;
}

async function loadConfig(configPath) {
  let text;
  try { text = await readFile(configPath, 'utf8'); } catch (error) {
    if (error.code === 'ENOENT') return {};
    throw error;
  }
  if (text.trim() === '') return {};
  let parsed;
  try { parsed = JSON.parse(text); } catch { parsed = undefined; }
  if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) {
    throw new UsageError(`${configPath} is malformed; fix or remove it first, nothing was changed`);
  }
  return parsed;
}

async function sameWorkspace(entry, target, home) {
  if (typeof entry !== 'string') return false;
  return (await canonicalWorkspace(expandHome(entry.trim(), home))) === target;
}

function contains(outer, inner) {
  const rel = relative(outer, inner);
  return rel === '' || (!rel.startsWith('..') && !isAbsolute(rel));
}

async function coveringEntries(entries, target, home) {
  const found = [];
  for (const entry of entries) {
    if (typeof entry !== 'string' || !entry.trim()) continue;
    const expanded = expandHome(entry.trim(), home);
    if (!isAbsolute(expanded)) continue;
    if (contains(await canonicalWorkspace(expanded), target)) found.push(entry);
  }
  return found;
}

export async function runIngest(parsed, { output = process.stdout, env = process.env, cwd = process.cwd(), home } = {}) {
  const userHome = home || env.HOME || env.USERPROFILE || homedir();
  const target = await canonicalWorkspace(parsed.path ? resolve(cwd, expandHome(parsed.path, userHome)) : cwd);
  const globalRoot = env.LOAM_INGEST_GLOBAL_ROOT || resolveGlobalRoot({ env, home: userHome });

  if (parsed.action === 'status') {
    const status = await ingestStatus({ globalRoot, workspace: target, env });
    if (parsed.json) {
      output.write(`${JSON.stringify({ workspace: target, enabled: status.enabled, enabled_by: status.enabled_by, config_path: status.config_path, config_error: status.config_error, ignored_entries: status.ignored_entries })}\n`);
    } else {
      output.write([
        `workspace: ${target}`,
        `enabled: ${status.enabled}`,
        `enabled_by: ${status.enabled_by}`,
        `config_path: ${status.config_path}`,
        `config_error: ${status.config_error ?? 'none'}`,
        `ignored_entries: ${status.ignored_entries.length ? JSON.stringify(status.ignored_entries) : 'none'}`,
        '',
      ].join('\n'));
    }
    return EXIT_CODES.OK;
  }

  const root = configRoot({ env, home: userHome });
  if (!root) throw new UsageError('cannot resolve the loam config directory');
  const configPath = join(root, 'config.json');
  const config = await loadConfig(configPath);
  const rawSection = config.background_ingest;
  if (rawSection !== undefined && (!rawSection || typeof rawSection !== 'object' || Array.isArray(rawSection))) {
    throw new UsageError(`${configPath} has a malformed background_ingest section (expected an object); fix it first, nothing was changed`);
  }
  const section = rawSection ?? {};
  if (section.workspaces !== undefined && !Array.isArray(section.workspaces)) {
    throw new UsageError(`${configPath} has a malformed background_ingest.workspaces (expected an array); fix it first, nothing was changed`);
  }
  const entries = section.workspaces ?? [];
  const matches = [];
  for (const entry of entries) matches.push(await sameWorkspace(entry, target, userHome));
  const present = matches.some(Boolean);

  if (parsed.action === 'enable') {
    if (parse(target).root === target) throw new UsageError('refusing to enable ingest for the filesystem root');
    if (contains(target, await canonicalWorkspace(userHome))) throw new UsageError('refusing to enable ingest for the home directory or a directory containing it');
    const warn = async () => {
      const status = await ingestStatus({ globalRoot, workspace: target, env });
      if (!status.enabled) output.write(`  warning: ingest is still off for this workspace (enabled_by: ${status.enabled_by}); see \`loam ingest status\`\n`);
    };
    if (present) {
      output.write(`background ingest is already enabled for ${target}\n`);
      await warn();
      return EXIT_CODES.OK;
    }
    await publishJson({ filePath: configPath, value: { ...config, background_ingest: { ...section, workspaces: [...entries, target] } } });
    output.write(`enabled background ingest for ${target}\n  config: ${configPath}\n`);
    await warn();
    return EXIT_CODES.OK;
  }

  if (!present) {
    const status = await ingestStatus({ globalRoot, workspace: target, env });
    if (!status.enabled) {
      output.write(`no entry for ${target}; background ingest is not enabled (enabled_by: ${status.enabled_by})\n`);
    } else if (status.enabled_by === 'allowlist') {
      const covering = await coveringEntries(entries, target, userHome);
      output.write(`no exact entry for ${target}; it is still enabled by the covering allowlist entry ${covering.map((c) => JSON.stringify(c)).join(', ') || '(see status)'}; disable that path instead\n`);
    } else {
      output.write(`no entry for ${target}; it is still enabled by ${status.enabled_by}, not the allowlist\n`);
    }
    return EXIT_CODES.OK;
  }
  await publishJson({ filePath: configPath, value: { ...config, background_ingest: { ...section, workspaces: entries.filter((_, i) => !matches[i]) } } });
  output.write(`removed ${target} from background ingest\n  config: ${configPath}\n`);
  return EXIT_CODES.OK;
}

// One-time upgrade notice. Returns the notice text (or '') and records an empty
// `background_ingest` section so it is shown once. A fresh machine records
// silently. Never throws: a notice must not fail setup, and a missing or
// malformed config.json is left untouched (only ENOENT is created).
export async function ingestUpgradeNotice({ earlierInstall, env = process.env, home, platform = process.platform } = {}) {
  try {
    const root = configRoot({ env, home: home || env.HOME || env.USERPROFILE, platform });
    if (!root) return '';
    const configPath = join(root, 'config.json');
    let config;
    try { config = await loadConfig(configPath); } catch { return ''; }
    if (Object.hasOwn(config, 'background_ingest')) return '';
    await publishJson({ filePath: configPath, value: { ...config, background_ingest: { workspaces: [] } } });
    if (!earlierInstall) return '';
    return [
      'Background ingest is now opt-in: it no longer runs for every workspace.',
      'Enable it for a workspace with: npx @scchearn/loam ingest enable [path]',
      `Allowlist: ${configPath}`,
    ].join('\n');
  } catch {
    return '';
  }
}
