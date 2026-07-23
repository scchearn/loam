#!/usr/bin/env node

import { realpathSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

import { EXIT_CODES, HELP_TEXT, PACKAGE_VERSION } from '../setup/constants.mjs';
import { parseArgs } from '../setup/args.mjs';

export async function main(argv = process.argv.slice(2), output = process.stdout, errorOutput = process.stderr) {
  try {
    const parsed = parseArgs(argv);
    if (parsed.command === 'help') {
      output.write(HELP_TEXT);
      return EXIT_CODES.OK;
    }
    if (parsed.command === 'version') {
      output.write(`${PACKAGE_VERSION}\n`);
      return EXIT_CODES.OK;
    }

    if (parsed.command === 'setup') {
      const { runSetup } = await import('../setup/main.mjs');
      return await runSetup(parsed, { output, errorOutput });
    }
    if (parsed.command === 'uninstall') {
      const { uninstall } = await import('../setup/uninstall.mjs');
      return await uninstall({ ...parsed, output, errorOutput });
    }
  } catch (error) {
    errorOutput.write(`loam: ${error instanceof Error ? error.message : String(error)}\n`);
    return Number.isInteger(error?.exitCode) ? error.exitCode : EXIT_CODES.FAILURE;
  }
}

// Compare real paths: npx invokes this through a node_modules/.bin symlink, so
// process.argv[1] and import.meta.url only match once both are resolved.
const invokedPath = process.argv[1] ? realpathSync(process.argv[1]) : '';
if (invokedPath === fileURLToPath(import.meta.url)) {
  process.exitCode = await main();
}
