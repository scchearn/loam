import readline from 'node:readline/promises';

export function renderDiscovery(discovery, output, { dryRun = false } = {}) {
  output.write(`Loam Setup${dryRun ? ' (dry-run)' : ''}\n`);
  output.write(`  Home: ${discovery.home}\n`);
  output.write(`  Global root: ${discovery.globalRoot}\n`);
  output.write(`  Skills source: scchearn/loam (global, universal)\n`);
  output.write(`  Runtime target: ${discovery.target}\n`);
  output.write(`  Workspace: ${discovery.workspace}\n`);
}

async function confirmAction({ yes = false, confirm, input, output, promptText, nonInteractiveMessage }) {
  if (yes) return true;
  if (confirm) return Boolean(await confirm());
  if (!input.isTTY) {
    output.write(`${nonInteractiveMessage}\n`);
    return false;
  }
  const prompt = readline.createInterface({ input, output });
  try {
    const answer = await prompt.question(promptText);
    return /^y(es)?$/i.test(answer.trim());
  } finally {
    prompt.close();
  }
}

export function confirmSetup({ yes = false, confirm, input = process.stdin, output = process.stdout } = {}) {
  return confirmAction({
    yes,
    confirm,
    input,
    output,
    promptText: 'Continue with global Loam setup? [y/N] ',
    nonInteractiveMessage: 'Setup requires confirmation; rerun with --yes.',
  });
}

export function confirmUninstall({ yes = false, confirm, input = process.stdin, output = process.stdout } = {}) {
  return confirmAction({
    yes,
    confirm,
    input,
    output,
    promptText: 'Proceed with global Loam uninstall? [y/N] ',
    nonInteractiveMessage: 'Uninstall requires confirmation; rerun with --yes.',
  });
}

export function stage(output, name, detail = '') {
  output.write(`${name}${detail ? `: ${detail}` : ''}\n`);
}
