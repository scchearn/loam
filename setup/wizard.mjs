import readline from 'node:readline/promises';

// ponytail: clack is the human-facing plane, not a hard requirement. A partial
// or offline install must still be answerable, so every prompt degrades to the
// readline path below. Cached because `stage()` is sync and reads it directly.
let prompts = null;
let promptsLoaded = false;

async function loadPrompts(output) {
  if (output !== process.stdout) return null;
  if (!promptsLoaded) {
    promptsLoaded = true;
    prompts = await import('@clack/prompts').catch(() => null);
  }
  return prompts;
}

function styled(output) {
  return output === process.stdout ? prompts : null;
}

const HARNESS_LABELS = { claude: 'Claude Code', codex: 'Codex' };

// Opens the run: a heading plus one indented block. Also primes the prompts
// cache that the synchronous `stage`/`finish` writers read.
export async function announce(output, title, lines, { level = 'info' } = {}) {
  const ui = await loadPrompts(output);
  if (ui) {
    ui.intro(title);
    ui.log[level](lines.join('\n'));
    return;
  }
  output.write(`${title}\n`);
  for (const line of lines) output.write(`  ${line}\n`);
}

export function renderDiscovery(discovery, output, { action = 'Setup', dryRun = false } = {}) {
  const rows = [
    ['Home', discovery.home],
    ['Global root', discovery.globalRoot],
    ['Skills source', 'scchearn/loam (global, universal)'],
    ['Runtime target', discovery.target],
    ['Workspace', discovery.workspace],
  ];
  const width = Math.max(...rows.map(([key]) => key.length));
  return announce(
    output,
    `Loam ${action}${dryRun ? ' (dry-run)' : ''}`,
    rows.map(([key, value]) => `${key}:${' '.repeat(width - key.length)} ${value}`),
  );
}

async function confirmAction({ yes = false, confirm, input, output, promptText, nonInteractiveMessage }) {
  if (yes) return true;
  if (confirm) return Boolean(await confirm());
  if (!input.isTTY) {
    output.write(`${nonInteractiveMessage}\n`);
    return false;
  }
  if (input === process.stdin) {
    const ui = await loadPrompts(output);
    if (ui) {
      const answer = await ui.confirm({ message: promptText.replace(/\s*\[y\/N\]\s*$/u, '') });
      return !ui.isCancel(answer) && answer === true;
    }
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

export async function selectMarketplaceHarnesses({
  yes = false,
  harnesses = {},
  select,
  input = process.stdin,
  output = process.stdout,
} = {}) {
  const candidates = ['claude', 'codex'].filter((id) => harnesses[id]?.state !== 'absent');
  if (yes || candidates.length === 0) return candidates;
  let isCancel = () => false;
  if (!select) {
    const ui = await loadPrompts(output);
    if (!ui) {
      // ponytail: all-or-nothing when clack is unavailable. Per-harness choice
      // needs a multiselect widget; rerun setup to change an individual one.
      const names = candidates.map((id) => HARNESS_LABELS[id]).join(' and ');
      const accepted = await confirmAction({
        input,
        output,
        promptText: `Install Loam marketplace plugins for ${names}? [y/N] `,
        nonInteractiveMessage: 'Marketplace plugin selection requires confirmation; rerun with --yes.',
      });
      return accepted ? candidates : [];
    }
    select = ui.multiselect;
    isCancel = ui.isCancel;
  }
  const selected = await select({
    message: 'Install Loam marketplace plugins',
    options: candidates.map((value) => ({ value, label: HARNESS_LABELS[value] })),
    initialValues: candidates,
    required: false,
  });
  return isCancel(selected) ? null : selected;
}

export function stage(output, name, detail = '') {
  const line = `${name}${detail ? `: ${detail}` : ''}`;
  const ui = styled(output);
  if (ui) {
    ui.log.step(line);
    return;
  }
  output.write(`${line}\n`);
}

export function finish(output, name, detail = '') {
  const line = `${name}${detail ? `: ${detail}` : ''}`;
  const ui = styled(output);
  if (ui) {
    ui.outro(line);
    return;
  }
  output.write(`${line}\n`);
}
