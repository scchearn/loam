import { discover } from './discovery.mjs';
import { verifyInstallation } from './verify.mjs';
import { CATALOG } from './integrations/catalog.mjs';

function detail(check) {
  return check?.detail || check?.message || check?.category || '';
}

function report(output, name, check) {
  const state = check?.ready ? 'ok' : 'failed';
  const reason = detail(check);
  output.write(`  ${name}: ${state}${reason ? ` (${reason})` : ''}\n`);
}

export async function runDoctor(options = {}) {
  const output = options.output || process.stdout;
  const errorOutput = options.errorOutput || process.stderr;
  try {
    const discovery = await discover({
      home: options.home,
      workspace: options.workspace,
      packageRoot: options.packageRoot,
      target: options.target,
      platform: options.platform,
      arch: options.arch,
      runner: options.runner,
    });
    const result = await verifyInstallation({
      discovery,
      packageRoot: discovery.packageRoot,
      runner: options.runner,
      runtimeRunner: options.runtimeRunner,
      runtimeTimeoutMs: options.runtimeTimeoutMs,
    });

    output.write('Loam Doctor\n');
    report(output, 'Install metadata', result.install ? { ready: true } : { ready: false, category: 'install_metadata_missing' });
    if (result.install) {
      output.write(`  Plugin version: ${result.install.plugin_version}\n`);
      // The runtime version is the config-dir ledger target (schema-2 install.json
      // no longer records it); the schema-1 field is a display fallback.
      const runtimeVersion = result.runtime?.ledger?.target ?? result.install.runtime_version ?? 'unknown';
      output.write(`  CLI version: ${runtimeVersion}\n`);
    }
    report(output, 'Global skills', result.skills);
    report(output, 'Native runtime', result.runtime);
    for (const [id, harness] of Object.entries(result.harnesses)) report(output, `${id} integration`, harness);
    report(output, 'Workspace migration', result.migration);

    // Optional integrations: informational only — an absent integration is a
    // choice, never a failure, so this section never affects the exit code.
    if (result.install) {
      output.write('Optional integrations (informational):\n');
      for (const entry of CATALOG) {
        const state = await entry.verify({ discovery, install: result.install, runner: options.runner });
        const harnessBits = Object.entries(state.registered).map(([id, on]) => `${id}:${on ? 'yes' : 'no'}`).join(' ');
        const toolBit = entry.tool
          ? `tool ${state.tool?.present ? `present (${state.tool.managed ? 'loam-managed' : 'PATH'})` : 'absent'}`
          : 'no tool';
        output.write(`  ${entry.id} (${entry.capability}): ${toolBit}; MCP ${harnessBits || 'no harness'}\n`);
      }
    }

    output.write(`Result: ${result.ready ? 'ready' : 'not ready'}\n`);
    return result.ready ? 0 : 1;
  } catch (error) {
    errorOutput.write(`Doctor failed: ${error instanceof Error ? error.message : String(error)}\n`);
    return 1;
  }
}
