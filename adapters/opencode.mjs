import { homedir } from 'node:os';
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';

const OWN_MARKER = 'You have loam';

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

async function defaultContext({ integrationPath, workspace }) {
  try {
    integrationPath ||= await defaultIntegrationPath();
    const integration = await import(pathToFileURL(integrationPath).href);
    const chunks = [];
    await integration.runIntegration(
      ['hook', '--harness', 'opencode', '--workspace', workspace],
      { integrationPath, output: { write: (chunk) => chunks.push(String(chunk)) } },
    );
    return chunks.join('');
  } catch {
    return '<LOAM_IMPORTANT>\nYou have loam.\nLoam is unavailable. Run: npx @scchearn/loam setup\n</LOAM_IMPORTANT>';
  }
}

export function createOpenCodeAdapter({ integrationPath, getContext = defaultContext } = {}) {
  return async ({ directory } = {}) => ({
    'experimental.chat.messages.transform': async (_input, output) => {
      if (!output?.messages?.length) return;
      const firstUser = output.messages.find((message) => message.info?.role === 'user');
      if (!firstUser?.parts?.length) return;
      if (firstUser.parts.some((part) => part.type === 'text' && part.text.includes(OWN_MARKER))) return;
      const context = await getContext({ harness: 'opencode', workspace: directory || process.cwd(), integrationPath });
      if (!context) return;
      const reference = firstUser.parts[0];
      firstUser.parts.unshift({ ...reference, type: 'text', text: context });
    },
  });
}

export const LoamPlugin = async ({ directory } = {}) => createOpenCodeAdapter()({ directory });
