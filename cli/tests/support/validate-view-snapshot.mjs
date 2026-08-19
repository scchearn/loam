// Reads a Loam View snapshot as JSON on stdin and validates it against the
// checked-in contract validator (view/server/validate-snapshot.mjs, T2's
// deliverable). Exits 0 and prints nothing on success; exits 1 and prints
// the validator's errors, one per line, on failure. Used as an optional
// schema round-trip from cli/tests/view_inventory.rs -- the Rust assertions
// are the suite; this just double-checks against the real validator.
import { validateSnapshot } from '../../../view/server/validate-snapshot.mjs';

const chunks = [];
for await (const chunk of process.stdin) chunks.push(chunk);
const snapshot = JSON.parse(Buffer.concat(chunks).toString('utf8'));

const { valid, errors } = validateSnapshot(snapshot);
if (!valid) {
  for (const error of errors) console.error(error);
  process.exit(1);
}
