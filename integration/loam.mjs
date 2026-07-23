#!/usr/bin/env node

import { fileURLToPath } from 'node:url';
import { resolve } from 'node:path';

import { formatContext } from './context.mjs';
import { readSkillContent } from './metadata.mjs';
import { resolveGlobalRoot, resolveSkillsRoot } from './paths.mjs';
import { probeState } from './runtime.mjs';

const HARNESS_IDS = new Set(['opencode', 'claude', 'cursor']);

function parseArgs(argv) {
  const [command, ...rest] = argv;
  if (command === 'status') return { command };
  if (command === 'hook') {
    let harness;
    let workspace;
    for (let index = 0; index < rest.length; index += 1) {
      const flag = rest[index];
      if (flag === '--harness') harness = rest[++index];
      else if (flag === '--workspace') workspace = rest[++index];
      else throw new Error(`unknown integration option: ${flag}`);
    }
    if (!HARNESS_IDS.has(harness)) throw new Error(`unsupported harness: ${harness || '(missing)'}`);
    if (!workspace) throw new Error('hook requires --workspace');
    return { command, harness, workspace: resolve(workspace) };
  }
  throw new Error('usage: loam.mjs status | hook --harness <id> --workspace <path>');
}

function publicStatus(result) {
  const { skillContent: _skillContent, ...status } = result;
  return status;
}

export async function runIntegration(argv = process.argv.slice(2), options = {}) {
  const parsed = parseArgs(argv);
  const integrationPath = options.integrationPath || fileURLToPath(import.meta.url);
  const env = options.env || process.env;
  const globalRoot = options.globalRoot || resolveGlobalRoot({ env, integrationPath });
  const skillsRoot = options.skillsRoot || resolveSkillsRoot({ home: options.home, env });
  const output = options.output || process.stdout;
  const target = options.target;

  if (parsed.command === 'status') {
    const result = await probeState({
      globalRoot,
      skillsRoot,
      target,
      platform: options.platform,
      arch: options.arch,
      env,
      workspace: options.workspace || process.cwd(),
      timeoutMs: options.timeoutMs,
      runner: options.runner,
      install: options.install,
    });
    output.write(`${JSON.stringify(publicStatus(result))}\n`);
    return result.ready ? 0 : 1;
  }

  const result = await probeState({
    globalRoot,
    skillsRoot,
    target,
    platform: options.platform,
    arch: options.arch,
    workspace: parsed.workspace,
    timeoutMs: options.timeoutMs,
    runner: options.runner,
    install: options.install,
  });
  const skillContent = result.skillContent || (await readSkillContent({ skillsRoot }).catch(() => ''));
  output.write(
    `${formatContext({
      skillContent,
      pluginVersion: result.install?.plugin_version,
      runtimePath: result.runtimePath,
      platform: options.platform,
      workspace: parsed.workspace,
      state: result.ready ? result.state : undefined,
      unavailable: result.ready ? undefined : result,
    })}\n`,
  );
  return 0;
}

if (process.argv[1] && resolve(process.argv[1]) === resolve(fileURLToPath(import.meta.url))) {
  try {
    process.exitCode = await runIntegration();
  } catch (error) {
    process.stderr.write(`loam: ${error instanceof Error ? error.message : String(error)}\n`);
    process.exitCode = 64;
  }
}
