import { discover } from './discovery.mjs';
import { verifyInstallation } from './verify.mjs';

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
    report(output, 'Install metadata', result.installReadiness || (result.install ? { ready: true } : { ready: false, category: 'install_metadata_missing' }));
    if (result.install) {
      output.write(`  Plugin version: ${result.install.plugin_version}\n`);
      output.write(`  CLI version: ${result.install.runtime_version}\n`);
    } else if (result.installReadiness?.category === 'install_metadata_missing') {
      output.write("  Fix: run 'loam setup' to initialize the install metadata.\n");
    }
    report(output, 'Global skills', result.skills);
    report(output, 'Native runtime', result.runtime);
    for (const [id, harness] of Object.entries(result.harnesses)) report(output, `${id} integration`, harness);
    report(output, 'Workspace migration', result.migration);
    output.write(`Result: ${result.ready ? 'ready' : 'not ready'}\n`);
    return result.ready ? 0 : 1;
  } catch (error) {
    errorOutput.write(`Doctor failed: ${error instanceof Error ? error.message : String(error)}\n`);
    return 1;
  }
}
