#!/usr/bin/env node

import { resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

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

    const { runSetup } = await import('../setup/main.mjs');
    return await runSetup(parsed, { output, errorOutput });
  } catch (error) {
    errorOutput.write(`loam: ${error instanceof Error ? error.message : String(error)}\n`);
    return Number.isInteger(error?.exitCode) ? error.exitCode : EXIT_CODES.FAILURE;
  }
}

const invokedPath = process.argv[1] ? pathToFileURL(resolve(process.argv[1])).href : '';
if (import.meta.url === invokedPath || fileURLToPath(import.meta.url) === process.argv[1]) {
  process.exitCode = await main();
}
