import assert from 'node:assert/strict';
import { join, resolve } from 'node:path';
import { test } from 'node:test';

import { configRoot as setupConfigRoot } from '../setup/profile.mjs';
import { configRoot as integrationConfigRoot } from '../integration/config-store.mjs';

// The config-dir ladder is deliberately duplicated: setup/profile.mjs owns it for
// the installer, integration/config-store.mjs owns a copy for the STAGED, self-
// contained integration tree (which must not import ../setup/*). The installer
// writes the ledger with one copy and readiness resolves it with the other, so a
// silent divergence would send them to different config dirs. This contract pins
// the two identical across the whole variation space — LOAM_CONFIG_DIR set / XDG
// set / platform dirs / home fallbacks / no-basis — so the copies cannot drift.
// (cli/src/provisioning.rs mirrors the same ladder; the DRY relocation of all
// three into one home is reconciliation-era cleanup.)

// `expected` (when present) also pins the resolved VALUE, not just parity, so a
// case can't pass by both copies drifting the same wrong way.
const cases = [
  {
    name: 'LOAM_CONFIG_DIR wins over XDG/APPDATA/home on every platform',
    input: { env: { LOAM_CONFIG_DIR: '/explicit/cfg', XDG_CONFIG_HOME: '/xdg', APPDATA: 'C:/AppData', HOME: '/home/u' }, platform: 'linux' },
    expected: resolve('/explicit/cfg'),
  },
  {
    name: 'LOAM_CONFIG_DIR wins on darwin',
    input: { env: { LOAM_CONFIG_DIR: '/explicit/cfg', HOME: '/Users/u' }, platform: 'darwin' },
    expected: resolve('/explicit/cfg'),
  },
  {
    name: 'blank LOAM_CONFIG_DIR is ignored, falls through',
    input: { env: { LOAM_CONFIG_DIR: '   ', XDG_CONFIG_HOME: '/xdg', HOME: '/home/u' }, platform: 'linux' },
    expected: resolve('/xdg', 'loam'),
  },
  {
    name: 'darwin platform dir',
    input: { env: { HOME: '/Users/u' }, platform: 'darwin' },
    expected: join('/Users/u', 'Library', 'Application Support', 'loam'),
  },
  {
    name: 'darwin without home resolves to null',
    input: { env: {}, platform: 'darwin' },
    expected: null,
  },
  {
    name: 'win32 APPDATA dir',
    input: { env: { APPDATA: 'C:/Users/u/AppData/Roaming', HOME: 'C:/Users/u' }, platform: 'win32' },
    expected: resolve('C:/Users/u/AppData/Roaming', 'loam'),
  },
  {
    name: 'win32 without APPDATA falls to XDG',
    input: { env: { XDG_CONFIG_HOME: '/xdg', HOME: '/home/u' }, platform: 'win32' },
    expected: resolve('/xdg', 'loam'),
  },
  {
    name: 'XDG_CONFIG_HOME on linux',
    input: { env: { XDG_CONFIG_HOME: '/xdg/config', HOME: '/home/u' }, platform: 'linux' },
    expected: resolve('/xdg/config', 'loam'),
  },
  {
    name: 'blank XDG falls to home/.config',
    input: { env: { XDG_CONFIG_HOME: '  ', HOME: '/home/u' }, platform: 'linux' },
    expected: join('/home/u', '.config', 'loam'),
  },
  {
    name: 'bare default home/.config',
    input: { env: { HOME: '/home/u' }, platform: 'linux' },
    expected: join('/home/u', '.config', 'loam'),
  },
  {
    name: 'USERPROFILE as home fallback',
    input: { env: { USERPROFILE: 'C:/Users/u' }, platform: 'win32' },
    // win32 with no APPDATA and no XDG → home/.config (home from USERPROFILE).
    expected: join('C:/Users/u', '.config', 'loam'),
  },
  {
    name: 'no basis at all resolves to null',
    input: { env: {}, platform: 'linux' },
    expected: null,
  },
  {
    name: 'explicit home arg overrides env HOME',
    input: { env: { HOME: '/env/home' }, home: '/arg/home', platform: 'linux' },
    expected: join('/arg/home', '.config', 'loam'),
  },
];

for (const { name, input, expected } of cases) {
  test(`configRoot parity: ${name}`, () => {
    const setup = setupConfigRoot(input);
    const integration = integrationConfigRoot(input);
    assert.equal(integration, setup, `setup and integration configRoot must agree for: ${name}`);
    if (expected !== undefined) {
      assert.equal(setup, expected, `configRoot value pinned for: ${name}`);
    }
  });
}
