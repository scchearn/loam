import { readFile, rm } from 'node:fs/promises';
import { join } from 'node:path';

import { publishJson } from '../atomic.mjs';

// The loam-owned integration ledger, under the global root next to install.json.
// It is the ownership source of truth: an MCP entry or tool is loam-owned iff it
// is recorded here. This mirrors the hook ownership model (an entry is loam's
// only when loam recorded it), so disable/uninstall removes exactly what loam
// created and never a user-owned MCP entry or a pre-existing tool.
//
// Shape: { integrations: { <id>: { mcp: { <harness>: <name> }, tool: { managed, pkg, path } | null } } }

function ledgerPath(globalRoot) {
  return join(globalRoot, 'integrations.json');
}

export async function readLedger(globalRoot) {
  try {
    const value = JSON.parse(await readFile(ledgerPath(globalRoot), 'utf8'));
    if (value && typeof value === 'object' && value.integrations) return value;
  } catch {}
  return { integrations: {} };
}

export async function recordIntegration(globalRoot, id, record) {
  const ledger = await readLedger(globalRoot);
  ledger.integrations[id] = record;
  await publishJson({ filePath: ledgerPath(globalRoot), value: ledger });
  return ledger;
}

export async function clearIntegration(globalRoot, id) {
  const ledger = await readLedger(globalRoot);
  if (!(id in ledger.integrations)) return ledger;
  delete ledger.integrations[id];
  if (Object.keys(ledger.integrations).length === 0) {
    await rm(ledgerPath(globalRoot), { force: true });
  } else {
    await publishJson({ filePath: ledgerPath(globalRoot), value: ledger });
  }
  return ledger;
}

export function ownedRecord(ledger, id) {
  return ledger.integrations?.[id] || null;
}
