import { homedir } from 'node:os';
import { readFile } from 'node:fs/promises';
import { basename, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

async function defaultIntegrationPath() {
  if (process.env.LOAM_INTEGRATION_PATH) return process.env.LOAM_INTEGRATION_PATH;
  const fallback = join(homedir(), '.agents', 'loam', 'integration', 'loam.mjs');
  try {
    const metadata = JSON.parse(await readFile(join(homedir(), '.agents', 'loam', 'install.json'), 'utf8'));
    return typeof metadata.integration_path === 'string' ? metadata.integration_path : fallback;
  } catch {
    return fallback;
  }
}

export function workspaceFromPayload(payload = {}, fallback = process.cwd()) {
  const value = payload.cwd || payload.workspaceRoot || payload.workspace?.root || payload.session?.cwd || fallback;
  return resolve(value);
}

async function defaultContext({ harness, integrationPath, workspace }) {
  try {
    integrationPath ||= await defaultIntegrationPath();
    const integration = await import(pathToFileURL(integrationPath).href);
    const candidates = harness === 'codex' ? ['codex', 'claude'] : [harness];
    for (const [index, candidate] of candidates.entries()) {
      const chunks = [];
      try {
        await integration.runIntegration(
          ['hook', '--harness', candidate, '--workspace', workspace],
          { integrationPath, output: { write: (chunk) => chunks.push(String(chunk)) } },
        );
        return chunks.join('');
      } catch (error) {
        if (index === candidates.length - 1) throw error;
      }
    }
  } catch {
    return '<LOAM_IMPORTANT>\nYou have loam.\nLoam is unavailable. Run: npx @scchearn/loam setup\n</LOAM_IMPORTANT>';
  }
}

export function createMarketplaceAdapter({ harness = 'claude', integrationPath, getContext = defaultContext } = {}) {
  return async (payload = {}) => ({
    hookSpecificOutput: {
      hookEventName: 'SessionStart',
      additionalContext: await getContext({
        harness,
        workspace: workspaceFromPayload(payload),
        integrationPath,
      }),
    },
  });
}

export function createClaudeAdapter(options = {}) {
  return createMarketplaceAdapter({ ...options, harness: 'claude' });
}

export async function handleMarketplaceHook(payload, options = {}) {
  return createMarketplaceAdapter(options)(typeof payload === 'string' ? JSON.parse(payload) : payload);
}

export async function handleClaudeHook(payload, options = {}) {
  return handleMarketplaceHook(payload, { ...options, harness: 'claude' });
}

if (process.argv[1] && resolve(process.argv[1]) === resolve(fileURLToPath(import.meta.url))) {
  let input = '';
  process.stdin.setEncoding('utf8');
  for await (const chunk of process.stdin) input += chunk;
  const harness = basename(process.argv[1]).startsWith('codex-') ? 'codex' : 'claude';
  process.stdout.write(`${JSON.stringify(await handleMarketplaceHook(input || '{}', { harness }))}\n`);
}
