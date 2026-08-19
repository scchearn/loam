/**
 * Stylesheet integrity.
 *
 * A CSS parser does not fail loudly: an unterminated rule makes the browser
 * swallow everything after it, so a single missing `}` silently deletes whole
 * sections of the design. That is exactly what a merge did to `.timeline-kind`,
 * which cost the browser every Atlas, Reader, and responsive rule below it.
 * These checks are the cheapest thing that fails when it happens again.
 */

import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { describe, it } from 'node:test';

const PUBLIC_ROOT = new URL('../public/', import.meta.url);
const SHEETS = ['styles/tokens.css', 'styles/components.css'];

/** Comments can hold braces; nothing else in these sheets does. */
const stripComments = (css) => css.replace(/\/\*[\s\S]*?\*\//g, '');

describe('the stylesheets parse to the end', () => {
  for (const sheet of SHEETS) {
    it(`${sheet} has balanced braces`, async () => {
      const css = stripComments(await readFile(new URL(sheet, PUBLIC_ROOT), 'utf8'));
      let depth = 0;
      let line = 1;
      for (const character of css) {
        if (character === '\n') line += 1;
        else if (character === '{') depth += 1;
        else if (character === '}') {
          depth -= 1;
          assert.ok(depth >= 0, `${sheet}: unmatched } at line ${line}`);
        }
      }
      assert.equal(depth, 0, `${sheet}: ${depth} unterminated rule(s) — everything after the first is dead CSS`);
    });
  }

  it('components.css still carries the sections that sit furthest down the file', async () => {
    const css = await readFile(new URL('styles/components.css', PUBLIC_ROOT), 'utf8');
    // One selector per section below the point the last truncation started, so
    // a swallowed tail is caught by name and not only by brace arithmetic.
    for (const selector of [
      '.timeline-kind',
      '.atlas-stage',
      '.reader-doc',
      '@media (prefers-reduced-motion: reduce)',
      '@media (max-width: 48rem)',
    ]) {
      assert.ok(css.includes(selector), `components.css lost ${selector}`);
    }
  });
});
