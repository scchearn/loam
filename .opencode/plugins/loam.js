const RECOVERY_CONTEXT = '<LOAM_IMPORTANT>\nYou have loam.\nLoam is unavailable. Recovery: npx @scchearn/loam setup\n</LOAM_IMPORTANT>';

async function loadAdapter() {
  try {
    return await import(new URL('../../adapters/opencode.mjs', import.meta.url));
  } catch {
    return null;
  }
}

function recoveryPlugin() {
  return {
    'experimental.chat.messages.transform': async (_input, output) => {
      if (!output?.messages?.length) return;
      const firstUser = output.messages.find((message) => message.info?.role === 'user');
      if (!firstUser?.parts?.length) return;
      if (firstUser.parts.some((part) => part.type === 'text' && part.text.includes('You have loam'))) return;
      const reference = firstUser.parts[0];
      firstUser.parts.unshift({ ...reference, type: 'text', text: RECOVERY_CONTEXT });
    },
  };
}

export async function LoamPlugin(options = {}) {
  const adapter = await loadAdapter();
  if (!adapter?.createOpenCodeAdapter) return recoveryPlugin();
  return adapter.createOpenCodeAdapter()(options);
}

export default LoamPlugin;
