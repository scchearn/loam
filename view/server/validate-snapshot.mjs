// Hand-written structural validator for the Loam View snapshot v1 contract
// (view/schema/snapshot-v1.schema.json). No npm JSON-Schema engine is
// vendored for the product, so this module is the runtime validation path;
// it must agree with the .schema.json document field-for-field.

const TIMESTAMP_RE = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?[+-]\d{2}:\d{2}$/;
const SHA256_RE = /^[0-9a-f]{64}$/;

const STATUS_VALUES = new Set(['ready', 'degraded', 'not-configured']);
const CAPABILITY_STATE_VALUES = new Set(['ready', 'absent', 'unavailable', 'degraded', 'unknown']);
const CAPABILITY_KEYS = ['wiki', 'code_graph', 'goals', 'work', 'checkpoints', 'git', 'qmd', 'search_corpus'];
const ARTIFACT_KIND_VALUES = new Set([
  'wiki-index',
  'wiki-schema',
  'topic',
  'entity',
  'concept',
  'analysis',
  'code',
  'checkpoint',
  'goal',
  'spec',
  'plan',
  'guidance',
  'wiki-other',
]);
const RELATIONSHIP_ORIGIN_VALUES = new Set(['explicit', 'derived']);
const EVENT_STRENGTH_VALUES = new Set(['strong', 'source']);
const METRIC_STATE_VALUES = new Set(['ready', 'unknown', 'unavailable']);
const SIGNAL_STATE_VALUES = new Set(['healthy', 'watch', 'critical', 'unknown', 'unavailable']);
const HINT_SEVERITY_VALUES = new Set(['info', 'warn', 'action']);

const TOP_LEVEL_REQUIRED = [
  'profile',
  'schema_version',
  'generated_at',
  'status',
  'workspace',
  'capabilities',
  'artifacts',
  'relationships',
  'events',
  'metrics',
  'signals',
  'hints',
  'probes',
];

function isPlainObject(value) {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function isString(value) {
  return typeof value === 'string';
}

function isNullableString(value) {
  return value === null || isString(value);
}

function isInteger(value) {
  return typeof value === 'number' && Number.isInteger(value);
}

function isNullableInteger(value) {
  return value === null || isInteger(value);
}

function isNullableBoolean(value) {
  return value === null || typeof value === 'boolean';
}

function isNumber(value) {
  return typeof value === 'number' && Number.isFinite(value);
}

function isTimestamp(value) {
  return isString(value) && TIMESTAMP_RE.test(value);
}

function isNullableTimestamp(value) {
  return value === null || isTimestamp(value);
}

function isSha256(value) {
  return isString(value) && SHA256_RE.test(value);
}

function isNullableObjectOrArray(value) {
  return value === null || isPlainObject(value) || Array.isArray(value);
}

class Reporter {
  constructor() {
    this.errors = [];
  }

  fail(path, message) {
    this.errors.push(`${path}: ${message}`);
  }

  requireKeys(obj, path, keys) {
    for (const key of keys) {
      if (!Object.prototype.hasOwnProperty.call(obj, key)) {
        this.fail(path, `missing required key "${key}"`);
      }
    }
  }

  rejectUnknownKeys(obj, path, allowedKeys) {
    const allowed = new Set(allowedKeys);
    for (const key of Object.keys(obj)) {
      if (!allowed.has(key)) {
        this.fail(path, `unexpected key "${key}"`);
      }
    }
  }
}

function validateEvidenceLocation(evidence, path, reporter) {
  if (!isPlainObject(evidence)) {
    reporter.fail(path, 'must be an object');
    return;
  }
  reporter.requireKeys(evidence, path, ['path', 'line', 'section', 'field', 'content_hash']);
  reporter.rejectUnknownKeys(evidence, path, ['path', 'line', 'section', 'field', 'content_hash']);
  if (!isNullableString(evidence.path)) reporter.fail(`${path}.path`, 'must be a string or null');
  if (!isNullableInteger(evidence.line) || (isInteger(evidence.line) && evidence.line < 1)) {
    reporter.fail(`${path}.line`, 'must be an integer >= 1 or null');
  }
  if (!isNullableString(evidence.section)) reporter.fail(`${path}.section`, 'must be a string or null');
  if (!isNullableString(evidence.field)) reporter.fail(`${path}.field`, 'must be a string or null');
  if (evidence.content_hash !== null && !isSha256(evidence.content_hash)) {
    reporter.fail(`${path}.content_hash`, 'must be a lowercase sha256 hex string or null');
  }
}

function validateCapability(capability, path, reporter) {
  if (!isPlainObject(capability)) {
    reporter.fail(path, 'must be an object');
    return;
  }
  const keys = ['state', 'required', 'reason', 'evidence'];
  reporter.requireKeys(capability, path, keys);
  reporter.rejectUnknownKeys(capability, path, keys);
  if (!CAPABILITY_STATE_VALUES.has(capability.state)) {
    reporter.fail(`${path}.state`, `must be one of ${[...CAPABILITY_STATE_VALUES].join(', ')}`);
  }
  if (typeof capability.required !== 'boolean') reporter.fail(`${path}.required`, 'must be a boolean');
  if (!isNullableString(capability.reason)) reporter.fail(`${path}.reason`, 'must be a string or null');
  if (!(capability.evidence === null || isPlainObject(capability.evidence))) {
    reporter.fail(`${path}.evidence`, 'must be an object or null');
  }
}

function validateArtifact(artifact, path, reporter) {
  if (!isPlainObject(artifact)) {
    reporter.fail(path, 'must be an object');
    return;
  }
  const keys = [
    'id',
    'path',
    'kind',
    'title',
    'lifecycle_status',
    'created_at',
    'updated_at',
    'captured_at',
    'content_hash',
    'bytes',
    'attributes',
    'parse_errors',
  ];
  reporter.requireKeys(artifact, path, keys);
  reporter.rejectUnknownKeys(artifact, path, keys);
  if (!isString(artifact.id) || artifact.id.length === 0) reporter.fail(`${path}.id`, 'must be a non-empty string');
  if (!isString(artifact.path) || artifact.path.length === 0) {
    reporter.fail(`${path}.path`, 'must be a non-empty string');
  }
  if (!ARTIFACT_KIND_VALUES.has(artifact.kind)) {
    reporter.fail(`${path}.kind`, `must be one of ${[...ARTIFACT_KIND_VALUES].join(', ')}`);
  }
  if (!isNullableString(artifact.title)) reporter.fail(`${path}.title`, 'must be a string or null');
  if (!isNullableString(artifact.lifecycle_status)) {
    reporter.fail(`${path}.lifecycle_status`, 'must be a string or null');
  }
  if (!isNullableTimestamp(artifact.created_at)) reporter.fail(`${path}.created_at`, 'must be an RFC3339 timestamp with a numeric offset, or null');
  if (!isNullableTimestamp(artifact.updated_at)) reporter.fail(`${path}.updated_at`, 'must be an RFC3339 timestamp with a numeric offset, or null');
  if (!isNullableTimestamp(artifact.captured_at)) reporter.fail(`${path}.captured_at`, 'must be an RFC3339 timestamp with a numeric offset, or null');
  if (!isSha256(artifact.content_hash)) reporter.fail(`${path}.content_hash`, 'must be a lowercase sha256 hex string');
  if (!isInteger(artifact.bytes) || artifact.bytes < 0) reporter.fail(`${path}.bytes`, 'must be an integer >= 0');
  if (!isPlainObject(artifact.attributes)) reporter.fail(`${path}.attributes`, 'must be an object');
  if (!Array.isArray(artifact.parse_errors) || !artifact.parse_errors.every(isString)) {
    reporter.fail(`${path}.parse_errors`, 'must be an array of strings');
  }
}

function validateRelationship(relationship, path, reporter) {
  if (!isPlainObject(relationship)) {
    reporter.fail(path, 'must be an object');
    return;
  }
  const keys = ['id', 'from', 'to', 'kind', 'origin', 'evidence', 'rule'];
  reporter.requireKeys(relationship, path, keys);
  reporter.rejectUnknownKeys(relationship, path, keys);
  if (!isSha256(relationship.id)) reporter.fail(`${path}.id`, 'must be a lowercase sha256 hex string');
  if (!isString(relationship.from) || relationship.from.length === 0) reporter.fail(`${path}.from`, 'must be a non-empty string');
  if (!isString(relationship.to) || relationship.to.length === 0) reporter.fail(`${path}.to`, 'must be a non-empty string');
  if (!isString(relationship.kind) || relationship.kind.length === 0) reporter.fail(`${path}.kind`, 'must be a non-empty string');
  if (!RELATIONSHIP_ORIGIN_VALUES.has(relationship.origin)) {
    reporter.fail(`${path}.origin`, `must be one of ${[...RELATIONSHIP_ORIGIN_VALUES].join(', ')}`);
  }
  validateEvidenceLocation(relationship.evidence, `${path}.evidence`, reporter);
  if (relationship.rule !== null) {
    if (!isPlainObject(relationship.rule)) {
      reporter.fail(`${path}.rule`, 'must be an object or null');
    } else {
      const ruleKeys = ['id', 'version', 'generated_at', 'confidence'];
      reporter.requireKeys(relationship.rule, `${path}.rule`, ruleKeys);
      reporter.rejectUnknownKeys(relationship.rule, `${path}.rule`, ruleKeys);
      if (!isString(relationship.rule.id) || relationship.rule.id.length === 0) {
        reporter.fail(`${path}.rule.id`, 'must be a non-empty string');
      }
      if (!isString(relationship.rule.version) || relationship.rule.version.length === 0) {
        reporter.fail(`${path}.rule.version`, 'must be a non-empty string');
      }
      if (!isTimestamp(relationship.rule.generated_at)) {
        reporter.fail(`${path}.rule.generated_at`, 'must be an RFC3339 timestamp with a numeric offset');
      }
      if (!isNumber(relationship.rule.confidence) || relationship.rule.confidence < 0 || relationship.rule.confidence > 1) {
        reporter.fail(`${path}.rule.confidence`, 'must be a number between 0 and 1');
      }
    }
  }
}

function validateEvent(event, path, reporter) {
  if (!isPlainObject(event)) {
    reporter.fail(path, 'must be an object');
    return;
  }
  const keys = ['id', 'occurred_at', 'kind', 'title', 'artifact_id', 'strength', 'evidence'];
  reporter.requireKeys(event, path, keys);
  reporter.rejectUnknownKeys(event, path, keys);
  if (!isString(event.id) || event.id.length === 0) reporter.fail(`${path}.id`, 'must be a non-empty string');
  if (!isTimestamp(event.occurred_at)) reporter.fail(`${path}.occurred_at`, 'must be an RFC3339 timestamp with a numeric offset');
  if (!isString(event.kind) || event.kind.length === 0) reporter.fail(`${path}.kind`, 'must be a non-empty string');
  if (!isString(event.title) || event.title.length === 0) reporter.fail(`${path}.title`, 'must be a non-empty string');
  if (!isNullableString(event.artifact_id)) reporter.fail(`${path}.artifact_id`, 'must be a string or null');
  if (!EVENT_STRENGTH_VALUES.has(event.strength)) {
    reporter.fail(`${path}.strength`, `must be one of ${[...EVENT_STRENGTH_VALUES].join(', ')}`);
  }
  validateEvidenceLocation(event.evidence, `${path}.evidence`, reporter);
}

function validateMetric(metric, path, reporter) {
  if (!isPlainObject(metric)) {
    reporter.fail(path, 'must be an object');
    return;
  }
  const keys = ['value', 'unit', 'state', 'evidence'];
  reporter.requireKeys(metric, path, keys);
  reporter.rejectUnknownKeys(metric, path, keys);
  const value = metric.value;
  const validValueType = value === null || typeof value === 'number' || typeof value === 'string' || typeof value === 'boolean';
  if (!validValueType) reporter.fail(`${path}.value`, 'must be a number, string, boolean, or null');
  if (!isNullableString(metric.unit)) reporter.fail(`${path}.unit`, 'must be a string or null');
  if (!METRIC_STATE_VALUES.has(metric.state)) {
    reporter.fail(`${path}.state`, `must be one of ${[...METRIC_STATE_VALUES].join(', ')}`);
  }
  if (!isNullableObjectOrArray(metric.evidence)) reporter.fail(`${path}.evidence`, 'must be an object, array, or null');
}

function validateSignal(signal, path, reporter) {
  if (!isPlainObject(signal)) {
    reporter.fail(path, 'must be an object');
    return;
  }
  const keys = ['id', 'state', 'message', 'evidence', 'command'];
  reporter.requireKeys(signal, path, keys);
  reporter.rejectUnknownKeys(signal, path, keys);
  if (!isString(signal.id) || signal.id.length === 0) reporter.fail(`${path}.id`, 'must be a non-empty string');
  if (!SIGNAL_STATE_VALUES.has(signal.state)) {
    reporter.fail(`${path}.state`, `must be one of ${[...SIGNAL_STATE_VALUES].join(', ')}`);
  }
  if (!isString(signal.message)) reporter.fail(`${path}.message`, 'must be a string');
  if (!isNullableObjectOrArray(signal.evidence)) reporter.fail(`${path}.evidence`, 'must be an object, array, or null');
  if (!isNullableString(signal.command)) reporter.fail(`${path}.command`, 'must be a string or null');
}

function validateHint(hint, path, reporter) {
  if (!isPlainObject(hint)) {
    reporter.fail(path, 'must be an object');
    return;
  }
  const keys = ['kind', 'group', 'severity', 'message', 'command', 'evidence'];
  reporter.requireKeys(hint, path, keys);
  reporter.rejectUnknownKeys(hint, path, keys);
  if (!isString(hint.kind) || hint.kind.length === 0) reporter.fail(`${path}.kind`, 'must be a non-empty string');
  if (!isString(hint.group) || hint.group.length === 0) reporter.fail(`${path}.group`, 'must be a non-empty string');
  if (!HINT_SEVERITY_VALUES.has(hint.severity)) {
    reporter.fail(`${path}.severity`, `must be one of ${[...HINT_SEVERITY_VALUES].join(', ')}`);
  }
  if (!isString(hint.message) || hint.message.length === 0) reporter.fail(`${path}.message`, 'must be a non-empty string');
  if (!isNullableString(hint.command)) reporter.fail(`${path}.command`, 'must be a string or null');
  if (!isNullableObjectOrArray(hint.evidence)) reporter.fail(`${path}.evidence`, 'must be an object, array, or null');
}

function validateProbe(probe, path, reporter) {
  if (!isPlainObject(probe)) {
    reporter.fail(path, 'must be an object');
    return;
  }
  const keys = ['id', 'state', 'duration_ms', 'message'];
  reporter.requireKeys(probe, path, keys);
  reporter.rejectUnknownKeys(probe, path, keys);
  if (!isString(probe.id) || probe.id.length === 0) reporter.fail(`${path}.id`, 'must be a non-empty string');
  if (!isString(probe.state) || probe.state.length === 0) reporter.fail(`${path}.state`, 'must be a non-empty string');
  if (!isNumber(probe.duration_ms) || probe.duration_ms < 0) reporter.fail(`${path}.duration_ms`, 'must be a number >= 0');
  if (!(probe.message === null || (isString(probe.message) && probe.message.length <= 500))) {
    reporter.fail(`${path}.message`, 'must be a string of at most 500 characters, or null');
  }
}

function validateArray(array, path, reporter, itemValidator) {
  if (!Array.isArray(array)) {
    reporter.fail(path, 'must be an array');
    return;
  }
  array.forEach((item, index) => itemValidator(item, `${path}[${index}]`, reporter));
}

/**
 * Structurally validate a parsed Loam View snapshot v1 document against the
 * contract in view/schema/snapshot-v1.schema.json.
 *
 * @param {unknown} snapshot
 * @returns {{valid: boolean, errors: string[]}}
 */
export function validateSnapshot(snapshot) {
  const reporter = new Reporter();

  if (!isPlainObject(snapshot)) {
    reporter.fail('$', 'snapshot must be a JSON object');
    return { valid: false, errors: reporter.errors };
  }

  reporter.requireKeys(snapshot, '$', TOP_LEVEL_REQUIRED);
  reporter.rejectUnknownKeys(snapshot, '$', TOP_LEVEL_REQUIRED);

  if (snapshot.profile !== 'loam-view') {
    reporter.fail('$.profile', 'must be the literal string "loam-view"');
  }

  if (snapshot.schema_version !== 1) {
    reporter.fail('$.schema_version', 'unsupported schema_version: only 1 is recognized by this validator');
  }

  if (!isTimestamp(snapshot.generated_at)) {
    reporter.fail('$.generated_at', 'must be an RFC3339 timestamp with a numeric offset');
  }

  if (!STATUS_VALUES.has(snapshot.status)) {
    reporter.fail('$.status', `must be one of ${[...STATUS_VALUES].join(', ')}`);
  }

  if (!isPlainObject(snapshot.workspace)) {
    reporter.fail('$.workspace', 'must be an object');
  } else {
    const workspace = snapshot.workspace;
    reporter.requireKeys(workspace, '$.workspace', ['root', 'name', 'platform', 'git']);
    reporter.rejectUnknownKeys(workspace, '$.workspace', ['root', 'name', 'platform', 'git']);
    if (!isString(workspace.root) || workspace.root.length === 0) reporter.fail('$.workspace.root', 'must be a non-empty string');
    if (!isString(workspace.name) || workspace.name.length === 0) reporter.fail('$.workspace.name', 'must be a non-empty string');
    if (!isString(workspace.platform) || workspace.platform.length === 0) {
      reporter.fail('$.workspace.platform', 'must be a non-empty string');
    }
    if (!isPlainObject(workspace.git)) {
      reporter.fail('$.workspace.git', 'must be an object');
    } else {
      const git = workspace.git;
      reporter.requireKeys(git, '$.workspace.git', ['state', 'branch', 'dirty', 'changed_count']);
      reporter.rejectUnknownKeys(git, '$.workspace.git', ['state', 'branch', 'dirty', 'changed_count']);
      if (!isString(git.state) || git.state.length === 0) reporter.fail('$.workspace.git.state', 'must be a non-empty string');
      if (!isNullableString(git.branch)) reporter.fail('$.workspace.git.branch', 'must be a string or null');
      if (!isNullableBoolean(git.dirty)) reporter.fail('$.workspace.git.dirty', 'must be a boolean or null');
      if (!isNullableInteger(git.changed_count) || (isInteger(git.changed_count) && git.changed_count < 0)) {
        reporter.fail('$.workspace.git.changed_count', 'must be an integer >= 0 or null');
      }
    }
  }

  if (!isPlainObject(snapshot.capabilities)) {
    reporter.fail('$.capabilities', 'must be an object');
  } else {
    reporter.requireKeys(snapshot.capabilities, '$.capabilities', CAPABILITY_KEYS);
    reporter.rejectUnknownKeys(snapshot.capabilities, '$.capabilities', CAPABILITY_KEYS);
    for (const key of CAPABILITY_KEYS) {
      if (Object.prototype.hasOwnProperty.call(snapshot.capabilities, key)) {
        validateCapability(snapshot.capabilities[key], `$.capabilities.${key}`, reporter);
      }
    }
  }

  validateArray(snapshot.artifacts, '$.artifacts', reporter, validateArtifact);
  validateArray(snapshot.relationships, '$.relationships', reporter, validateRelationship);
  validateArray(snapshot.events, '$.events', reporter, validateEvent);

  if (!isPlainObject(snapshot.metrics)) {
    reporter.fail('$.metrics', 'must be an object');
  } else {
    for (const [key, value] of Object.entries(snapshot.metrics)) {
      validateMetric(value, `$.metrics.${key}`, reporter);
    }
  }

  validateArray(snapshot.signals, '$.signals', reporter, validateSignal);
  validateArray(snapshot.hints, '$.hints', reporter, validateHint);
  validateArray(snapshot.probes, '$.probes', reporter, validateProbe);

  return { valid: reporter.errors.length === 0, errors: reporter.errors };
}
