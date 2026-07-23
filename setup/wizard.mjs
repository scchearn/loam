import readline from 'node:readline/promises';

export function renderDiscovery(discovery, output, { dryRun = false } = {}) {
  output.write(`Loam Setup${dryRun ? ' (dry-run)' : ''}\n`);
  output.write(`  Home: ${discovery.home}\n`);
  output.write(`  Global root: ${discovery.globalRoot}\n`);
  output.write(`  Skills source: scchearn/loam (global, Claude target)\n`);
  output.write(`  Runtime target: ${discovery.target}\n`);
  output.write(`  Workspace: ${discovery.workspace}\n`);
}

export async function confirmSetup({ yes = false, confirm, input = process.stdin, output = process.stdout } = {}) {
  if (yes) return true;
  if (confirm) return Boolean(await confirm());
  if (!input.isTTY) {
    output.write('Setup requires confirmation; rerun with --yes.\n');
    return false;
  }
  const prompt = readline.createInterface({ input, output });
  try {
    const answer = await prompt.question('Continue with global Loam setup? [y/N] ');
    return /^y(es)?$/i.test(answer.trim());
  } finally {
    prompt.close();
  }
}

export function stage(output, name, detail = '') {
  output.write(`${name}${detail ? `: ${detail}` : ''}\n`);
}
