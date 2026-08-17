import assert from 'node:assert/strict';
import { mkdtemp, readFile, readdir, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { test } from 'node:test';

const integrationDir = new URL('../integration/', import.meta.url);

// `stageIntegration` copies ONLY the top-level `integration/*.mjs` into the
// published tree (flat, no recursion, no `setup/`). So no `integration/*.mjs`
// may import `../setup/*` — that path does not exist beside the staged copy and
// the dynamic import fails on a real install, breaking every staged adapter.
//
// The canary in ingest.test.mjs cannot catch a reintroduced `../setup` import:
// it only drives early-return adapter paths (background disabled, codex no-op)
// that short-circuit before anything loads ledger.mjs/runtime.mjs — the files
// that carried the broken imports. This test loads the staged modules DIRECTLY
// from a setup-less tree, so a `../setup` import anywhere in their chain fails
// loudly here. It closes the regression class, not just the one instance.
test('the staged integration modules load with no setup/ tree beside them', async () => {
  const staged = await mkdtemp(join(tmpdir(), 'loam-staged-integration-'));
  for (const name of await readdir(integrationDir)) {
    if (name.endsWith('.mjs')) {
      await writeFile(join(staged, name), await readFile(new URL(name, integrationDir)));
    }
  }
  // Deliberately no `setup/` next to `staged`: `../setup/*` is unresolvable, so
  // any integration module reaching into it throws ERR_MODULE_NOT_FOUND on load.
  for (const entry of ['config-store.mjs', 'ledger.mjs', 'runtime.mjs', 'ingest.mjs', 'loam.mjs', 'hooks.mjs']) {
    await assert.doesNotReject(
      () => import(pathToFileURL(join(staged, entry)).href),
      `${entry} must load from the staged tree without importing ../setup/*`,
    );
  }
});
