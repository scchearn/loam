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

use crate::transport::{
    AuthenticatedTransportPrincipal, DeliveryProcessor, ReceiveOutcome, TransportConfig,
};

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
    /// Pump exactly one inbound frame through Slice B's `DeliveryProcessor` and
    /// report the topic together with the delivery outcome.
    ///
    /// Unlike [`Transport::receive`], which exists to find the probe's own echo
    /// and therefore swallows everything else, this reports duplicates, stale
    /// state, and tombstones. The snapshot store needs them: a swallowed
    /// tombstone would leave a resolved item on screen, and a swallowed
    /// duplicate would hide the very property "one logical item per message"
    /// asserts. `now` is passed per call because a live session outlives the
    /// single timestamp the probe was constructed with.
    ///
    /// Read-only by construction: it never publishes.
    pub fn receive_outcome(
        &mut self,
        deadline: Duration,
        now: DateTime<Utc>,
        roster: &PeerRoster,
    ) -> Result<Option<(String, ReceiveOutcome)>, ProbeError> {
        let identity = self
            .identity
            .clone()
            .ok_or(ProbeError::AuthenticationFailed)?;
        // The session admits its own principal and instance plus exactly the
        // provisioned roster — nothing derived from the frame itself, or an
        // untrusted sender would authorize itself.
        let mut claims: Vec<&str> = identity.allowed_claims.iter().map(String::as_str).collect();
        claims.extend(roster.principals.iter().map(String::as_str));
        let mut origins: Vec<&str> = vec![identity.instance_id.as_str()];
        origins.extend(roster.origins.iter().map(String::as_str));
        let authenticated = AuthenticatedTransportPrincipal::new(
            AuthenticatedPrincipal::new(&identity.principal_id, &claims),
            &origins,
        );
        let deadline = Instant::now() + deadline;
        let Some(publish) = self.take_publish(deadline) else {
            return Ok(None);
        };
        let Ok(topic) = String::from_utf8(publish.topic.to_vec()) else {
            return Err(ProbeError::WrongSelfReceive);
        };
        match self
            .processor
            .receive(&topic, &publish.payload, &authenticated, now)
        {
            Ok(outcome) => Ok(Some((topic, outcome))),
            // A rejected frame is a sender's problem, not a session failure: the
            // pump keeps running and the snapshot simply never sees it.
            Err(_) => Ok(None),
        }
    }

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

use std::collections::VecDeque;
use std::path::Path;

use crate::ipc::{self, IpcConfig, Operation, Request};

/// The connector's volatile in-process state: the Slice C inject-channel
/// registry and Slice D's live project sessions with their snapshot store. All
/// of it dies with the process and none of it is ever written to SQLite.
pub struct ConnectorState {
    pub channels: ChannelRegistry,
    pub sessions: ProjectSessions,
}

impl ConnectorState {
    pub fn new() -> Self {
        ConnectorState {
            channels: ChannelRegistry::new(),
            sessions: ProjectSessions::new(SNAPSHOT_CAPACITY),
        }
    }
}

impl Default for ConnectorState {
    fn default() -> Self {
        Self::new()
    }
}

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

// ---------------------------------------------------------------------------
// Bounded in-memory snapshot store and live project sessions (Slice D T1)
// ---------------------------------------------------------------------------
//
// Slice B's `DeliveryProcessor` is the single validator, deduplicator, and
// expiry tracker; it tracks *ids*, not bodies. The store below is the only place
// a renderable body is retained, and it retains one per logical item so QoS 1
// duplicates and redelivery collapse. It is in-memory only: there is no snapshot
// table, no sidecar store, and no schema change, because MQTT is never durable
// authority and retained state plus the inbox re-deliver on reconnect. A
// restarted connector therefore serves nothing until state re-delivers, which is
// correct rather than a bug.

/// One renderable item in a project's snapshot: normalized, already-deduped, and
/// already-expiry-filtered. Carries no envelope bytes, no credential, and no raw
/// remote URL — only what a hook may render.
#[derive(Debug, Clone, PartialEq)]
pub struct SnapshotItem {
    /// The logical identity of this item. QoS 1 duplicates and redelivery of the
    /// same message share it, so the snapshot holds exactly one entry per
    /// message; a later state revision replaces the earlier one in place.
    pub key: String,
    pub source: String,
    pub item_type: String,
    pub summary: String,
    pub to: Vec<(String, String)>,
    pub org_id: String,
    pub project_id: String,
    pub repository_id: String,
    pub from_principal_id: String,
    pub from_agent_id: String,
    pub from_instance_id: String,
    pub payload: crate::json::Value,
    pub expires_at: DateTime<Utc>,
}

/// A bounded, per-project, in-memory item store with the same drop-on-restart
/// lifetime as [`ChannelRegistry`]. Oldest is evicted at capacity.
#[derive(Debug)]
pub struct SnapshotStore {
    capacity: usize,
    projects: std::collections::HashMap<String, std::collections::VecDeque<SnapshotItem>>,
}

impl SnapshotStore {
    pub fn new(capacity: usize) -> Result<Self, crate::transport::TransportError> {
        if capacity == 0 {
            return Err(crate::transport::TransportError::ZeroTrackingCapacity);
        }
        Ok(SnapshotStore {
            capacity,
            projects: std::collections::HashMap::new(),
        })
    }

    /// Admit one delivery outcome that `DeliveryProcessor` has already ruled on.
    /// Only `Accepted` retains a body and only `Removed` resolves one; a
    /// duplicate, stale, or conflicting outcome changes nothing here, which is
    /// what makes one logical item per message hold under QoS 1. Returns whether
    /// the store changed.
    pub fn admit(&mut self, topic: &str, outcome: &ReceiveOutcome) -> bool {
        let Ok(parsed) = crate::envelope::parse_topic(topic) else {
            return false;
        };
        match outcome {
            ReceiveOutcome::Accepted(validated) => {
                let key = accepted_key(&parsed.delivery, &validated.as_envelope().id);
                let Some(item) = snapshot_item(key, validated) else {
                    return false;
                };
                self.store(parsed.project, item)
            }
            ReceiveOutcome::Removed => match tombstone_key(&parsed.delivery) {
                Some(key) => self.remove(parsed.project, &key),
                None => false,
            },
            _ => false,
        }
    }

    fn store(&mut self, project_id: &str, item: SnapshotItem) -> bool {
        let items = self.projects.entry(project_id.to_owned()).or_default();
        // A later revision of the same logical item replaces the earlier one in
        // place, so a live state key never occupies two snapshot slots.
        if let Some(existing) = items.iter_mut().find(|held| held.key == item.key) {
            *existing = item;
            return true;
        }
        if items.len() == self.capacity {
            items.pop_front();
        }
        items.push_back(item);
        true
    }

    fn remove(&mut self, project_id: &str, key: &str) -> bool {
        match self.projects.get_mut(project_id) {
            Some(items) => {
                let before = items.len();
                items.retain(|held| held.key != key);
                items.len() != before
            }
            None => false,
        }
    }

    /// The current snapshot for one project, oldest first, with expired items
    /// dropped. An unresolved item is *not* dropped by a read, so it reappears on
    /// a later hook until it expires or a tombstone resolves it.
    pub fn snapshot(&mut self, project_id: &str, now: DateTime<Utc>) -> Vec<SnapshotItem> {
        match self.projects.get_mut(project_id) {
            Some(items) => {
                items.retain(|held| held.expires_at > now);
                items.iter().cloned().collect()
            }
            None => Vec::new(),
        }
    }

    pub fn len(&self, project_id: &str) -> usize {
        self.projects.get(project_id).map_or(0, VecDeque::len)
    }

    pub fn is_empty(&self) -> bool {
        self.projects.values().all(VecDeque::is_empty)
    }
}

fn accepted_key(delivery: &crate::envelope::TopicDelivery<'_>, envelope_id: &str) -> String {
    use crate::envelope::TopicDelivery;
    match delivery {
        TopicDelivery::Event { .. } => format!("event:{envelope_id}"),
        TopicDelivery::State { origin, key } => format!("state:{origin}/{key}"),
        TopicDelivery::Inbox { message_id, .. } => format!("inbox:{message_id}"),
    }
}

/// The logical key an empty-payload tombstone resolves. An event cannot be
/// tombstoned (Slice B rejects that), so only state and inbox have one.
fn tombstone_key(delivery: &crate::envelope::TopicDelivery<'_>) -> Option<String> {
    use crate::envelope::TopicDelivery;
    match delivery {
        TopicDelivery::Event { .. } => None,
        TopicDelivery::State { origin, key } => Some(format!("state:{origin}/{key}")),
        TopicDelivery::Inbox { message_id, .. } => Some(format!("inbox:{message_id}")),
    }
}

fn snapshot_item(key: String, validated: &ValidatedEnvelope) -> Option<SnapshotItem> {
    let envelope = validated.as_envelope();
    let expires_at = DateTime::parse_from_rfc3339(&envelope.data.expires_at)
        .ok()?
        .with_timezone(&Utc);
    Some(SnapshotItem {
        key,
        source: envelope.source.clone(),
        item_type: envelope.message_type.clone(),
        summary: envelope.data.summary.clone(),
        to: envelope
            .data
            .to
            .iter()
            .map(|recipient| (recipient.kind.clone(), recipient.id.clone()))
            .collect(),
        org_id: envelope.data.context.org_id.clone(),
        project_id: envelope.data.context.project_id.clone(),
        repository_id: envelope.data.context.repository_id.clone(),
        from_principal_id: envelope.data.from.principal_id.clone(),
        from_agent_id: envelope.data.from.agent_id.clone(),
        from_instance_id: envelope.data.from.instance_id.clone(),
        payload: envelope.data.payload.clone(),
        expires_at,
    })
}

/// Who a project's live session will admit frames from. Slice B checks every
/// received frame's topic origin and `data.from.principal_id` against these, so
/// a session with no roster hears only its own instance. Injected by the
/// deployment at provisioning time — never invented here and never supplied by
/// an IPC caller.
#[derive(Debug, Clone, Default)]
pub struct PeerRoster {
    pub principals: Vec<String>,
    pub origins: Vec<String>,
}

impl PeerRoster {
    pub fn is_empty(&self) -> bool {
        self.principals.is_empty() && self.origins.is_empty()
    }
}

/// Resolve one enrolled project into a live broker session and the peer roster
/// its received frames are checked against — the single injection point for both.
///
/// It returns `None` today, and that is the honest Phase-1 answer, not an
/// oversight: `credential_ref` is a deployment-owned reference (`vault://…`) with
/// no secret backend behind it yet, and no enrollment field carries a per-project
/// peer roster. Both are **named residuals owned by the broker-provisioning and
/// enrollment track**, not by harness integration, and until they are filled the
/// federation is not operational — an agent cannot actually see a real colleague.
/// `project.attach` therefore answers `credentials-unresolved` rather than
/// fabricating a session.
///
/// The seam is a plain function rather than a stored callable on purpose: the
/// crate capability guard bars function-pointer and trait-object capabilities,
/// and a provisioner that could be swapped at runtime would be exactly that.
/// ponytail: returns None. Fill it in when provisioning lands a secret backend
/// and a peer roster; `ProjectSessions::attach` already takes the resolved value.
pub fn provision_session(
    _row: &crate::enrollment::EnrolledRow,
) -> Option<(MqttSession, PeerRoster)> {
    None
}

/// What a `project.attach` actually achieved. Reported honestly: an unopened
/// session is never described as attached-and-live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionState {
    /// A live subscribed broker session is held in this process for the project.
    Live,
    /// A session for this project was already live; attach is idempotent.
    AlreadyLive,
    /// No secret backend resolved this enrollment's `credential_ref`.
    CredentialsUnresolved,
    /// Credentials resolved but no peer roster was provisioned, so the session
    /// could admit no colleague and was not opened.
    NoPeerRoster,
    /// Credentials and roster resolved but the broker refused the session.
    Unreachable(String),
}

impl SessionState {
    pub fn code(&self) -> &'static str {
        match self {
            SessionState::Live => "live",
            SessionState::AlreadyLive => "already-live",
            SessionState::CredentialsUnresolved => "credentials-unresolved",
            SessionState::NoPeerRoster => "no-peer-roster",
            SessionState::Unreachable(_) => "unreachable",
        }
    }
}

/// How long a pump thread blocks on one receive before re-checking its stop
/// flag. Short enough that a detach is prompt, long enough that an idle project
/// costs nothing.
const PUMP_POLL: Duration = Duration::from_millis(500);
/// Renderable items retained per project. The hook's own item budget is smaller;
/// this is the store's ceiling, not the render budget.
const SNAPSHOT_CAPACITY: usize = 64;

/// The live broker sessions this connector process holds — one per enrolled
/// project, in the same process, with no second daemon. Each pumps its received
/// frames through Slice B's `DeliveryProcessor` into the shared snapshot store.
pub struct ProjectSessions {
    snapshots: std::sync::Arc<std::sync::Mutex<SnapshotStore>>,
    live: std::collections::HashMap<String, LiveSession>,
}

struct LiveSession {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl ProjectSessions {
    pub fn new(capacity: usize) -> Self {
        ProjectSessions {
            snapshots: std::sync::Arc::new(std::sync::Mutex::new(
                SnapshotStore::new(capacity).expect("snapshot capacity is a non-zero constant"),
            )),
            live: std::collections::HashMap::new(),
        }
    }

    /// The shared store, so a test (or a future in-process reader) can admit
    /// frames and read them back without a broker.
    pub fn store(&self) -> std::sync::Arc<std::sync::Mutex<SnapshotStore>> {
        std::sync::Arc::clone(&self.snapshots)
    }

    pub fn snapshot(&self, project_id: &str, now: DateTime<Utc>) -> Vec<SnapshotItem> {
        match self.snapshots.lock() {
            Ok(mut store) => store.snapshot(project_id, now),
            // A poisoned store means a pump thread panicked. Serving an empty
            // snapshot is the honest answer; fabricating one is not.
            Err(_) => Vec::new(),
        }
    }

    /// Open (or confirm) the project's live session from an already-resolved
    /// provisioning result. Idempotent. Taking `provisioned` as an argument is
    /// what lets a test drive a real session without the connector holding a
    /// swappable callable.
    pub fn attach(
        &mut self,
        row: &crate::enrollment::EnrolledRow,
        provisioned: Option<(MqttSession, PeerRoster)>,
        now: DateTime<Utc>,
    ) -> SessionState {
        if self.live.contains_key(&row.project_id) {
            return SessionState::AlreadyLive;
        }
        let Some((session, roster)) = provisioned else {
            return SessionState::CredentialsUnresolved;
        };
        if roster.is_empty() {
            return SessionState::NoPeerRoster;
        }
        let mut transport = match MqttTransport::new(session, ValidationConfig::default(), now) {
            Ok(transport) => transport,
            Err(error) => return SessionState::Unreachable(error.code().to_owned()),
        };
        let identity = match transport.authenticate() {
            Ok(identity) => identity,
            Err(error) => return SessionState::Unreachable(error.code().to_owned()),
        };
        for filter in live_filters(&row.org_id, &row.project_id, &identity) {
            if let Err(error) = transport.subscribe(&filter, false) {
                return SessionState::Unreachable(error.code().to_owned());
            }
        }

        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let thread = std::thread::spawn({
            let stop = std::sync::Arc::clone(&stop);
            let snapshots = std::sync::Arc::clone(&self.snapshots);
            move || pump(transport, roster, snapshots, stop)
        });
        self.live.insert(
            row.project_id.clone(),
            LiveSession {
                stop,
                thread: Some(thread),
            },
        );
        SessionState::Live
    }

    /// Stop the project's session, if any. A detached project keeps no session
    /// and no snapshot.
    pub fn detach(&mut self, project_id: &str) -> bool {
        let Some(mut session) = self.live.remove(project_id) else {
            return false;
        };
        session
            .stop
            .store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(thread) = session.thread.take() {
            let _ = thread.join();
        }
        if let Ok(mut store) = self.snapshots.lock() {
            store.projects.remove(project_id);
        }
        true
    }

    pub fn is_live(&self, project_id: &str) -> bool {
        self.live.contains_key(project_id)
    }
}

/// A live session hears colleagues, not only itself: every origin's events and
/// state for the project, plus this connector's own three typed inboxes. Slice
/// B's per-frame origin and principal checks are what actually bound admission;
/// the filters only decide what the broker sends.
fn live_filters(org_id: &str, project_id: &str, identity: &SessionIdentity) -> Vec<String> {
    let base = format!("loam/v1/{org_id}/{project_id}");
    vec![
        format!("{base}/event/+"),
        format!("{base}/state/+/+"),
        format!("{base}/inbox/instance/{}/+/+", identity.instance_id),
        format!("{base}/inbox/principal/{}/+/+", identity.principal_id),
        format!("{base}/inbox/agent/{}/+/+", identity.agent_id),
    ]
}

/// Pump one project's received frames into the snapshot store until stopped. The
/// pump only reads: it never publishes, and a lost session simply goes quiet
/// rather than fabricating state.
fn pump(
    mut transport: MqttTransport,
    roster: PeerRoster,
    snapshots: std::sync::Arc<std::sync::Mutex<SnapshotStore>>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    while !stop.load(std::sync::atomic::Ordering::Relaxed) {
        match transport.receive_outcome(PUMP_POLL, Utc::now(), &roster) {
            Ok(Some((topic, outcome))) => {
                if let Ok(mut store) = snapshots.lock() {
                    store.admit(&topic, &outcome);
                }
            }
            Ok(None) => {}
            Err(_) => break,
        }
    }
    transport.disconnect();
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
    let mut state = ConnectorState::new();
    // Bring up a live session for every already-enrolled project before serving,
    // so a hook's first snapshot read does not have to wait for an attach.
    attach_enrolled(&db_path, &mut state);
    accept_loop(&endpoint, &db_path, &mut state);
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
fn accept_loop(endpoint: &ipc::unix::OwnedEndpoint, db_path: &Path, state: &mut ConnectorState) {
    let config = IpcConfig::default();
    loop {
        // One failed connection never takes the connector down; keep serving.
        let _ = serve_one(endpoint, db_path, &config, state);
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
    state: &mut ConnectorState,
) -> Result<(), ipc::IpcError> {
    let mut connection = endpoint.accept_verified()?;
    serve_connection(&mut connection, db_path, config, state)
}

/// One request/response exchange on an already owner-proven connection. Both
/// platforms share it, so the codec, dispatch, and error shape cannot drift
/// between them; the peer proof stays with each platform's accept.
fn serve_connection<S: std::io::Read + std::io::Write>(
    connection: &mut S,
    db_path: &Path,
    config: &IpcConfig,
    state: &mut ConnectorState,
) -> Result<(), ipc::IpcError> {
    let frame = ipc::read_frame(connection, config)?;
    let response = match ipc::parse_request(&frame, config) {
        Ok(request) => dispatch(&request, db_path, config, state),
        Err(error) => ipc::error_response("", &error, config),
    };
    ipc::write_frame(connection, &response, config)
}

/// Run the connector on Windows. Same contract as the Unix path: the registry
/// decides whether an endpoint exists at all, and the peer's SID is proven
/// inside `accept_verified` before the codec sees a byte.
#[cfg(windows)]
pub fn run_service(global_root: &Path) -> Result<ServiceOutcome, ServiceError> {
    let db_path = global_root.join("loam.sqlite3");
    if !registry_has_enrollments(&db_path)? {
        return Ok(ServiceOutcome::Inert);
    }
    let endpoint = ipc::windows::bind(global_root).map_err(ServiceError::Ipc)?;
    let mut state = ConnectorState::new();
    attach_enrolled(&db_path, &mut state);
    let config = IpcConfig::default();
    // The named-pipe accept is bounded, so the loop wakes regularly instead of
    // blocking forever; a timeout is simply "no client yet".
    let accept_wait = config.lifecycle_deadline;
    loop {
        match endpoint.accept_verified(accept_wait) {
            Ok(served) => {
                let mut served = served.with_io_deadline(config.read_deadline);
                let _ = serve_connection(&mut served, &db_path, &config, &mut state);
            }
            // Neither an idle wait nor a rejected peer takes the connector down.
            Err(ipc::IpcError::Timeout) | Err(ipc::IpcError::UnauthorizedPeer) => {}
            Err(error) => return Err(ServiceError::Ipc(error)),
        }
    }
}

/// Resolve the request's workspace through the registry, enforce the project
/// binding, and run the closed operation. Returns an encoded response body.
fn dispatch(
    request: &Request,
    db_path: &Path,
    config: &IpcConfig,
    state: &mut ConnectorState,
) -> Vec<u8> {
    match resolve_and_run(request, db_path, state) {
        Ok(result) => ipc::ok_response(&request.request_id, result),
        Err(error) => ipc::error_response(&request.request_id, &error, config),
    }
}

fn resolve_and_run(
    request: &Request,
    db_path: &Path,
    state: &mut ConnectorState,
) -> Result<crate::json::Value, ipc::IpcError> {
    // Resolve the workspace to its physical identity exactly as enrollment did,
    // so a path alias resolves to the same enrollment and a non-workspace path is
    // treated as unenrolled.
    let workspace = crate::enrollment::PhysicalWorkspace::resolve(Path::new(&request.workspace))
        .map_err(|_| ipc::IpcError::WorkspaceUnenrolled)?;
    let key = crate::enrollment::identity_key(&workspace);
    dispatch_for_key(request, &key, db_path, state)
}

/// Dispatch a request that has already been resolved to a physical identity key.
/// Separated from workspace resolution so the registry-binding and operation
/// logic is testable without a Git workspace.
fn dispatch_for_key(
    request: &Request,
    key: &str,
    db_path: &Path,
    state: &mut ConnectorState,
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
            // The enrollment already exists (looked up above). Open the live
            // subscribed broker session in this same process — no second daemon,
            // no per-project process — and report what actually happened rather
            // than an unconditional acknowledgement.
            let session_state = state
                .sessions
                .attach(&row, provision_session(&row), Utc::now());
            Ok(attach_json(&row, &session_state))
        }
        Operation::ProjectDetach => {
            let mut write =
                crate::enrollment::open_writable(db_path).map_err(|_| ipc::IpcError::Internal)?;
            let removed = crate::enrollment::delete_enrollment(&mut write, key)
                .map_err(|_| ipc::IpcError::Internal)?;
            if removed {
                // Any live inject channels for this project become moot; the real
                // per-session drop is driven by Slice E's session end. The live
                // broker session and its snapshot go now, so a detached project
                // is never readable.
                state.sessions.detach(&row.project_id);
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
            state.channels.register(InjectChannel {
                session_id: session_id.to_owned(),
                project_id: row.project_id.clone(),
                channel_ref: channel_ref.to_owned(),
            });
            Ok(register_ack_json(session_id, &row.project_id))
        }
        Operation::SnapshotGet => {
            // A read. Enrollment and project binding were already proven above,
            // so an unenrolled or cross-project caller never reaches here. The
            // snapshot is served from memory: nothing is opened for writing,
            // nothing is persisted, and no envelope bytes leave the connector.
            let items = state.sessions.snapshot(&row.project_id, Utc::now());
            Ok(snapshot_json(&row.project_id, &items))
        }
    }
}

/// Bring up a live session for every project already in the registry. Failure to
/// provision one project never blocks the others or the endpoint.
fn attach_enrolled(db_path: &Path, state: &mut ConnectorState) {
    let rows = match crate::enrollment::open_readonly(db_path) {
        Ok(Some(connection)) => {
            crate::enrollment::list_enrollments(&connection).unwrap_or_default()
        }
        _ => Vec::new(),
    };
    let now = Utc::now();
    for row in &rows {
        let _ = state.sessions.attach(row, provision_session(row), now);
    }
}

/// The normalized snapshot projection. Already deduped and expiry-filtered by
/// the store, and deliberately body-shaped: `source`, `type`, `summary`, `to`,
/// `context`, sender attribution, and the preserved payload — never envelope
/// bytes, a credential, or a raw remote URL.
fn snapshot_json(project_id: &str, items: &[SnapshotItem]) -> crate::json::Value {
    use crate::json::Value;
    let rendered = items
        .iter()
        .map(|item| {
            Value::Object(vec![
                ("source".into(), Value::String(item.source.clone())),
                ("type".into(), Value::String(item.item_type.clone())),
                ("summary".into(), Value::String(item.summary.clone())),
                (
                    "to".into(),
                    Value::Array(
                        item.to
                            .iter()
                            .map(|(kind, id)| {
                                Value::Object(vec![
                                    ("kind".into(), Value::String(kind.clone())),
                                    ("id".into(), Value::String(id.clone())),
                                ])
                            })
                            .collect(),
                    ),
                ),
                (
                    "context".into(),
                    Value::Object(vec![
                        ("org_id".into(), Value::String(item.org_id.clone())),
                        ("project_id".into(), Value::String(item.project_id.clone())),
                        (
                            "repository_id".into(),
                            Value::String(item.repository_id.clone()),
                        ),
                    ]),
                ),
                (
                    "from".into(),
                    Value::Object(vec![
                        (
                            "principal_id".into(),
                            Value::String(item.from_principal_id.clone()),
                        ),
                        ("agent_id".into(), Value::String(item.from_agent_id.clone())),
                        (
                            "instance_id".into(),
                            Value::String(item.from_instance_id.clone()),
                        ),
                    ]),
                ),
                ("payload".into(), item.payload.clone()),
            ])
        })
        .collect();
    Value::Object(vec![
        ("schema".into(), Value::Number("1".into())),
        ("project_id".into(), Value::String(project_id.to_owned())),
        ("items".into(), Value::Array(rendered)),
    ])
}

/// The attach projection. `session_state` is observational: an unopened session
/// is reported as such, never as attached-and-live.
fn attach_json(
    row: &crate::enrollment::EnrolledRow,
    session_state: &SessionState,
) -> crate::json::Value {
    use crate::json::Value;
    let mut fields = vec![
        ("schema".into(), Value::Number("1".into())),
        ("action".into(), Value::String("attached".into())),
        ("project_id".into(), Value::String(row.project_id.clone())),
        (
            "session_state".into(),
            Value::String(session_state.code().to_owned()),
        ),
    ];
    if let SessionState::Unreachable(reason) = session_state {
        fields.push(("session_diagnostic".into(), Value::String(reason.clone())));
    }
    Value::Object(fields)
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
            &mut ConnectorState::new(),
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
            &mut ConnectorState::new(),
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
            &mut ConnectorState::new(),
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
            &mut ConnectorState::new(),
        )
        .expect("detach");
        let after = dispatch_for_key(
            &request(Operation::StatusGet, crate::json::Value::Object(vec![])),
            &key,
            &path,
            &mut ConnectorState::new(),
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
        let mut state = ConnectorState::new();
        let result = dispatch_for_key(
            &register_request("sess-1", "chan-token-1"),
            &key,
            &path,
            &mut state,
        )
        .expect("register");
        assert!(result.to_json().contains("inject-channel-registered"));
        assert!(state.channels.contains("sess-1"));
        assert_eq!(state.channels.len(), 1);

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

    /// A whole snapshot session — attach, admit real frames, read repeatedly —
    /// must leave the database byte-identical (Slice D T1). The snapshot is
    /// in-memory only: no snapshot table, no schema bump, no enrollment churn.
    #[test]
    fn a_full_snapshot_session_leaves_sqlite_byte_unchanged() {
        let (path, key) = enrolled_db("snapshot-nonpersistence", 9, 90);
        // One long-lived read connection is the witness: SQLite bumps
        // `data_version` on it whenever *another* connection commits a write, so
        // an unchanged value is a real "nothing was written" proof — and unlike a
        // byte comparison it needs no filesystem capability, which this module
        // deliberately does not have.
        let witness = crate::enrollment::open_readonly(&path).unwrap().unwrap();
        let data_version = |connection: &rusqlite::Connection| -> i64 {
            connection
                .query_row("PRAGMA data_version", [], |r| r.get(0))
                .unwrap()
        };
        let schema_sql = |connection: &rusqlite::Connection| -> String {
            let mut statement = connection
                .prepare("SELECT COALESCE(group_concat(name || '|' || COALESCE(sql, '')), '') FROM sqlite_master ORDER BY name")
                .unwrap();
            statement.query_row([], |r| r.get(0)).unwrap()
        };
        let before_version = data_version(&witness);
        let before_schema_sql = schema_sql(&witness);
        let before_schema: i64 = witness
            .query_row("SELECT version FROM federation_schema", [], |r| r.get(0))
            .unwrap();

        let mut state = ConnectorState::new();
        // Attach opens no session here (nothing is provisioned) but must still
        // answer honestly rather than claiming a live broker session.
        let attach = dispatch_for_key(
            &request(Operation::ProjectAttach, crate::json::Value::Object(vec![])),
            &key,
            &path,
            &mut state,
        )
        .expect("attach");
        assert!(
            attach.to_json().contains("credentials-unresolved"),
            "an unprovisioned attach must report its real session state: {}",
            attach.to_json()
        );

        // Admit real frames straight into the store the dispatch reads from, so
        // the read below returns something and the absence of writes is not
        // vacuous.
        {
            let store = state.sessions.store();
            let mut store = store.lock().unwrap();
            store.store(
                "loam",
                SnapshotItem {
                    key: "state:instance-01/work-SB-42".into(),
                    source: "urn:loam:instance:instance-01".into(),
                    item_type: "io.loam.work.state".into(),
                    summary: "Work is active.".into(),
                    to: vec![("project".into(), "loam".into())],
                    org_id: "org-3A1".into(),
                    project_id: "loam".into(),
                    repository_id: "repo-2F8".into(),
                    from_principal_id: "employee-184".into(),
                    from_agent_id: "agent-72".into(),
                    from_instance_id: "instance-01".into(),
                    payload: crate::json::Value::Object(vec![]),
                    expires_at: Utc::now() + chrono::Duration::hours(1),
                },
            );
        }

        for _ in 0..3 {
            let snapshot = dispatch_for_key(
                &request(Operation::SnapshotGet, crate::json::Value::Object(vec![])),
                &key,
                &path,
                &mut state,
            )
            .expect("snapshot");
            let text = snapshot.to_json();
            assert!(
                text.contains("Work is active."),
                "the snapshot read must serve the held item: {text}"
            );
            // A read never consumes: the same unresolved item is still there.
        }

        // The database is untouched: no committed write, the same table
        // inventory, the same schema version, the same enrollment count, and no
        // snapshot table was invented.
        assert_eq!(
            data_version(&witness),
            before_version,
            "a snapshot session must commit no write to SQLite"
        );
        assert_eq!(
            schema_sql(&witness),
            before_schema_sql,
            "a snapshot session must add no table"
        );
        let after_schema: i64 = witness
            .query_row("SELECT version FROM federation_schema", [], |r| r.get(0))
            .unwrap();
        assert_eq!(before_schema, after_schema);
        let snapshot_tables: i64 = witness
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name LIKE '%snapshot%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(snapshot_tables, 0, "no snapshot table may exist in SQLite");
        assert_eq!(
            crate::enrollment::list_enrollments(&witness).unwrap().len(),
            1
        );

        // Positive control: the witness *can* see a write. A real committed
        // insert from another connection must advance the very `data_version`
        // asserted unchanged above, so "unchanged" is evidence of no write
        // rather than of a blind witness.
        let mut writer = crate::enrollment::open_writable(&path).unwrap();
        crate::enrollment::insert_enrollment(&mut writer, &synthetic(19, 190), &caps(), "t")
            .unwrap();
        assert_ne!(
            data_version(&witness),
            before_version,
            "the witness must observe a real committed write, or the unchanged assertion is vacuous"
        );
        assert_eq!(
            crate::enrollment::list_enrollments(&witness).unwrap().len(),
            2
        );
    }

    /// The snapshot read inherits `dispatch_for_key`'s enrollment resolution and
    /// project-binding proof rather than sitting beside them.
    #[test]
    fn the_snapshot_read_rejects_unenrolled_and_cross_project_callers() {
        let (path, key) = enrolled_db("snapshot-binding", 10, 100);
        let mut state = ConnectorState::new();

        let unenrolled = dispatch_for_key(
            &request(Operation::SnapshotGet, crate::json::Value::Object(vec![])),
            "unix:404:404",
            &path,
            &mut state,
        );
        assert_eq!(unenrolled.err(), Some(ipc::IpcError::WorkspaceUnenrolled));

        let cross_project = dispatch_for_key(
            &request(
                Operation::SnapshotGet,
                crate::json::Value::Object(vec![(
                    "project_id".into(),
                    crate::json::Value::String("someone-elses-project".into()),
                )]),
            ),
            &key,
            &path,
            &mut state,
        );
        assert_eq!(
            cross_project.err(),
            Some(ipc::IpcError::ProjectBindingMismatch)
        );
    }

    #[test]
    fn register_inject_requires_an_enrolled_workspace() {
        let (path, _key) = enrolled_db("register-unenrolled", 7, 70);
        let outcome = dispatch_for_key(
            &register_request("sess-x", "chan"),
            "unix:404:404",
            &path,
            &mut ConnectorState::new(),
        );
        assert_eq!(outcome.err(), Some(ipc::IpcError::WorkspaceUnenrolled));
    }

    #[test]
    fn a_channel_is_dropped_on_session_end() {
        let mut state = ConnectorState::new();
        state.channels.register(InjectChannel {
            session_id: "sess-2".into(),
            project_id: "loam".into(),
            channel_ref: "c".into(),
        });
        assert!(state.channels.contains("sess-2"));
        assert!(state.channels.drop_session("sess-2"));
        assert!(!state.channels.contains("sess-2"));
        assert!(!state.channels.drop_session("sess-2")); // idempotent
    }

    #[test]
    fn a_restart_starts_with_an_empty_registry() {
        // Register a channel for real, against a real enrolled database, so the
        // absence below is the *loss* of something that existed rather than a
        // registry that was never populated.
        let (path, key) = enrolled_db("restart", 8, 80);
        let mut before = ConnectorState::new();
        dispatch_for_key(
            &register_request("sess-restart", "chan-restart"),
            &key,
            &path,
            &mut before,
        )
        .expect("register");
        assert!(before.channels.contains("sess-restart"));

        // The restart: the process-local registry is gone, the database is not.
        drop(before);
        let after = ConnectorState::new();
        assert!(
            after.channels.is_empty(),
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

#[cfg(test)]
mod snapshot_tests {
    //! The bounded in-memory snapshot contract (Slice D T1).
    //!
    //! Every case drives real frames through Slice B's `DeliveryProcessor` — the
    //! single validator, deduplicator, and expiry tracker — and then reads the
    //! store back, so "exactly one logical item per message" is proven against
    //! the actual dedupe path rather than a hand-written stub of it.

    use super::*;
    use crate::json::Value;
    use crate::transport::DeliveryProcessor;

    const CASES: &str = include_str!("../tests/fixtures/mqtt/harness-snapshot-cases.json");
    const SENDER_INSTANCE: &str = "instance-01";
    const SENDER_PRINCIPAL: &str = "employee-184";
    const RECIPIENT_INSTANCE: &str = "instance-02";

    fn base_time() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-07-24T14:20:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn number(value: &Value, key: &str) -> Option<i64> {
        match value.get(key) {
            Some(Value::Number(literal)) => literal.parse().ok(),
            _ => None,
        }
    }

    fn flag(value: &Value, key: &str) -> bool {
        matches!(value.get(key), Some(Value::Bool(true)))
    }

    /// One frame's topic and wire bytes. An empty body is a tombstone.
    fn frame(frame: &Value, org: &str, project: &str, now: DateTime<Utc>) -> (String, Vec<u8>) {
        let kind = frame.get("kind").and_then(Value::as_str).expect("kind");
        let expires =
            now + chrono::Duration::seconds(number(frame, "expires_in_seconds").unwrap_or(86_400));
        let expires = expires.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let time = now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let summary = frame
            .get("summary")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        match kind {
            "inbox" => {
                let message_id = frame
                    .get("message_id")
                    .and_then(Value::as_str)
                    .expect("message_id");
                let topic = format!(
                    "loam/v1/{org}/{project}/inbox/instance/{RECIPIENT_INSTANCE}/{SENDER_INSTANCE}/{message_id}"
                );
                let body = envelope_json(
                    message_id,
                    "io.loam.message",
                    "urn:loam:schema:message:1",
                    &time,
                    &expires,
                    &summary,
                    project,
                    org,
                    Value::Array(vec![Value::Object(vec![
                        ("kind".into(), Value::String("instance".into())),
                        ("id".into(), Value::String(RECIPIENT_INSTANCE.into())),
                    ])]),
                    Value::Object(vec![("class".into(), Value::String("inbox".into()))]),
                    Value::Object(vec![
                        ("action".into(), Value::String("collaboration.note".into())),
                        ("params".into(), Value::Object(vec![])),
                        ("response_status".into(), Value::Null),
                    ]),
                );
                (topic, body.into_bytes())
            }
            "state" => {
                let key = frame
                    .get("state_key")
                    .and_then(Value::as_str)
                    .expect("state_key");
                let topic = format!("loam/v1/{org}/{project}/state/{SENDER_INSTANCE}/{key}");
                if flag(frame, "tombstone") {
                    // An empty MQTT payload is the tombstone: Slice B resolves it
                    // and the store must drop the same logical item.
                    return (topic, Vec::new());
                }
                let revision = frame
                    .get("revision")
                    .and_then(Value::as_str)
                    .unwrap_or("1")
                    .to_owned();
                let body = envelope_json(
                    "01K6Q6ESWMT48TPB",
                    "io.loam.work.state",
                    "urn:loam:schema:work-state:1",
                    &time,
                    &expires,
                    &summary,
                    project,
                    org,
                    Value::Array(vec![Value::Object(vec![
                        ("kind".into(), Value::String("project".into())),
                        ("id".into(), Value::String(project.to_owned())),
                    ])]),
                    Value::Object(vec![
                        ("class".into(), Value::String("latest-state".into())),
                        ("key".into(), Value::String(key.to_owned())),
                        ("revision".into(), Value::Number(revision)),
                    ]),
                    Value::Object(vec![
                        ("state".into(), Value::String("active".into())),
                        ("acceptance".into(), Value::Object(vec![])),
                        ("verification".into(), Value::Array(vec![])),
                    ]),
                );
                (topic, body.into_bytes())
            }
            other => panic!("unknown frame kind `{other}`"),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn envelope_json(
        id: &str,
        message_type: &str,
        dataschema: &str,
        time: &str,
        expires: &str,
        summary: &str,
        project: &str,
        org: &str,
        to: Value,
        delivery: Value,
        payload: Value,
    ) -> String {
        Value::Object(vec![
            ("specversion".into(), Value::String("1.0".into())),
            ("id".into(), Value::String(id.to_owned())),
            (
                "source".into(),
                Value::String(format!("urn:loam:instance:{SENDER_INSTANCE}")),
            ),
            ("type".into(), Value::String(message_type.to_owned())),
            ("time".into(), Value::String(time.to_owned())),
            (
                "datacontenttype".into(),
                Value::String("application/json".into()),
            ),
            ("dataschema".into(), Value::String(dataschema.to_owned())),
            (
                "data".into(),
                Value::Object(vec![
                    ("intent".into(), Value::String("inform".into())),
                    (
                        "from".into(),
                        Value::Object(vec![
                            (
                                "principal_id".into(),
                                Value::String(SENDER_PRINCIPAL.into()),
                            ),
                            ("agent_id".into(), Value::String("agent-72".into())),
                            ("instance_id".into(), Value::String(SENDER_INSTANCE.into())),
                        ]),
                    ),
                    ("to".into(), to),
                    ("delivery".into(), delivery),
                    (
                        "thread".into(),
                        Value::Object(vec![
                            ("id".into(), Value::String("thread-01K6Q5".into())),
                            ("correlation_id".into(), Value::String(id.to_owned())),
                            ("causation_id".into(), Value::Null),
                        ]),
                    ),
                    (
                        "context".into(),
                        Value::Object(vec![
                            ("org_id".into(), Value::String(org.to_owned())),
                            ("project_id".into(), Value::String(project.to_owned())),
                            ("repository_id".into(), Value::String("repo-2F8".into())),
                            (
                                "git".into(),
                                Value::Object(vec![
                                    (
                                        "base_oid".into(),
                                        Value::String(
                                            "84be000000000000000000000000000000000002".into(),
                                        ),
                                    ),
                                    (
                                        "plan_oid".into(),
                                        Value::String(
                                            "61af000000000000000000000000000000000001".into(),
                                        ),
                                    ),
                                ]),
                            ),
                            ("artifacts".into(), Value::Array(vec![])),
                        ]),
                    ),
                    ("expires_at".into(), Value::String(expires.to_owned())),
                    ("summary".into(), Value::String(summary.to_owned())),
                    ("payload".into(), payload),
                ]),
            ),
        ])
        .to_json()
    }

    fn keys(items: &[SnapshotItem]) -> Vec<String> {
        items.iter().map(|item| item.key.clone()).collect()
    }

    #[test]
    fn the_snapshot_store_honors_every_recorded_case() {
        let cases = crate::json::parse(CASES).expect("fixture parses");
        let org = cases.get("org_id").and_then(Value::as_str).unwrap();
        let project = cases.get("project_id").and_then(Value::as_str).unwrap();
        let other_project = cases
            .get("other_project_id")
            .and_then(Value::as_str)
            .unwrap();
        let now = base_time();

        for case in cases.get("cases").and_then(Value::as_array).unwrap() {
            let name = case.get("name").and_then(Value::as_str).unwrap();
            let capacity = number(case, "capacity").unwrap() as usize;
            let mut store = SnapshotStore::new(capacity).expect("capacity is non-zero");
            // Generously sized so the store's own capacity, not the processor's
            // tracking window, is what the overflow case exercises.
            let mut processor =
                DeliveryProcessor::new(ValidationConfig::default(), 64, 64, 64).expect("processor");
            let claims = [SENDER_PRINCIPAL];
            let origins = [SENDER_INSTANCE];
            let identity = AuthenticatedTransportPrincipal::new(
                AuthenticatedPrincipal::new(SENDER_PRINCIPAL, &claims),
                &origins,
            );

            for value in case.get("frames").and_then(Value::as_array).unwrap() {
                let scope = if flag(value, "other_project") {
                    other_project
                } else {
                    project
                };
                let (topic, bytes) = frame(value, org, scope, now);
                let outcome = processor
                    .receive(&topic, &bytes, &identity, now)
                    .unwrap_or_else(|error| panic!("{name}: frame rejected: {error:?}"));
                store.admit(&topic, &outcome);
            }

            let read_at =
                now + chrono::Duration::seconds(number(case, "read_after_seconds").unwrap_or(0));
            let expected: Vec<String> = case
                .get("expect")
                .and_then(Value::as_array)
                .unwrap()
                .iter()
                .map(|value| value.as_str().unwrap().to_owned())
                .collect();

            // Keys alone would not catch an in-place *content* update, so a case
            // may also pin the served summaries.
            let expected_summaries: Option<Vec<String>> = case
                .get("expect_summaries")
                .and_then(Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .map(|value| value.as_str().unwrap().to_owned())
                        .collect()
                });

            // A read never consumes: an unresolved item must still be there on
            // the next hook, so repeated reads must agree.
            for round in 0..number(case, "reads").unwrap_or(1) {
                let items = store.snapshot(project, read_at);
                assert_eq!(
                    keys(&items),
                    expected,
                    "{name}: snapshot mismatch on read {round}"
                );
                if let Some(summaries) = &expected_summaries {
                    assert_eq!(
                        &items
                            .iter()
                            .map(|item| item.summary.clone())
                            .collect::<Vec<_>>(),
                        summaries,
                        "{name}: served summary mismatch on read {round}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_zero_capacity_store_is_refused() {
        assert!(SnapshotStore::new(0).is_err());
    }

    #[test]
    fn a_restarted_connector_serves_an_empty_snapshot() {
        // Retained state and the inbox re-deliver on reconnect, so the correct
        // post-restart snapshot is empty rather than recovered from disk.
        let mut before = SnapshotStore::new(4).expect("capacity");
        let now = base_time();
        let (topic, bytes) = frame(
            &crate::json::parse(
                r#"{"kind":"inbox","message_id":"01K6Q6ESWMT48TPD","summary":"Held."}"#,
            )
            .unwrap(),
            "org-3A1",
            "project-7M3",
            now,
        );
        let claims = [SENDER_PRINCIPAL];
        let origins = [SENDER_INSTANCE];
        let identity = AuthenticatedTransportPrincipal::new(
            AuthenticatedPrincipal::new(SENDER_PRINCIPAL, &claims),
            &origins,
        );
        let mut processor =
            DeliveryProcessor::new(ValidationConfig::default(), 8, 8, 8).expect("processor");
        let outcome = processor.receive(&topic, &bytes, &identity, now).unwrap();
        assert!(before.admit(&topic, &outcome));
        assert_eq!(before.len("project-7M3"), 1);

        drop(before);
        let mut after = SnapshotStore::new(4).expect("capacity");
        assert!(after.snapshot("project-7M3", now).is_empty());
        assert!(after.is_empty());
    }

    #[test]
    fn an_unprovisioned_attach_opens_no_session_and_says_so() {
        // Phase 1 ships no secret backend and no peer roster, so the honest
        // answer is `credentials-unresolved` — never a fabricated live session.
        let mut sessions = ProjectSessions::new(4);
        let row = crate::enrollment::EnrolledRow {
            identity_key: "unix:1:1".into(),
            org_id: "org-3A1".into(),
            project_id: "project-7M3".into(),
            repository_id: "repo-2F8".into(),
            descriptor_digest: "d".into(),
            display_path: "/w".into(),
            instance_id: RECIPIENT_INSTANCE.into(),
            broker_profile: "p".into(),
            commit: "84be000000000000000000000000000000000001".into(),
            capabilities: crate::enrollment::CapabilityRecord {
                authentication: true,
                publish: true,
                subscribe: true,
                self_receive: true,
                verified_at: "2026-07-24T14:20:00Z".into(),
            },
            remotes: Vec::new(),
        };
        assert_eq!(
            sessions.attach(&row, provision_session(&row), base_time()),
            SessionState::CredentialsUnresolved
        );
        assert!(!sessions.is_live(&row.project_id));
        assert!(sessions.snapshot(&row.project_id, base_time()).is_empty());
    }

    #[test]
    fn a_provisioned_but_rosterless_project_opens_no_session() {
        // Credentials without a peer roster would open a session that can admit
        // no colleague; refusing it beats a live session that hears nothing.
        fn rosterless() -> Option<(MqttSession, PeerRoster)> {
            let config = TransportConfig::new(
                "localhost",
                1883,
                "loam-connector-test",
                8,
                400_000,
                ValidationConfig::default(),
            )
            .expect("transport config");
            Some((
                MqttSession {
                    config,
                    username: "actor-a".into(),
                    password: "unused".into(),
                    ca_certificate: Vec::new(),
                    client_authentication: None,
                    claimed_identity: SessionIdentity {
                        principal_id: SENDER_PRINCIPAL.into(),
                        agent_id: "agent-72".into(),
                        instance_id: RECIPIENT_INSTANCE.into(),
                        allowed_claims: Vec::new(),
                    },
                },
                PeerRoster::default(),
            ))
        }

        let mut sessions = ProjectSessions::new(4);
        let row = crate::enrollment::EnrolledRow {
            identity_key: "unix:1:2".into(),
            org_id: "org-3A1".into(),
            project_id: "project-7M3".into(),
            repository_id: "repo-2F8".into(),
            descriptor_digest: "d".into(),
            display_path: "/w".into(),
            instance_id: RECIPIENT_INSTANCE.into(),
            broker_profile: "p".into(),
            commit: "84be000000000000000000000000000000000001".into(),
            capabilities: crate::enrollment::CapabilityRecord {
                authentication: true,
                publish: true,
                subscribe: true,
                self_receive: true,
                verified_at: "2026-07-24T14:20:00Z".into(),
            },
            remotes: Vec::new(),
        };
        assert_eq!(
            sessions.attach(&row, rosterless(), base_time()),
            SessionState::NoPeerRoster
        );
        assert!(!sessions.is_live(&row.project_id));
    }

    #[test]
    fn a_live_session_subscribes_to_colleagues_not_only_itself() {
        let identity = SessionIdentity {
            principal_id: SENDER_PRINCIPAL.into(),
            agent_id: "agent-72".into(),
            instance_id: RECIPIENT_INSTANCE.into(),
            allowed_claims: Vec::new(),
        };
        let filters = live_filters("org-3A1", "project-7M3", &identity);
        assert!(filters.contains(&"loam/v1/org-3A1/project-7M3/event/+".to_string()));
        assert!(filters.contains(&"loam/v1/org-3A1/project-7M3/state/+/+".to_string()));
        assert!(filters
            .iter()
            .any(|filter| filter == "loam/v1/org-3A1/project-7M3/inbox/instance/instance-02/+/+"));
        // Not a self-only event filter: that is the probe's shape, not a live
        // collaboration session's.
        assert!(!filters
            .iter()
            .any(|filter| filter.ends_with("/event/instance-02")));
    }

    #[test]
    fn the_snapshot_projection_carries_no_envelope_bytes_or_credential() {
        let item = SnapshotItem {
            key: "inbox:01K6Q6ESWMT48TPD".into(),
            source: format!("urn:loam:instance:{SENDER_INSTANCE}"),
            item_type: "io.loam.message".into(),
            summary: "Held.".into(),
            to: vec![("instance".into(), RECIPIENT_INSTANCE.into())],
            org_id: "org-3A1".into(),
            project_id: "project-7M3".into(),
            repository_id: "repo-2F8".into(),
            from_principal_id: SENDER_PRINCIPAL.into(),
            from_agent_id: "agent-72".into(),
            from_instance_id: SENDER_INSTANCE.into(),
            payload: crate::json::Value::Object(vec![]),
            expires_at: base_time(),
        };
        let text = snapshot_json("project-7M3", std::slice::from_ref(&item)).to_json();
        for field in [
            "source", "type", "summary", "to", "context", "from", "payload",
        ] {
            assert!(text.contains(field), "the projection must carry `{field}`");
        }
        for forbidden in [
            "specversion",
            "dataschema",
            "password",
            "credential",
            "mqtts://",
        ] {
            assert!(
                !text.contains(forbidden),
                "the projection must not carry `{forbidden}`"
            );
        }
    }
}
