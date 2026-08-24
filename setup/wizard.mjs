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

const HARNESS_LABELS = {
  claude: 'Claude Code',
  codex: 'Codex',
  opencode: 'OpenCode',
  cursor: 'Cursor',
};
const HARNESS_ORDER = ['claude', 'codex', 'opencode', 'cursor'];

export function harnessLabel(id) {
  return HARNESS_LABELS[id] || id;
}

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
  const detected = HARNESS_ORDER
    .filter((id) => discovery.harnesses?.[id]?.state !== 'absent')
    .map((id) => HARNESS_LABELS[id]);
  const rows = [
    ['Home', discovery.home],
    ['Global root', discovery.globalRoot],
    ['Skills source', 'scchearn/loam (global, universal)'],
    ['Runtime target', discovery.target],
    ['Workspace', discovery.workspace],
    ['Harnesses', detected.length ? detected.join(', ') : 'none detected'],
  ];
  const width = Math.max(...rows.map(([key]) => key.length));
  return announce(
    output,
    `🌱 Loam ${action}${dryRun ? ' (dry-run)' : ''}`,
    rows.map(([key, value]) => `${key}:${' '.repeat(width - key.length)} ${value}`),
  );
}

export async function confirmAction({ yes = false, confirm, input, output, promptText, nonInteractiveMessage }) {
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

// Unified harness picker. Returns { selected, toRemove } (or null on cancel).
// - selected: harnesses to configure this run (all four are eligible).
// - toRemove: harnesses that were previously configured but are now deselected;
//   interactive-only, since a deselect means "tear this one down". Non-interactive
//   runs (including `update`) never remove.
// Backwards compat anchors on `previouslyConfigured` (install.json's
// configured_harnesses): update maintains exactly that set, so an OpenCode that
// was auto-configured before an upgrade keeps working.
export async function selectHarnesses({
  yes = false,
  refresh = false,
  harnesses = {},
  previouslyConfigured = [],
  select,
  input = process.stdin,
  output = process.stdout,
} = {}) {
  const detected = HARNESS_ORDER.filter((id) => harnesses[id]?.state !== 'absent');
  const prev = new Set(previouslyConfigured);
  if (detected.length === 0) return { selected: [], toRemove: [] };

  if (yes) {
    // update: maintain the previously-configured set only (no add, no remove).
    // fresh --yes: configure everything detected.
    const selected = refresh ? detected.filter((id) => prev.has(id)) : detected;
    return { selected, toRemove: [] };
  }

  const finalize = (selected) => {
    const chosen = new Set(selected);
    return {
      selected,
      toRemove: detected.filter((id) => prev.has(id) && !chosen.has(id)),
    };
  };

  let isCancel = () => false;
  let widget = select;
  if (!widget) {
    const ui = await loadPrompts(output);
    if (!ui) {
      // ponytail: all-or-nothing when clack's multiselect is unavailable. Per-harness
      // toggling needs the widget; rerun setup to change an individual one.
      const names = detected.map((id) => HARNESS_LABELS[id]).join(', ');
      const accepted = await confirmAction({
        input,
        output,
        promptText: `Configure Loam for ${names}? [y/N] `,
        nonInteractiveMessage: 'Harness selection requires confirmation; rerun with --yes.',
      });
      return finalize(accepted ? detected : []);
    }
    widget = ui.multiselect;
    isCancel = ui.isCancel;
  }

  const chosen = await widget({
    message: 'Configure Loam for',
    options: detected.map((value) => ({ value, label: HARNESS_LABELS[value] })),
    initialValues: detected,
    required: false,
  });
  if (isCancel(chosen)) return null;
  return finalize(Array.isArray(chosen) ? chosen : []);
}

function cleanCatalogText(value) {
  return typeof value === 'string' ? value.replace(/\s+/gu, ' ').trim() : '';
}

function catalogOptions(catalog) {
  if (!Array.isArray(catalog)) return [];
  return catalog
    .map((entry) => ({
      id: cleanCatalogText(entry?.id),
      label: cleanCatalogText(entry?.label),
      capability: cleanCatalogText(entry?.capability),
    }))
    .filter(({ id }) => id);
}

// Interactive installs may opt into integrations; --yes and non-TTY installs
// deliberately select none so automation never enables an egressing service or
// downloads a large companion tool without an explicit human choice.
export async function selectIntegrations({
  yes = false,
  catalog = [],
  select,
  input = process.stdin,
  output = process.stdout,
} = {}) {
  if (yes) return [];
  const entries = catalogOptions(catalog);
  if (!entries.length) return [];
  const options = entries.map(({ id, label, capability }) => ({
    value: id,
    label: label || capability || id,
    ...(capability && label ? { hint: capability } : {}),
  }));
  const valid = new Set(entries.map(({ id }) => id));
  const normalize = (chosen) => Array.isArray(chosen)
    ? [...new Set(chosen.filter((id) => valid.has(id)))]
    : [];

  if (select) {
    const chosen = await select({
      message: 'Enable optional integrations',
      options,
      initialValues: [],
      required: false,
    });
    return chosen === null ? null : normalize(chosen);
  }
  if (!input?.isTTY) return [];

  const ui = await loadPrompts(output);
  if (ui) {
    const chosen = await ui.multiselect({
      message: 'Enable optional integrations',
      options,
      initialValues: [],
      required: false,
    });
    return ui.isCancel(chosen) ? null : normalize(chosen);
  }

  // Readline fallback keeps the choice explicit when the presentation dependency
  // is unavailable; blank input means no integrations.
  const prompt = readline.createInterface({ input, output });
  try {
    const answer = await prompt.question(
      `Enable optional integrations (${options.map(({ value }) => value).join(', ')})? Enter ids separated by commas, or press Enter for none: `,
    );
    return normalize(answer.split(',').map((id) => id.trim()));
  } finally {
    prompt.close();
  }
}

export function optionalIntegrationSummary(catalog = [], enabled = [], previouslyEnabled = []) {
  const entries = catalogOptions(catalog);
  if (!entries.length) return '';
  const enabledIds = new Set(Array.isArray(enabled) ? enabled : []);
  const hiddenIds = new Set([
    ...enabledIds,
    ...(Array.isArray(previouslyEnabled) ? previouslyEnabled : []),
  ]);
  const enabledEntries = entries.filter(({ id }) => enabledIds.has(id));
  const remaining = entries.filter(({ id }) => !hiddenIds.has(id));
  const lines = [];
  if (enabledEntries.length) lines.push(`Integrations enabled: ${enabledEntries.map(({ id }) => id).join(', ')}`);
  if (remaining.length) {
    if (lines.length) lines.push('');
    lines.push('Optional integrations — enable anytime:');
    for (const { id, label, capability } of remaining) {
      const purpose = [label, capability].filter(Boolean).filter((value, index, values) => values.indexOf(value) === index).join(' · ');
      lines.push(`npx @scchearn/loam setup --integration ${id}${purpose ? ` — ${purpose}` : ''}`);
    }
  }
  return lines.join('\n');
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

// Rich-narration primitives. Each degrades to a prefixed plain line off-TTY.
export function stepStart(output, message) {
  const ui = styled(output);
  if (ui) {
    ui.log.step(message);
    return;
  }
  output.write(`→ ${message}\n`);
}

export function stepDetail(output, message) {
  const ui = styled(output);
  if (ui) {
    (ui.log.message || ui.log.info)(message);
    return;
  }
  output.write(`    ${message}\n`);
}

export function stepDone(output, message) {
  const ui = styled(output);
  if (ui) {
    ui.log.success(message);
    return;
  }
  output.write(`✓ ${message}\n`);
}

export function stepSkip(output, message) {
  const ui = styled(output);
  if (ui) {
    ui.log.warn(message);
    return;
  }
  output.write(`– ${message}\n`);
}

export function summaryNote(output, title, body) {
  const ui = styled(output);
  if (ui && ui.note) {
    ui.note(body, title);
    return;
  }
  output.write(`\n${title}\n`);
  for (const line of body.split('\n')) output.write(`  ${line}\n`);
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
