import { join, resolve } from 'node:path';

// The federation profile root (the durable `federation/` subtree that survives
// uninstall), resolved with the same hand-rolled ladder the Rust runtime uses:
// `LOAM_CONFIG_DIR` -> platform config dir -> legacy install root. Node never
// writes the profile; it only reports where it is, so setup can preserve it on
// uninstall and `--purge` can destroy it. Mirrors `cli/src/provisioning.rs`.

export function configRoot({
  env = process.env,
  home = env.HOME || env.USERPROFILE,
  platform = process.platform,
} = {}) {
  const explicit = env.LOAM_CONFIG_DIR?.trim();
  if (explicit) return resolve(explicit);

  if (platform === 'darwin' && home) {
    return join(home, 'Library', 'Application Support', 'loam');
  }
  if (platform === 'win32' && env.APPDATA) {
    return resolve(env.APPDATA, 'loam');
  }
  if (env.XDG_CONFIG_HOME?.trim()) return resolve(env.XDG_CONFIG_HOME, 'loam');
  if (home) return join(home, '.config', 'loam');
  return null;
}

export function profileRoot(options = {}) {
  const root = configRoot(options);
  return root ? join(root, 'federation') : null;
}

export function federationRoot({ env = process.env, home, platform } = {}) {
  // The legacy install root where a pre-spec profile lived.
  const legacy = env.LOAM_HOME
    ? resolve(env.LOAM_HOME)
    : home
      ? join(home, '.agents', 'loam')
      : null;
  return { config: configRoot({ env, home, platform }), legacy: legacy ? join(legacy, 'federation') : null };
}
