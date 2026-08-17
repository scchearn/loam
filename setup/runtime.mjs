import { createHash } from 'node:crypto';
import { chmod, copyFile, mkdir, readFile, stat, writeFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { join, resolve } from 'node:path';

import { invokeRuntime, verifyRuntimeFile } from '../integration/runtime.mjs';
import { runtimeStorePath, writeLedger } from '../integration/ledger.mjs';
import { createStagingDirectory, cleanupStaging, publishAtomic } from './atomic.mjs';
import { configRoot } from './profile.mjs';
import { assertSupportedTarget, detectTarget } from './target.mjs';

// Core semver with optional semver 2.0.0 prerelease (`-` plus dot-separated
// identifiers). Build metadata (`+...`) stays rejected: npm refuses it and
// `+` in tag-derived URLs is unsafe. Numeric prerelease identifiers must not
// have leading zeros.
const SEMVER = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-(?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*)(?:\.(?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*))*)?$/;
const SHA256 = /^[a-f0-9]{64}$/i;
const DEFAULT_RELEASE_BASE = 'https://github.com/scchearn/loam/releases/download';
const REDIRECT_STATUSES = new Set([301, 302, 303, 307, 308]);
const GITHUB_REDIRECT_HOSTS = new Set([
  'github.com',
  'objects.githubusercontent.com',
  'release-assets.githubusercontent.com',
  'github-releases.githubusercontent.com',
]);

function releaseUrl(base, name) {
  return `${base.replace(/\/+$/, '')}/${name}`;
}

async function downloadToFile(url, destination, { maxBytes, timeoutMs }) {
  if (url.startsWith('file://')) {
    const source = fileURLToPath(url);
    const info = await stat(source);
    if (info.size > maxBytes) throw new Error(`download too large: ${url}`);
    await copyFile(source, destination);
    return;
  }

  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);
  try {
    let currentUrl = url;
    const initialUrl = new URL(url);
    let response;
    for (let redirects = 0; redirects <= 5; redirects += 1) {
      const parsedUrl = new URL(currentUrl);
      if (!['http:', 'https:'].includes(parsedUrl.protocol)) {
        throw new Error(`unsupported download URL scheme: ${parsedUrl.protocol}`);
      }
      response = await fetch(currentUrl, { signal: controller.signal, redirect: 'manual' });
      if (!REDIRECT_STATUSES.has(response.status)) break;
      if (redirects === 5) throw new Error(`download redirect limit exceeded: ${url}`);
      const location = response.headers.get('location');
      if (!location) throw new Error(`download redirect has no location: ${currentUrl}`);
      await response.body?.cancel();
      const nextUrl = new URL(location, currentUrl);
      if (initialUrl.protocol === 'https:' && nextUrl.protocol !== 'https:') {
        throw new Error(`HTTPS redirect downgrade is not allowed: ${nextUrl.href}`);
      }
      const allowedHosts = initialUrl.hostname === 'github.com'
        ? GITHUB_REDIRECT_HOSTS
        : new Set([initialUrl.hostname]);
      if (!allowedHosts.has(nextUrl.hostname)) {
        throw new Error(`untrusted redirect host: ${nextUrl.hostname}`);
      }
      currentUrl = nextUrl.href;
    }
    if (!response.ok) throw new Error(`download failed (${response.status}): ${url}`);
    const length = Number(response.headers.get('content-length') || 0);
    if (length > maxBytes) throw new Error(`download too large: ${url}`);
    const reader = response.body?.getReader();
    const chunks = [];
    let size = 0;
    if (reader) {
      while (true) {
        const next = await reader.read();
        if (next.done) break;
        size += next.value.byteLength;
        if (size > maxBytes) {
          await reader.cancel();
          throw new Error(`download too large: ${url}`);
        }
        chunks.push(Buffer.from(next.value));
      }
    } else {
      chunks.push(Buffer.from(await response.arrayBuffer()));
      size = chunks[0].length;
    }
    if (size > maxBytes) throw new Error(`download too large: ${url}`);
    const bytes = Buffer.concat(chunks, size);
    await writeFile(destination, bytes);
  } catch (error) {
    if (error?.name === 'AbortError') throw new Error(`download timed out: ${url}`);
    throw error;
  } finally {
    clearTimeout(timer);
  }
}

async function sha256File(filePath) {
  const hash = createHash('sha256');
  hash.update(await readFile(filePath));
  return hash.digest('hex');
}

async function readManifest(path, version, target) {
  let manifest;
  try {
    manifest = JSON.parse(await readFile(path, 'utf8'));
  } catch (error) {
    throw new Error(`manifest is invalid: ${error instanceof Error ? error.message : String(error)}`);
  }
  if (manifest.version !== version) throw new Error(`manifest version mismatch: expected ${version}`);
  if (!Array.isArray(manifest.runtimes)) throw new Error('manifest runtimes list is invalid');
  const entries = manifest.runtimes.filter((entry) => entry?.target === target);
  if (entries.length !== 1) throw new Error(`manifest has no runtime for target ${target}`);
  const entry = entries[0];
  if (
    typeof entry.file !== 'string' ||
    !entry.file ||
    entry.file === '.' ||
    entry.file === '..' ||
    entry.file !== entry.file.split(/[\\/]/).pop()
  ) {
    throw new Error('manifest runtime file is invalid');
  }
  if (typeof entry.sha256 !== 'string' || !SHA256.test(entry.sha256)) {
    throw new Error('manifest runtime checksum is invalid');
  }
  return { ...entry, sha256: entry.sha256.toLowerCase() };
}

// A not-yet-published runtime (the manifest 404s, or the file:// fixture is
// missing) is the relocated 78/75 wait-retry case, not a corruption — install
// must signal wait/retry and never wipe or downgrade the active binary.
function isNotPublished(error) {
  const message = error instanceof Error ? error.message : String(error);
  return error?.code === 'ENOENT' || /download failed \(404\)/.test(message) || /ENOENT/.test(message);
}

async function smoke({ runtimePath: executable, workspace, smokeRunner, timeoutMs, version }) {
  const result = await (smokeRunner
    ? smokeRunner({ runtimePath: executable, args: ['state', '--fast', resolve(workspace || process.cwd())], cwd: workspace || process.cwd(), timeoutMs })
    : invokeRuntime({
        runtimePath: executable,
        args: ['state', '--fast', resolve(workspace || process.cwd())],
        cwd: workspace || process.cwd(),
        timeoutMs,
      }));
  if (result?.category === 'timeout' || result?.code !== 0) {
    throw new Error(`runtime smoke failed: ${result?.stderr || `exit ${result?.code}`}`);
  }
  let parsed;
  try {
    parsed = JSON.parse(result.stdout || '');
    if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) throw new Error('state output is not an object');
  } catch (error) {
    throw new Error(`runtime smoke failed: invalid state JSON (${error instanceof Error ? error.message : error})`);
  }
  // Close the mis-versioned-release trap: a cli-v<X> release that ships a binary
  // whose CARGO_PKG_VERSION is not <X> fails loudly here instead of publishing
  // and then failing readiness as stale on every machine forever.
  // ponytail: assert only when the binary reports a version. Post-T1 runtimes
  // always do, so a wrong-version binary (version present, mismatched) is caught;
  // a version-absent output is a pre-T1 stub and is skipped.
  if (version && typeof parsed.version === 'string' && parsed.version !== version) {
    throw new Error(`runtime smoke failed: binary reports version ${parsed.version}, expected ${version}`);
  }
}

export async function installRuntime({
  configDir,
  env = process.env,
  home,
  version,
  target,
  platform = process.platform,
  arch = process.arch,
  channel,
  releaseBaseUrl,
  workspace,
  timeoutMs = 120_000,
  maxDownloadBytes = 64 * 1024 * 1024,
  smokeRunner,
  expectedSha256,
  force = false,
} = {}) {
  if (!SEMVER.test(version || '')) throw new Error(`invalid runtime version: ${version || '(missing)'}`);
  const selectedTarget = target || detectTarget({ platform, arch, override: env.LOAM_TARGET });
  assertSupportedTarget(selectedTarget);
  // The store lives under the durable config dir, not the volatile global root,
  // so it survives an uninstall-preserve. `configRoot` returns null when no
  // config basis resolves — never write to a null path.
  const config = configDir ? resolve(configDir) : configRoot({ env, home, platform });
  if (!config) throw new Error('cannot resolve a config dir for the runtime store');
  const storeRoot = join(config, 'runtime');
  const destination = runtimeStorePath({ version, target: selectedTarget, platform, root: config });
  await mkdir(storeRoot, { recursive: true, mode: 0o700 });

  // The ledger is the readiness authority; write it on every successful install
  // (publish or verified reuse). Gated on `channel` so ledger-agnostic callers
  // and unit fixtures can exercise the download/publish path without one; the
  // real transaction always supplies it (T7).
  const commitLedger = async (sha256) => {
    if (!channel) return;
    await writeLedger({ channel, target: version, sha256, store_path: destination }, { root: config });
  };

  const install = async () => {
    if (!force && expectedSha256) {
      const trusted = await verifyRuntimeFile({ runtimePath: destination, globalRoot: config, expectedSha256 });
      if (trusted.ready) {
        await smoke({ runtimePath: destination, workspace, smokeRunner, timeoutMs, version });
        await commitLedger(trusted.sha256);
        return { reused: true, published: false, path: destination, sha256: trusted.sha256, storePath: destination };
      }
    }

    const staging = await createStagingDirectory(storeRoot, { prefix: `runtime-${version}-${selectedTarget}` });
    try {
      const base = releaseBaseUrl || `${DEFAULT_RELEASE_BASE}/cli-v${version}`;
      const manifestPath = join(staging, 'loam-runtime-manifest.json');
      try {
        await downloadToFile(releaseUrl(base, 'loam-runtime-manifest.json'), manifestPath, { maxBytes: 1_048_576, timeoutMs });
      } catch (error) {
        if (isNotPublished(error)) {
          return { pending: true, reused: false, published: false, path: destination, storePath: destination };
        }
        throw error;
      }
      const manifestEntry = await readManifest(manifestPath, version, selectedTarget);
      const artifactPath = join(staging, manifestEntry.file);
      await downloadToFile(releaseUrl(base, manifestEntry.file), artifactPath, { maxBytes: maxDownloadBytes, timeoutMs });
      const actual = await sha256File(artifactPath);
      if (actual !== manifestEntry.sha256) {
        throw new Error(`checksum mismatch: expected ${manifestEntry.sha256}, got ${actual}`);
      }
      await chmod(artifactPath, 0o700).catch((error) => {
        if (platform !== 'win32') throw error;
      });
      await smoke({ runtimePath: artifactPath, workspace, smokeRunner, timeoutMs, version });
      const publication = await publishAtomic({ stagedPath: artifactPath, destination, mode: 0o700 });
      await commitLedger(manifestEntry.sha256);
      return {
        reused: false,
        published: true,
        path: publication.destination,
        storePath: destination,
        backupPath: publication.backupPath,
        manifest: manifestEntry,
        sha256: manifestEntry.sha256,
      };
    } finally {
      await cleanupStaging(staging);
    }
  };
  return install();
}
