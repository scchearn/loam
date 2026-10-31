import { isAbsolute, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

const OPERATIONS = new Set(['registerIdentity', 'send', 'spawn', 'status', 'cancel', 'resume']);
const REQUIRED_METHODS = ['registerIdentity', 'send', 'subscribe', 'spawn', 'status', 'cancel', 'resume'];

export class CoordinatorUnavailableError extends Error {
  constructor(message) {
    super(message);
    this.name = 'CoordinatorUnavailableError';
  }
}

function moduleSpecifier(modulePath) {
  if (!modulePath) {
    throw new CoordinatorUnavailableError(
      'coordinator is not configured; set LOAM_COORDINATOR_URL and LOAM_COORDINATOR_TOKEN, or LOAM_COORDINATOR_MODULE',
    );
  }
  if (modulePath.startsWith('file://')) return modulePath;
  return pathToFileURL(isAbsolute(modulePath) ? modulePath : resolve(modulePath)).href;
}

function readCoordinator(namespace, modulePath) {
  const candidate = namespace.coordinator ?? namespace.default;
  const value = typeof candidate === 'function' ? candidate() : candidate;
  if (!value || typeof value !== 'object') {
    throw new CoordinatorUnavailableError(
      `coordinator module does not export a ready Coordinator: ${modulePath}`,
    );
  }
  const missing = REQUIRED_METHODS.filter((method) => typeof value[method] !== 'function');
  if (missing.length > 0) {
    throw new CoordinatorUnavailableError(
      `coordinator module is missing methods: ${missing.join(', ')}`,
    );
  }
  return value;
}

function createHttpCoordinator(baseUrl, token) {
  const parsed = new URL(baseUrl);
  if (parsed.protocol !== 'http:' || !['127.0.0.1', 'localhost', '[::1]'].includes(parsed.hostname)) {
    throw new CoordinatorUnavailableError('coordinator URL must use loopback HTTP');
  }
  if (!token) throw new CoordinatorUnavailableError('LOAM_COORDINATOR_TOKEN is required with LOAM_COORDINATOR_URL');
  const root = baseUrl.replace(/\/$/, '');
  const request = async (operation, input) => {
    const response = await fetch(`${root}/v1/${operation}`, {
      method: 'POST',
      headers: { Authorization: `Bearer ${token}`, 'Content-Type': 'application/json' },
      body: JSON.stringify(input),
      signal: AbortSignal.timeout(30_000),
    });
    let body;
    try {
      body = await response.json();
    } catch {
      throw new CoordinatorUnavailableError(`coordinator returned invalid JSON for ${operation}`);
    }
    if (!response.ok) throw new CoordinatorUnavailableError(body?.error ?? `coordinator returned HTTP ${response.status}`);
    return body;
  };
  return {
    registerIdentity: (input) => request('registerIdentity', input),
    send: (input) => request('send', input),
    subscribe: async () => { throw new CoordinatorUnavailableError('subscribe is managed by the coordinator server'); },
    spawn: (input) => request('spawn', input),
    status: (input) => request('status', input),
    cancel: (input) => request('cancel', input),
    resume: (input) => request('resume', input),
  };
}

export async function loadCoordinator({
  modulePath = process.env.LOAM_COORDINATOR_MODULE,
  baseUrl = process.env.LOAM_COORDINATOR_URL,
  token = process.env.LOAM_COORDINATOR_TOKEN,
} = {}) {
  if (!modulePath && baseUrl) return createHttpCoordinator(baseUrl, token);
  const specifier = moduleSpecifier(modulePath);
  try {
    return readCoordinator(await import(specifier), modulePath);
  } catch (error) {
    if (error instanceof CoordinatorUnavailableError) throw error;
    throw new CoordinatorUnavailableError(
      `could not load coordinator module ${modulePath}: ${error instanceof Error ? error.message : String(error)}`,
    );
  }
}

export async function invokeCoordinator(operation, input, options = {}) {
  if (!OPERATIONS.has(operation)) {
    throw new CoordinatorUnavailableError(`unsupported coordinator operation: ${operation}`);
  }
  const coordinator = options.coordinator ?? await loadCoordinator(options);
  return coordinator[operation](input);
}

export function parseCoordinatorArgs(argv) {
  const args = [...argv];
  const operation = args.shift();
  if (!operation || !OPERATIONS.has(operation)) {
    throw new Error(`usage: loam-coordinator <${[...OPERATIONS].join('|')}> --json <payload>`);
  }

  let payload;
  while (args.length > 0) {
    const flag = args.shift();
    if (flag !== '--json' || args.length === 0) {
      throw new Error('usage: loam-coordinator <operation> --json <payload>');
    }
    if (payload !== undefined) throw new Error('--json may only be supplied once');
    try {
      payload = JSON.parse(args.shift());
    } catch (error) {
      throw new Error(`invalid --json payload: ${error instanceof Error ? error.message : String(error)}`);
    }
  }
  if (payload === undefined) throw new Error('usage: loam-coordinator <operation> --json <payload>');
  return { operation, payload };
}

export async function runCoordinator(argv, { output = process.stdout, modulePath, coordinator } = {}) {
  const { operation, payload } = parseCoordinatorArgs(argv);
  const result = await invokeCoordinator(operation, payload, { modulePath, coordinator });
  output.write(`${JSON.stringify(result)}\n`);
  return result;
}
