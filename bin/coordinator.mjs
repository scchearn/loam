#!/usr/bin/env node

import { realpathSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

import { runCoordinator } from '../integration/coordinator.mjs';

export async function main(argv = process.argv.slice(2), output = process.stdout, errorOutput = process.stderr) {
  try {
    await runCoordinator(argv, { output });
    return 0;
  } catch (error) {
    errorOutput.write(`loam-coordinator: ${error instanceof Error ? error.message : String(error)}\n`);
    return 1;
  }
}

const invokedPath = process.argv[1] ? realpathSync(process.argv[1]) : '';
if (invokedPath === fileURLToPath(import.meta.url)) {
  process.exitCode = await main();
}
