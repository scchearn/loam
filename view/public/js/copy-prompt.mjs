/**
 * Copy-prompt ("sparkle") control — shared by Pulse and Stewardship.
 *
 * A signal or hint that maps to a loam command gets one compact control that
 * copies a paste-ready instruction for the human's agent harness. A null command
 * gets nothing: the product never invents an action it cannot name.
 *
 * Prompt derivation is the spec's rule, applied once, here: strip the leading
 * `/loam::` namespace, prefix `Run loam `, and let the signal's own message
 * supply the purpose. No other phrasing is generated anywhere.
 */

const TOAST_MS = 2600;
const CONFIRM_MS = 1400;

export const PASTE_HINT =
  'Prompt copied — paste it into your agent harness (Claude Code, OpenCode, Antigravity…).';

/** `/loam::setting-goals` + message -> `Run loam setting-goals to address: <message>`. */
export function promptFor(command, message) {
  const skill = String(command ?? '').trim().replace(/^\/loam::/, '');
  if (!skill) return null;
  const purpose = String(message ?? '').trim();
  return purpose ? `Run loam ${skill} to address: ${purpose}` : `Run loam ${skill}`;
}

function icon(doc, id) {
  const svg = doc.createElementNS('http://www.w3.org/2000/svg', 'svg');
  svg.setAttribute('class', 'icon');
  const use = doc.createElementNS('http://www.w3.org/2000/svg', 'use');
  use.setAttribute('href', `#${id}`);
  svg.append(use);
  return svg;
}

async function writeClipboard(win, text) {
  try {
    await win.navigator?.clipboard?.writeText(text);
    return;
  } catch {
    // Fall through: a denied or missing async clipboard is normal on non-secure
    // origins, and the human still needs the text.
  }
  const doc = win.document;
  const field = doc.createElement('textarea');
  field.value = text;
  doc.body.append(field);
  field.select();
  try {
    doc.execCommand?.('copy');
  } finally {
    field.remove();
  }
}

/**
 * The one shared toast node. index.html ships it empty because a live region
 * inserted with its text already in it is not reliably announced — the first
 * copy would be silent. Creating it here is only a fallback for a host that
 * mounted the controls without the shell markup.
 */
function showToast(doc, message) {
  let toast = doc.querySelector('[data-toast]');
  if (!toast) {
    toast = doc.createElement('div');
    toast.className = 'toast';
    toast.dataset.toast = '';
    toast.setAttribute('role', 'status');
    toast.setAttribute('aria-live', 'polite');
    doc.body.append(toast);
  }
  toast.textContent = message;
  toast.classList.add('is-visible');
  clearTimeout(toast.dataset.timer && Number(toast.dataset.timer));
  const timer = doc.defaultView.setTimeout(() => toast.classList.remove('is-visible'), TOAST_MS);
  toast.dataset.timer = String(timer);
  return toast;
}

/**
 * Build the control for a command/message pair, or return null when there is no
 * command — the caller appends whatever it gets back, so "no action" needs no
 * branch at every call site.
 */
export function createCopyPrompt(doc, { command, message }) {
  const prompt = promptFor(command, message);
  if (!prompt) return null;

  const win = doc.defaultView ?? globalThis;
  const button = doc.createElement('button');
  button.type = 'button';
  button.className = 'copy-prompt';
  button.dataset.tip = 'Copy prompt';
  button.dataset.copyPrompt = prompt;
  // A view can carry a dozen of these; an identical name on each one tells a
  // screen-reader user nothing about which signal they are about to copy.
  const purpose = String(message ?? '').trim();
  button.setAttribute(
    'aria-label',
    purpose ? `Copy a paste-ready prompt for your agent: ${purpose}` : 'Copy a paste-ready prompt for your agent',
  );
  button.append(icon(doc, 'i-sparkle'));

  button.addEventListener('click', async () => {
    await writeClipboard(win, prompt);
    button.classList.add('copied');
    button.replaceChildren(icon(doc, 'i-seal'));
    showToast(doc, PASTE_HINT);
    win.setTimeout(() => {
      button.classList.remove('copied');
      button.replaceChildren(icon(doc, 'i-sparkle'));
    }, CONFIRM_MS);
  });

  return button;
}
