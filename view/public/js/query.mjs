/**
 * Global Query palette (Ctrl/Cmd+K).
 *
 * Search only. There is no Ask UI in v1: the spec makes Ask conditional on a
 * configured model or harness adapter, and no adapter exists, so retrieval is
 * never dressed up as synthesis.
 *
 * Security: `/api/search` returns snippets as plain text for DOM text insertion.
 * Every server-supplied string in this module is written through
 * `document.createTextNode` / `textContent`. There is no `innerHTML` here, and
 * there must never be.
 */

// Deterministic filter order: the snapshot schema's artifact kind enum, so the
// same workspace always renders the same filter row.
const KIND_ORDER = [
  'wiki-index',
  'topic',
  'entity',
  'concept',
  'analysis',
  'code',
  'checkpoint',
  'goal',
  'spec',
  'plan',
  'guidance',
  'wiki-other',
];

const MIN_QUERY_LENGTH = 2;

function kindsInSnapshot(snapshot) {
  const present = new Set((snapshot?.artifacts ?? []).map((artifact) => artifact.kind));
  return KIND_ORDER.filter((kind) => present.has(kind));
}

export function initQuery({ root = document, getSnapshot = () => null, debounceMs = 120 } = {}) {
  const dialog = root.querySelector('[data-query-dialog]');
  const input = root.querySelector('[data-query-input]');
  const filterRow = root.querySelector('[data-query-filters]');
  const list = root.querySelector('[data-query-results]');
  const empty = root.querySelector('[data-query-empty]');
  if (!dialog || !input || !list) return null;

  const doc = dialog.ownerDocument;
  let kind = '';
  let results = [];
  let selected = -1;
  let timer = null;
  let generation = 0;

  function setMessage(text) {
    empty.textContent = text;
    empty.classList.toggle('is-visible', Boolean(text));
  }

  function renderFilters() {
    filterRow.replaceChildren();
    const kinds = ['', ...kindsInSnapshot(getSnapshot())];
    for (const value of kinds) {
      const button = doc.createElement('button');
      button.type = 'button';
      button.className = 'filter-button';
      button.dataset.kind = value;
      button.setAttribute('aria-pressed', String(value === kind));
      button.appendChild(doc.createTextNode(value || 'All'));
      button.addEventListener('click', () => {
        kind = value;
        renderFilters();
        run(input.value);
      });
      filterRow.appendChild(button);
    }
  }

  function select(index) {
    if (!results.length) return;
    selected = (index + results.length) % results.length;
    for (const [i, item] of [...list.children].entries()) {
      item.setAttribute('aria-selected', String(i === selected));
    }
    // Combobox pattern: the input keeps focus, so it must point at the active option.
    input.setAttribute('aria-activedescendant', list.children[selected].id);
    list.children[selected].scrollIntoView?.({ block: 'nearest' });
  }

  function activate(index) {
    const hit = results[index];
    if (!hit) return;
    close();
    // Reader (a later task) owns document opening; the palette just announces
    // which inventoried artifact the human picked.
    const CustomEventCtor = doc.defaultView?.CustomEvent ?? CustomEvent;
    doc.dispatchEvent(new CustomEventCtor('loam:open-document', {
      detail: { path: hit.path, kind: hit.kind, title: hit.title },
    }));
  }

  function renderResults() {
    list.replaceChildren();
    for (const [index, hit] of results.entries()) {
      const item = doc.createElement('li');
      item.className = 'query-result';
      item.id = `query-result-${index}`;
      item.setAttribute('role', 'option');
      item.setAttribute('aria-selected', 'false');

      // ARIA: an option may not contain interactive descendants, and the combobox
      // already drives selection via aria-activedescendant — so the row is a plain
      // element and the option itself takes the click.
      const row = doc.createElement('div');
      row.className = 'result-row';

      const kindEl = doc.createElement('span');
      kindEl.className = 'result-kind';
      kindEl.appendChild(doc.createTextNode(hit.kind ?? ''));

      const copy = doc.createElement('span');
      copy.className = 'result-copy';
      const title = doc.createElement('strong');
      title.appendChild(doc.createTextNode(hit.title ?? hit.path ?? ''));
      const snippet = doc.createElement('span');
      snippet.appendChild(doc.createTextNode(hit.snippet ?? ''));
      copy.append(title, snippet);

      row.append(kindEl, copy);
      item.addEventListener('click', () => activate(index));
      item.appendChild(row);
      list.appendChild(item);
    }
    selected = -1;
    input.removeAttribute('aria-activedescendant');
    if (results.length) select(0);
  }

  async function run(rawQuery) {
    const query = rawQuery.trim();
    if (query.length < MIN_QUERY_LENGTH) {
      results = [];
      renderResults();
      setMessage(query ? `Type at least ${MIN_QUERY_LENGTH} characters.` : 'Search memory, code, and work.');
      return;
    }

    const params = new URLSearchParams({ q: query });
    if (kind) params.set('kind', kind);
    const ticket = ++generation;
    let payload;
    try {
      const response = await fetch(`/api/search?${params}`);
      if (!response.ok) {
        const body = await response.json().catch(() => null);
        throw new Error([body?.error, body?.message].filter(Boolean).join(': ') || `HTTP ${response.status}`);
      }
      payload = await response.json();
    } catch (error) {
      if (ticket !== generation) return;
      results = [];
      renderResults();
      setMessage(`Search unavailable: ${error.message}`);
      return;
    }
    // A slower earlier request must not overwrite a newer one's results.
    if (ticket !== generation) return;
    results = Array.isArray(payload?.results) ? payload.results : [];
    renderResults();
    setMessage(results.length ? '' : `No results for "${query}".`);
  }

  function schedule(value) {
    if (timer) clearTimeout(timer);
    if (!debounceMs) return void run(value);
    timer = setTimeout(() => run(value), debounceMs);
  }

  function open() {
    renderFilters();
    // jsdom (the shell's DOM test harness) has no showModal; the open attribute
    // is the same visible state minus the browser's backdrop and focus trap.
    if (typeof dialog.showModal === 'function') dialog.showModal();
    else dialog.setAttribute('open', '');
    input.focus();
    input.select?.();
    run(input.value);
  }

  function close() {
    if (typeof dialog.close === 'function') dialog.close();
    else dialog.removeAttribute('open');
  }

  list.setAttribute('role', 'listbox');
  input.setAttribute('role', 'combobox');
  input.setAttribute('aria-expanded', 'true');

  input.addEventListener('input', () => schedule(input.value));

  dialog.addEventListener('keydown', (event) => {
    if (event.key === 'Escape') {
      event.preventDefault();
      close();
    } else if (event.key === 'ArrowDown') {
      event.preventDefault();
      select(selected + 1);
    } else if (event.key === 'ArrowUp') {
      event.preventDefault();
      select(selected - 1);
    } else if (event.key === 'Enter') {
      event.preventDefault();
      activate(selected);
    }
  });

  for (const closer of root.querySelectorAll('[data-query-close]')) {
    closer.addEventListener('click', close);
  }
  for (const opener of root.querySelectorAll('[data-query-open]')) {
    opener.addEventListener('click', open);
  }

  doc.addEventListener('keydown', (event) => {
    if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === 'k') {
      event.preventDefault();
      if (dialog.hasAttribute('open')) close();
      else open();
    }
  });

  return { open, close, isOpen: () => dialog.hasAttribute('open') };
}
