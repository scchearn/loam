/**
 * Snapshot store. One validated snapshot at a time, fetched from the local
 * read-only server, plus a Refresh action.
 *
 * The refresh contract (spec: Runtime API and failure contract) is that the
 * server keeps serving the prior snapshot when a refresh fails, so a failure
 * here records an error and leaves `state.snapshot` exactly as it was. Views
 * never lose their render because a refresh went wrong.
 */

export const state = {
  snapshot: null,
  /** Message from the last failed load or refresh, or null. */
  error: null,
  refreshing: false,
};

// The server answers errors as bounded JSON ({error, message}); anything else
// (proxy, truncated body) collapses to the status line.
async function failureMessage(response) {
  try {
    const body = await response.json();
    return [body?.error, body?.message].filter(Boolean).join(': ') || `HTTP ${response.status}`;
  } catch {
    return `HTTP ${response.status}`;
  }
}

async function fetchSnapshot() {
  const response = await fetch('/api/snapshot', { headers: { accept: 'application/json' } });
  if (!response.ok) throw new Error(await failureMessage(response));
  return response.json();
}

/** Initial boot load. On failure the error is recorded and re-thrown to the caller. */
export async function load() {
  try {
    state.snapshot = await fetchSnapshot();
    state.error = null;
  } catch (error) {
    state.error = error.message;
    throw error;
  }
}

/**
 * Run the producer server-side and re-read the snapshot. Returns true when the
 * refresh landed. A failed refresh keeps the previous snapshot in place.
 */
export async function refresh() {
  if (state.refreshing) return false;
  state.refreshing = true;
  state.error = null;
  try {
    const response = await fetch('/api/refresh', { method: 'POST' });
    if (!response.ok) throw new Error(await failureMessage(response));
    state.snapshot = await fetchSnapshot();
    return true;
  } catch (error) {
    state.error = error.message;
    return false;
  } finally {
    state.refreshing = false;
  }
}
