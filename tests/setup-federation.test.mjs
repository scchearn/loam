import assert from 'node:assert/strict';
import { test } from 'node:test';

import {
  removeFederationService,
  stageFederationService,
  verifyFederationService,
} from '../setup/federation.mjs';

// The federation lifecycle helper delegates entirely to the private runtime's
// hidden `federation service <verb>` commands; the platform manager specifics
// (systemd/launchd/schtasks) live in the Rust service module and are proven by
// the hosted service-smoke legs (T8). Here we inject a recording runner so the
// delegation contract — verbs, ordering, runtime-path linkage, desired-state
// preservation, rollback, and egress denial — is deterministic and offline.

const ROOT = '/tmp/loam-fed-root';
const RUNTIME_NEW = '/tmp/loam-fed-root/bin/0.9.2/x86_64-unknown-linux-musl/loam';

function recordingRunner(codes = {}) {
  const calls = [];
  const runner = async (request) => {
    calls.push({ runtimePath: request.runtimePath, args: [...request.args] });
    const verb = request.args[2];
    const code = Object.prototype.hasOwnProperty.call(codes, verb) ? codes[verb] : 0;
    return { code, stdout: '', stderr: '' };
  };
  return { runner, calls };
}

function verbs(calls) {
  return calls.map((call) => call.args[2]);
}

test('a fresh machine stages a dormant definition and never enables it', async () => {
  // status exit 1 => not enabled => inert desired state.
  const { runner, calls } = recordingRunner({ status: 1 });
  const result = await stageFederationService({ runtimePath: RUNTIME_NEW, globalRoot: ROOT, runner });

  assert.equal(result.ready, true);
  assert.equal(result.wasActive, false);
  assert.deepEqual(verbs(calls), ['status', 'install']);
  assert.ok(!verbs(calls).includes('enable'), 'a dormant install must not enable the connector');
});

test('an active service is re-enabled on the new runtime across an update', async () => {
  // status exit 0 => was active => preserve desired state after the re-install.
  const { runner, calls } = recordingRunner({ status: 0 });
  const result = await stageFederationService({ runtimePath: RUNTIME_NEW, globalRoot: ROOT, runner });

  assert.equal(result.ready, true);
  assert.equal(result.wasActive, true);
  assert.deepEqual(verbs(calls), ['status', 'install', 'enable']);
  // Runtime-path replacement: every verb targets the new runtime, so the
  // definition references the trusted current runtime.
  for (const call of calls) assert.equal(call.runtimePath, RUNTIME_NEW);
});

test('a failed definition install fails staging with a clean no-op rollback', async () => {
  const { runner, calls } = recordingRunner({ status: 1, install: 1 });
  const result = await stageFederationService({ runtimePath: RUNTIME_NEW, globalRoot: ROOT, runner });

  assert.equal(result.ready, false);
  assert.equal(result.category, 'federation_install_failed');
  assert.deepEqual(verbs(calls), ['status', 'install']);
  // Nothing was created, so rollback issues no manager work.
  await result.rollback();
  assert.deepEqual(verbs(calls), ['status', 'install']);
});

test('a fresh-install rollback removes the newly created definition', async () => {
  const { runner, calls } = recordingRunner({ status: 1 });
  const result = await stageFederationService({ runtimePath: RUNTIME_NEW, globalRoot: ROOT, runner });
  assert.equal(result.ready, true);

  await result.rollback();
  assert.deepEqual(verbs(calls), ['status', 'install', 'uninstall']);
});

test('an update rollback leaves a previously-active service as found', async () => {
  const { runner, calls } = recordingRunner({ status: 0 });
  const result = await stageFederationService({ runtimePath: RUNTIME_NEW, globalRoot: ROOT, runner });
  assert.equal(result.wasActive, true);

  await result.rollback();
  // No uninstall/disable: the active service already existed and its definition
  // now points at the committed new runtime.
  assert.deepEqual(verbs(calls), ['status', 'install', 'enable']);
});

test('staging issues only federation service lifecycle verbs — no network or broker command', async () => {
  const { runner, calls } = recordingRunner({ status: 0 });
  await stageFederationService({ runtimePath: RUNTIME_NEW, globalRoot: ROOT, runner });

  for (const call of calls) {
    assert.deepEqual(
      call.args,
      ['federation', 'service', call.args[2], '--global-root', ROOT],
      'no connect/probe/credential/broker argument may appear',
    );
    // Identity is owned by the runtime; Node never passes an instance identity.
    assert.ok(!call.args.some((arg) => arg === '--instance-id' || arg === 'connect'));
  }
});

test('uninstall disables then removes the definition, and never touches a broker', async () => {
  const { runner, calls } = recordingRunner();
  const result = await removeFederationService({ runtimePath: RUNTIME_NEW, globalRoot: ROOT, runner });

  assert.equal(result.ok, true);
  assert.deepEqual(verbs(calls), ['disable', 'uninstall']);
  for (const call of calls) {
    assert.deepEqual(call.args, ['federation', 'service', call.args[2], '--global-root', ROOT]);
  }
});

test('verifyFederationService accepts a clean disabled status and rejects a runtime crash', async () => {
  const disabled = await verifyFederationService({
    runtimePath: RUNTIME_NEW,
    globalRoot: ROOT,
    runner: async () => ({ code: 1, stdout: '', stderr: 'disabled' }),
  });
  assert.equal(disabled.ready, true);
  assert.equal(disabled.active, false);

  const crashed = await verifyFederationService({
    runtimePath: RUNTIME_NEW,
    globalRoot: ROOT,
    runner: async () => ({ code: null, category: 'process_error', stdout: '', stderr: 'no such file' }),
  });
  assert.equal(crashed.ready, false);
});
