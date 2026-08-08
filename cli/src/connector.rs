//! Slice C connector: the authority-preserving transport seam, its in-memory
//! stub, and the enrollment connection probe.
//!
//! The transport seam is a trait consumed by **generics** (static dispatch) —
//! never a trait object — so the crate's no-dispatch tripwire stays green and no
//! callable capability is introduced. Slice B later supplies a real adapter
//! against the same seam (T13); this module never reaches into Slice B.
//!
//! `AuthenticatedPrincipal` is constructed only inside a transport adapter,
//! after the transport reports an authenticated session. The probe derives every
//! authority-bearing envelope field in trusted code; nothing is caller-supplied.
//!
//! Consumed by the connect orchestration in T9/T10, which retires this
//! module-level allow once the stub and probe are wired to the CLI surface.
#![allow(dead_code)]

use std::time::Duration;

use chrono::{DateTime, Utc};

use crate::envelope::{self, AuthenticatedPrincipal, ValidatedEnvelope, ValidationConfig};

/// What a transport can report and do. Implemented by [`StubTransport`] here and
/// by the real Slice B adapter in T13. Consumed only through generics.
pub trait Transport {
    /// Authenticate the session. On success the adapter learns the canonical
    /// principal and the claims it may assert; only the adapter may turn these
    /// into an [`AuthenticatedPrincipal`].
    fn authenticate(&mut self) -> Result<SessionIdentity, ProbeError>;

    /// Subscribe to a filter and require a successful SUBACK. `no_local` must be
    /// `false` on every filter the probe verifies, because the self-published
    /// echo is the positive receive proof.
    fn subscribe(&mut self, filter: &str, no_local: bool) -> Result<(), ProbeError>;

    /// Publish one validated envelope and require a PUBACK. `retain` must be
    /// `false` for the probe: no retained probe may be left on the broker.
    fn publish(
        &mut self,
        topic: &str,
        envelope: &ValidatedEnvelope,
        retain: bool,
    ) -> Result<(), ProbeError>;

    /// Receive the next frame within the deadline, or `None` on timeout.
    fn receive(&mut self, deadline: Duration) -> Result<Option<ReceivedFrame>, ProbeError>;
}

/// The canonical identity a transport reports after authentication. The adapter
/// maps this into the envelope authority model; the caller cannot supply it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionIdentity {
    pub principal_id: String,
    pub agent_id: String,
    pub instance_id: String,
    pub allowed_claims: Vec<String>,
}

/// One received frame: the wire bytes and the topic it arrived on. The probe
/// re-validates the bytes so "self-receive" means a validated envelope, not just
/// any echo.
#[derive(Debug, Clone)]
pub struct ReceivedFrame {
    pub topic: String,
    pub bytes: Vec<u8>,
}

/// The discrete capabilities the probe observed, with the moment it observed
/// them. This is historical evidence — never a claim of enduring readiness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityEvidence {
    pub authentication: bool,
    pub publish: bool,
    pub subscribe: bool,
    pub self_receive: bool,
    pub verified_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeError {
    AuthenticationFailed,
    SubscribeDenied {
        filter: String,
    },
    PublishDenied,
    /// Self-echo never arrived within the deadline.
    NoSelfReceive,
    /// An echo arrived but was not the exact probe event.
    WrongSelfReceive,
    /// A retained probe was observed — the probe must be non-retained.
    RetainedProbe,
    /// The probe envelope failed its own validation before publication.
    InvalidProbe(String),
    Timeout,
}

impl ProbeError {
    pub fn code(&self) -> &'static str {
        match self {
            ProbeError::AuthenticationFailed => "probe_authentication_failed",
            ProbeError::SubscribeDenied { .. } => "probe_subscribe_denied",
            ProbeError::PublishDenied => "probe_publish_denied",
            ProbeError::NoSelfReceive => "probe_no_self_receive",
            ProbeError::WrongSelfReceive => "probe_wrong_self_receive",
            ProbeError::RetainedProbe => "probe_retained",
            ProbeError::InvalidProbe(_) => "probe_invalid_envelope",
            ProbeError::Timeout => "probe_timeout",
        }
    }
}

/// The non-secret inputs the connector supplies to build the probe. Every field
/// is derived by the connector from the validated enrollment, never by a caller.
#[derive(Debug, Clone)]
pub struct ProbeContext {
    pub org_id: String,
    pub project_id: String,
    pub repository_id: String,
    pub base_oid: String,
    pub plan_oid: String,
}

/// The filters the probe subscribes to before publishing. Kept explicit so the
/// SUBACK requirement is auditable. `{origin}` is the connector's own instance.
fn required_filters(context: &ProbeContext, identity: &SessionIdentity) -> Vec<String> {
    let base = format!("loam/v1/{}/{}", context.org_id, context.project_id);
    vec![
        format!("{base}/event/{}", identity.instance_id),
        format!("{base}/state/{}/+", identity.instance_id),
        // The connector's own typed inbox (both kind and id bound), so the
        // enrollment proves it can receive direct messages, not only events.
        format!("{base}/inbox/instance/{}/+/+", identity.instance_id),
    ]
}

/// The event topic the probe publishes on: `{origin}` binds to `instance_id`.
fn probe_topic(context: &ProbeContext, identity: &SessionIdentity) -> String {
    format!(
        "loam/v1/{}/{}/event/{}",
        context.org_id, context.project_id, identity.instance_id
    )
}

/// A unique, envelope-legal probe id derived from time and the instance.
fn probe_id(identity: &SessionIdentity, now: DateTime<Utc>) -> String {
    // Uppercase alphanumeric, ULID-shaped enough for the envelope's id rule.
    format!(
        "01{:012X}{}",
        now.timestamp_millis() & 0xFFFF_FFFF_FFFF,
        short_suffix(&identity.instance_id)
    )
}

fn short_suffix(instance: &str) -> String {
    instance
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .rev()
        .take(2)
        .collect::<String>()
        .to_ascii_uppercase()
}

/// Serialize the `federation.connection-probe` envelope as an `io.loam.message`,
/// `intent=inform`, `delivery.class=event`, project-recipient, no body, with a
/// summary that says it is a capability probe rather than enrollment/readiness.
///
/// Built as JSON directly (never touching the merged Slice A envelope module) so
/// the connector owns nothing but data. Every value here is derived by the
/// connector in trusted code; none is caller-supplied. The shape mirrors the
/// event-class exemplar so it passes Slice A's structural, identity, topic,
/// anchor, and context-inventory validators, which `run_probe` re-checks.
fn probe_envelope_json(
    context: &ProbeContext,
    identity: &SessionIdentity,
    id: &str,
    now: DateTime<Utc>,
) -> String {
    let time = now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let expires =
        (now + chrono::Duration::seconds(60)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let outer = crate::json::Value::Object(vec![
        ("specversion".into(), json_str("1.0")),
        ("id".into(), json_str(id)),
        (
            "source".into(),
            json_str(&format!("urn:loam:instance:{}", identity.instance_id)),
        ),
        ("type".into(), json_str("io.loam.message")),
        ("time".into(), json_str(&time)),
        ("datacontenttype".into(), json_str("application/json")),
        ("dataschema".into(), json_str("urn:loam:schema:message:1")),
        (
            "data".into(),
            crate::json::Value::Object(vec![
                ("intent".into(), json_str("inform")),
                (
                    "from".into(),
                    crate::json::Value::Object(vec![
                        ("principal_id".into(), json_str(&identity.principal_id)),
                        ("agent_id".into(), json_str(&identity.agent_id)),
                        ("instance_id".into(), json_str(&identity.instance_id)),
                    ]),
                ),
                (
                    "to".into(),
                    crate::json::Value::Array(vec![crate::json::Value::Object(vec![
                        ("kind".into(), json_str("project")),
                        ("id".into(), json_str(&context.project_id)),
                    ])]),
                ),
                (
                    "delivery".into(),
                    crate::json::Value::Object(vec![("class".into(), json_str("event"))]),
                ),
                (
                    "thread".into(),
                    crate::json::Value::Object(vec![
                        ("id".into(), json_str(id)),
                        ("correlation_id".into(), json_str(id)),
                        ("causation_id".into(), crate::json::Value::Null),
                    ]),
                ),
                (
                    "context".into(),
                    crate::json::Value::Object(vec![
                        ("org_id".into(), json_str(&context.org_id)),
                        ("project_id".into(), json_str(&context.project_id)),
                        ("repository_id".into(), json_str(&context.repository_id)),
                        (
                            "git".into(),
                            crate::json::Value::Object(vec![
                                ("base_oid".into(), json_str(&context.base_oid)),
                                ("plan_oid".into(), json_str(&context.plan_oid)),
                            ]),
                        ),
                        ("artifacts".into(), crate::json::Value::Array(vec![])),
                    ]),
                ),
                ("expires_at".into(), json_str(&expires)),
                (
                    "summary".into(),
                    json_str("Federation connection probe: a capability check, not enrollment or readiness."),
                ),
                (
                    "payload".into(),
                    crate::json::Value::Object(vec![
                        ("action".into(), json_str("federation.connection-probe")),
                        (
                            "params".into(),
                            crate::json::Value::Object(vec![]),
                        ),
                        ("response_status".into(), crate::json::Value::Null),
                    ]),
                ),
            ]),
        ),
    ]);
    outer.to_json()
}

fn json_str(value: &str) -> crate::json::Value {
    crate::json::Value::String(value.to_owned())
}

/// Run the enrollment probe against a transport: authenticate, subscribe-first
/// with No Local unset and required SUBACKs, publish one unique non-retained
/// validated probe, and require the exact validated self-event within the
/// deadline. Returns the four discrete capabilities observed.
pub fn run_probe<T: Transport>(
    transport: &mut T,
    context: &ProbeContext,
    config: &ValidationConfig,
    deadline: Duration,
    now: DateTime<Utc>,
) -> Result<CapabilityEvidence, ProbeError> {
    // 1. Authenticate; the adapter alone learns the canonical principal.
    let identity = transport.authenticate()?;

    // 2. Subscribe first, No Local unset, require every SUBACK.
    for filter in required_filters(context, &identity) {
        transport.subscribe(&filter, false)?;
    }

    // 3. Build and validate the probe in trusted code before publishing.
    let id = probe_id(&identity, now);
    let json = probe_envelope_json(context, &identity, &id, now);
    let topic = probe_topic(context, &identity);
    let claims: Vec<&str> = identity.allowed_claims.iter().map(String::as_str).collect();
    let principal = AuthenticatedPrincipal::new(&identity.principal_id, &claims);
    let validated = envelope::validate(json.as_bytes(), &topic, &principal, config, now)
        .map_err(|violation| ProbeError::InvalidProbe(format!("{violation:?}")))?;

    // 4. Publish non-retained, require PUBACK.
    transport.publish(&topic, &validated, false)?;

    // 5. Require the exact validated self-event within the deadline.
    let frame = transport
        .receive(deadline)?
        .ok_or(ProbeError::NoSelfReceive)?;
    let echoed = envelope::validate(&frame.bytes, &frame.topic, &principal, config, now)
        .map_err(|_| ProbeError::WrongSelfReceive)?;
    if echoed.as_envelope().id != id {
        return Err(ProbeError::WrongSelfReceive);
    }

    Ok(CapabilityEvidence {
        authentication: true,
        publish: true,
        subscribe: true,
        self_receive: true,
        verified_at: now,
    })
}

// ---------------------------------------------------------------------------
// In-memory stub
// ---------------------------------------------------------------------------

/// A deterministic in-memory transport for exercising the probe without a
/// broker. It echoes a non-retained published event back on the matching
/// subscription (No Local unset), and can be configured to inject each failure.
#[derive(Debug, Default)]
pub struct StubTransport {
    pub identity: Option<SessionIdentity>,
    pub deny_auth: bool,
    pub deny_subscribe: bool,
    pub deny_publish: bool,
    /// Drop the echo entirely (simulates a broker that cannot self-deliver).
    pub swallow_echo: bool,
    /// Echo a different id (simulates a wrong/foreign delivery).
    pub corrupt_echo: bool,
    /// Never deliver in time (simulates a stalled broker).
    pub stall: bool,
    subscriptions: Vec<String>,
    /// The last non-retained publish, held for echo.
    pending_echo: Option<ReceivedFrame>,
    /// Retained publishes observed — must stay empty for a healthy probe.
    pub retained: Vec<String>,
}

impl StubTransport {
    pub fn healthy(identity: SessionIdentity) -> Self {
        StubTransport {
            identity: Some(identity),
            ..StubTransport::default()
        }
    }

    pub fn subscriptions(&self) -> &[String] {
        &self.subscriptions
    }
}

impl Transport for StubTransport {
    fn authenticate(&mut self) -> Result<SessionIdentity, ProbeError> {
        if self.deny_auth {
            return Err(ProbeError::AuthenticationFailed);
        }
        self.identity
            .clone()
            .ok_or(ProbeError::AuthenticationFailed)
    }

    fn subscribe(&mut self, filter: &str, no_local: bool) -> Result<(), ProbeError> {
        // The probe must never set No Local on a verified filter.
        assert!(!no_local, "probe must subscribe with No Local unset");
        if self.deny_subscribe {
            return Err(ProbeError::SubscribeDenied {
                filter: filter.to_owned(),
            });
        }
        self.subscriptions.push(filter.to_owned());
        Ok(())
    }

    fn publish(
        &mut self,
        topic: &str,
        envelope: &ValidatedEnvelope,
        retain: bool,
    ) -> Result<(), ProbeError> {
        if self.deny_publish {
            return Err(ProbeError::PublishDenied);
        }
        if retain {
            // A healthy probe is non-retained; record the violation so a test can
            // observe a positive retained sentinel.
            self.retained.push(topic.to_owned());
        }
        let bytes = envelope.as_envelope().to_json().into_bytes();
        if !self.swallow_echo {
            let bytes = if self.corrupt_echo {
                corrupt_id(&bytes)
            } else {
                bytes
            };
            self.pending_echo = Some(ReceivedFrame {
                topic: topic.to_owned(),
                bytes,
            });
        }
        Ok(())
    }

    fn receive(&mut self, _deadline: Duration) -> Result<Option<ReceivedFrame>, ProbeError> {
        if self.stall {
            return Ok(None);
        }
        Ok(self.pending_echo.take())
    }
}

/// Flip one hex digit of the CloudEvents `id` so the echo re-validates but has a
/// different id — the "wrong self-receive" case.
fn corrupt_id(bytes: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(bytes);
    if let Some(value) = crate::json::parse(&text)
        .ok()
        .and_then(|v| v.get("id").and_then(|id| id.as_str()).map(str::to_owned))
    {
        let mut chars: Vec<char> = value.chars().collect();
        if let Some(last) = chars.last_mut() {
            *last = if *last == 'A' { 'B' } else { 'A' };
        }
        let replaced: String = chars.into_iter().collect();
        return text.replacen(&value, &replaced, 1).into_bytes();
    }
    bytes.to_vec()
}

// ---------------------------------------------------------------------------
// Inert-by-default connector service loop (T9)
// ---------------------------------------------------------------------------
//
// One per-user connector hosts every enrolled project. The empty registry is the
// desired-state switch: reconciliation runs *before* any endpoint or transport,
// so a missing or empty registry means no socket, no process footprint, and no
// network. Each request crosses two boundaries in order — the kernel peer check
// (owner-only), then a registry workspace/project-binding resolution — before a
// closed operation is dispatched. There is no generic dispatch.

use std::path::Path;

use crate::ipc::{self, IpcConfig, Operation, Request};

/// Whether the service found work to host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceOutcome {
    /// No enrollment exists: no endpoint was created and no network occurred.
    Inert,
    /// The endpoint was bound and the accept loop ran.
    Served,
}

#[derive(Debug)]
pub enum ServiceError {
    Registry(crate::enrollment::RegistryError),
    Ipc(ipc::IpcError),
}

/// A volatile, in-memory per-session inject-channel registry (2026-08-08
/// amendment, T18). Held only for the life of one connector process: a restart
/// drops every channel, and nothing here is ever written to the SQLite registry.
/// Injection over a channel is Slice E; Slice C only admits, holds, hands back,
/// and drops it.
#[derive(Debug, Default)]
pub struct ChannelRegistry {
    sessions: std::collections::HashMap<String, InjectChannel>,
}

/// One registered inject channel. `channel_ref` is opaque: the plugin hands it
/// over and the connector holds it without interpreting it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InjectChannel {
    pub session_id: String,
    pub project_id: String,
    pub channel_ref: String,
}

impl ChannelRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, channel: InjectChannel) {
        self.sessions.insert(channel.session_id.clone(), channel);
    }

    pub fn drop_session(&mut self, session_id: &str) -> bool {
        self.sessions.remove(session_id).is_some()
    }

    pub fn contains(&self, session_id: &str) -> bool {
        self.sessions.contains_key(session_id)
    }

    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }
}

/// Run the connector. Reconciles the registry before touching an endpoint: a
/// missing database or an empty registry returns [`ServiceOutcome::Inert`]
/// without binding a socket. Only a non-empty registry binds the owner-only
/// endpoint and serves.
#[cfg(unix)]
pub fn run_service(global_root: &Path) -> Result<ServiceOutcome, ServiceError> {
    let db_path = global_root.join("loam.sqlite3");
    if !registry_has_enrollments(&db_path)? {
        return Ok(ServiceOutcome::Inert);
    }
    let run_dir = global_root.join("run");
    let endpoint = ipc::unix::bind(&run_dir).map_err(ServiceError::Ipc)?;
    // The channel registry lives only for this process; a restart starts empty.
    let mut channels = ChannelRegistry::new();
    accept_loop(&endpoint, &db_path, &mut channels);
    Ok(ServiceOutcome::Served)
}

fn registry_has_enrollments(db_path: &Path) -> Result<bool, ServiceError> {
    match crate::enrollment::open_readonly(db_path).map_err(ServiceError::Registry)? {
        None => Ok(false),
        Some(connection) => Ok(!crate::enrollment::list_enrollments(&connection)
            .map_err(ServiceError::Registry)?
            .is_empty()),
    }
}

#[cfg(unix)]
fn accept_loop(
    endpoint: &ipc::unix::OwnedEndpoint,
    db_path: &Path,
    channels: &mut ChannelRegistry,
) {
    let config = IpcConfig::default();
    loop {
        // One failed connection never takes the connector down; keep serving.
        let _ = serve_one(endpoint, db_path, &config, channels);
    }
}

/// Serve exactly one connection: prove the peer (inside `accept_verified`),
/// read one bounded frame, dispatch through the registry, and write one
/// response. Exposed for tests.
#[cfg(unix)]
pub fn serve_one(
    endpoint: &ipc::unix::OwnedEndpoint,
    db_path: &Path,
    config: &IpcConfig,
    channels: &mut ChannelRegistry,
) -> Result<(), ipc::IpcError> {
    let mut connection = endpoint.accept_verified()?;
    let frame = ipc::read_frame(&mut connection, config)?;
    let response = match ipc::parse_request(&frame, config) {
        Ok(request) => dispatch(&request, db_path, config, channels),
        Err(error) => ipc::error_response("", &error, config),
    };
    ipc::write_frame(&mut connection, &response, config)
}

/// Resolve the request's workspace through the registry, enforce the project
/// binding, and run the closed operation. Returns an encoded response body.
fn dispatch(
    request: &Request,
    db_path: &Path,
    config: &IpcConfig,
    channels: &mut ChannelRegistry,
) -> Vec<u8> {
    match resolve_and_run(request, db_path, channels) {
        Ok(result) => ipc::ok_response(&request.request_id, result),
        Err(error) => ipc::error_response(&request.request_id, &error, config),
    }
}

fn resolve_and_run(
    request: &Request,
    db_path: &Path,
    channels: &mut ChannelRegistry,
) -> Result<crate::json::Value, ipc::IpcError> {
    // Resolve the workspace to its physical identity exactly as enrollment did,
    // so a path alias resolves to the same enrollment and a non-workspace path is
    // treated as unenrolled.
    let workspace = crate::enrollment::PhysicalWorkspace::resolve(Path::new(&request.workspace))
        .map_err(|_| ipc::IpcError::WorkspaceUnenrolled)?;
    let key = crate::enrollment::identity_key(&workspace);
    dispatch_for_key(request, &key, db_path, channels)
}

/// Dispatch a request that has already been resolved to a physical identity key.
/// Separated from workspace resolution so the registry-binding and operation
/// logic is testable without a Git workspace.
fn dispatch_for_key(
    request: &Request,
    key: &str,
    db_path: &Path,
    channels: &mut ChannelRegistry,
) -> Result<crate::json::Value, ipc::IpcError> {
    let read = crate::enrollment::open_readonly(db_path)
        .map_err(|_| ipc::IpcError::Internal)?
        .ok_or(ipc::IpcError::WorkspaceUnenrolled)?;
    let row = crate::enrollment::lookup(&read, key)
        .map_err(|_| ipc::IpcError::Internal)?
        .ok_or(ipc::IpcError::WorkspaceUnenrolled)?;

    // Project binding: if the caller names a project, it must match the
    // enrollment's, or the request is rejected before any operation runs.
    if let Some(claimed) = request.payload.get("project_id").and_then(|v| v.as_str()) {
        if claimed != row.project_id {
            return Err(ipc::IpcError::ProjectBindingMismatch);
        }
    }
    drop(read);

    match request.operation {
        Operation::StatusGet => Ok(status_json(&row)),
        Operation::ProjectAttach => {
            // The enrollment already exists (looked up above); the broker session
            // wiring is stubbed until the real adapter (T13). Acknowledge attach.
            Ok(ack_json(&row, "attached"))
        }
        Operation::ProjectDetach => {
            let mut write =
                crate::enrollment::open_writable(db_path).map_err(|_| ipc::IpcError::Internal)?;
            let removed = crate::enrollment::delete_enrollment(&mut write, key)
                .map_err(|_| ipc::IpcError::Internal)?;
            if removed {
                // Any live inject channels for this project become moot; the real
                // per-session drop is driven by Slice E's session end.
                Ok(ack_json(&row, "detached"))
            } else {
                Err(ipc::IpcError::WorkspaceUnenrolled)
            }
        }
        Operation::SessionRegisterInject => {
            // Admit the session's inject channel to the volatile in-memory
            // registry (2026-08-08 amendment). The enrollment + project binding
            // were already proven above. Nothing is written to SQLite; injection
            // over the channel is Slice E.
            let session_id = request
                .payload
                .get("session_id")
                .and_then(|v| v.as_str())
                .ok_or(ipc::IpcError::InvalidRequest)?;
            let channel_ref = request
                .payload
                .get("channel_ref")
                .and_then(|v| v.as_str())
                .ok_or(ipc::IpcError::InvalidRequest)?;
            channels.register(InjectChannel {
                session_id: session_id.to_owned(),
                project_id: row.project_id.clone(),
                channel_ref: channel_ref.to_owned(),
            });
            Ok(register_ack_json(session_id, &row.project_id))
        }
    }
}

fn register_ack_json(session_id: &str, project_id: &str) -> crate::json::Value {
    use crate::json::Value;
    Value::Object(vec![
        ("schema".into(), Value::Number("1".into())),
        (
            "action".into(),
            Value::String("inject-channel-registered".into()),
        ),
        ("session_id".into(), Value::String(session_id.to_owned())),
        ("project_id".into(), Value::String(project_id.to_owned())),
    ])
}

/// A precise, aggregate-free status projection: enrollment, historical verified
/// capabilities, and a broker-session field that is explicitly not-live here (a
/// real session is the adapter's, T13). No `connected`/`ready` boolean.
fn status_json(row: &crate::enrollment::EnrolledRow) -> crate::json::Value {
    use crate::json::Value;
    Value::Object(vec![
        ("schema".into(), Value::Number("1".into())),
        (
            "enrollment".into(),
            Value::Object(vec![
                ("state".into(), Value::String("enrolled".into())),
                ("org_id".into(), Value::String(row.org_id.clone())),
                ("project_id".into(), Value::String(row.project_id.clone())),
                (
                    "repository_id".into(),
                    Value::String(row.repository_id.clone()),
                ),
                (
                    "display_path".into(),
                    Value::String(row.display_path.clone()),
                ),
            ]),
        ),
        (
            "verification".into(),
            Value::Object(vec![
                (
                    "capabilities".into(),
                    Value::Array(capability_names(&row.capabilities)),
                ),
                (
                    "verified_at".into(),
                    Value::String(row.capabilities.verified_at.clone()),
                ),
            ]),
        ),
        (
            "broker".into(),
            Value::Object(vec![(
                "session_state".into(),
                Value::String("not-live-in-connector".into()),
            )]),
        ),
    ])
}

fn capability_names(record: &crate::enrollment::CapabilityRecord) -> Vec<crate::json::Value> {
    let mut names = Vec::new();
    for (present, name) in [
        (record.authentication, "authentication"),
        (record.publish, "publish"),
        (record.subscribe, "subscribe"),
        (record.self_receive, "self_receive"),
    ] {
        if present {
            names.push(crate::json::Value::String(name.to_owned()));
        }
    }
    names
}

fn ack_json(row: &crate::enrollment::EnrolledRow, action: &str) -> crate::json::Value {
    use crate::json::Value;
    Value::Object(vec![
        ("schema".into(), Value::Number("1".into())),
        ("action".into(), Value::String(action.to_owned())),
        ("project_id".into(), Value::String(row.project_id.clone())),
    ])
}

#[cfg(test)]
mod probe_tests {
    use super::*;

    fn identity() -> SessionIdentity {
        SessionIdentity {
            principal_id: "employee-184".into(),
            agent_id: "agent-72".into(),
            instance_id: "instance-01".into(),
            allowed_claims: vec![],
        }
    }

    fn context() -> ProbeContext {
        ProbeContext {
            org_id: "org-3A1".into(),
            project_id: "project-7M3".into(),
            repository_id: "repo-2F8".into(),
            base_oid: "84be000000000000000000000000000000000001".into(),
            plan_oid: "61af000000000000000000000000000000000001".into(),
        }
    }

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-07-24T14:20:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn deadline() -> Duration {
        Duration::from_millis(50)
    }

    #[test]
    fn ordered_success_records_four_capabilities_and_no_retained_probe() {
        let mut transport = StubTransport::healthy(identity());
        let evidence = run_probe(
            &mut transport,
            &context(),
            &ValidationConfig::default(),
            deadline(),
            now(),
        )
        .expect("healthy probe succeeds");
        assert!(
            evidence.authentication
                && evidence.publish
                && evidence.subscribe
                && evidence.self_receive
        );
        // subscribe-before-publish: three required filters subscribed.
        assert_eq!(transport.subscriptions().len(), 3);
        // No retained probe.
        assert!(transport.retained.is_empty());
    }

    #[test]
    fn authentication_failure_is_reported() {
        let mut transport = StubTransport {
            deny_auth: true,
            ..StubTransport::healthy(identity())
        };
        assert_eq!(
            run_probe(
                &mut transport,
                &context(),
                &ValidationConfig::default(),
                deadline(),
                now()
            ),
            Err(ProbeError::AuthenticationFailed)
        );
    }

    #[test]
    fn subscribe_denied_stops_before_publish() {
        let mut transport = StubTransport {
            deny_subscribe: true,
            ..StubTransport::healthy(identity())
        };
        assert!(matches!(
            run_probe(
                &mut transport,
                &context(),
                &ValidationConfig::default(),
                deadline(),
                now()
            ),
            Err(ProbeError::SubscribeDenied { .. })
        ));
    }

    #[test]
    fn publish_denied_is_reported() {
        let mut transport = StubTransport {
            deny_publish: true,
            ..StubTransport::healthy(identity())
        };
        assert_eq!(
            run_probe(
                &mut transport,
                &context(),
                &ValidationConfig::default(),
                deadline(),
                now()
            ),
            Err(ProbeError::PublishDenied)
        );
    }

    #[test]
    fn no_self_receive_is_reported() {
        let mut transport = StubTransport {
            swallow_echo: true,
            ..StubTransport::healthy(identity())
        };
        assert_eq!(
            run_probe(
                &mut transport,
                &context(),
                &ValidationConfig::default(),
                deadline(),
                now()
            ),
            Err(ProbeError::NoSelfReceive)
        );
    }

    #[test]
    fn wrong_self_receive_is_reported() {
        let mut transport = StubTransport {
            corrupt_echo: true,
            ..StubTransport::healthy(identity())
        };
        assert_eq!(
            run_probe(
                &mut transport,
                &context(),
                &ValidationConfig::default(),
                deadline(),
                now()
            ),
            Err(ProbeError::WrongSelfReceive)
        );
    }

    #[test]
    fn stalled_broker_times_out_as_no_self_receive() {
        let mut transport = StubTransport {
            stall: true,
            ..StubTransport::healthy(identity())
        };
        assert_eq!(
            run_probe(
                &mut transport,
                &context(),
                &ValidationConfig::default(),
                deadline(),
                now()
            ),
            Err(ProbeError::NoSelfReceive)
        );
    }

    #[test]
    fn probe_envelope_is_a_valid_non_retained_event_message() {
        // The probe envelope must pass Slice A validation on its event topic.
        let id = probe_id(&identity(), now());
        let json = probe_envelope_json(&context(), &identity(), &id, now());
        let topic = probe_topic(&context(), &identity());
        let principal = AuthenticatedPrincipal::new("employee-184", &[]);
        let validated = envelope::validate(
            json.as_bytes(),
            &topic,
            &principal,
            &ValidationConfig::default(),
            now(),
        )
        .expect("probe envelope validates");
        assert_eq!(validated.as_envelope().id, id);
    }
}

#[cfg(test)]
mod service_tests {
    use super::*;
    use crate::enrollment::{
        CapabilityRecord, PhysicalWorkspace, PlatformIdentity, ValidatedEnrollment, ValidatedRemote,
    };

    fn temp_db(label: &str) -> std::path::PathBuf {
        // Leaked on purpose: connector.rs is not on the filesystem capability
        // allowlist, so these tests never perform any filesystem cleanup here.
        std::env::temp_dir().join(format!(
            "loam-svc-{label}-{}.sqlite3",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn synthetic(device: u64, inode: u64) -> ValidatedEnrollment {
        ValidatedEnrollment {
            org_id: "acme".into(),
            project_id: "loam".into(),
            repository_id: "repo".into(),
            broker_profile: "acme-prod".into(),
            broker_endpoint: "mqtts://h:8883".into(),
            tls_server_name: "h".into(),
            credential_ref: "vault://c".into(),
            ca_ref: None,
            commit: "0123456789abcdef0123456789abcdef01234567".into(),
            remotes: vec![ValidatedRemote {
                name: "origin".into(),
                url_digest: "a".repeat(64),
                allowed_refs: vec!["refs/heads/main".into()],
            }],
            workspace: PhysicalWorkspace {
                display_path: "/w/proj".into(),
                identity: PlatformIdentity::Unix { device, inode },
            },
        }
    }

    fn caps() -> CapabilityRecord {
        CapabilityRecord {
            authentication: true,
            publish: true,
            subscribe: true,
            self_receive: true,
            verified_at: "2026-08-08T10:00:00Z".into(),
        }
    }

    fn enrolled_db(label: &str, device: u64, inode: u64) -> (std::path::PathBuf, String) {
        let path = temp_db(label);
        let mut connection = crate::enrollment::open_writable(&path).unwrap();
        let enrollment = synthetic(device, inode);
        crate::enrollment::insert_enrollment(&mut connection, &enrollment, &caps(), "t").unwrap();
        let key = crate::enrollment::identity_key(&enrollment.workspace);
        (path, key)
    }

    fn request(operation: Operation, payload: crate::json::Value) -> Request {
        Request {
            request_id: "r-1".into(),
            workspace: "/w/proj".into(),
            operation,
            payload,
        }
    }

    #[test]
    fn empty_and_missing_registries_are_inert() {
        // A path with no database: reconciliation finds nothing, no endpoint.
        let missing = temp_db("inert-missing");
        assert!(!registry_has_enrollments(&missing).unwrap());

        // An existing database with zero enrollments is equally inert.
        let empty = temp_db("inert-empty");
        drop(crate::enrollment::open_writable(&empty).unwrap());
        assert!(!registry_has_enrollments(&empty).unwrap());
    }

    #[test]
    fn a_populated_registry_reports_work_to_host() {
        let (path, _key) = enrolled_db("populated", 1, 10);
        assert!(registry_has_enrollments(&path).unwrap());
    }

    #[test]
    fn status_get_returns_an_aggregate_free_projection() {
        let (path, key) = enrolled_db("status", 2, 20);
        let result = dispatch_for_key(
            &request(Operation::StatusGet, crate::json::Value::Object(vec![])),
            &key,
            &path,
            &mut ChannelRegistry::new(),
        )
        .expect("status");
        let text = result.to_json();
        assert!(text.contains("\"enrollment\""));
        assert!(text.contains("\"capabilities\""));
        assert!(text.contains("not-live-in-connector"));
        assert!(!text.contains("\"connected\"") && !text.contains("\"ready\""));
    }

    #[test]
    fn an_unenrolled_workspace_is_rejected() {
        let (path, _key) = enrolled_db("unenrolled", 3, 30);
        let outcome = dispatch_for_key(
            &request(Operation::StatusGet, crate::json::Value::Object(vec![])),
            "unix:999:999",
            &path,
            &mut ChannelRegistry::new(),
        );
        assert_eq!(outcome.err(), Some(ipc::IpcError::WorkspaceUnenrolled));
    }

    #[test]
    fn a_cross_project_binding_is_rejected() {
        let (path, key) = enrolled_db("binding", 4, 40);
        let payload = crate::json::Value::Object(vec![(
            "project_id".into(),
            crate::json::Value::String("some-other-project".into()),
        )]);
        let outcome = dispatch_for_key(
            &request(Operation::StatusGet, payload),
            &key,
            &path,
            &mut ChannelRegistry::new(),
        );
        assert_eq!(outcome.err(), Some(ipc::IpcError::ProjectBindingMismatch));
    }

    #[test]
    fn detach_removes_then_status_is_unenrolled() {
        let (path, key) = enrolled_db("detach", 5, 50);
        dispatch_for_key(
            &request(Operation::ProjectDetach, crate::json::Value::Object(vec![])),
            &key,
            &path,
            &mut ChannelRegistry::new(),
        )
        .expect("detach");
        let after = dispatch_for_key(
            &request(Operation::StatusGet, crate::json::Value::Object(vec![])),
            &key,
            &path,
            &mut ChannelRegistry::new(),
        );
        assert_eq!(after.err(), Some(ipc::IpcError::WorkspaceUnenrolled));
    }

    // --- T18 register-inject + volatile channel registry ---

    fn register_request(session_id: &str, channel_ref: &str) -> Request {
        Request {
            request_id: "r-1".into(),
            workspace: "/w/proj".into(),
            operation: Operation::SessionRegisterInject,
            payload: crate::json::Value::Object(vec![
                (
                    "session_id".into(),
                    crate::json::Value::String(session_id.into()),
                ),
                (
                    "channel_ref".into(),
                    crate::json::Value::String(channel_ref.into()),
                ),
            ]),
        }
    }

    #[test]
    fn register_inject_admits_a_channel_without_persisting() {
        let (path, key) = enrolled_db("register", 6, 60);
        let mut channels = ChannelRegistry::new();
        let result = dispatch_for_key(
            &register_request("sess-1", "chan-token-1"),
            &key,
            &path,
            &mut channels,
        )
        .expect("register");
        assert!(result.to_json().contains("inject-channel-registered"));
        assert!(channels.contains("sess-1"));
        assert_eq!(channels.len(), 1);

        // Nothing about the channel is written to SQLite: no table holds it, and
        // the enrollment row count is unchanged.
        let connection = crate::enrollment::open_readonly(&path).unwrap().unwrap();
        let channel_tables: i64 = connection
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name LIKE '%channel%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(channel_tables, 0, "no channel table may exist in SQLite");
        assert_eq!(
            crate::enrollment::list_enrollments(&connection)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn register_inject_requires_an_enrolled_workspace() {
        let (path, _key) = enrolled_db("register-unenrolled", 7, 70);
        let outcome = dispatch_for_key(
            &register_request("sess-x", "chan"),
            "unix:404:404",
            &path,
            &mut ChannelRegistry::new(),
        );
        assert_eq!(outcome.err(), Some(ipc::IpcError::WorkspaceUnenrolled));
    }

    #[test]
    fn a_channel_is_dropped_on_session_end() {
        let mut channels = ChannelRegistry::new();
        channels.register(InjectChannel {
            session_id: "sess-2".into(),
            project_id: "loam".into(),
            channel_ref: "c".into(),
        });
        assert!(channels.contains("sess-2"));
        assert!(channels.drop_session("sess-2"));
        assert!(!channels.contains("sess-2"));
        assert!(!channels.drop_session("sess-2")); // idempotent
    }

    #[test]
    fn a_restart_starts_with_an_empty_registry() {
        // A fresh registry (a new process) holds nothing — channels never persist.
        let channels = ChannelRegistry::new();
        assert!(channels.is_empty());
    }
}
