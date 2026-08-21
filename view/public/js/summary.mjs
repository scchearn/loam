/**
 * Per-kind summary extraction for the Inspector.
 *
 * Loam already writes a summary for every page kind — it just keeps it in a
 * different place per kind (spec: "Research: Inspector summary coverage on a
 * real workspace"). This module only knows where to look. It never synthesises
 * a sentence: when the document has no summary, the return says so and the
 * Inspector prints that instead.
 *
 * Input is raw Markdown text from /api/document. Output is plain text for a
 * text node — no Markdown rendering happens here (that is Reader's job).
 */

/** What we tell the human when the document simply does not carry a summary. */
const MISSING = {
  code: 'No Summary section in this code page.',
  topic: 'No opening paragraph or Summary section in this page.',
  entity: 'No opening paragraph or Summary section in this page.',
  concept: 'No opening paragraph or Summary section in this page.',
  analysis: 'No verdict line or opening paragraph in this analysis.',
  spec: 'No Problem section or opening paragraph in this spec.',
  plan: 'No description in this plan front matter.',
  checkpoint: 'No Scope or Intended return recorded in this checkpoint.',
  goal: 'No Intent section in this goal.',
};
const MISSING_DEFAULT = 'No summary recorded for this artifact.';
const NO_DOCUMENT = 'No code page ingested for this file.';

const collapse = (text) => text.replace(/\s+/g, ' ').trim();

function splitFrontMatter(content) {
  const match = /^---\r?\n([\s\S]*?)\r?\n---\r?\n?/.exec(content);
  return match ? { frontMatter: match[1], body: content.slice(match[0].length) } : { frontMatter: '', body: content };
}

/**
 * One scalar out of the front matter block.
 *
 * ponytail: a hand-rolled reader for plain, quoted, and block scalars rather
 * than a YAML parser — js-yaml is vendored but not served to the browser yet.
 * Swap in the real parser when Reader ships it (T13).
 */
function frontMatterField(frontMatter, field) {
  const lines = frontMatter.split(/\r?\n/);
  const index = lines.findIndex((line) => line.startsWith(`${field}:`));
  if (index === -1) return '';

  const inline = lines[index].slice(field.length + 1).trim();
  if (inline && inline !== '>' && inline !== '|' && inline !== '>-' && inline !== '|-') {
    return collapse(inline.replace(/^(['"])([\s\S]*)\1$/, '$2'));
  }
  if (!inline) return '';

  // Block scalar: the indented lines that follow, joined.
  const block = [];
  for (const line of lines.slice(index + 1)) {
    if (line.trim() && !/^\s/.test(line)) break;
    block.push(line.trim());
  }
  return collapse(block.join(' '));
}

/** Blocks of a Markdown body, with fenced code removed and front matter already gone. */
function blocks(body) {
  const kept = [];
  let fenced = false;
  for (const line of body.split(/\r?\n/)) {
    if (/^\s*(```|~~~)/.test(line)) {
      fenced = !fenced;
      continue;
    }
    kept.push(fenced ? '' : line);
  }
  return kept.join('\n').split(/\n\s*\n/).map((block) => block.trim()).filter(Boolean);
}

/** Body of `## <name>`, up to the next heading of any level. */
function section(body, name) {
  const heading = new RegExp(`^#{2,3}\\s+${name}\\s*$`, 'im');
  const match = heading.exec(body);
  if (!match) return '';
  const rest = body.slice(match.index + match[0].length);
  const end = rest.search(/^#{1,6}\s/m);
  return collapse(end === -1 ? rest : rest.slice(0, end));
}

/**
 * The opening paragraph after the H1 — the prose-page convention. Stops at the
 * first `##`, so a page that opens straight into a section has no lead
 * paragraph and falls back to `## Summary` rather than stealing that section.
 */
function leadParagraph(body) {
  const all = blocks(body);
  for (const block of all.slice(all.findIndex((block) => block.startsWith('# ')) + 1)) {
    if (block.startsWith('#')) return '';
    if (block.startsWith('-') || block.startsWith('|') || block.startsWith('>')) continue;
    return collapse(block);
  }
  return '';
}

/** `- Field: value` — the checkpoint front-matter list shape. */
function checkpointField(body, field) {
  const match = new RegExp(`^\\s*-\\s*${field}\\s*:(.*)$`, 'im').exec(body);
  return match ? collapse(match[1]) : '';
}

const EXTRACTORS = {
  code: (body) => section(body, 'Summary'),
  topic: (body) => leadParagraph(body) || section(body, 'Summary'),
  entity: (body) => leadParagraph(body) || section(body, 'Summary'),
  concept: (body) => leadParagraph(body) || section(body, 'Summary'),
  analysis: (body) => {
    const verdict = /^\s*(?:\*\*)?Verdict(?:\*\*)?\s*:.*$/im.exec(body);
    return verdict ? collapse(verdict[0].replace(/\*\*/g, '')) : leadParagraph(body);
  },
  spec: (body) => section(body, 'Problem') || leadParagraph(body),
  plan: (body, frontMatter) => frontMatterField(frontMatter, 'description'),
  checkpoint: (body) => {
    const parts = [checkpointField(body, 'Scope'), checkpointField(body, 'Intended return')];
    return parts.filter(Boolean).join(' — ');
  },
  goal: (body) => section(body, 'Intent'),
};

/**
 * @param {{kind: string, content: string|null}} artifact
 * @returns {{text: string, present: boolean}} `present: false` means the text is
 *   an honest statement of what is missing, not a summary.
 */
export function summarize({ kind, content }) {
  if (typeof content !== 'string' || !content.trim()) {
    return { text: kind === 'code' ? NO_DOCUMENT : MISSING[kind] ?? MISSING_DEFAULT, present: false };
  }
  const { frontMatter, body } = splitFrontMatter(content);
  const extract = EXTRACTORS[kind] ?? ((text) => leadParagraph(text) || section(text, 'Summary'));
  const text = extract(body, frontMatter);
  return text ? { text, present: true } : { text: MISSING[kind] ?? MISSING_DEFAULT, present: false };
}
