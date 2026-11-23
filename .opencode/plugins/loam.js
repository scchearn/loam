const RECOVERY_CONTEXT = '<LOAM_IMPORTANT>\nLoam is installed for this workspace, but its runtime failed to load for this session, so the loam memory, goals, plans, and checkpoint skills are unavailable right now.\nTo repair it, run: npx @scchearn/loam install\nThen start a new session.\n</LOAM_IMPORTANT>';

async function loadAdapter() {
  try {
    return await import(new URL('../../adapters/opencode.mjs', import.meta.url));
  } catch {
    return null;
  }
}

function recoveryPlugin() {
  return {
    // #209: ride the system prompt, which OpenCode rebuilds per model call, so
    // the notice survives past the first call. The system array is rebuilt each
    // time, so no dedupe check is needed.
    'experimental.chat.system.transform': async (_input, output) => {
      if (!Array.isArray(output?.system)) return;
      output.system.push(RECOVERY_CONTEXT);
    },
  };
}

export async function LoamPlugin(options = {}) {
  const adapter = await loadAdapter();
  if (!adapter?.createOpenCodeAdapter) return recoveryPlugin();
  return adapter.createOpenCodeAdapter()(options);
}

export default LoamPlugin;
