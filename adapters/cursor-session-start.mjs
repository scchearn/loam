import { homedir } from 'node:os';
import { readFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
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

async function defaultContext({ integrationPath, workspace }) {
  try {
    integrationPath ||= await defaultIntegrationPath();
    const integration = await import(pathToFileURL(integrationPath).href);
    const chunks = [];
    await integration.runIntegration(
      ['hook', '--harness', 'cursor', '--workspace', workspace],
      { integrationPath, output: { write: (chunk) => chunks.push(String(chunk)) } },
    );
    return chunks.join('');
  } catch {
    return '<LOAM_IMPORTANT>\nYou have loam.\nLoam is unavailable. Run: npx @scchearn/loam setup\n</LOAM_IMPORTANT>';
  }
}

export function createCursorAdapter({ integrationPath, getContext = defaultContext } = {}) {
  return async (payload = {}) => ({
    additional_context: await getContext({
      harness: 'cursor',
      workspace: workspaceFromPayload(payload),
      integrationPath,
    }),
  });
}

export async function handleCursorHook(payload, options) {
  return createCursorAdapter(options)(typeof payload === 'string' ? JSON.parse(payload) : payload);
}

if (process.argv[1] && resolve(process.argv[1]) === resolve(fileURLToPath(import.meta.url))) {
  let input = '';
  process.stdin.setEncoding('utf8');
  for await (const chunk of process.stdin) input += chunk;
  process.stdout.write(`${JSON.stringify(await handleCursorHook(input || '{}'))}\n`);
}
