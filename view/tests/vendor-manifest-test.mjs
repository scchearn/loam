import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync, readdirSync, statSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(here, '..', '..');
const vendorRoot = path.join(repoRoot, 'view', 'vendor');
const manifestPath = path.join(vendorRoot, 'manifest.json');

const manifest = JSON.parse(readFileSync(manifestPath, 'utf8'));

function sha256(filePath) {
  return createHash('sha256').update(readFileSync(filePath)).digest('hex');
}

function walk(dir) {
  const out = [];
  for (const entry of readdirSync(dir)) {
    const full = path.join(dir, entry);
    if (statSync(full).isDirectory()) {
      out.push(...walk(full));
    } else {
      out.push(full);
    }
  }
  return out;
}

for (const entry of manifest.entries) {
  test(`${entry.package}@${entry.version}: vendored file exists and hash matches manifest`, () => {
    const filePath = path.join(repoRoot, entry.file);
    assert.ok(statSync(filePath).isFile(), `missing vendored file: ${entry.file}`);
    assert.equal(sha256(filePath), entry.sha256, `sha256 mismatch for ${entry.file}`);
  });

  test(`${entry.package}@${entry.version}: license file present`, () => {
    const licensePath = path.join(repoRoot, entry.license);
    assert.ok(statSync(licensePath).isFile(), `missing license file: ${entry.license}`);
  });
}

test('no unmanifested files exist under view/vendor/', () => {
  const manifested = new Set([
    manifestPath,
    ...manifest.entries.flatMap((entry) => [
      path.join(repoRoot, entry.file),
      path.join(repoRoot, entry.license),
    ]),
  ]);
  const actual = walk(vendorRoot);
  const extra = actual.filter((f) => !manifested.has(f));
  assert.deepEqual(extra, [], `unmanifested files under view/vendor/: ${extra.join(', ')}`);
});
