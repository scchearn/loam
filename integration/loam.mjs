#!/usr/bin/env node

import { fileURLToPath } from 'node:url';
import { resolve } from 'node:path';

import { resolveGlobalRoot, resolveSkillsRoot } from './paths.mjs';
import { probeState } from './runtime.mjs';
import { ingestStatus } from './ingest.mjs';

function parseArgs(argv) {
  const [command, ...rest] = argv;
  if (command === 'status') return { command };
  if (command === 'ingest-status') {
    let workspace = process.cwd();
    let jsonOutput = false;
    for (let index = 0; index < rest.length; index += 1) {
      if (rest[index] === '--workspace') workspace = rest[++index];
      else if (rest[index] === '--json') jsonOutput = true;
      else throw new Error('unknown integration option: ' + rest[index]);
    }
    return { command, workspace: resolve(workspace), jsonOutput };
  }
  // `hook` is gone: the harness read path is the native `loam hook <harness>`
  // command, which setup writes into each harness registration directly.
  throw new Error('usage: loam.mjs status | ingest-status --workspace <path> [--json]');
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

  if (parsed.command === 'ingest-status') {
    const result = await ingestStatus({ globalRoot, workspace: parsed.workspace, env });
    output.write(JSON.stringify(result) + '\n');
    return 0;
  }

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

if (process.argv[1] && resolve(process.argv[1]) === resolve(fileURLToPath(import.meta.url))) {
  try {
    process.exitCode = await runIntegration();
  } catch (error) {
    process.stderr.write(`loam: ${error instanceof Error ? error.message : String(error)}\n`);
    process.exitCode = 64;
  }
}
