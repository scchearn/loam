//! Slice C connector: the authority-preserving transport seam, its in-memory
//! stub, and the enrollment connection probe.
//!
//! The transport seam is a trait consumed by **generics** (static dispatch) —
//! never a trait object — so the crate's no-dispatch tripwire stays green and no
//! callable capability is introduced. [`StubTransport`] keeps the probe testable
//! without a broker; [`MqttTransport`] (T13) implements the same seam over Slice
//! B's public `transport` surface and is the only code here that touches it.
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
// Real MQTT adapter over the Slice B transport (T13)
// ---------------------------------------------------------------------------
//
// The only place an accepted broker session becomes an `AuthenticatedPrincipal`:
// no authority exists before the CONNACK, and the caller can never supply one.
// Wire encoding is delegated to Slice B's `transport::publish` (the single
// encoder) and every received frame is admitted by Slice B's `DeliveryProcessor`
// (the single validator/deduplicator). This adapter adds no second encoder and
// no capability of its own: certificate bytes arrive from the caller, so the
// module stays filesystem-free, and rumqttc owns the socket.

use std::time::Instant;

use rumqttc::v5::mqttbytes::v5::{
    ConnectReturnCode, Filter, Packet, PubAckReason, Publish, SubscribeReasonCode,
};
use rumqttc::v5::mqttbytes::QoS;
use rumqttc::v5::{Client, Connection, Event, RecvTimeoutError};

use crate::transport::{AuthenticatedTransportPrincipal, DeliveryProcessor, TransportConfig};

/// How long the adapter waits for one broker acknowledgement (CONNACK, SUBACK,
/// PUBACK). The probe's own receive deadline is passed in separately.
const ACK_TIMEOUT: Duration = Duration::from_secs(10);
const KEEP_ALIVE: Duration = Duration::from_secs(5);
const REQUEST_CAPACITY: usize = 8;
/// Bounded per-class delivery tracking for one probe session.
const TRACKING_CAPACITY: usize = 32;

/// One authenticated broker session's inputs. Secrets live only here, never in
/// an envelope, a registry row, or a report.
pub struct MqttSession {
    /// Slice B's validated broker configuration (endpoint, client id, bounds).
    pub config: TransportConfig,
    pub username: String,
    pub password: String,
    /// PEM bytes supplied by the caller: this module reads no files.
    pub ca_certificate: Vec<u8>,
    pub client_authentication: Option<(Vec<u8>, Vec<u8>)>,
    /// The identity these credentials assert. It becomes authority only after
    /// the broker accepts the connection.
    pub claimed_identity: SessionIdentity,
}

/// The real transport: a connected rumqttc client plus Slice B's delivery
/// processor, exposed through the same seam the stub implements.
pub struct MqttTransport {
    session: MqttSession,
    client: Option<Client>,
    connection: Option<Connection>,
    /// Set only after an accepted CONNACK.
    identity: Option<SessionIdentity>,
    processor: DeliveryProcessor,
    /// Inbound publishes parked while waiting for a control packet.
    pending: Vec<Publish>,
    now: DateTime<Utc>,
}

impl MqttTransport {
    pub fn new(
        session: MqttSession,
        validation: ValidationConfig,
        now: DateTime<Utc>,
    ) -> Result<Self, ProbeError> {
        let processor = DeliveryProcessor::new(
            validation,
            TRACKING_CAPACITY,
            TRACKING_CAPACITY,
            TRACKING_CAPACITY,
        )
        .map_err(|error| ProbeError::InvalidProbe(format!("{error}")))?;
        Ok(Self {
            session,
            client: None,
            connection: None,
            identity: None,
            processor,
            pending: Vec::new(),
            now,
        })
    }

    /// Disconnect the session. Best-effort: the probe's evidence is already
    /// recorded, and a broker that drops us first is not a probe failure.
    pub fn disconnect(&mut self) {
        if let Some(client) = self.client.take() {
            let _ = client.disconnect();
        }
        if let Some(mut connection) = self.connection.take() {
            let _ = poll_incoming(&mut connection, Instant::now() + Duration::from_secs(1));
        }
        self.identity = None;
    }
}

impl Transport for MqttTransport {
    fn authenticate(&mut self) -> Result<SessionIdentity, ProbeError> {
        if let Some(identity) = &self.identity {
            return Ok(identity.clone());
        }
        let mut options = self.session.config.mqtt_options();
        options
            .set_credentials(&self.session.username, &self.session.password)
            .set_transport(rumqttc::Transport::tls(
                self.session.ca_certificate.clone(),
                self.session.client_authentication.clone(),
                None,
            ))
            .set_keep_alive(KEEP_ALIVE)
            .set_clean_start(true);
        let (client, mut connection) = Client::new(options, REQUEST_CAPACITY);
        let deadline = Instant::now() + ACK_TIMEOUT;
        let accepted = loop {
            match await_control(&mut connection, &mut self.pending, deadline) {
                Some(Packet::ConnAck(ack)) => break ack.code == ConnectReturnCode::Success,
                Some(_) => {}
                None => break false,
            }
        };
        if !accepted {
            return Err(ProbeError::AuthenticationFailed);
        }
        // Authority starts here and nowhere else.
        self.client = Some(client);
        self.connection = Some(connection);
        self.identity = Some(self.session.claimed_identity.clone());
        Ok(self.session.claimed_identity.clone())
    }

    fn subscribe(&mut self, filter: &str, no_local: bool) -> Result<(), ProbeError> {
        let denied = || ProbeError::SubscribeDenied {
            filter: filter.to_owned(),
        };
        let (Some(client), Some(connection)) = (self.client.clone(), self.connection.as_mut())
        else {
            return Err(denied());
        };
        client
            .subscribe_many([Filter {
                nolocal: no_local,
                ..Filter::new(filter, QoS::AtLeastOnce)
            }])
            .map_err(|_| denied())?;
        let deadline = Instant::now() + ACK_TIMEOUT;
        loop {
            match await_control(connection, &mut self.pending, deadline) {
                Some(Packet::SubAck(ack)) => {
                    return match ack.return_codes.first() {
                        Some(SubscribeReasonCode::Success(_)) => Ok(()),
                        _ => Err(denied()),
                    };
                }
                Some(_) => {}
                None => return Err(denied()),
            }
        }
    }

    fn publish(
        &mut self,
        _topic: &str,
        envelope: &ValidatedEnvelope,
        retain: bool,
    ) -> Result<(), ProbeError> {
        // The probe is never retained, and Slice B derives both the topic and
        // the retain flag from the validated envelope itself — a probe that
        // asked to be retained would mean the class is no longer `event`.
        if retain {
            return Err(ProbeError::RetainedProbe);
        }
        let (Some(client), Some(connection)) = (self.client.clone(), self.connection.as_mut())
        else {
            return Err(ProbeError::PublishDenied);
        };
        crate::transport::publish(&client, envelope.clone(), self.now)
            .map_err(|_| ProbeError::PublishDenied)?;
        let deadline = Instant::now() + ACK_TIMEOUT;
        loop {
            match await_control(connection, &mut self.pending, deadline) {
                Some(Packet::PubAck(ack)) => {
                    return match ack.reason {
                        PubAckReason::Success | PubAckReason::NoMatchingSubscribers => Ok(()),
                        _ => Err(ProbeError::PublishDenied),
                    };
                }
                Some(_) => {}
                None => return Err(ProbeError::PublishDenied),
            }
        }
    }

    fn receive(&mut self, deadline: Duration) -> Result<Option<ReceivedFrame>, ProbeError> {
        let identity = self
            .identity
            .clone()
            .ok_or(ProbeError::AuthenticationFailed)?;
        let claims: Vec<&str> = identity.allowed_claims.iter().map(String::as_str).collect();
        // The adapter is the only constructor of envelope authority, and the
        // only origin this session may speak or hear for is its own instance.
        let origins = [identity.instance_id.as_str()];
        let authenticated = AuthenticatedTransportPrincipal::new(
            AuthenticatedPrincipal::new(&identity.principal_id, &claims),
            &origins,
        );
        let deadline = Instant::now() + deadline;
        loop {
            let publish = match self.take_publish(deadline) {
                Some(publish) => publish,
                None => return Ok(None),
            };
            // A retained frame means a probe outlived its session on the broker.
            if publish.retain {
                return Err(ProbeError::RetainedProbe);
            }
            let Ok(topic) = String::from_utf8(publish.topic.to_vec()) else {
                return Err(ProbeError::WrongSelfReceive);
            };
            match self
                .processor
                .receive(&topic, &publish.payload, &authenticated, self.now)
            {
                Ok(crate::transport::ReceiveOutcome::Accepted(_)) => {
                    return Ok(Some(ReceivedFrame {
                        topic,
                        bytes: publish.payload.to_vec(),
                    }));
                }
                // Duplicates and tombstones are not the probe's echo; keep
                // waiting until the deadline rather than failing the probe.
                Ok(_) => {}
                Err(_) => return Err(ProbeError::WrongSelfReceive),
            }
        }
    }
}

impl MqttTransport {
    /// The next inbound publish: parked frames first, then the wire.
    fn take_publish(&mut self, deadline: Instant) -> Option<Publish> {
        if !self.pending.is_empty() {
            return Some(self.pending.remove(0));
        }
        let connection = self.connection.as_mut()?;
        loop {
            match poll_incoming(connection, deadline)? {
                Packet::Publish(publish) => return Some(publish),
                _ => continue,
            }
        }
    }
}

/// The next incoming packet before `deadline`; `None` on timeout or a closed
/// connection. Outgoing events carry no broker decision, so they are skipped.
fn poll_incoming(connection: &mut Connection, deadline: Instant) -> Option<Packet> {
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return None;
        }
        match connection.recv_timeout(remaining) {
            Ok(Ok(Event::Incoming(packet))) => return Some(packet),
            Ok(Ok(Event::Outgoing(_))) => {}
            Ok(Err(_)) | Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => {
                return None;
            }
        }
    }
}

/// The next control packet, parking inbound publishes for `receive` so an echo
/// that races an acknowledgement is never dropped.
fn await_control(
    connection: &mut Connection,
    pending: &mut Vec<Publish>,
    deadline: Instant,
) -> Option<Packet> {
    loop {
        match poll_incoming(connection, deadline)? {
            Packet::Publish(publish) => pending.push(publish),
            packet => return Some(packet),
        }
    }
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

// ---------------------------------------------------------------------------
// Transactional connect orchestration and rollback (T10)
// ---------------------------------------------------------------------------
//
// The ordered contract: validate locally (done by the caller), then check
// idempotence/conflict against the registry BEFORE probing, then run the
// subscribe-first exact round-trip probe, then commit the enrollment
// transactionally, then activate exactly one service and reach readiness. Any
// failure after the registry commit removes only this attempt's row and
// stops/disables an otherwise-empty service; a compound compensation failure is
// surfaced as `RollbackIncomplete`, never as success. The transport and the
// service manager are seams so the whole ordered failure matrix is testable.

use crate::service;

#[derive(Debug, PartialEq, Eq)]
pub enum ConnectOutcome {
    /// A new enrollment was probed, committed, and activated.
    Connected {
        capabilities: crate::enrollment::CapabilityRecord,
    },
    /// The same physical workspace with the same descriptor was already enrolled;
    /// lifecycle drift was repaired without a re-probe.
    AlreadyConnected,
}

#[derive(Debug)]
pub enum ConnectError {
    Probe(ProbeError),
    Registry(crate::enrollment::RegistryError),
    /// The same physical workspace is enrolled with a different binding.
    EnrollmentConflict,
    /// Activation failed but compensation succeeded (no residual enrollment).
    ActivationFailed(String),
    /// Compensation itself failed; the residual local state is reported.
    RollbackIncomplete(String),
}

impl ConnectError {
    pub fn code(&self) -> &'static str {
        match self {
            ConnectError::Probe(_) => "connect_probe_failed",
            ConnectError::Registry(_) => "connect_registry_error",
            ConnectError::EnrollmentConflict => "enrollment_conflict",
            ConnectError::ActivationFailed(_) => "connect_activation_failed",
            ConnectError::RollbackIncomplete(_) => "rollback_incomplete",
        }
    }
}

fn capability_record(evidence: &CapabilityEvidence) -> crate::enrollment::CapabilityRecord {
    crate::enrollment::CapabilityRecord {
        authentication: evidence.authentication,
        publish: evidence.publish,
        subscribe: evidence.subscribe,
        self_receive: evidence.self_receive,
        verified_at: evidence.verified_at.to_rfc3339(),
    }
}

fn probe_context_from(enrolled: &crate::enrollment::ValidatedEnrollment) -> ProbeContext {
    // The probe is a capability check; its git anchors just need to be valid
    // OIDs, so the enrolled commit serves for both.
    ProbeContext {
        org_id: enrolled.org_id.clone(),
        project_id: enrolled.project_id.clone(),
        repository_id: enrolled.repository_id.clone(),
        base_oid: enrolled.commit.clone(),
        plan_oid: enrolled.commit.clone(),
    }
}

/// Orchestrate connect from an already-validated enrollment. Splitting the
/// Git-dependent validation (the caller's step) from this lets the ordered
/// failure matrix be tested without a real workspace.
#[allow(clippy::too_many_arguments)]
pub fn orchestrate_from_validated<T: Transport, R: service::CommandRunner>(
    enrolled: &crate::enrollment::ValidatedEnrollment,
    transport: &mut T,
    service_runner: &R,
    service_ctx: &service::ServiceContext,
    db_path: &std::path::Path,
    config: &ValidationConfig,
    deadline: Duration,
    now: DateTime<Utc>,
) -> Result<ConnectOutcome, ConnectError> {
    let key = crate::enrollment::identity_key(&enrolled.workspace);
    let digest = crate::enrollment::descriptor_digest(enrolled);

    // Idempotence / conflict — before any probe, so a healthy existing enrollment
    // is never re-probed.
    if let Some(connection) =
        crate::enrollment::open_readonly(db_path).map_err(ConnectError::Registry)?
    {
        if let Some(existing) =
            crate::enrollment::lookup(&connection, &key).map_err(ConnectError::Registry)?
        {
            drop(connection);
            if existing.descriptor_digest == digest {
                // Repair lifecycle drift, no re-publication.
                let _ = service::install(service_runner, service_ctx);
                let _ = service::enable_start(service_runner, service_ctx);
                return Ok(ConnectOutcome::AlreadyConnected);
            }
            return Err(ConnectError::EnrollmentConflict);
        }
    }

    // Subscribe-first exact round-trip probe.
    let probe_ctx = probe_context_from(enrolled);
    let evidence =
        run_probe(transport, &probe_ctx, config, deadline, now).map_err(ConnectError::Probe)?;
    let capabilities = capability_record(&evidence);

    // Commit the enrollment transactionally before activation.
    let mut connection =
        crate::enrollment::open_writable(db_path).map_err(ConnectError::Registry)?;
    match crate::enrollment::insert_enrollment(
        &mut connection,
        enrolled,
        &capabilities,
        &now.to_rfc3339(),
    )
    .map_err(ConnectError::Registry)?
    {
        crate::enrollment::InsertOutcome::Inserted => {}
        crate::enrollment::InsertOutcome::AlreadyEnrolled => {
            return Ok(ConnectOutcome::AlreadyConnected)
        }
        crate::enrollment::InsertOutcome::Conflict => return Err(ConnectError::EnrollmentConflict),
    }
    drop(connection);

    // Activate exactly one service; compensate on any failure.
    let activation = service::install(service_runner, service_ctx)
        .and_then(|()| service::enable_start(service_runner, service_ctx));
    if let Err(error) = activation {
        return Err(compensate_after_activation(
            db_path,
            &key,
            service_runner,
            service_ctx,
            &error,
        ));
    }

    Ok(ConnectOutcome::Connected { capabilities })
}

/// Remove only this attempt's enrollment row and, if the registry is now empty,
/// stop/disable the service. A failure to compensate is `RollbackIncomplete`.
fn compensate_after_activation<R: service::CommandRunner>(
    db_path: &std::path::Path,
    key: &str,
    service_runner: &R,
    service_ctx: &service::ServiceContext,
    activation_error: &service::ServiceError,
) -> ConnectError {
    let mut connection = match crate::enrollment::open_writable(db_path) {
        Ok(connection) => connection,
        Err(_) => {
            return ConnectError::RollbackIncomplete(format!(
                "registry unreachable during rollback after activation error: {activation_error}"
            ))
        }
    };
    if crate::enrollment::delete_enrollment(&mut connection, key).is_err() {
        return ConnectError::RollbackIncomplete(format!(
            "enrollment row not removed during rollback after activation error: {activation_error}"
        ));
    }
    let now_empty = crate::enrollment::list_enrollments(&connection)
        .map(|rows| rows.is_empty())
        .unwrap_or(false);
    drop(connection);
    if now_empty {
        let _ = service::disable_stop(service_runner, service_ctx);
    }
    ConnectError::ActivationFailed(activation_error.to_string())
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

// ---------------------------------------------------------------------------
// Disconnect, observational status, and lifecycle convergence (T11)
// ---------------------------------------------------------------------------
//
// Local removal is authoritative: a disconnect deletes only the named
// enrollment, preserves every other project, and — when the registry becomes
// empty — stops/disables the one service. Broker cleanup (the best-effort
// project tombstone) is a separate, failure-tolerant channel that never blocks
// local removal; the real ordered tombstone via the adapter is T13, so here its
// outcome is injected so both the success and the broker-down paths are tested.
// Manager stop/disable failure is a precise degraded result, never hidden
// success. Status is strictly read-only: it never creates the database, starts
// a process, or claims an aggregate `connected`/`ready`.

/// Whether the named enrollment was actually removed by this disconnect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalOutcome {
    /// The row existed and was deleted — local removal is authoritative.
    Removed,
    /// Nothing was enrolled for this workspace (already gone / never enrolled).
    AlreadyAbsent,
}

/// The best-effort broker-side cleanup outcome, reported separately from local
/// removal so a broker-down tombstone failure is visible without blocking it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CleanupOutcome {
    Ok,
    Failed(String),
}

impl From<Result<(), String>> for CleanupOutcome {
    fn from(result: Result<(), String>) -> Self {
        match result {
            Ok(()) => CleanupOutcome::Ok,
            Err(reason) => CleanupOutcome::Failed(reason),
        }
    }
}

/// The service lifecycle result after reconciling against registry truth.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LifecycleOutcome {
    /// Other projects remain; the service stays up.
    Preserved { remaining: usize },
    /// The registry is now empty; the service was stopped and disabled.
    StoppedDisabled,
    /// The registry is empty but the manager stop/disable failed — a precise
    /// degraded result, not hidden success.
    ManagerDegraded(String),
    /// The machine was already fully dormant (no store); nothing to reconcile.
    Untouched,
}

/// The three separate lifecycle facts a disconnect reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisconnectReport {
    pub local: LocalOutcome,
    pub broker_cleanup: CleanupOutcome,
    pub lifecycle: LifecycleOutcome,
}

#[derive(Debug)]
pub enum DisconnectError {
    Registry(crate::enrollment::RegistryError),
}

/// Disconnect a single physical workspace by its identity key. Best-effort
/// broker cleanup (injected) is reported separately; local deletion is
/// authoritative; the service is reconciled from registry truth afterward.
///
/// A missing database means a fully dormant machine: there is nothing to remove
/// and no manager state to repair, so this never creates the store on a read.
pub fn disconnect_by_key<R: service::CommandRunner>(
    db_path: &std::path::Path,
    key: &str,
    broker_cleanup: Result<(), String>,
    service_runner: &R,
    service_ctx: &service::ServiceContext,
) -> Result<DisconnectReport, DisconnectError> {
    let cleanup = CleanupOutcome::from(broker_cleanup);

    // Dormant machine: no store, so nothing to delete and nothing to reconcile.
    let existed =
        match crate::enrollment::open_readonly(db_path).map_err(DisconnectError::Registry)? {
            None => {
                return Ok(DisconnectReport {
                    local: LocalOutcome::AlreadyAbsent,
                    broker_cleanup: cleanup,
                    lifecycle: LifecycleOutcome::Untouched,
                });
            }
            Some(connection) => crate::enrollment::lookup(&connection, key)
                .map_err(DisconnectError::Registry)?
                .is_some(),
        };

    // The store exists, so we may open it writable to delete and to reconcile the
    // manager lifecycle from registry truth (also the repeated-disconnect repair).
    let mut connection =
        crate::enrollment::open_writable(db_path).map_err(DisconnectError::Registry)?;
    let removed = existed
        && crate::enrollment::delete_enrollment(&mut connection, key)
            .map_err(DisconnectError::Registry)?;
    let remaining = crate::enrollment::list_enrollments(&connection)
        .map_err(DisconnectError::Registry)?
        .len();
    drop(connection);

    let lifecycle = if remaining == 0 {
        // Final removal, or a repeated disconnect repairing residual drift:
        // reconcile the one service to the empty desired state. Idempotent.
        match service::disable_stop(service_runner, service_ctx) {
            Ok(()) => LifecycleOutcome::StoppedDisabled,
            Err(error) => LifecycleOutcome::ManagerDegraded(error.to_string()),
        }
    } else {
        LifecycleOutcome::Preserved { remaining }
    };

    Ok(DisconnectReport {
        local: if removed {
            LocalOutcome::Removed
        } else {
            LocalOutcome::AlreadyAbsent
        },
        broker_cleanup: cleanup,
        lifecycle,
    })
}

/// A read-only, aggregate-free status projection for the whole machine (or one
/// workspace when `key` is given). Never creates the database and never starts a
/// process: enrollment comes from a read-only registry open, the definition from
/// a filesystem presence check, and the process/enabled state from a read-only
/// manager query. Historical verified capabilities, the (not-observed) live
/// broker session, and per-project health are separate fields — there is no
/// `connected`/`ready` boolean.
pub fn status_report<R: service::CommandRunner>(
    db_path: &std::path::Path,
    service_runner: &R,
    service_ctx: &service::ServiceContext,
    key: Option<&str>,
) -> crate::json::Value {
    use crate::json::Value;

    // Read-only registry open: a missing store yields no enrollments and is never
    // created here.
    let enrollments: Vec<crate::enrollment::EnrolledRow> =
        match crate::enrollment::open_readonly(db_path) {
            Ok(Some(connection)) => match key {
                Some(key) => crate::enrollment::lookup(&connection, key)
                    .ok()
                    .flatten()
                    .into_iter()
                    .collect(),
                None => crate::enrollment::list_enrollments(&connection).unwrap_or_default(),
            },
            _ => Vec::new(),
        };

    let enrollment_values = enrollments
        .iter()
        .map(|row| {
            Value::Object(vec![
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
                // Per-project historical verification, kept beside the enrollment
                // and never collapsed into a readiness claim.
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
                // Per-project health is the enrollment's own state, separate from
                // any live-session claim.
                ("health".into(), Value::String("enrolled".into())),
            ])
        })
        .collect();

    // Definition presence is a filesystem fact; the process/enabled state is a
    // read-only manager query. Neither starts anything.
    let definition_present = service::definition_path(service_ctx).exists();
    let manager_state = match service::status(service_runner, service_ctx) {
        Ok(0) => "enabled",
        Ok(_) => "disabled",
        Err(_) => "unknown",
    };

    Value::Object(vec![
        ("schema".into(), Value::Number("1".into())),
        ("enrollments".into(), Value::Array(enrollment_values)),
        (
            "definition".into(),
            Value::Object(vec![("present".into(), Value::Bool(definition_present))]),
        ),
        (
            "process".into(),
            Value::Object(vec![(
                "manager_state".into(),
                Value::String(manager_state.into()),
            )]),
        ),
        (
            "broker".into(),
            // Read-only status does not observe a live broker session; the live
            // session belongs to the running connector/adapter (T13).
            Value::Object(vec![
                ("session_observed".into(), Value::Bool(false)),
                (
                    "session_state".into(),
                    Value::String("not-observed-in-read-only-status".into()),
                ),
            ]),
        ),
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
        // Register a channel for real, against a real enrolled database, so the
        // absence below is the *loss* of something that existed rather than a
        // registry that was never populated.
        let (path, key) = enrolled_db("restart", 8, 80);
        let mut before = ChannelRegistry::new();
        dispatch_for_key(
            &register_request("sess-restart", "chan-restart"),
            &key,
            &path,
            &mut before,
        )
        .expect("register");
        assert!(before.contains("sess-restart"));

        // The restart: the process-local registry is gone, the database is not.
        drop(before);
        let after = ChannelRegistry::new();
        assert!(
            after.is_empty(),
            "a restarted connector must recover no channel"
        );

        // And the database it re-opens still holds no channel state to recover,
        // so nothing could repopulate the registry behind our back.
        let connection = crate::enrollment::open_readonly(&path).unwrap().unwrap();
        let channel_rows: i64 = connection
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type IN ('table','view') AND (name LIKE '%channel%' OR name LIKE '%session%')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            channel_rows, 0,
            "no channel or session state may survive a restart in SQLite"
        );
        assert_eq!(
            crate::enrollment::list_enrollments(&connection)
                .unwrap()
                .len(),
            1,
            "the enrollment itself must survive the restart"
        );
    }
}

#[cfg(test)]
mod connect_tests {
    use super::*;
    use crate::enrollment::{
        PhysicalWorkspace, PlatformIdentity, ValidatedEnrollment, ValidatedRemote,
    };
    use crate::service::{CommandRunner, ManagerCommand, ServiceContext, ServiceError};
    use std::cell::RefCell;

    /// A recording service runner that can be made to fail when a command line
    /// contains a substring (e.g. "enable" to fail activation).
    struct FakeService {
        fail_on: Option<String>,
        recorded: RefCell<Vec<String>>,
    }

    impl FakeService {
        fn ok() -> Self {
            FakeService {
                fail_on: None,
                recorded: RefCell::new(Vec::new()),
            }
        }
        fn failing(substr: &str) -> Self {
            FakeService {
                fail_on: Some(substr.to_owned()),
                recorded: RefCell::new(Vec::new()),
            }
        }
    }

    impl FakeService {
        /// Case-insensitive, because the three managers do not agree on case:
        /// `systemctl --user enable --now` and `launchctl enable` are lowercase,
        /// `schtasks /Change /ENABLE` is not.
        fn recorded_any(&self, mark: &str) -> bool {
            let mark = mark.to_ascii_lowercase();
            self.recorded
                .borrow()
                .iter()
                .any(|line| line.to_ascii_lowercase().contains(&mark))
        }
    }

    /// Activation and deactivation are spelled differently per manager, so the
    /// tests match the word the *current* platform's commands actually use:
    /// systemd `enable --now`/`disable --now`, launchctl `bootstrap`+`enable`/
    /// `bootout`, Task Scheduler `/Change /ENABLE`/`/Change /DISABLE`.
    const ENABLE_MARK: &str = "enable";
    #[cfg(target_os = "macos")]
    const DISABLE_MARK: &str = "bootout";
    #[cfg(not(target_os = "macos"))]
    const DISABLE_MARK: &str = "disable";

    impl CommandRunner for FakeService {
        fn run(&self, command: &ManagerCommand) -> Result<i32, ServiceError> {
            let line = format!("{} {}", command.program, command.args.join(" "));
            self.recorded.borrow_mut().push(line.clone());
            if let Some(fail) = &self.fail_on {
                if line
                    .to_ascii_lowercase()
                    .contains(&fail.to_ascii_lowercase())
                {
                    return Err(ServiceError::ManagerFailed { code: 1 });
                }
            }
            Ok(0)
        }
    }

    fn identity() -> SessionIdentity {
        SessionIdentity {
            principal_id: "employee-184".into(),
            agent_id: "agent-72".into(),
            instance_id: "instance-01".into(),
            allowed_claims: vec![],
        }
    }

    fn enrolled(device: u64, inode: u64, commit: &str) -> ValidatedEnrollment {
        ValidatedEnrollment {
            org_id: "acme".into(),
            project_id: "loam".into(),
            repository_id: "repo-2F8".into(),
            broker_profile: "acme-prod".into(),
            broker_endpoint: "mqtts://broker:8883".into(),
            tls_server_name: "broker".into(),
            credential_ref: "vault://c".into(),
            ca_ref: None,
            commit: commit.into(),
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

    fn setup(label: &str) -> (std::path::PathBuf, ServiceContext) {
        let root = std::env::temp_dir().join(format!(
            "loam-connect-{label}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        // ensure_instance_id creates the global root (via the service module),
        // so open_writable can create the database beneath it.
        let instance_id = crate::service::ensure_instance_id(&root).unwrap();
        let ctx = ServiceContext {
            global_root: root.clone(),
            instance_id,
            runtime_path: std::env::temp_dir().join("loam-rt").join("loam"),
        };
        (root.join("loam.sqlite3"), ctx)
    }

    fn deadline() -> Duration {
        Duration::from_millis(50)
    }
    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-08-08T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    const COMMIT_A: &str = "0123456789abcdef0123456789abcdef01234567";
    const COMMIT_B: &str = "ffffffffffffffffffffffffffffffffffffffff";

    #[test]
    fn happy_path_probes_commits_and_activates() {
        let (db, ctx) = setup("happy");
        let mut transport = StubTransport::healthy(identity());
        let service = FakeService::ok();
        let outcome = orchestrate_from_validated(
            &enrolled(1, 10, COMMIT_A),
            &mut transport,
            &service,
            &ctx,
            &db,
            &ValidationConfig::default(),
            deadline(),
            now(),
        )
        .expect("connect");
        assert!(matches!(outcome, ConnectOutcome::Connected { .. }));
        // Enrollment persisted.
        let connection = crate::enrollment::open_readonly(&db).unwrap().unwrap();
        assert_eq!(
            crate::enrollment::list_enrollments(&connection)
                .unwrap()
                .len(),
            1
        );
        // The service was enabled/started.
        assert!(service.recorded_any(ENABLE_MARK));
    }

    #[test]
    fn probe_failure_leaves_no_enrollment_and_no_activation() {
        let (db, ctx) = setup("probe-fail");
        let mut transport = StubTransport {
            deny_auth: true,
            ..StubTransport::healthy(identity())
        };
        let service = FakeService::ok();
        let outcome = orchestrate_from_validated(
            &enrolled(2, 20, COMMIT_A),
            &mut transport,
            &service,
            &ctx,
            &db,
            &ValidationConfig::default(),
            deadline(),
            now(),
        );
        assert!(matches!(outcome, Err(ConnectError::Probe(_))));
        // No database row, no activation command.
        assert!(crate::enrollment::open_readonly(&db).unwrap().is_none());
        assert!(service.recorded.borrow().is_empty());
    }

    #[test]
    fn activation_failure_rolls_the_enrollment_back() {
        let (db, ctx) = setup("activate-fail");
        let mut transport = StubTransport::healthy(identity());
        let service = FakeService::failing(ENABLE_MARK); // enable_start fails
        let outcome = orchestrate_from_validated(
            &enrolled(3, 30, COMMIT_A),
            &mut transport,
            &service,
            &ctx,
            &db,
            &ValidationConfig::default(),
            deadline(),
            now(),
        );
        assert!(matches!(outcome, Err(ConnectError::ActivationFailed(_))));
        // The row was removed (registry now empty) and the service was disabled.
        let connection = crate::enrollment::open_readonly(&db).unwrap().unwrap();
        assert!(crate::enrollment::list_enrollments(&connection)
            .unwrap()
            .is_empty());
        assert!(service.recorded_any(DISABLE_MARK));
    }

    #[test]
    fn repeated_identical_connect_repairs_without_reprobe() {
        let (db, ctx) = setup("idempotent");
        // First connect.
        let mut transport = StubTransport::healthy(identity());
        orchestrate_from_validated(
            &enrolled(4, 40, COMMIT_A),
            &mut transport,
            &FakeService::ok(),
            &ctx,
            &db,
            &ValidationConfig::default(),
            deadline(),
            now(),
        )
        .expect("first connect");
        // Second identical connect: a fresh transport that would FAIL a probe,
        // proving no re-probe happens for a healthy existing enrollment.
        let mut no_probe = StubTransport {
            deny_auth: true,
            ..StubTransport::healthy(identity())
        };
        let service = FakeService::ok();
        let outcome = orchestrate_from_validated(
            &enrolled(4, 40, COMMIT_A),
            &mut no_probe,
            &service,
            &ctx,
            &db,
            &ValidationConfig::default(),
            deadline(),
            now(),
        )
        .expect("idempotent connect");
        assert_eq!(outcome, ConnectOutcome::AlreadyConnected);
    }

    #[test]
    fn a_changed_binding_for_the_same_workspace_is_a_conflict() {
        let (db, ctx) = setup("conflict");
        orchestrate_from_validated(
            &enrolled(5, 50, COMMIT_A),
            &mut StubTransport::healthy(identity()),
            &FakeService::ok(),
            &ctx,
            &db,
            &ValidationConfig::default(),
            deadline(),
            now(),
        )
        .expect("first connect");
        // Same physical identity, different commit -> conflict, no re-probe.
        let outcome = orchestrate_from_validated(
            &enrolled(5, 50, COMMIT_B),
            &mut StubTransport {
                deny_auth: true,
                ..StubTransport::healthy(identity())
            },
            &FakeService::ok(),
            &ctx,
            &db,
            &ValidationConfig::default(),
            deadline(),
            now(),
        );
        assert!(matches!(outcome, Err(ConnectError::EnrollmentConflict)));
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use crate::enrollment::{
        CapabilityRecord, PhysicalWorkspace, PlatformIdentity, ValidatedEnrollment, ValidatedRemote,
    };
    use crate::service::{CommandRunner, ManagerCommand, ServiceContext, ServiceError};
    use std::cell::RefCell;
    use std::path::{Path, PathBuf};

    /// A recording service runner. `failing_all` makes every manager command fail
    /// (fail substring `""` matches any line), so the degraded-lifecycle path is
    /// exercised without depending on a platform-specific command string.
    struct FakeService {
        fail_all: bool,
        recorded: RefCell<Vec<String>>,
    }
    impl FakeService {
        fn ok() -> Self {
            FakeService {
                fail_all: false,
                recorded: RefCell::new(Vec::new()),
            }
        }
        fn failing_all() -> Self {
            FakeService {
                fail_all: true,
                recorded: RefCell::new(Vec::new()),
            }
        }
        fn calls(&self) -> Vec<String> {
            self.recorded.borrow().clone()
        }
    }
    impl CommandRunner for FakeService {
        fn run(&self, command: &ManagerCommand) -> Result<i32, ServiceError> {
            self.recorded.borrow_mut().push(format!(
                "{} {}",
                command.program,
                command.args.join(" ")
            ));
            if self.fail_all {
                return Err(ServiceError::ManagerFailed { code: 1 });
            }
            Ok(0)
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

    fn synthetic(project: &str, device: u64, inode: u64) -> ValidatedEnrollment {
        ValidatedEnrollment {
            org_id: "acme".into(),
            project_id: project.into(),
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
                display_path: format!("/w/{project}"),
                identity: PlatformIdentity::Unix { device, inode },
            },
        }
    }

    fn setup(label: &str) -> (PathBuf, ServiceContext) {
        let root = std::env::temp_dir().join(format!(
            "loam-lifecycle-{label}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let instance_id = crate::service::ensure_instance_id(&root).unwrap();
        let ctx = ServiceContext {
            global_root: root.clone(),
            instance_id,
            runtime_path: std::env::temp_dir().join("loam-rt").join("loam"),
        };
        (root.join("loam.sqlite3"), ctx)
    }

    fn insert(db: &Path, enrollment: &ValidatedEnrollment) {
        let mut connection = crate::enrollment::open_writable(db).unwrap();
        crate::enrollment::insert_enrollment(&mut connection, enrollment, &caps(), "t").unwrap();
    }

    fn key_of(enrollment: &ValidatedEnrollment) -> String {
        crate::enrollment::identity_key(&enrollment.workspace)
    }

    #[test]
    fn intermediate_disconnect_removes_one_and_preserves_the_others() {
        let (db, ctx) = setup("intermediate");
        let a = synthetic("proj-a", 1, 10);
        let b = synthetic("proj-b", 1, 11);
        let c = synthetic("proj-c", 1, 12);
        insert(&db, &a);
        insert(&db, &b);
        insert(&db, &c);

        let service = FakeService::ok();
        let report =
            disconnect_by_key(&db, &key_of(&b), Ok(()), &service, &ctx).expect("disconnect");

        assert_eq!(report.local, LocalOutcome::Removed);
        assert_eq!(
            report.lifecycle,
            LifecycleOutcome::Preserved { remaining: 2 }
        );
        // Preserving the service means no stop/disable was issued.
        assert!(
            service.calls().is_empty(),
            "an intermediate disconnect must not touch the manager: {:?}",
            service.calls()
        );
        // The other two projects survive.
        let read = crate::enrollment::open_readonly(&db).unwrap().unwrap();
        assert!(crate::enrollment::lookup(&read, &key_of(&a))
            .unwrap()
            .is_some());
        assert!(crate::enrollment::lookup(&read, &key_of(&c))
            .unwrap()
            .is_some());
        assert!(crate::enrollment::lookup(&read, &key_of(&b))
            .unwrap()
            .is_none());
    }

    #[test]
    fn final_disconnect_removes_and_stops_the_service() {
        let (db, ctx) = setup("final");
        let only = synthetic("proj-only", 2, 20);
        insert(&db, &only);

        let service = FakeService::ok();
        let report =
            disconnect_by_key(&db, &key_of(&only), Ok(()), &service, &ctx).expect("disconnect");

        assert_eq!(report.local, LocalOutcome::Removed);
        assert_eq!(report.lifecycle, LifecycleOutcome::StoppedDisabled);
        // The registry is empty and the manager stop/disable was attempted.
        assert!(
            !service.calls().is_empty(),
            "final disconnect stops the service"
        );
        let read = crate::enrollment::open_readonly(&db).unwrap().unwrap();
        assert!(crate::enrollment::list_enrollments(&read)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn final_disconnect_stops_even_when_broker_cleanup_fails() {
        let (db, ctx) = setup("broker-down");
        let only = synthetic("proj-only", 3, 30);
        insert(&db, &only);

        let service = FakeService::ok();
        // Broker tombstone failed (broker down / credential revoked) — local
        // removal and service stop proceed regardless, and the failure is
        // reported separately.
        let report = disconnect_by_key(
            &db,
            &key_of(&only),
            Err("broker unreachable".into()),
            &service,
            &ctx,
        )
        .expect("disconnect");

        assert_eq!(report.local, LocalOutcome::Removed);
        assert_eq!(report.lifecycle, LifecycleOutcome::StoppedDisabled);
        assert_eq!(
            report.broker_cleanup,
            CleanupOutcome::Failed("broker unreachable".into())
        );
    }

    #[test]
    fn manager_failure_on_final_disconnect_is_a_degraded_result_not_hidden_success() {
        let (db, ctx) = setup("degraded");
        let only = synthetic("proj-only", 4, 40);
        insert(&db, &only);

        let service = FakeService::failing_all();
        let report =
            disconnect_by_key(&db, &key_of(&only), Ok(()), &service, &ctx).expect("disconnect");

        // Local removal still succeeded; the manager failure is surfaced.
        assert_eq!(report.local, LocalOutcome::Removed);
        assert!(matches!(
            report.lifecycle,
            LifecycleOutcome::ManagerDegraded(_)
        ));
        let read = crate::enrollment::open_readonly(&db).unwrap().unwrap();
        assert!(crate::enrollment::list_enrollments(&read)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn repeated_disconnect_repairs_residual_manager_state() {
        let (db, ctx) = setup("repair");
        let only = synthetic("proj-only", 5, 50);
        insert(&db, &only);
        // First disconnect empties the registry.
        disconnect_by_key(&db, &key_of(&only), Ok(()), &FakeService::ok(), &ctx)
            .expect("first disconnect");

        // A repeated disconnect of the same (now-absent) workspace: nothing to
        // remove, but the empty registry is reconciled — the manager is
        // stopped/disabled again to repair any residual drift, without recreating
        // an enrollment or contacting the broker.
        let service = FakeService::ok();
        let report =
            disconnect_by_key(&db, &key_of(&only), Ok(()), &service, &ctx).expect("repeat");
        assert_eq!(report.local, LocalOutcome::AlreadyAbsent);
        assert_eq!(report.lifecycle, LifecycleOutcome::StoppedDisabled);
        assert!(!service.calls().is_empty(), "repair reconciles the manager");
    }

    #[test]
    fn disconnect_on_a_dormant_machine_creates_no_database_and_touches_no_manager() {
        let (db, ctx) = setup("dormant");
        // No enrollment ever existed: the store is absent.
        assert!(!db.exists());
        let service = FakeService::ok();
        let report =
            disconnect_by_key(&db, "unix:9:9", Ok(()), &service, &ctx).expect("disconnect");
        assert_eq!(report.local, LocalOutcome::AlreadyAbsent);
        assert_eq!(report.lifecycle, LifecycleOutcome::Untouched);
        assert!(service.calls().is_empty(), "a dormant disconnect is inert");
        // A disconnect read never creates the database.
        assert!(!db.exists(), "disconnect must not create the store");
    }

    #[test]
    fn status_on_a_missing_registry_is_empty_and_creates_no_database() {
        let (db, ctx) = setup("status-missing");
        assert!(!db.exists());
        let service = FakeService::ok();
        let report = status_report(&db, &service, &ctx, None);
        let text = report.to_json();
        assert!(text.contains("\"enrollments\":[]"));
        // No aggregate readiness claim, and the read created no database.
        assert!(!text.contains("\"connected\"") && !text.contains("\"ready\""));
        assert!(!db.exists(), "status must not create the store");
    }

    #[test]
    fn status_reports_separate_enrollment_and_verification_fields() {
        let (db, ctx) = setup("status-enrolled");
        let a = synthetic("proj-a", 6, 60);
        let b = synthetic("proj-b", 6, 61);
        insert(&db, &a);
        insert(&db, &b);

        let service = FakeService::ok();
        let report = status_report(&db, &service, &ctx, None);
        let text = report.to_json();
        // Two enrollments, each with its own historical verification, plus the
        // separate definition/process/broker fields — no aggregate boolean.
        assert!(text.contains("proj-a") && text.contains("proj-b"));
        assert!(text.contains("\"verification\""));
        assert!(text.contains("\"definition\""));
        assert!(text.contains("\"process\""));
        assert!(text.contains("\"session_observed\":false"));
        assert!(!text.contains("\"connected\"") && !text.contains("\"ready\""));

        // A workspace filter narrows to one enrollment.
        let one = status_report(&db, &service, &ctx, Some(&key_of(&a)));
        let one_text = one.to_json();
        assert!(one_text.contains("proj-a") && !one_text.contains("proj-b"));
    }
}
