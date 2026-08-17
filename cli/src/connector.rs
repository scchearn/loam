//! The connector: the authority-preserving transport seam, its in-memory
//! stub, and the enrollment connection probe.
//!
//! The transport seam is a trait consumed by **generics** (static dispatch) —
//! never a trait object — so the crate's no-dispatch tripwire stays green and no
//! callable capability is introduced. [`StubTransport`] keeps the probe testable
//! without a broker; [`MqttTransport`] implements the same seam over the
//! transport layer's public `transport` surface and is the only code here that touches it.
//!
//! `AuthenticatedPrincipal` is constructed only inside a transport adapter,
//! after the transport reports an authenticated session. The probe derives every
//! authority-bearing envelope field in trusted code; nothing is caller-supplied.
//!
//! Consumed by the connect orchestration, which retires this
//! module-level allow once the stub and probe are wired to the CLI surface.
#![allow(dead_code)]

use std::io::Write;
use std::time::Duration;

use chrono::{DateTime, Utc};

use crate::envelope::{self, AuthenticatedPrincipal, ValidatedEnvelope, ValidationConfig};

/// What a transport can report and do. Implemented by [`StubTransport`] here and
/// by the real transport adapter. Consumed only through generics.
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
    /// The sender's given name from the same authenticated certificate the
    /// principal came from. Provenance, never authority.
    pub display_name: Option<String>,
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
    /// TLS configuration failed (invalid cert/key or trust store).
    ConfigurationFailure(String),
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
            ProbeError::ConfigurationFailure(_) => "tls_configuration_failure",
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
/// Built as JSON directly (never touching the envelope module) so
/// the connector owns nothing but data. Every value here is derived by the
/// connector in trusted code; none is caller-supplied. The shape mirrors the
/// event-class exemplar so it passes the envelope module's structural, identity, topic,
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
// Real MQTT adapter over the transport
// ---------------------------------------------------------------------------
//
// The only place an accepted broker session becomes an `AuthenticatedPrincipal`:
// no authority exists before the CONNACK, and the caller can never supply one.
// Wire encoding is delegated to `transport::publish` (the single
// encoder) and every received frame is admitted by the `DeliveryProcessor`
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
    /// The validated broker configuration (endpoint, client id, bounds).
    pub config: TransportConfig,
    /// Password authentication, which a provisioned session never uses: mTLS is
    /// the sole authentication and the effective username is the certificate CN
    /// the broker assigns. Sending a username would be an identity claim the
    /// client is not entitled to make. Both stay for the password-port test tier
    /// and are sent only when both are present.
    pub username: Option<String>,
    pub password: Option<String>,
    /// Root certificate store for verifying the broker.
    pub ca_certificate: rustls::RootCertStore,
    pub client_authentication: Option<(Vec<u8>, Vec<u8>)>,
    /// The identity these credentials assert. It becomes authority only after
    /// the broker accepts the connection.
    pub claimed_identity: SessionIdentity,
}

fn build_tls_transport(
    roots: &rustls::RootCertStore,
    client_auth: &Option<(Vec<u8>, Vec<u8>)>,
) -> Result<rumqttc::Transport, &'static str> {
    let builder = rustls::ClientConfig::builder().with_root_certificates(roots.clone());
    let config = if let Some((cert_pem, key_pem)) = client_auth {
        use std::io::Cursor;
        let mut cert_reader = Cursor::new(cert_pem.as_slice());
        let certs = rustls_pemfile::certs(&mut cert_reader)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| "certificate PEM parsing failed")?;
        let mut key_reader = Cursor::new(key_pem.as_slice());
        let key = rustls_pemfile::private_key(&mut key_reader)
            .map_err(|_| "private key PEM parsing failed")?
            .ok_or("no private key in PEM")?;
        if certs.is_empty() {
            return Err("empty certificate in PEM");
        }
        builder
            .with_client_auth_cert(certs, key)
            .map_err(|_| "TLS client auth configuration failed")?
    } else {
        builder.with_no_client_auth()
    };
    Ok(rumqttc::Transport::tls_with_config(
        rumqttc::TlsConfiguration::Rustls(std::sync::Arc::new(config)),
    ))
}

/// The real transport: a connected rumqttc client plus the delivery
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
    /// How long the CONNACK wait may run before the dial is abandoned. The probe
    /// keeps the short default; a live session sets the longer `DIAL_DEADLINE`,
    /// so a wedged SYN cannot block one establishment cycle forever.
    dial_deadline: Duration,
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
            dial_deadline: ACK_TIMEOUT,
        })
    }

    /// Extend the CONNACK wait for a live session's establishment cycle. The
    /// probe leaves the default; a session dials with `DIAL_DEADLINE`.
    fn set_dial_deadline(&mut self, deadline: Duration) {
        self.dial_deadline = deadline;
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
        // Only when both are present: an empty username is still a username on
        // the wire, and an mTLS broker that assigns the CN would refuse it.
        if let (Some(username), Some(password)) = (&self.session.username, &self.session.password) {
            if !username.is_empty() {
                options.set_credentials(username, password);
            }
        }
        let tls_transport = build_tls_transport(
            &self.session.ca_certificate,
            &self.session.client_authentication,
        )
        .map_err(|e| ProbeError::ConfigurationFailure(e.to_string()))?;
        options
            .set_transport(tls_transport)
            .set_keep_alive(KEEP_ALIVE)
            .set_clean_start(true);
        let (client, mut connection) = Client::new(options, REQUEST_CAPACITY);
        let deadline = Instant::now() + self.dial_deadline;
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
        // The probe is never retained, and the transport derives both the topic and
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
    /// Publish a raw retained payload on a broker-track topic (a self-announced
    /// member card). The card is not a loam envelope — it is the connector's own
    /// retained card, so it is published verbatim and never routed through the
    /// envelope encoder. Requires an authenticated session.
    fn publish_raw_retained(&mut self, topic: &str, payload: Vec<u8>) -> Result<(), ProbeError> {
        if self.client.is_none() {
            return Err(ProbeError::PublishDenied);
        }
        let client = self.client.clone().expect("checked above");
        let (client, connection) = (client, self.connection.as_mut().expect("checked above"));
        client
            .publish(
                topic,
                rumqttc::v5::mqttbytes::QoS::AtLeastOnce,
                true,
                payload,
            )
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

    /// Ship one already-validated outbound envelope on the live session. Unlike
    /// the probe's publish this honors the transport's retain derivation (state and
    /// inbox are retained; an event is not) and takes `now` per call, because a
    /// live session outlives the timestamp it was constructed with.
    fn publish_outbound(
        &mut self,
        envelope: &ValidatedEnvelope,
        now: DateTime<Utc>,
    ) -> Result<(), ProbeError> {
        let (Some(client), Some(connection)) = (self.client.clone(), self.connection.as_mut())
        else {
            return Err(ProbeError::PublishDenied);
        };
        crate::transport::publish(&client, envelope.clone(), now)
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

    /// Pump exactly one inbound frame through the `DeliveryProcessor` and
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
// Inert-by-default connector service loop
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

/// The connector's volatile in-process state: the inject-channel
/// registry, the per-session mailbox queues, and the live project sessions with
/// their snapshot store. All of it dies with the process and none of it is ever
/// written to SQLite.
pub struct ConnectorState {
    pub channels: ChannelRegistry,
    pub sessions: ProjectSessions,
}

impl ConnectorState {
    pub fn new() -> Self {
        // ONE registry, shared by construction: `channels` is the IPC side
        // (SessionRegisterInject writes here) and the pump side (wake_all and
        // mailbox push read from here) must see the same registrations. A
        // second registry inside `ProjectSessions` was the live-wake defect: IPC
        // registrations landed in one Arc and the pump's `wake_targets`/`push`
        // read the other, so no wake ever fired in production.
        let channels = ChannelRegistry::new();
        ConnectorState {
            sessions: ProjectSessions::new(SNAPSHOT_CAPACITY, channels.clone()),
            channels,
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
/// amendment, T18) and per-session mailbox queue (T2). Held only for the life
/// of one connector process: a restart drops every channel and every mailbox,
/// and nothing here is ever written to the SQLite registry. Injection over a
/// channel is live injection; the connector only admits, holds, hands back,
/// and drops it.
#[derive(Debug, Default, Clone)]
pub struct ChannelRegistry {
    inner: std::sync::Arc<std::sync::Mutex<MailboxInner>>,
}

/// The shared mailbox state: the channel registry and the per-session bounded
/// queues. One mutex guards both so the receive path can atomically see which
/// sessions belong to a project and enqueue for exactly those — a session that
/// registers between the lookup and the push is not missed, and one that drops
/// between them is not written to.
#[derive(Debug, Default)]
struct MailboxInner {
    sessions: std::collections::HashMap<String, InjectChannel>,
    mailboxes: std::collections::HashMap<String, std::collections::VecDeque<SnapshotItem>>,
}

/// One registered inject channel. `channel_ref` is opaque: the plugin hands it
/// over and the connector holds it without interpreting it. `wake_ref` is the
/// optional wake target the connector fires on new items — either or both may
/// be absent (a registration may be mailbox-only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InjectChannel {
    pub session_id: String,
    pub project_id: String,
    pub channel_ref: Option<String>,
    pub wake_ref: Option<String>,
}

impl ChannelRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, channel: InjectChannel) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.sessions.insert(channel.session_id.clone(), channel);
        }
    }

    pub fn drop_session(&mut self, session_id: &str) -> bool {
        if let Ok(mut inner) = self.inner.lock() {
            let removed = inner.sessions.remove(session_id).is_some();
            inner.mailboxes.remove(session_id);
            return removed;
        }
        false
    }

    pub fn contains(&self, session_id: &str) -> bool {
        self.inner
            .lock()
            .map(|inner| inner.sessions.contains_key(session_id))
            .unwrap_or(false)
    }

    pub fn len(&self) -> usize {
        self.inner
            .lock()
            .map(|inner| inner.sessions.len())
            .unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.inner
            .lock()
            .map(|inner| inner.sessions.is_empty())
            .unwrap_or(true)
    }

    /// Enqueue one admitted item into every registered session's mailbox for
    /// the given project. Non-blocking: a full queue drops the oldest item
    /// (same eviction as the snapshot store). Never blocks the receive loop.
    pub fn push(&self, project_id: &str, item: &SnapshotItem, capacity: usize) {
        if let Ok(mut inner) = self.inner.lock() {
            // Collect the matching session ids first: the mailboxes map is
            // borrowed mutably per entry, so the sessions map cannot stay
            // borrowed across it.
            let session_ids: Vec<String> = inner
                .sessions
                .values()
                .filter(|channel| channel.project_id == project_id)
                .map(|channel| channel.session_id.clone())
                .collect();
            for session_id in session_ids {
                let queue = inner.mailboxes.entry(session_id).or_default();
                if queue.len() == capacity {
                    queue.pop_front();
                }
                queue.push_back(item.clone());
            }
        }
    }

    /// Drain one session's mailbox, oldest first, consuming every item that
    /// arrived since the last poll. `None` when the session is not registered.
    pub fn poll(&self, session_id: &str) -> Option<Vec<SnapshotItem>> {
        let mut inner = self.inner.lock().ok()?;
        if !inner.sessions.contains_key(session_id) {
            return None;
        }
        Some(
            inner
                .mailboxes
                .remove(session_id)
                .unwrap_or_default()
                .into_iter()
                .collect(),
        )
    }

    /// Collect the wake targets of every registered session for a project,
    /// without touching the lock: the caller performs the I/O after the lock
    /// is dropped.
    fn wake_targets(&self, project_id: &str) -> Vec<String> {
        let Ok(inner) = self.inner.lock() else {
            return Vec::new();
        };
        inner
            .sessions
            .values()
            .filter(|channel| channel.project_id == project_id)
            .filter_map(|channel| channel.wake_ref.clone())
            .collect()
    }
}

/// Wake frame shape shared by every adapter. Metadata-only by construction:
/// `project` and `hint` are the only fields, and `hint` is a topic-derived id,
/// never sender text. The structural test scans serialized wake bytes for
/// every rendered field of the admitted item and finds none of them.
fn wake_frame(project_id: &str, hint: Option<&str>) -> String {
    crate::json::Value::Object(vec![
        (
            "kind".into(),
            crate::json::Value::String("loam-wake".into()),
        ),
        (
            "project".into(),
            crate::json::Value::String(project_id.into()),
        ),
        (
            "hint".into(),
            crate::json::Value::String(hint.unwrap_or_default().into()),
        ),
    ])
    .to_json()
}

/// Best-effort, one-shot wake of every registered session for a project after
/// a changed admit. Never blocks the receive loop and never lets an error
/// escape: connect failures and unknown schemes are all eaten silently per the
/// degrade rule. Cross-platform std APIs only — no unix-only syscalls, so the
/// windows-2022 CI legs exercise the same wake path.
fn wake_all(channels: &ChannelRegistry, project_id: &str, hint: Option<&str>) {
    // Collect targets under the lock, then do the I/O after it is dropped: a
    // blocking connect inside the lock would stall every other pump sharing
    // the mailbox mutex.
    let targets = channels.wake_targets(project_id);
    for target in targets {
        let _ = wake_one(&target, project_id, hint);
    }
}

fn wake_one(target: &str, project_id: &str, hint: Option<&str>) -> Result<(), String> {
    let Some(rest) = target.strip_prefix("notify-tcp://") else {
        // Unknown scheme: skip silently — a wake target a connector does not
        // understand is a wake it does not attempt, and a malformed wake_ref
        // never blocks the pump or fails the mailbox.
        return Ok(());
    };
    let (host, port) = rest
        .rsplit_once(':')
        .ok_or_else(|| format!("malformed notify-tcp wake_ref: {target}"))?;
    let address = format!("{host}:{port}");
    let mut stream = std::net::TcpStream::connect_timeout(
        &address
            .parse()
            .map_err(|_| format!("bad address {address}"))?,
        Duration::from_secs(1),
    )
    .map_err(|error| format!("wake connect to {address}: {error}"))?;
    let frame = wake_frame(project_id, hint);
    stream
        .write_all(frame.as_bytes())
        .map_err(|error| format!("wake write to {address}: {error}"))
}

// ---------------------------------------------------------------------------
// Bounded in-memory snapshot store and live project sessions
// ---------------------------------------------------------------------------
//
// The `DeliveryProcessor` is the single validator, deduplicator, and
// expiry tracker; it tracks *ids*, not bodies. The store below is the only place
// a renderable body is retained, and it retains one per logical item so QoS 1
// duplicates and redelivery collapse. It is in-memory only: there is no snapshot
// table, no sidecar store, and no schema change, because MQTT is never durable
// authority and retained state plus the inbox re-deliver on reconnect. A
// restarted connector therefore serves nothing until state re-delivers, which is
// correct rather than a bug.

/// Whether a received work claim was reconciled against Git *at receive time*.
///
/// Every direction other than a real proof — a provisional claim, an oracle that
/// could not be built for the project, an unreachable commit, a failed fetch —
/// is [`Publication::Unverified`], so the fail-safe answer is always "sender
/// claim, not reconciled". The renderer can only display a work claim as current
/// when this says `Verified`, which is why the stamp lives here on the receive
/// path rather than in the 2 s session hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Publication {
    #[default]
    Unverified,
    Verified,
}

impl Publication {
    pub fn code(self) -> &'static str {
        match self {
            Publication::Unverified => "unverified",
            Publication::Verified => "verified",
        }
    }
}

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
    /// The sender's given name from their authenticated certificate, when they
    /// published one. Absent is the ordinary case, not an error.
    pub from_display_name: Option<String>,
    pub from_agent_id: String,
    pub from_instance_id: String,
    pub payload: crate::json::Value,
    pub expires_at: DateTime<Utc>,
    /// Git-first reconciliation result, stamped by the receive path before the
    /// item is ever readable. Never derived from the sender's own claim.
    pub publication: Publication,
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
    /// `publication` is the receive path's Git verdict for this frame; the store
    /// never derives it, so a sender cannot stamp its own claim as verified.
    pub fn admit(
        &mut self,
        topic: &str,
        outcome: &ReceiveOutcome,
        publication: Publication,
    ) -> bool {
        let Ok(parsed) = crate::envelope::parse_topic(topic) else {
            return false;
        };
        match outcome {
            ReceiveOutcome::Accepted(validated) => {
                let key = accepted_key(&parsed.delivery, &validated.as_envelope().id);
                let Some(item) = snapshot_item(key, validated, publication) else {
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
        // A membership frame never becomes a snapshot item.
        TopicDelivery::Membership | TopicDelivery::MemberCard { .. } => String::new(),
    }
}

/// The logical key an empty-payload tombstone resolves. An event cannot be
/// tombstoned (the transport rejects that), so only state and inbox have one.
fn tombstone_key(delivery: &crate::envelope::TopicDelivery<'_>) -> Option<String> {
    use crate::envelope::TopicDelivery;
    match delivery {
        TopicDelivery::Event { .. } => None,
        TopicDelivery::State { origin, key } => Some(format!("state:{origin}/{key}")),
        TopicDelivery::Inbox { message_id, .. } => Some(format!("inbox:{message_id}")),
        TopicDelivery::Membership | TopicDelivery::MemberCard { .. } => None,
    }
}

fn snapshot_item(
    key: String,
    validated: &ValidatedEnvelope,
    publication: Publication,
) -> Option<SnapshotItem> {
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
        from_display_name: envelope.data.from.display_name.clone(),
        from_agent_id: envelope.data.from.agent_id.clone(),
        from_instance_id: envelope.data.from.instance_id.clone(),
        payload: envelope.data.payload.clone(),
        expires_at,
        publication,
    })
}

/// Who a project's live session will admit frames from. The transport checks every
/// received frame's topic origin and `data.from.principal_id` against these, so
/// a session with no roster hears only its own instance. Injected by the
/// deployment at provisioning time — never invented here and never supplied by
/// an IPC caller.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
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
/// The work happens in `crate::provisioning`, which is admitted to the capability
/// guard's filesystem and process lists by name. This module holds the broker
/// socket, so it takes the resolved value and reaches for neither capability
/// itself; that separation is the whole reason the resolver is a delegate rather
/// than a body.
///
/// The seam is a plain function rather than a stored callable on purpose: the
/// crate capability guard bars function-pointer and trait-object capabilities,
/// and a provisioner that could be swapped at runtime would be exactly that.
pub fn provision_session(
    row: &crate::enrollment::EnrolledRow,
) -> Result<(MqttSession, PeerRoster), ProvisionFailure> {
    crate::provisioning::resolve(row)
}

/// The stable reasons the two provisioning failure states carry. They name the
/// *input* that failed and never any material behind it, and they are additive:
/// the `code()` strings above them are a tested IPC contract and do not move.
pub mod reason {
    pub const ENDPOINT_MALFORMED: &str = "endpoint-malformed";
    pub const CREDENTIAL_REF_UNRESOLVED: &str = "credential-ref-unresolved";
    /// No identity bundle at the identity path (`client.pem`/`key.pem` missing):
    /// the certificate is the machine's only identity source, so a machine with
    /// none cannot open a session and nothing is minted to paper over it.
    pub const IDENTITY_REQUIRED: &str = "identity-required";
    pub const CA_UNRESOLVED: &str = "ca-unresolved";
    /// The local Git email and the authenticated certificate's common name
    /// disagree. The certificate is authoritative and the disagreement is
    /// surfaced rather than resolved in either direction.
    pub const IDENTITY_MISMATCH: &str = "identity-mismatch";
    pub const ROSTER_ABSENT: &str = "roster-absent";
    pub const ROSTER_EMPTY: &str = "roster-empty";
    /// Principals but no origins: not empty by `PeerRoster::is_empty`, yet it
    /// admits nothing, because the receive path checks the topic origin first.
    /// A session opened on it would look connected and hear no one.
    pub const ROSTER_NO_ORIGINS: &str = "roster-no-origins";
    /// Origins but no principals: the mirror of `ROSTER_NO_ORIGINS`, and deaf
    /// for the same reason — the session would admit only its own principal, so
    /// a colleague's frame arriving from an admitted origin is still refused.
    pub const ROSTER_NO_PRINCIPALS: &str = "roster-no-principals";
    pub const ROSTER_WILDCARD: &str = "roster-wildcard";
    pub const ROSTER_MALFORMED: &str = "roster-malformed";
    /// No federation profile resolved at all: no config dir, no legacy global
    /// root. Distinct from `ROSTER_ABSENT` (profile resolved but no roster
    /// file) so an operator can tell "no home to look in" from "nothing
    /// written yet".
    pub const PROFILE_ABSENT: &str = "profile-absent";
    /// A one-time legacy→config-dir profile migration could not be performed.
    pub const PROFILE_COPY_FAILED: &str = "profile-copy-failed";
}

/// Why provisioning refused, split by which half failed so the connector maps it
/// to a state without sniffing the reason string. Carrying the reason is what
/// lets an operator tell six failed inputs apart without six new states.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProvisionFailure {
    /// The endpoint, the credential reference, or the CA did not resolve.
    Credentials(&'static str),
    /// Credentials resolved; the peer roster did not.
    Roster(&'static str),
}

/// What a `project.attach` actually achieved. Reported honestly: an unopened
/// session is never described as attached-and-live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionState {
    /// A live subscribed broker session is held in this process for the project.
    Live,
    /// A session for this project was already live; attach is idempotent.
    AlreadyLive,
    /// An endpoint, credential, or CA input did not resolve.
    CredentialsUnresolved(&'static str),
    /// Credentials resolved but no usable peer roster was provisioned, so the
    /// session could admit no colleague and was not opened.
    NoPeerRoster(&'static str),
    /// Credentials and roster resolved but the broker refused the session.
    Unreachable(String),
}

impl SessionState {
    pub fn code(&self) -> &'static str {
        match self {
            SessionState::Live => "live",
            SessionState::AlreadyLive => "already-live",
            SessionState::CredentialsUnresolved(_) => "credentials-unresolved",
            SessionState::NoPeerRoster(_) => "no-peer-roster",
            SessionState::Unreachable(_) => "unreachable",
        }
    }

    /// Which input failed, for the two states that have one. `None` is not a
    /// missing reason — it means nothing failed to resolve.
    pub fn reason(&self) -> Option<&'static str> {
        match self {
            SessionState::CredentialsUnresolved(reason) | SessionState::NoPeerRoster(reason) => {
                Some(reason)
            }
            _ => None,
        }
    }
}

/// How long a pump thread blocks on one receive before re-checking its stop
/// flag. Short enough that a detach is prompt, long enough that an idle project
/// costs nothing.
const PUMP_POLL: Duration = Duration::from_millis(500);
/// How long a receive-path reconciliation stays fresh before the oracle refetches.
/// The pump is not on a session's critical path, so this trades a little
/// staleness for not running `git ls-remote` on every received work claim.
const RECONCILE_FRESHNESS: Duration = Duration::from_secs(30);
/// Renderable items retained per project. The hook's own item budget is smaller;
/// this is the store's ceiling, not the render budget.
const SNAPSHOT_CAPACITY: usize = 64;

// ---------------------------------------------------------------------------
// Self-healing establishment supervisor (connector-self-healing)
// ---------------------------------------------------------------------------
//
// Each live project runs a supervisor loop that owns an explicit establishment
// cycle — build → dial → authenticate → subscribe → run — rebuilt from scratch on
// every attempt rather than trusting rumqttc's internal eternal redial on a stale
// socket path. The supervisor records what it observes into a shared
// `ObservedSession` so status can never again claim "live" for a process that
// holds no broker session.

/// Per-attempt dial deadline: the whole build → dial → CONNACK must complete
/// inside this, so a wedged SYN never blocks an establishment cycle forever.
const DIAL_DEADLINE: Duration = Duration::from_secs(30);
/// Reconnect backoff floor; doubled per consecutive failure, jittered, and
/// capped at `BACKOFF_CAP`.
const BACKOFF_BASE: Duration = Duration::from_secs(1);
/// Hard ceiling on the reconnect backoff, jitter included.
const BACKOFF_CAP: Duration = Duration::from_secs(60);

/// The outcome of one supervised establishment attempt, as the control loop sees
/// it.
enum EstablishOutcome {
    /// A session opened and later dropped — reconnect.
    Ended,
    /// An establishment step failed before any session ran; the string is the
    /// error category for the breadcrumb and the observed status.
    Failed(String),
    /// A typed provisioning refusal (credentials-unresolved / no-peer-roster).
    /// Steady state, not a wedge: the supervisor stops rather than churning.
    Refused(&'static str),
}

/// The connector's in-process observation of one project's broker session,
/// shared between the supervisor thread that writes it and the IPC threads that
/// read it for a truthful status. Volatile by design: it dies with the process,
/// like every other connector session fact.
#[derive(Debug, Clone)]
pub struct ObservedSession {
    /// True only while a broker session is established and subscribed *now*.
    pub established: bool,
    /// When the current established/disconnected state began.
    pub since: DateTime<Utc>,
    /// Consecutive failed establishment cycles since the last live session.
    pub consecutive_failures: u32,
    /// The last establishment error category, when not established.
    pub last_error: Option<String>,
    /// A typed provisioning refusal, when the supervisor has disarmed on one.
    pub refusal: Option<&'static str>,
}

impl ObservedSession {
    /// Whether every status surface should read observed-degraded: the session is
    /// down and `DEGRADE_AFTER` consecutive establishment cycles have failed.
    pub fn degraded(&self) -> bool {
        !self.established && self.consecutive_failures >= DEGRADE_AFTER
    }
}

/// Consecutive failed establishment cycles before every status surface flips to
/// observed-degraded.
const DEGRADE_AFTER: u32 = 3;

/// How long the connector may stay enrolled-and-provisioned but sessionless
/// before the watchdog hands recovery to the OS supervisor. The incident proved
/// a fresh process image is the cure a wedge cannot heal in place.
const SESSIONLESS_EXIT_AFTER: Duration = Duration::from_secs(600);

/// The exit code the watchdog uses so systemd (`Restart=on-failure`) and launchd
/// (`KeepAlive`/`SuccessfulExit=false`) treat the exit as a temporary failure and
/// respawn a fresh process. `EX_TEMPFAIL` from sysexits.
pub const WATCHDOG_EXIT_CODE: i32 = 75;

// Test-tier tuning knobs. The spec thresholds above are the production defaults;
// these env overrides exist ONLY so the `LOAM_MQTT_TEST` live respawn gate can
// drive a real watchdog exit in seconds instead of ten minutes. They are additive
// and never consulted unless set — production behavior is the consts, unchanged.
const ENV_DIAL_SECS: &str = "LOAM_WATCHDOG_DIAL_SECS";
const ENV_BACKOFF_CAP_SECS: &str = "LOAM_WATCHDOG_BACKOFF_CAP_SECS";
const ENV_SESSIONLESS_SECS: &str = "LOAM_WATCHDOG_SESSIONLESS_SECS";

/// Read a whole-seconds duration override from the environment, or the default.
fn env_secs_or(name: &str, default: Duration) -> Duration {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or(default)
}

fn configured_dial_deadline() -> Duration {
    env_secs_or(ENV_DIAL_SECS, DIAL_DEADLINE)
}

fn configured_backoff_cap() -> Duration {
    env_secs_or(ENV_BACKOFF_CAP_SECS, BACKOFF_CAP)
}

fn configured_sessionless_budget() -> Duration {
    env_secs_or(ENV_SESSIONLESS_SECS, SESSIONLESS_EXIT_AFTER)
}

/// The watchdog's verdict on a failed establishment cycle: keep retrying in this
/// process, or exit so the OS supervisor respawns a fresh one. Pure and
/// clock-injected so the ten-minute budget is unit-testable without a real wait
/// and without ever calling `std::process::exit` inside a test.
#[derive(Debug, PartialEq, Eq)]
enum WatchdogVerdict {
    Retry,
    Exit,
}

/// Whether an enrolled, provisioned-but-sessionless connector has been down long
/// enough to hand recovery to its OS supervisor. `sessionless_since` is when the
/// current sessionless stretch began; `now` is the current instant; `budget` is
/// the sessionless allowance (the const default, or the test-tier override).
fn watchdog_verdict(sessionless_since: Instant, now: Instant, budget: Duration) -> WatchdogVerdict {
    if now.saturating_duration_since(sessionless_since) >= budget {
        WatchdogVerdict::Exit
    } else {
        WatchdogVerdict::Retry
    }
}

/// The shared authenticated identity of a supervised session: `None` until the
/// first successful open, then set by the supervisor thread and read by the emit
/// path. Identity is stable across reconnects (the certificate CN does not
/// change), so it is set once and never cleared.
type IdentitySlot = std::sync::Arc<std::sync::Mutex<Option<SessionIdentity>>>;

/// A cloneable handle to one project's `ObservedSession`. Every mutation is a
/// named state transition so the supervisor cannot record an inconsistent view.
#[derive(Clone)]
pub struct LivenessHandle(std::sync::Arc<std::sync::Mutex<ObservedSession>>);

impl LivenessHandle {
    /// A handle for a session that has just been established (the synchronous
    /// first attach succeeded).
    fn established(now: DateTime<Utc>) -> Self {
        LivenessHandle(std::sync::Arc::new(std::sync::Mutex::new(ObservedSession {
            established: true,
            since: now,
            consecutive_failures: 0,
            last_error: None,
            refusal: None,
        })))
    }

    /// A handle for a session whose first establishment cycle failed but is
    /// retryable (a dial/auth/subscribe failure, not a typed refusal). The
    /// supervisor keeps trying and the watchdog is already counting: the first
    /// failure is seeded here so the observation is truthful from attach.
    fn down(now: DateTime<Utc>, category: String) -> Self {
        LivenessHandle(std::sync::Arc::new(std::sync::Mutex::new(ObservedSession {
            established: false,
            since: now,
            consecutive_failures: 1,
            last_error: Some(category),
            refusal: None,
        })))
    }

    /// A read-only snapshot of the current observation.
    pub fn observe(&self) -> ObservedSession {
        self.0
            .lock()
            .map(|inner| inner.clone())
            .unwrap_or_else(|poisoned| poisoned.into_inner().clone())
    }

    /// A broker session just opened and subscribed: established, failure count
    /// cleared, error cleared.
    fn mark_established(&self, now: DateTime<Utc>) {
        if let Ok(mut inner) = self.0.lock() {
            inner.established = true;
            inner.since = now;
            inner.consecutive_failures = 0;
            inner.last_error = None;
            inner.refusal = None;
        }
    }

    /// A running session dropped. Not a failed cycle — the count stays cleared —
    /// but no session is established until the next cycle opens one.
    fn mark_disconnected(&self, now: DateTime<Utc>) {
        if let Ok(mut inner) = self.0.lock() {
            inner.established = false;
            inner.since = now;
        }
    }

    /// An establishment cycle failed. Increments the consecutive count and
    /// records the error category; returns the new count for backoff.
    fn record_failure(&self, category: String) -> u32 {
        if let Ok(mut inner) = self.0.lock() {
            inner.established = false;
            inner.consecutive_failures = inner.consecutive_failures.saturating_add(1);
            inner.last_error = Some(category);
            return inner.consecutive_failures;
        }
        0
    }

    /// The supervisor disarmed on a typed provisioning refusal — a steady state,
    /// never a wedge to churn against.
    fn mark_refused(&self, reason: &'static str) {
        if let Ok(mut inner) = self.0.lock() {
            inner.established = false;
            inner.refusal = Some(reason);
        }
    }
}

/// Exponential backoff with jitter, hard-capped at `BACKOFF_CAP`. `failures` is
/// the consecutive count (>=1). Jitter only ever *subtracts* (up to 25%) so the
/// cap is a true ceiling; it is derived from the system clock's subsecond nanos —
/// reconnect spacing needs decorrelation, not cryptographic randomness, so no new
/// dependency is pulled in.
fn backoff_with_jitter(failures: u32, cap: Duration) -> Duration {
    let shift = failures.saturating_sub(1).min(6);
    let base = BACKOFF_BASE.saturating_mul(1u32 << shift);
    let capped = base.min(cap);
    let jitter_ceiling = (capped.as_millis() as u64) / 4;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::from(d.subsec_nanos()))
        .unwrap_or(0);
    let jitter = if jitter_ceiling > 0 {
        nanos % jitter_ceiling
    } else {
        0
    };
    capped.saturating_sub(Duration::from_millis(jitter))
}

/// Sleep up to `total`, but wake promptly if the stop flag is set so a detach or
/// shutdown is never blocked behind a full backoff interval.
fn interruptible_sleep(total: Duration, stop: &std::sync::atomic::AtomicBool) {
    let step = Duration::from_millis(100);
    let mut slept = Duration::ZERO;
    while slept < total && !stop.load(std::sync::atomic::Ordering::Relaxed) {
        let chunk = step.min(total - slept);
        std::thread::sleep(chunk);
        slept += chunk;
    }
}

/// The supervised reconnect loop. Generic over the establishment attempt and the
/// sleeper so the control logic — reconnect on drop, backoff on failure, disarm
/// on a typed refusal — is unit-testable with a scripted attempt sequence and no
/// real broker or wall-clock delay.
fn supervise<A, S>(
    project_id: &str,
    mut attempt: A,
    mut sleep: S,
    stop: &std::sync::atomic::AtomicBool,
    liveness: &LivenessHandle,
) where
    A: FnMut() -> EstablishOutcome,
    S: FnMut(Duration),
{
    use std::sync::atomic::Ordering::Relaxed;
    // Tuning read once per supervisor: the production consts, unless the test tier
    // overrode them via the environment.
    let backoff_cap = configured_backoff_cap();
    let sessionless_budget = configured_sessionless_budget();
    // When the current sessionless stretch began. `None` while a session is live;
    // set on the first failed cycle after a drop, cleared when one re-establishes.
    // The watchdog measures its budget from here.
    let mut sessionless_since: Option<Instant> = None;
    while !stop.load(Relaxed) {
        match attempt() {
            // A session ran and dropped: the attempt already marked the session
            // down. A session did exist, so the sessionless clock resets — the
            // watchdog only fires on a sustained inability to establish one.
            EstablishOutcome::Ended => {
                sessionless_since = None;
            }
            EstablishOutcome::Failed(category) => {
                let failures = liveness.record_failure(category.clone());
                // Exactly one breadcrumb when the observation first crosses into
                // degraded, not one per cycle after it.
                if failures == DEGRADE_AFTER {
                    eprintln!(
                        "loam connector: observed-degraded project={project_id} after {failures} failed cycles (last error {category})"
                    );
                }
                eprintln!(
                    "loam connector: establishment failed project={project_id} error={category} (attempt {failures})"
                );
                let since = *sessionless_since.get_or_insert_with(Instant::now);
                if watchdog_verdict(since, Instant::now(), sessionless_budget)
                    == WatchdogVerdict::Exit
                {
                    eprintln!(
                        "loam connector: watchdog exit project={project_id} — enrolled but sessionless for {}s; exiting {WATCHDOG_EXIT_CODE} for supervisor respawn",
                        sessionless_budget.as_secs()
                    );
                    std::process::exit(WATCHDOG_EXIT_CODE);
                }
                sleep(backoff_with_jitter(failures, backoff_cap));
            }
            EstablishOutcome::Refused(reason) => {
                liveness.mark_refused(reason);
                eprintln!(
                    "loam connector: provisioning refused project={project_id} reason={reason}; supervisor disarmed"
                );
                return;
            }
        }
    }
}

/// The live broker sessions this connector process holds — one per enrolled
/// project, in the same process, with no second daemon. Each pumps its received
/// frames through the `DeliveryProcessor` into the shared snapshot store.
pub struct ProjectSessions {
    snapshots: std::sync::Arc<std::sync::Mutex<SnapshotStore>>,
    live: std::collections::HashMap<String, LiveSession>,
    /// The shared channel registry + mailbox state, handed to each pump so the
    /// receive path can push admitted items into registered sessions' mailboxes.
    channels: ChannelRegistry,
}

struct LiveSession {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    /// The session's authenticated identity, captured at CONNACK and shared with
    /// the supervisor thread that (re)establishes it. `None` until the first
    /// successful open — a connector that boots while the broker is down keeps
    /// retrying with no identity yet. This — not anything a caller supplies — is
    /// what binds `data.from` on an outbound emit, which is why `federation emit`
    /// forwards a derived operation rather than a finished envelope.
    identity: IdentitySlot,
    /// Outbound queue drained by the pump thread, which owns the client. The
    /// CLI never opens a broker connection; the connector owns every publish.
    outbound: std::sync::mpsc::Sender<ValidatedEnvelope>,
    /// The connector's own observation of this project's broker session, updated
    /// by the supervisor thread and read by the IPC status path so "live" can
    /// only ever come from an actually-established session.
    liveness: LivenessHandle,
}

/// What happened to one outbound emit. `NotShipped` is observational: an
/// unopened session is reported as such, never as a silent success.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmitOutcome {
    Queued,
    NotShipped(&'static str),
}

impl ProjectSessions {
    /// A shared channel registry + mailbox state, handed to each pump so the
    /// receive path can push admitted items into registered sessions' mailboxes
    /// and fire wakes at their targets. Taking the registry as an argument is
    /// what keeps the IPC side (`ConnectorState::channels`) and the pump side
    /// on ONE registry — a second instance was the live-wake defect.
    pub fn new(capacity: usize, channels: ChannelRegistry) -> Self {
        ProjectSessions {
            snapshots: std::sync::Arc::new(std::sync::Mutex::new(
                SnapshotStore::new(capacity).expect("snapshot capacity is a non-zero constant"),
            )),
            live: std::collections::HashMap::new(),
            channels,
        }
    }

    /// The shared store, so a test (or a future in-process reader) can admit
    /// frames and read them back without a broker.
    pub fn store(&self) -> std::sync::Arc<std::sync::Mutex<SnapshotStore>> {
        std::sync::Arc::clone(&self.snapshots)
    }

    /// The shared channel registry + mailbox state, so a test can register a
    /// session and drive the push/poll path without a broker.
    pub fn channels(&self) -> &ChannelRegistry {
        &self.channels
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
    /// provisioning result. Idempotent. The first establishment cycle runs here
    /// synchronously so `project.attach` reports honestly (a refusal is returned,
    /// not masked); on success a supervisor thread takes over, re-establishing
    /// from scratch on every drop so a wedged socket path is never inherited.
    /// Taking `provisioned` as an argument is what lets a test drive the first
    /// cycle without the connector holding a swappable callable.
    pub fn attach(
        &mut self,
        row: &crate::enrollment::EnrolledRow,
        provisioned: Result<(MqttSession, PeerRoster), ProvisionFailure>,
        now: DateTime<Utc>,
    ) -> SessionState {
        if self.live.contains_key(&row.project_id) {
            return SessionState::AlreadyLive;
        }
        // The first cycle runs synchronously so `project.attach` reports honestly.
        // A typed refusal is steady state — no supervisor, no watchdog. A live
        // open primes the supervisor; a retryable failure (Unreachable) still
        // arms it, because a connector that boots while the broker is down must
        // keep trying and eventually exit for its supervisor to respawn.
        let (primed, liveness, identity, state) = match open_transport(row, provisioned, now) {
            Ok(open) => {
                eprintln!("loam connector: session up project={}", row.project_id);
                let identity: IdentitySlot =
                    std::sync::Arc::new(std::sync::Mutex::new(Some(open.identity.clone())));
                (
                    Some(open),
                    LivenessHandle::established(now),
                    identity,
                    SessionState::Live,
                )
            }
            Err(refusal @ (SessionState::CredentialsUnresolved(_) | SessionState::NoPeerRoster(_))) => {
                return refusal;
            }
            Err(SessionState::Unreachable(category)) => (
                None,
                LivenessHandle::down(now, category.clone()),
                std::sync::Arc::new(std::sync::Mutex::new(None)),
                SessionState::Unreachable(category),
            ),
            // AlreadyLive/Live are never returned by open_transport; treat any
            // other value as a retryable failure rather than dropping the session.
            Err(other) => (
                None,
                LivenessHandle::down(now, other.code().to_owned()),
                std::sync::Arc::new(std::sync::Mutex::new(None)),
                other,
            ),
        };
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (outbound, inbound) = std::sync::mpsc::channel();
        let thread = std::thread::spawn({
            let stop = std::sync::Arc::clone(&stop);
            let snapshots = std::sync::Arc::clone(&self.snapshots);
            let channels = self.channels.clone();
            let liveness = liveness.clone();
            let identity = std::sync::Arc::clone(&identity);
            let row = row.clone();
            move || {
                // The first cycle runs the session `attach` already opened (when
                // primed); every cycle after — and the first, when the first open
                // failed — re-provisions and rebuilds from scratch.
                let mut primed = primed;
                let project_id = row.project_id.clone();
                supervise(
                    &project_id,
                    || match primed.take() {
                        Some(open) => {
                            run_pump_loop(open, &snapshots, &channels, &inbound, &stop, &liveness)
                        }
                        None => establish_and_run(
                            &row, &snapshots, &channels, &inbound, &stop, &liveness, &identity,
                        ),
                    },
                    |backoff| interruptible_sleep(backoff, &stop),
                    &stop,
                    &liveness,
                );
            }
        });
        self.live.insert(
            row.project_id.clone(),
            LiveSession {
                stop,
                thread: Some(thread),
                identity,
                outbound,
                liveness,
            },
        );
        state
    }

    /// The authenticated identity of a project's live session, if one has opened.
    /// The emit path needs it to bind `data.from` before validating. `None` while
    /// a supervised project has not yet completed a first successful open.
    pub fn identity(&self, project_id: &str) -> Option<SessionIdentity> {
        self.live
            .get(project_id)
            .and_then(|session| session.identity.lock().ok().and_then(|slot| slot.clone()))
    }

    /// Hand one validated envelope to the project's live session for publishing.
    /// Refuses honestly when no session is open rather than dropping it.
    pub fn ship(&self, project_id: &str, envelope: ValidatedEnvelope) -> EmitOutcome {
        let Some(session) = self.live.get(project_id) else {
            return EmitOutcome::NotShipped("no-live-session");
        };
        match session.outbound.send(envelope) {
            Ok(()) => EmitOutcome::Queued,
            // The pump ended; the session is live in the map but not in fact.
            Err(_) => EmitOutcome::NotShipped("session-ended"),
        }
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
/// state for the project, plus this connector's own three typed inboxes. The
/// transport's per-frame origin and principal checks are what actually bound admission;
/// the filters only decide what the broker sends.
fn live_filters(org_id: &str, project_id: &str, identity: &SessionIdentity) -> Vec<String> {
    let base = format!("loam/v1/{org_id}/{project_id}");
    let mut filters = vec![
        format!("{base}/event/+"),
        format!("{base}/state/+/+"),
        format!("{base}/inbox/instance/{}/+/+", identity.instance_id),
        format!("{base}/inbox/principal/{}/+/+", identity.principal_id),
        format!("{base}/inbox/agent/{}/+/+", identity.agent_id),
    ];
    // The broker-served membership topic: the retained payload the connector
    // writes to the local roster file (`federation-enrollment-simplification.md`).
    filters.push(format!("{base}/membership"));
    // The self-announced member-card feed: every member's retained card on
    // `loam/v1/{org}/members/{instance_id}`, from which this connector assembles
    // the per-project roster (including its own card). Org-scoped, so the same
    // filter covers every project a machine is in.
    filters.push(crate::provisioning::member_filter(org_id));
    filters
}

/// Pump one project's received frames into the snapshot store until stopped. The
/// pump only reads: it never publishes, and a lost session simply goes quiet
/// rather than fabricating state.
/// The receive path's Git oracle for one project, derived entirely from the
/// enrollment row and the provisioned roster — never from a message. Returns
/// `None` whenever the workspace, remote, wiki root, scope, or origin set cannot
/// be resolved, which leaves every work claim stamped as a sender claim.
fn receive_oracle(
    row: &crate::enrollment::EnrolledRow,
    roster: &PeerRoster,
) -> Option<crate::transport::GitOracle> {
    let remote = row.remotes.first()?;
    let workspace = std::path::PathBuf::from(&row.display_path);
    let wiki_root = workspace.join("wiki");
    let scope =
        crate::transport::GitScope::new(&row.org_id, &row.project_id, &row.repository_id).ok()?;
    crate::transport::GitOracle::new(
        &workspace,
        &wiki_root,
        &remote.name,
        scope,
        &remote.allowed_refs,
        &roster.origins,
        RECONCILE_FRESHNESS,
    )
    .ok()
}

/// Build this machine's own retained member card from the enrollment and the
/// authenticated identity. `projects` lists the enrolled project(s) this
/// instance announces for; peers assemble per-project rosters from the subset
/// of cards listing their project. `None` means no card can be announced (an
/// identity with no principal or no instance) — a machine that cannot even
/// build its own card has no member presence to publish.
fn own_member_card(
    row: &crate::enrollment::EnrolledRow,
    identity: &SessionIdentity,
    now: DateTime<Utc>,
) -> Result<Option<crate::provisioning::MemberCard>, &'static str> {
    if row.org_id.is_empty() || identity.instance_id.is_empty() || identity.principal_id.is_empty()
    {
        return Ok(None);
    }
    Ok(Some(crate::provisioning::MemberCard {
        instance_id: identity.instance_id.clone(),
        principal_id: identity.principal_id.clone(),
        display_name: identity.display_name.clone(),
        joined_at: now.to_rfc3339(),
        projects: vec![row.project_id.clone()],
    }))
}

/// The Git verdict for one received frame. Only an `io.loam.work.state` frame
/// that the oracle proves published earns [`Publication::Verified`]; every other
/// outcome, including every error, falls back to the sender-claim answer.
fn stamp_publication(
    oracle: Option<&mut crate::transport::GitOracle>,
    outcome: &ReceiveOutcome,
) -> Publication {
    let (Some(oracle), ReceiveOutcome::Accepted(validated)) = (oracle, outcome) else {
        return Publication::Unverified;
    };
    if validated.as_envelope().message_type != "io.loam.work.state" {
        return Publication::Unverified;
    }
    match oracle.evaluate_work_state(validated) {
        Ok(crate::transport::PublicationStatus::Verified(_)) => Publication::Verified,
        _ => Publication::Unverified,
    }
}

/// One established broker session and everything the pump reads from it. Produced
/// by `open_transport` and consumed by `run_pump_loop`.
struct OpenSession {
    transport: MqttTransport,
    identity: SessionIdentity,
    roster: PeerRoster,
    oracle: Option<crate::transport::GitOracle>,
    org_id: String,
    project_id: String,
}

/// Build, authenticate, subscribe, and self-announce one broker session from an
/// already-resolved provisioning result. Every call rebuilds the transport from
/// scratch — fresh `TransportConfig`, DNS, TLS, and rumqttc client — so a wedged
/// socket path is never inherited across establishment cycles. Returns the open
/// session, or the `SessionState` that explains why none opened.
fn open_transport(
    row: &crate::enrollment::EnrolledRow,
    provisioned: Result<(MqttSession, PeerRoster), ProvisionFailure>,
    now: DateTime<Utc>,
) -> Result<OpenSession, SessionState> {
    let (session, roster) = match provisioned {
        Ok(pair) => pair,
        Err(ProvisionFailure::Credentials(reason)) => {
            return Err(SessionState::CredentialsUnresolved(reason))
        }
        Err(ProvisionFailure::Roster(reason)) => return Err(SessionState::NoPeerRoster(reason)),
    };
    if roster.is_empty() {
        return Err(SessionState::NoPeerRoster(reason::ROSTER_EMPTY));
    }
    let mut transport = MqttTransport::new(session, ValidationConfig::default(), now)
        .map_err(|error| SessionState::Unreachable(error.code().to_owned()))?;
    // The live session dials with the longer deadline; the probe keeps the short
    // default. A wedged SYN is abandoned after the dial deadline, not held
    // forever. The test tier may shorten it to drive the watchdog quickly.
    transport.set_dial_deadline(configured_dial_deadline());
    let identity = transport
        .authenticate()
        .map_err(|error| SessionState::Unreachable(error.code().to_owned()))?;
    for filter in live_filters(&row.org_id, &row.project_id, &identity) {
        transport
            .subscribe(&filter, false)
            .map_err(|error| SessionState::Unreachable(error.code().to_owned()))?;
    }

    // Self-announce: publish this machine's own retained member card on
    // `loam/v1/{org}/members/{instance_id}`. Every connector does this on connect,
    // so colleagues assembling their rosters from retained cards pick this machine
    // up without an operator authoring anything. A refused self-publish is a
    // broker/ACL fault: peers are still known, but this instance is invisible to
    // first joiners.
    let card_topic = crate::provisioning::member_topic(&row.org_id, &identity.instance_id);
    if let Ok(Some(card)) = own_member_card(row, &identity, now) {
        let body = crate::provisioning::member_card_to_json(&card);
        if transport
            .publish_raw_retained(&card_topic, body.into_bytes())
            .is_ok()
        {
            if let Ok(root) = crate::provisioning::configured_roster_root() {
                // Persist the own card immediately so a restart before the pump
                // collects the broker's redelivery still admits this machine.
                let _ = crate::provisioning::write_member_card(&root, &row.org_id, &card);
            }
        }
    }

    // Built before the pump starts, from the enrollment and the same roster the
    // receive path admits frames against. `None` is the fail-safe: every work
    // claim then renders as an unreconciled sender claim.
    let oracle = receive_oracle(row, &roster);
    Ok(OpenSession {
        transport,
        identity,
        roster,
        oracle,
        org_id: row.org_id.clone(),
        project_id: row.project_id.clone(),
    })
}

/// One full establishment cycle for the supervisor: re-provision from scratch,
/// open a fresh session, and run it until it drops. A typed provisioning refusal
/// returns `Refused` (the supervisor disarms); a dial/auth/subscribe failure
/// returns `Failed` (the supervisor backs off and retries).
fn establish_and_run(
    row: &crate::enrollment::EnrolledRow,
    snapshots: &std::sync::Arc<std::sync::Mutex<SnapshotStore>>,
    channels: &ChannelRegistry,
    outbound: &std::sync::mpsc::Receiver<ValidatedEnvelope>,
    stop: &std::sync::atomic::AtomicBool,
    liveness: &LivenessHandle,
    identity: &IdentitySlot,
) -> EstablishOutcome {
    let now = Utc::now();
    let open = match open_transport(row, provision_session(row), now) {
        Ok(open) => open,
        Err(SessionState::CredentialsUnresolved(reason))
        | Err(SessionState::NoPeerRoster(reason)) => return EstablishOutcome::Refused(reason),
        Err(SessionState::Unreachable(category)) => return EstablishOutcome::Failed(category),
        Err(other) => return EstablishOutcome::Failed(other.code().to_owned()),
    };
    // Publish the authenticated identity for the emit path. Stable across
    // reconnects, so a set-once is enough; a session that had never opened before
    // (broker was down at boot) becomes emittable from here.
    if let Ok(mut slot) = identity.lock() {
        if slot.is_none() {
            *slot = Some(open.identity.clone());
        }
    }
    liveness.mark_established(now);
    eprintln!("loam connector: session up project={}", row.project_id);
    run_pump_loop(open, snapshots, channels, outbound, stop, liveness)
}

/// Pump one open session's received frames into the snapshot store until it drops
/// or a stop is requested, then mark the session down and return `Ended` so the
/// supervisor re-establishes. The pump only reads: it never publishes on its own,
/// and a lost session simply goes quiet rather than fabricating state.
fn run_pump_loop(
    open: OpenSession,
    snapshots: &std::sync::Arc<std::sync::Mutex<SnapshotStore>>,
    channels: &ChannelRegistry,
    outbound: &std::sync::mpsc::Receiver<ValidatedEnvelope>,
    stop: &std::sync::atomic::AtomicBool,
    liveness: &LivenessHandle,
) -> EstablishOutcome {
    let OpenSession {
        mut transport,
        identity: _,
        mut roster,
        mut oracle,
        org_id,
        project_id,
    } = open;
    // A malformed retained member card is degrade-not-crash, but silence hid a
    // parser bug that stranded every roster. Surface the first one per session
    // so it is at least visible; one line, no framework.
    let mut warned_malformed_card = false;
    while !stop.load(std::sync::atomic::Ordering::Relaxed) {
        // Outbound first: an emit the user just made should not wait behind a
        // full poll interval of inbound traffic.
        while let Ok(envelope) = outbound.try_recv() {
            // A refused publish is the connector's problem, not the session's:
            // one rejected envelope never takes the pump down.
            let _ = transport.publish_outbound(&envelope, Utc::now());
        }
        match transport.receive_outcome(PUMP_POLL, Utc::now(), &roster) {
            Ok(Some((topic, outcome))) => {
                // A self-announced member card: persist the card to the cache
                // and reassemble this project's roster from every retained card
                // listing the project. The connector is the roster author, so
                // the assembled file is the durable truth the next session
                // (and `provisioning::resolve`) reads.
                if let ReceiveOutcome::MemberCard { payload, .. } = &outcome {
                    if let Ok(text) = std::str::from_utf8(payload) {
                        if let Ok(root) = crate::provisioning::configured_roster_root() {
                            match crate::provisioning::parse_member_card_pub(text) {
                                Ok(card) => {
                                    let _ = crate::provisioning::write_member_card(
                                        &root, &org_id, &card,
                                    );
                                    if let Ok(assembled) =
                                        crate::provisioning::assemble_project_roster(
                                            &root,
                                            &org_id,
                                            &project_id,
                                        )
                                    {
                                        let body = crate::provisioning::roster_body(&assembled);
                                        let _ = crate::provisioning::write_roster(
                                            &root,
                                            &org_id,
                                            &project_id,
                                            &body,
                                        );
                                        roster = assembled;
                                    }
                                }
                                Err(reason) if !warned_malformed_card => {
                                    warned_malformed_card = true;
                                    eprintln!("loam: ignoring a malformed member card ({reason}); roster may be incomplete");
                                }
                                Err(_) => {}
                            }
                        }
                    }
                    continue;
                }
                // Broker-served membership: write the roster file from the
                // retained payload. The write validates through the same rules
                // the session build uses, so a payload that admits nobody is
                // never persisted.
                if let ReceiveOutcome::Membership(payload) = &outcome {
                    if let Ok(text) = std::str::from_utf8(payload) {
                        if let Ok(parsed) = crate::envelope::parse_topic(&topic) {
                            if let Ok(root) = crate::provisioning::configured_roster_root() {
                                let _ = crate::provisioning::write_roster(
                                    &root,
                                    parsed.organization,
                                    parsed.project,
                                    text,
                                );
                            }
                        }
                    }
                    continue;
                }
                // Git-first: reconcile *before* the item is readable, so a hook
                // can never display a provisional claim as current.
                let publication = stamp_publication(oracle.as_mut(), &outcome);
                if let Ok(mut store) = snapshots.lock() {
                    let changed = store.admit(&topic, &outcome, publication);
                    // Push delivery (T2): every registered session for this
                    // project gets the new item in its mailbox, so the next
                    // turn boundary can drain it without re-reading the whole
                    // snapshot. The push is non-blocking and never fails the
                    // receive loop.
                    if changed {
                        if let Ok(parsed) = crate::envelope::parse_topic(&topic) {
                            let items = store.snapshot(parsed.project, Utc::now());
                            for item in items {
                                channels.push(parsed.project, &item, SNAPSHOT_CAPACITY);
                            }
                            // Wake fanout (live-push T1): after the mailbox push,
                            // fire a best-effort metadata-only wake to every
                            // registered session on this project that asked for
                            // one. The hint is the admitted envelope's event id
                            // when the outcome carries one, else the topic class.
                            let hint = match &outcome {
                                ReceiveOutcome::Accepted(validated) => {
                                    Some(validated.as_envelope().id.clone())
                                }
                                _ => {
                                    Some(topic.split('/').next_back().unwrap_or("state").to_owned())
                                }
                            };
                            wake_all(channels, parsed.project, hint.as_deref());
                        }
                    }
                }
            }
            Ok(None) => {}
            Err(_) => break,
        }
    }
    transport.disconnect();
    liveness.mark_disconnected(Utc::now());
    eprintln!("loam connector: session down project={project_id}");
    EstablishOutcome::Ended
}

/// Run the connector. Reconciles the registry before touching an endpoint: a
/// missing database or an empty registry returns [`ServiceOutcome::Inert`]
/// without binding a socket. Only a non-empty registry binds the owner-only
/// endpoint and serves.
#[cfg(unix)]
pub fn run_service(global_root: &Path) -> Result<ServiceOutcome, ServiceError> {
    let Ok(db_path) = crate::provisioning::configured_registry_path(Some(global_root)) else {
        return Ok(ServiceOutcome::Inert);
    };
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
    let Ok(db_path) = crate::provisioning::configured_registry_path(Some(global_root)) else {
        return Ok(ServiceOutcome::Inert);
    };
    if !registry_has_enrollments(&db_path)? {
        return Ok(ServiceOutcome::Inert);
    }
    // The pipe name is a digest of the run dir, and every client (the harness
    // hook, `federation emit`) derives it from `global_root/run` — so the
    // endpoint must bind the same directory the clients digest, or the two
    // sides can never agree on a name.
    let run_dir = global_root.join("run");
    let endpoint = ipc::windows::bind(&run_dir).map_err(ServiceError::Ipc)?;
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
pub(crate) fn dispatch_for_key(
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
                // per-session drop is driven by the live-injection session end. The live
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
            // over the channel is live injection. Either ref may be absent: a
            // wake_ref-only or mailbox-only registration is valid.
            let session_id = request
                .payload
                .get("session_id")
                .and_then(|v| v.as_str())
                .ok_or(ipc::IpcError::InvalidRequest)?;
            let channel_ref = request
                .payload
                .get("channel_ref")
                .and_then(|v| v.as_str())
                .map(str::to_owned);
            let wake_ref = request
                .payload
                .get("wake_ref")
                .and_then(|v| v.as_str())
                .map(str::to_owned);
            state.channels.register(InjectChannel {
                session_id: session_id.to_owned(),
                project_id: row.project_id.clone(),
                channel_ref,
                wake_ref,
            });
            Ok(register_ack_json(session_id, &row.project_id))
        }
        Operation::FederationEmit => {
            // The CLI derived every authority-bearing field except one: the
            // authenticated principal, which only a live session knows. Bind it
            // here, validate the finished envelope through the envelope module, and hand it
            // to the session that owns the client. Nothing publishes from the
            // CLI process.
            let Some(identity) = state.sessions.identity(&row.project_id) else {
                return Ok(emit_json(&row, "not-shipped", "no-live-session", ""));
            };
            let (document, topic) = outbound_envelope(&request.payload, &row, &identity)
                .ok_or(ipc::IpcError::InvalidRequest)?;
            let owned_claims: Vec<String> = if identity.allowed_claims.is_empty() {
                vec![identity.principal_id.clone()]
            } else {
                identity.allowed_claims.clone()
            };
            let claims: Vec<&str> = owned_claims.iter().map(String::as_str).collect();
            let principal = AuthenticatedPrincipal::new(&identity.principal_id, &claims);
            let validated = crate::envelope::validate(
                document.as_bytes(),
                &topic,
                &principal,
                &ValidationConfig::default(),
                Utc::now(),
            )
            .map_err(|_| ipc::IpcError::InvalidRequest)?;
            let event_id = validated.as_envelope().id.clone();
            match state.sessions.ship(&row.project_id, validated) {
                EmitOutcome::Queued => Ok(emit_json(&row, "queued", "queued", &event_id)),
                EmitOutcome::NotShipped(reason) => Ok(emit_json(&row, "not-shipped", reason, "")),
            }
        }
        Operation::SnapshotGet => {
            // A read. Enrollment and project binding were already proven above,
            // so an unenrolled or cross-project caller never reaches here. The
            // snapshot is served from memory: nothing is opened for writing,
            // nothing is persisted, and no envelope bytes leave the connector.
            let items = state.sessions.snapshot(&row.project_id, Utc::now());
            Ok(snapshot_json(&row.project_id, &items))
        }
        Operation::SessionPollInject => {
            // Drain the session's mailbox (T2). The session must be registered
            // and bound to this project; the enrollment + project binding were
            // already proven above. The mailbox is volatile in-memory state:
            // nothing is persisted, and the drain consumes each item exactly
            // once. An unregistered session is refused, not silently empty —
            // a hook that never registered would otherwise read "no new items"
            // forever.
            let session_id = request
                .payload
                .get("session_id")
                .and_then(|v| v.as_str())
                .ok_or(ipc::IpcError::InvalidRequest)?;
            let items = state
                .channels
                .poll(session_id)
                .ok_or(ipc::IpcError::InvalidRequest)?;
            Ok(snapshot_json(&row.project_id, &items))
        }
        Operation::SessionDropInject => {
            // Remove the session from the volatile channel registry (live-push
            // T2). The mailbox is dropped with it. An unknown session is
            // refused, not silently accepted — a plugin that never registered
            // would otherwise believe its wake target was still live.
            let session_id = request
                .payload
                .get("session_id")
                .and_then(|v| v.as_str())
                .ok_or(ipc::IpcError::InvalidRequest)?;
            if !state.channels.drop_session(session_id) {
                return Err(ipc::IpcError::InvalidRequest);
            }
            Ok(crate::json::Value::Object(vec![
                ("schema".into(), crate::json::Value::Number("1".into())),
                (
                    "action".into(),
                    crate::json::Value::String("inject-channel-dropped".into()),
                ),
                (
                    "session_id".into(),
                    crate::json::Value::String(session_id.to_owned()),
                ),
            ]))
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
/// The emit projection. `status` is observational: an operation that reached no
/// live session is reported as not shipped, never as sent.
fn emit_json(
    row: &crate::enrollment::EnrolledRow,
    status: &str,
    reason: &str,
    event_id: &str,
) -> crate::json::Value {
    use crate::json::Value;
    Value::Object(vec![
        ("schema".into(), Value::Number("1".into())),
        ("status".into(), Value::String(status.to_owned())),
        ("reason".into(), Value::String(reason.to_owned())),
        ("project_id".into(), Value::String(row.project_id.clone())),
        ("event_id".into(), Value::String(event_id.to_owned())),
    ])
}

/// Build the outbound CloudEvents document and its topic from the CLI's derived
/// operation plus the live session's authenticated identity. Returns `None` for
/// a structurally impossible operation; everything else is the envelope module's job to
/// refuse when the document is validated.
fn outbound_envelope(
    operation: &crate::json::Value,
    row: &crate::enrollment::EnrolledRow,
    identity: &SessionIdentity,
) -> Option<(String, String)> {
    use crate::json::Value;
    let string = |key: &str| operation.get(key).and_then(Value::as_str).unwrap_or("");
    let owned = |key: &str| operation.get(key).cloned().unwrap_or(Value::Null);

    let (message_type, dataschema, class, intent) = match string("type") {
        "message.reply" => (
            "io.loam.message",
            "urn:loam:schema:message:1",
            "inbox",
            "response",
        ),
        "message.ack" => (
            "io.loam.message",
            "urn:loam:schema:message:1",
            "inbox",
            "ack",
        ),
        "work.report" => (
            "io.loam.work.state",
            "urn:loam:schema:work-state:1",
            "latest-state",
            "inform",
        ),
        _ => return None,
    };

    let event_id = string("id");
    let prefix = format!("loam/v1/{}/{}", row.org_id, row.project_id);
    let (delivery, to, topic) = if class == "inbox" {
        let recipients = operation.get("to").and_then(Value::as_array)?;
        let first = recipients.iter().find(|recipient| {
            matches!(
                recipient.get("kind").and_then(Value::as_str),
                Some("agent" | "principal" | "instance")
            )
        })?;
        let kind = first.get("kind").and_then(Value::as_str)?;
        let id = first.get("id").and_then(Value::as_str)?;
        (
            Value::Object(vec![("class".into(), Value::String("inbox".into()))]),
            Value::Array(recipients.to_vec()),
            format!(
                "{prefix}/inbox/{kind}/{id}/{}/{event_id}",
                identity.instance_id
            ),
        )
    } else {
        let key = operation.get("state_key").and_then(Value::as_str)?;
        let revision = operation.get("revision").and_then(Value::as_str)?;
        (
            Value::Object(vec![
                ("class".into(), Value::String("latest-state".into())),
                ("key".into(), Value::String(key.to_owned())),
                ("revision".into(), Value::Number(revision.to_owned())),
            ]),
            Value::Array(vec![Value::Object(vec![
                ("kind".into(), Value::String("project".into())),
                ("id".into(), Value::String(row.project_id.clone())),
            ])]),
            format!("{prefix}/state/{}/{key}", identity.instance_id),
        )
    };

    let mut context = vec![
        ("org_id".into(), Value::String(row.org_id.clone())),
        ("project_id".into(), Value::String(row.project_id.clone())),
        (
            "repository_id".into(),
            Value::String(row.repository_id.clone()),
        ),
        (
            "git".into(),
            Value::Object(vec![("base_oid".into(), Value::String(row.commit.clone()))]),
        ),
    ];
    context.push((
        "artifacts".into(),
        match operation.get("artifacts") {
            Some(artifacts @ Value::Array(_)) => artifacts.clone(),
            _ => Value::Array(Vec::new()),
        },
    ));

    let document = Value::Object(vec![
        ("specversion".into(), Value::String("1.0".into())),
        ("id".into(), Value::String(event_id.to_owned())),
        ("source".into(), Value::String(string("source").to_owned())),
        ("type".into(), Value::String(message_type.to_owned())),
        ("time".into(), Value::String(string("time").to_owned())),
        (
            "datacontenttype".into(),
            Value::String("application/json".into()),
        ),
        ("dataschema".into(), Value::String(dataschema.to_owned())),
        (
            "data".into(),
            Value::Object(vec![
                ("intent".into(), Value::String(intent.to_owned())),
                (
                    "from".into(),
                    Value::Object(
                        vec![
                            (
                                "principal_id".into(),
                                Value::String(identity.principal_id.clone()),
                            ),
                            ("agent_id".into(), Value::String(identity.agent_id.clone())),
                            (
                                "instance_id".into(),
                                Value::String(identity.instance_id.clone()),
                            ),
                        ]
                        .into_iter()
                        .chain(identity.display_name.clone().map(|name| {
                            // From the certificate the broker authenticated, so a
                            // caller cannot supply it and an absent one stays absent.
                            ("display_name".to_owned(), Value::String(name))
                        }))
                        .collect(),
                    ),
                ),
                ("to".into(), to),
                ("delivery".into(), delivery),
                ("thread".into(), owned("thread")),
                ("context".into(), Value::Object(context)),
                (
                    "expires_at".into(),
                    Value::String(string("expires_at").to_owned()),
                ),
                (
                    "summary".into(),
                    Value::String(string("summary").to_owned()),
                ),
                ("payload".into(), owned("payload")),
            ]),
        ),
    ]);
    Some((document.to_json(), topic))
}

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
                    Value::Object(
                        vec![
                            (
                                "principal_id".into(),
                                Value::String(item.from_principal_id.clone()),
                            ),
                            ("agent_id".into(), Value::String(item.from_agent_id.clone())),
                            (
                                "instance_id".into(),
                                Value::String(item.from_instance_id.clone()),
                            ),
                        ]
                        .into_iter()
                        .chain(item.from_display_name.clone().map(|name| {
                            // Only when the sender published one: an always-present
                            // empty name would render as an anonymous colleague.
                            ("display_name".to_owned(), Value::String(name))
                        }))
                        .collect(),
                    ),
                ),
                ("payload".into(), item.payload.clone()),
                // The receive path's Git verdict. Always present and always
                // explicit, so a missing field can never read as verified.
                (
                    "publication".into(),
                    Value::String(item.publication.code().to_owned()),
                ),
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
    if let Some(reason) = session_state.reason() {
        fields.push(("reason".into(), Value::String(reason.to_owned())));
    }
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
        &service_ctx.instance_id,
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
            display_name: None,
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
        // The probe envelope must pass envelope validation on its event topic.
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
    use std::io::Read;

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
        crate::enrollment::insert_enrollment(
            &mut connection,
            &enrollment,
            "instance-under-test",
            &caps(),
            "t",
        )
        .unwrap();
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
    /// must leave the database byte-identical. The snapshot is
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
                    from_display_name: None,
                    from_agent_id: "agent-72".into(),
                    from_instance_id: "instance-01".into(),
                    payload: crate::json::Value::Object(vec![]),
                    expires_at: Utc::now() + chrono::Duration::hours(1),
                    publication: Publication::Unverified,
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
        crate::enrollment::insert_enrollment(
            &mut writer,
            &synthetic(19, 190),
            "instance-under-test",
            &caps(),
            "t",
        )
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
            channel_ref: Some("c".into()),
            wake_ref: None,
        });
        assert!(state.channels.contains("sess-2"));
        assert!(state.channels.drop_session("sess-2"));
        assert!(!state.channels.contains("sess-2"));
        assert!(!state.channels.drop_session("sess-2")); // idempotent
    }

    // --- T2: mailbox queue + SessionPollInject ---

    fn poll_request(session_id: &str) -> Request {
        Request {
            request_id: "r-poll".into(),
            workspace: "/w/proj".into(),
            operation: Operation::SessionPollInject,
            payload: crate::json::Value::Object(vec![(
                "session_id".into(),
                crate::json::Value::String(session_id.into()),
            )]),
        }
    }

    fn sample_item(key: &str, summary: &str) -> SnapshotItem {
        SnapshotItem {
            key: key.into(),
            source: "urn:loam:instance:instance-01".into(),
            item_type: "io.loam.message".into(),
            summary: summary.into(),
            to: vec![("instance".into(), "instance-02".into())],
            org_id: "acme".into(),
            project_id: "loam".into(),
            repository_id: "repo".into(),
            from_principal_id: "employee-184".into(),
            from_display_name: None,
            from_agent_id: "agent-72".into(),
            from_instance_id: "instance-01".into(),
            payload: crate::json::Value::Object(vec![]),
            expires_at: Utc::now() + chrono::Duration::hours(1),
            publication: Publication::Unverified,
        }
    }

    #[test]
    fn poll_inject_drains_the_mailbox_and_a_second_poll_is_empty() {
        let (path, key) = enrolled_db("poll-drain", 11, 110);
        let mut state = ConnectorState::new();
        dispatch_for_key(
            &register_request("sess-poll", "chan-poll"),
            &key,
            &path,
            &mut state,
        )
        .expect("register");

        // The pump pushes after admit; drive the same push the pump performs.
        state
            .channels
            .push("loam", &sample_item("inbox:01", "Held."), SNAPSHOT_CAPACITY);

        let first =
            dispatch_for_key(&poll_request("sess-poll"), &key, &path, &mut state).expect("poll");
        let text = first.to_json();
        assert!(
            text.contains("Held."),
            "the poll must return the item: {text}"
        );

        let second = dispatch_for_key(&poll_request("sess-poll"), &key, &path, &mut state)
            .expect("poll again");
        assert!(
            !second.to_json().contains("Held."),
            "a second poll must be empty (drained): {}",
            second.to_json()
        );
    }

    #[test]
    fn two_sessions_on_the_same_project_both_receive_the_item() {
        let (path, key) = enrolled_db("poll-two", 12, 120);
        let mut state = ConnectorState::new();
        for session in ["sess-a", "sess-b"] {
            dispatch_for_key(
                &register_request(session, &format!("chan-{session}")),
                &key,
                &path,
                &mut state,
            )
            .expect("register");
        }

        state
            .channels
            .push("loam", &sample_item("inbox:02", "Both."), SNAPSHOT_CAPACITY);

        for session in ["sess-a", "sess-b"] {
            let polled =
                dispatch_for_key(&poll_request(session), &key, &path, &mut state).expect("poll");
            assert!(
                polled.to_json().contains("Both."),
                "session {session} must receive the item: {}",
                polled.to_json()
            );
        }
    }

    #[test]
    fn a_session_on_another_project_does_not_receive_the_item() {
        let (path, key) = enrolled_db("poll-other-project", 13, 130);
        let mut state = ConnectorState::new();
        dispatch_for_key(
            &register_request("sess-other", "chan-other"),
            &key,
            &path,
            &mut state,
        )
        .expect("register");

        // The registered session is bound to "loam"; pushing for another
        // project must not reach it.
        state.channels.push(
            "other-project",
            &sample_item("inbox:03", "Not yours."),
            SNAPSHOT_CAPACITY,
        );

        let polled =
            dispatch_for_key(&poll_request("sess-other"), &key, &path, &mut state).expect("poll");
        assert!(
            !polled.to_json().contains("Not yours."),
            "a session must not receive another project's items: {}",
            polled.to_json()
        );
    }

    #[test]
    fn drop_session_removes_its_mailbox_and_poll_refuses() {
        let (path, key) = enrolled_db("poll-drop", 14, 140);
        let mut state = ConnectorState::new();
        dispatch_for_key(
            &register_request("sess-drop", "chan-drop"),
            &key,
            &path,
            &mut state,
        )
        .expect("register");
        state.channels.push(
            "loam",
            &sample_item("inbox:04", "Dropped."),
            SNAPSHOT_CAPACITY,
        );

        assert!(state.channels.drop_session("sess-drop"));
        assert!(!state.channels.contains("sess-drop"));

        // An unregistered session is refused, not silently empty.
        let outcome = dispatch_for_key(&poll_request("sess-drop"), &key, &path, &mut state);
        assert_eq!(outcome.err(), Some(ipc::IpcError::InvalidRequest));
    }

    #[test]
    fn poll_inject_requires_an_enrolled_workspace() {
        let (path, _key) = enrolled_db("poll-unenrolled", 15, 150);
        let outcome = dispatch_for_key(
            &poll_request("sess-x"),
            "unix:404:404",
            &path,
            &mut ConnectorState::new(),
        );
        assert_eq!(outcome.err(), Some(ipc::IpcError::WorkspaceUnenrolled));
    }

    #[test]
    fn a_restart_starts_with_empty_mailboxes() {
        // Register + push for real, then drop the state: a restarted connector
        // must recover no mailbox (in-memory only, like the channel registry).
        let (path, key) = enrolled_db("poll-restart", 16, 160);
        let mut before = ConnectorState::new();
        dispatch_for_key(
            &register_request("sess-restart", "chan-restart"),
            &key,
            &path,
            &mut before,
        )
        .expect("register");
        before.channels.push(
            "loam",
            &sample_item("inbox:05", "Volatile."),
            SNAPSHOT_CAPACITY,
        );

        drop(before);
        let mut after = ConnectorState::new();
        let outcome = dispatch_for_key(&poll_request("sess-restart"), &key, &path, &mut after);
        assert_eq!(
            outcome.err(),
            Some(ipc::IpcError::InvalidRequest),
            "a restarted connector must recover no mailbox"
        );
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

    // --- T1 (live-push): wake fanout ---

    /// Bind a one-shot localhost TCP listener and return it plus its address,
    /// so a test can register a `notify-tcp://` wake_ref and observe the wake
    /// frame the connector delivers.
    fn wake_listener() -> (std::net::TcpListener, std::net::SocketAddr) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind wake listener");
        let address = listener.local_addr().expect("listener address");
        (listener, address)
    }

    /// Accept one wake connection on the listener and return the bytes read,
    /// waiting at most 2 seconds so a missing wake never hangs the test.
    fn accept_wake_frame(listener: &std::net::TcpListener) -> String {
        listener
            .set_nonblocking(true)
            .expect("listener nonblocking");
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let mut bytes = Vec::new();
                    let mut buffer = [0u8; 4096];
                    let read = stream
                        .set_read_timeout(Some(Duration::from_secs(1)))
                        .and_then(|_| stream.read(&mut buffer))
                        .unwrap_or(0);
                    bytes.extend_from_slice(&buffer[..read]);
                    return String::from_utf8_lossy(&bytes).into_owned();
                }
                Err(ref error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if std::time::Instant::now() >= deadline {
                        panic!("no wake connection arrived within 2s");
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(other) => panic!("wake accept failed: {other}"),
            }
        }
    }

    /// A wake_ref-bearing registration, mirroring the pump's fanout shape.
    fn wake_register_request(session_id: &str, channel_ref: &str, wake_ref: &str) -> Request {
        Request {
            request_id: "r-wake".into(),
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
                (
                    "wake_ref".into(),
                    crate::json::Value::String(wake_ref.into()),
                ),
            ]),
        }
    }

    /// The rendered field values of an admitted item that must never appear in
    /// a wake frame. Scanning for these proves the wake channel is
    /// metadata-only.
    fn item_render_fields(item: &SnapshotItem) -> Vec<String> {
        vec![
            item.summary.clone(),
            item.from_principal_id.clone(),
            item.key.clone(),
            item.payload.to_json(),
        ]
    }

    #[test]
    fn registered_session_with_wake_ref_receives_a_metadata_only_wake_frame() {
        let (path, key) = enrolled_db("wake-tcp", 20, 200);
        let mut state = ConnectorState::new();
        let (listener, address) = wake_listener();
        let wake_ref = format!("notify-tcp://{address}");
        dispatch_for_key(
            &wake_register_request("sess-wake", "chan-wake", &wake_ref),
            &key,
            &path,
            &mut state,
        )
        .expect("register with wake_ref");

        let item = sample_item("inbox:wake:01", "Private summary.");

        // Fan out through the *pump's* registry, exactly as the receive path
        // does after a changed admit. `ConnectorState::new` shares ONE registry
        // between the IPC side and the pumps — the live-wake defect was a
        // second registry inside ProjectSessions that never saw IPC
        // registrations, so no wake ever fired in production.
        let pump_registry = state.sessions.channels().clone();
        pump_registry.push("loam", &item, SNAPSHOT_CAPACITY);
        wake_all(&pump_registry, "loam", Some("event-id-42"));

        let frame = accept_wake_frame(&listener);
        let parsed = crate::json::parse(&frame).expect("wake frame is valid JSON");
        assert_eq!(
            parsed.get("kind").and_then(crate::json::Value::as_str),
            Some("loam-wake")
        );
        assert_eq!(
            parsed.get("project").and_then(crate::json::Value::as_str),
            Some("loam")
        );
        assert_eq!(
            parsed.get("hint").and_then(crate::json::Value::as_str),
            Some("event-id-42")
        );
        for field in item_render_fields(&item) {
            assert!(
                !frame.contains(&field),
                "the wake frame must not carry item content, found {field:?}: {frame}"
            );
        }
        assert!(
            !frame.contains("summary") && !frame.contains("principal"),
            "no sender-content keys may ride the wake frame: {frame}"
        );
    }

    #[test]
    fn an_ipc_registration_is_seen_by_the_pump_registry() {
        // The live-wake regression: `ConnectorState::new` must hand the pumps
        // the SAME registry the IPC side writes to. Before the fix, a second
        // registry inside `ProjectSessions` made `wake_all` fire zero connects
        // no matter how many sessions registered.
        let (path, key) = enrolled_db("wake-shared", 21, 210);
        let mut state = ConnectorState::new();
        let (listener, address) = wake_listener();
        let wake_ref = format!("notify-tcp://{address}");
        dispatch_for_key(
            &wake_register_request("sess-shared", "chan-shared", &wake_ref),
            &key,
            &path,
            &mut state,
        )
        .expect("register with wake_ref");

        // The pump side is `state.sessions.channels()`; a registration made
        // through the IPC side (`state.channels`) must be visible there.
        assert!(
            state.sessions.channels().contains("sess-shared"),
            "an IPC registration must be visible to the pump registry"
        );
        assert_eq!(state.sessions.channels().len(), 1);

        // And a fanout driven through the pump side must fire the connect.
        wake_all(state.sessions.channels(), "loam", Some("event-id-43"));
        let frame = accept_wake_frame(&listener);
        assert!(
            frame.contains("loam-wake"),
            "the pump-side fanout must reach the registered wake target: {frame}"
        );
    }

    #[test]
    fn session_without_wake_ref_produces_no_wake() {
        let mut channels = ChannelRegistry::new();
        let (listener, address) = wake_listener();
        // Register without a wake_ref; a plain channel is the mailbox-only case.
        channels.register(InjectChannel {
            session_id: "sess-plain".into(),
            project_id: "loam".into(),
            channel_ref: Some("chan-plain".into()),
            wake_ref: None,
        });
        // A second session has a wake_ref, so a wake *would* fire if the plain
        // one's absence were mis-read.
        channels.register(InjectChannel {
            session_id: "sess-wakey".into(),
            project_id: "loam".into(),
            channel_ref: Some("chan-wakey".into()),
            wake_ref: Some(format!("notify-tcp://{address}")),
        });

        channels.push(
            "loam",
            &sample_item("inbox:wake:02", "Plain."),
            SNAPSHOT_CAPACITY,
        );
        wake_all(&channels, "loam", None);

        // Only the wake_ref-bearing session connects.
        let frame = accept_wake_frame(&listener);
        assert!(
            !frame.is_empty(),
            "the wake_ref session must still be woken"
        );
    }

    #[test]
    fn wake_to_a_dead_port_never_blocks_or_fails_the_push() {
        let mut channels = ChannelRegistry::new();
        // An address nothing listens on: connect must fail fast, and the error
        // must be swallowed — the pump loop keeps going either way.
        channels.register(InjectChannel {
            session_id: "sess-dead".into(),
            project_id: "loam".into(),
            channel_ref: Some("chan-dead".into()),
            wake_ref: Some("notify-tcp://127.0.0.1:1".into()),
        });

        let item = sample_item("inbox:wake:03", "Still stored.");
        channels.push("loam", &item, SNAPSHOT_CAPACITY);
        // No panic, no hang: wake_all returns normally.
        wake_all(&channels, "loam", Some("hint-dead"));

        // And the mailbox still holds the item for the next poll.
        let drained = channels.poll("sess-dead").expect("registered");
        assert_eq!(drained.len(), 1);
    }

    #[test]
    fn two_sessions_with_wake_refs_both_get_woken() {
        let mut channels = ChannelRegistry::new();
        let (listener_a, address_a) = wake_listener();
        let (listener_b, address_b) = wake_listener();
        channels.register(InjectChannel {
            session_id: "sess-a".into(),
            project_id: "loam".into(),
            channel_ref: Some("chan-a".into()),
            wake_ref: Some(format!("notify-tcp://{address_a}")),
        });
        channels.register(InjectChannel {
            session_id: "sess-b".into(),
            project_id: "loam".into(),
            channel_ref: Some("chan-b".into()),
            wake_ref: Some(format!("notify-tcp://{address_b}")),
        });

        channels.push(
            "loam",
            &sample_item("inbox:wake:04", "Both woken."),
            SNAPSHOT_CAPACITY,
        );
        wake_all(&channels, "loam", Some("hint-both"));

        let frame_a = accept_wake_frame(&listener_a);
        let frame_b = accept_wake_frame(&listener_b);
        assert!(frame_a.contains("loam-wake"));
        assert!(frame_b.contains("loam-wake"));
    }

    #[test]
    fn an_unknown_wake_scheme_is_ignored_silently() {
        let mut channels = ChannelRegistry::new();
        // A wake_ref the connector does not understand: skipped silently, and
        // the mailbox still holds the item for the next poll.
        channels.register(InjectChannel {
            session_id: "sess-odd".into(),
            project_id: "loam".into(),
            channel_ref: Some("chan-odd".into()),
            wake_ref: Some("telepathy://session-1".into()),
        });
        channels.push(
            "loam",
            &sample_item("inbox:wake:05", "Odd."),
            SNAPSHOT_CAPACITY,
        );
        // Must not panic, must not hang, must not propagate an error.
        wake_all(&channels, "loam", Some("hint-odd"));
        let drained = channels.poll("sess-odd").expect("registered");
        assert_eq!(drained.len(), 1, "mailbox survives an unknown wake scheme");
        assert_eq!(channels.len(), 1, "the session stays registered");
    }

    #[test]
    fn registration_accepts_mailbox_only_without_channel_or_wake_ref() {
        let (path, key) = enrolled_db("wake-mailbox-only", 21, 210);
        let mut state = ConnectorState::new();
        // Neither ref present: mailbox-only registration, valid per the plan.
        let request = Request {
            request_id: "r-mb".into(),
            workspace: "/w/proj".into(),
            operation: Operation::SessionRegisterInject,
            payload: crate::json::Value::Object(vec![(
                "session_id".into(),
                crate::json::Value::String("sess-mb".into()),
            )]),
        };
        let result = dispatch_for_key(&request, &key, &path, &mut state).expect("register");
        assert!(result.to_json().contains("inject-channel-registered"));
        assert!(state.channels.contains("sess-mb"));
        // And the channel is pollable: a mailbox-only session still receives
        // items.
        state.channels.push(
            "loam",
            &sample_item("inbox:wake:07", "Mailbox."),
            SNAPSHOT_CAPACITY,
        );
        let drained = state.channels.poll("sess-mb").expect("registered");
        assert_eq!(drained.len(), 1);
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
            display_name: None,
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
        // The global root is created explicitly; the instance id is a test
        // constant (the certificate is the identity source in production).
        let root = crate::enrollment::temp_global_root(label);
        let instance_id = "01ARZ3NDEKTSV4RRFFQ69G5FAV".to_owned();
        let ctx = ServiceContext {
            global_root: root.clone(),
            instance_id,
            runtime_path: std::env::temp_dir().join("loam-rt").join("loam"),
            // A temp systemd user dir, so the Linux symlink step never touches
            // a real user config in tests.
            systemd_user_dir: Some(std::env::temp_dir().join(format!(
                    "loam-connect-{label}-systemd-{}",
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_nanos()
                ))),
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
        // The global root is created explicitly; the instance id is a test
        // constant (the certificate is the identity source in production).
        let root = crate::enrollment::temp_global_root(label);
        let instance_id = "01ARZ3NDEKTSV4RRFFQ69G5FAV".to_owned();
        let ctx = ServiceContext {
            global_root: root.clone(),
            instance_id,
            runtime_path: std::env::temp_dir().join("loam-rt").join("loam"),
            // A temp systemd user dir, so the Linux symlink step never touches
            // a real user config in tests.
            systemd_user_dir: Some(std::env::temp_dir().join(format!(
                    "loam-lifecycle-{label}-systemd-{}",
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_nanos()
                ))),
        };
        (root.join("loam.sqlite3"), ctx)
    }

    fn insert(db: &Path, enrollment: &ValidatedEnrollment) {
        let mut connection = crate::enrollment::open_writable(db).unwrap();
        crate::enrollment::insert_enrollment(
            &mut connection,
            enrollment,
            "instance-under-test",
            &caps(),
            "t",
        )
        .unwrap();
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
    //! The bounded in-memory snapshot contract.
    //!
    //! Every case drives real frames through the `DeliveryProcessor` — the
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
                    // An empty MQTT payload is the tombstone: the transport resolves it
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
                store.admit(&topic, &outcome, Publication::Unverified);
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
        assert!(before.admit(&topic, &outcome, Publication::Unverified));
        assert_eq!(before.len("project-7M3"), 1);

        drop(before);
        let mut after = SnapshotStore::new(4).expect("capacity");
        assert!(after.snapshot("project-7M3", now).is_empty());
        assert!(after.is_empty());
    }

    /// One enrolled row for the attach-path tests. Only the identity key varies,
    /// so a test that needs two distinct workspaces cannot collide by accident.
    fn sample_row(identity_key: &str) -> crate::enrollment::EnrolledRow {
        crate::enrollment::EnrolledRow {
            identity_key: identity_key.into(),
            org_id: "org-3A1".into(),
            project_id: "project-7M3".into(),
            repository_id: "repo-2F8".into(),
            descriptor_digest: "d".into(),
            display_path: "/w".into(),
            instance_id: RECIPIENT_INSTANCE.into(),
            broker_profile: "p".into(),
            broker_endpoint: "mqtts://broker.example:8883".into(),
            tls_server_name: "broker.example".into(),
            ca_ref: None,
            commit: "84be000000000000000000000000000000000001".into(),
            capabilities: crate::enrollment::CapabilityRecord {
                authentication: true,
                publish: true,
                subscribe: true,
                self_receive: true,
                verified_at: "2026-07-24T14:20:00Z".into(),
            },
            remotes: Vec::new(),
        }
    }

    #[test]
    fn the_failure_codes_are_unchanged_and_the_reasons_are_additive() {
        // `code()` is a tested IPC contract other slices pin, so the reason is
        // an added field and never a renamed state. An operator debugging a real
        // deployment needs to know which of eight inputs failed; the eight do
        // not each deserve a variant.
        assert_eq!(SessionState::Live.code(), "live");
        assert_eq!(SessionState::AlreadyLive.code(), "already-live");
        assert_eq!(
            SessionState::CredentialsUnresolved(reason::CREDENTIAL_REF_UNRESOLVED).code(),
            "credentials-unresolved"
        );
        assert_eq!(
            SessionState::NoPeerRoster(reason::ROSTER_ABSENT).code(),
            "no-peer-roster"
        );
        assert_eq!(SessionState::Unreachable("x".into()).code(), "unreachable");

        // Every reason is reachable and distinct: a duplicated constant would
        // make two different failures indistinguishable to the operator.
        let all = [
            reason::ENDPOINT_MALFORMED,
            reason::CREDENTIAL_REF_UNRESOLVED,
            reason::IDENTITY_REQUIRED,
            reason::CA_UNRESOLVED,
            reason::ROSTER_ABSENT,
            reason::ROSTER_EMPTY,
            reason::ROSTER_NO_ORIGINS,
            reason::ROSTER_WILDCARD,
            reason::ROSTER_MALFORMED,
        ];
        let mut seen = all.to_vec();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), all.len(), "reason codes must be distinct");

        let row = sample_row("unix:1:9");
        for state in [
            SessionState::CredentialsUnresolved(reason::ENDPOINT_MALFORMED),
            SessionState::NoPeerRoster(reason::ROSTER_WILDCARD),
        ] {
            let json = attach_json(&row, &state).to_json();
            assert!(
                json.contains(&format!(
                    "\"reason\":\"{}\"",
                    state.reason().expect("reason")
                )),
                "attach JSON must name the failed input: {json}"
            );
            assert!(
                json.contains(&format!("\"session_state\":\"{}\"", state.code())),
                "the code contract must survive alongside the reason: {json}"
            );
        }

        // Positive control: a state with no failed input carries no reason key,
        // so `reason` is never a decorative always-present field.
        assert!(SessionState::Live.reason().is_none());
        let live = attach_json(&row, &SessionState::Live).to_json();
        assert!(!live.contains("\"reason\""), "{live}");
    }

    #[test]
    fn an_unresolvable_attach_opens_no_session_and_names_the_input() {
        // Driven through the real seam, and deliberately through the one input
        // that fails *before* the secret store is consulted: a unit test must
        // not depend on this machine's keyring, and it must never risk a
        // desktop unlock prompt inside `cargo test`. The backend's own refusals
        // are covered against an explicit failing backend in `provisioning`.
        let mut sessions = ProjectSessions::new(4, ChannelRegistry::new());
        let mut row = sample_row("unix:1:1");
        row.broker_endpoint = "not-an-endpoint".into();
        assert_eq!(
            sessions.attach(&row, provision_session(&row), base_time()),
            SessionState::CredentialsUnresolved(reason::ENDPOINT_MALFORMED)
        );
        assert!(!sessions.is_live(&row.project_id));
        assert!(sessions.snapshot(&row.project_id, base_time()).is_empty());
    }

    #[test]
    fn a_provisioned_but_rosterless_project_opens_no_session() {
        // Credentials without a peer roster would open a session that can admit
        // no colleague; refusing it beats a live session that hears nothing.
        fn rosterless() -> Result<(MqttSession, PeerRoster), ProvisionFailure> {
            let config = TransportConfig::new(
                "localhost",
                1883,
                "loam-connector-test",
                8,
                400_000,
                ValidationConfig::default(),
            )
            .expect("transport config");
            Ok((
                MqttSession {
                    config,
                    username: None,
                    password: None,
                    ca_certificate: rustls::RootCertStore::empty(),
                    client_authentication: None,
                    claimed_identity: SessionIdentity {
                        principal_id: SENDER_PRINCIPAL.into(),
                        agent_id: "agent-72".into(),
                        instance_id: RECIPIENT_INSTANCE.into(),
                        display_name: None,
                        allowed_claims: Vec::new(),
                    },
                },
                PeerRoster::default(),
            ))
        }

        let mut sessions = ProjectSessions::new(4, ChannelRegistry::new());
        let row = sample_row("unix:1:2");
        assert_eq!(
            sessions.attach(&row, rosterless(), base_time()),
            SessionState::NoPeerRoster(reason::ROSTER_EMPTY)
        );
        assert!(!sessions.is_live(&row.project_id));
    }

    #[test]
    fn a_first_dial_failure_still_arms_the_supervisor_and_watchdog() {
        // A connector that boots while the broker is down must not give up: the
        // first dial fails (closed local port, no broker), attach reports the
        // honest Unreachable, but a supervised session exists and keeps retrying
        // so the watchdog can eventually hand recovery to the OS supervisor. A
        // typed refusal, by contrast, arms nothing (covered above).
        fn to_a_closed_port() -> Result<(MqttSession, PeerRoster), ProvisionFailure> {
            let config = TransportConfig::new(
                "127.0.0.1",
                1, // closed: connection refused is an immediate dial failure
                "loam-connector-test",
                8,
                400_000,
                ValidationConfig::default(),
            )
            .expect("transport config");
            Ok((
                MqttSession {
                    config,
                    username: None,
                    password: None,
                    ca_certificate: rustls::RootCertStore::empty(),
                    client_authentication: None,
                    claimed_identity: SessionIdentity {
                        principal_id: SENDER_PRINCIPAL.into(),
                        agent_id: "agent-72".into(),
                        instance_id: RECIPIENT_INSTANCE.into(),
                        display_name: None,
                        allowed_claims: Vec::new(),
                    },
                },
                PeerRoster {
                    principals: vec![SENDER_PRINCIPAL.into()],
                    origins: vec![RECIPIENT_INSTANCE.into()],
                },
            ))
        }

        let mut sessions = ProjectSessions::new(4, ChannelRegistry::new());
        let row = sample_row("unix:1:9");
        let state = sessions.attach(&row, to_a_closed_port(), base_time());
        assert!(
            matches!(state, SessionState::Unreachable(_)),
            "a closed-port first dial reports Unreachable, got {state:?}"
        );
        assert!(
            sessions.is_live(&row.project_id),
            "an Unreachable first cycle must still arm a supervised session"
        );
        // Stop the retry loop so the test leaves no dialing thread behind.
        assert!(sessions.detach(&row.project_id));
    }

    #[test]
    fn supervisor_reconnects_after_a_transient_drop_and_clears_the_failure_count() {
        // Scenario: transient drop heals without escalation. Cycle 1 fails to
        // establish; cycle 2 opens, runs, and drops. The supervisor must run the
        // second cycle (reconnect happened) and a successful open must clear the
        // failure count, so no status surface ever reaches observed-degraded.
        use std::sync::atomic::{AtomicBool, Ordering};
        let stop = AtomicBool::new(false);
        let liveness = LivenessHandle::established(base_time());
        let mut cycle = 0u32;
        supervise(
            "project-7M3",
            || {
                let outcome = match cycle {
                    0 => EstablishOutcome::Failed("dial-timeout".into()),
                    _ => {
                        // A session opened and dropped, exactly as the real
                        // establish-and-run would record it.
                        liveness.mark_established(base_time());
                        liveness.mark_disconnected(base_time());
                        EstablishOutcome::Ended
                    }
                };
                cycle += 1;
                if cycle == 2 {
                    stop.store(true, Ordering::Relaxed);
                }
                outcome
            },
            |_backoff| {},
            &stop,
            &liveness,
        );
        assert_eq!(cycle, 2, "the supervisor must re-establish after a drop");
        let observed = liveness.observe();
        assert!(!observed.established);
        assert_eq!(observed.consecutive_failures, 0, "a successful cycle clears the count");
        assert!(!observed.degraded());
    }

    #[test]
    fn supervisor_counts_consecutive_failures_toward_degrade() {
        // Every cycle fails; after DEGRADE_AFTER cycles the observation is
        // degraded and carries the last error category.
        use std::sync::atomic::{AtomicBool, Ordering};
        let stop = AtomicBool::new(false);
        let liveness = LivenessHandle::established(base_time());
        let mut cycle = 0u32;
        supervise(
            "project-7M3",
            || {
                cycle += 1;
                if cycle == DEGRADE_AFTER {
                    stop.store(true, Ordering::Relaxed);
                }
                EstablishOutcome::Failed("connection-refused".into())
            },
            |_backoff| {},
            &stop,
            &liveness,
        );
        assert_eq!(cycle, DEGRADE_AFTER);
        let observed = liveness.observe();
        assert_eq!(observed.consecutive_failures, DEGRADE_AFTER);
        assert!(observed.degraded());
        assert_eq!(observed.last_error.as_deref(), Some("connection-refused"));
    }

    #[test]
    fn supervisor_disarms_on_a_typed_refusal() {
        // A provisioning refusal is steady state, not a wedge: the supervisor
        // stops rather than churning, and records the refusal reason.
        use std::sync::atomic::AtomicBool;
        let stop = AtomicBool::new(false);
        let liveness = LivenessHandle::established(base_time());
        let mut cycle = 0u32;
        supervise(
            "project-7M3",
            || {
                cycle += 1;
                EstablishOutcome::Refused(reason::ROSTER_ABSENT)
            },
            |_backoff| panic!("a disarmed supervisor must not back off"),
            &stop,
            &liveness,
        );
        assert_eq!(cycle, 1, "the supervisor stops after one refusal");
        assert_eq!(liveness.observe().refusal, Some(reason::ROSTER_ABSENT));
    }

    #[test]
    fn watchdog_holds_until_the_sessionless_budget_then_exits() {
        // A mock clock: synthetic instants, so the ten-minute budget is proven
        // without a wait and without ever calling process::exit in a test.
        let start = std::time::Instant::now();
        let budget = SESSIONLESS_EXIT_AFTER;
        assert_eq!(watchdog_verdict(start, start, budget), WatchdogVerdict::Retry);
        assert_eq!(
            watchdog_verdict(start, start + budget - Duration::from_secs(1), budget),
            WatchdogVerdict::Retry,
            "one second short of the budget keeps retrying"
        );
        assert_eq!(
            watchdog_verdict(start, start + budget, budget),
            WatchdogVerdict::Exit,
            "the budget boundary hands recovery to the supervisor"
        );
        assert_eq!(WATCHDOG_EXIT_CODE, 75);
    }

    #[test]
    fn backoff_is_hard_capped_and_grows_with_failures() {
        // Jitter only ever subtracts, so the cap is a true ceiling, and more
        // consecutive failures never produce a shorter nominal backoff.
        assert!(backoff_with_jitter(1, BACKOFF_CAP) <= BACKOFF_BASE);
        assert!(backoff_with_jitter(50, BACKOFF_CAP) <= BACKOFF_CAP);
        assert!(backoff_with_jitter(3, BACKOFF_CAP) > backoff_with_jitter(1, BACKOFF_CAP));
    }

    #[test]
    fn a_live_session_subscribes_to_colleagues_not_only_itself() {
        let identity = SessionIdentity {
            principal_id: SENDER_PRINCIPAL.into(),
            agent_id: "agent-72".into(),
            instance_id: RECIPIENT_INSTANCE.into(),
            display_name: None,
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
            from_display_name: None,
            from_agent_id: "agent-72".into(),
            from_instance_id: SENDER_INSTANCE.into(),
            payload: crate::json::Value::Object(vec![]),
            expires_at: base_time(),
            publication: Publication::Unverified,
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

#[cfg(test)]
mod outbound_tests {
    //! The CLI derives an *operation*; this module builds the
    //! envelope from it. Nothing tested that seam, and `work.report` was broken
    //! across it — `revision` was derived as a JSON number and read back with
    //! `as_str`, so `outbound_envelope` returned `None` and every work report
    //! surfaced as `connector_refused`.

    use super::*;
    use crate::json::Value;

    fn row() -> crate::enrollment::EnrolledRow {
        crate::enrollment::EnrolledRow {
            identity_key: "unix:1:1".into(),
            org_id: "acme".into(),
            project_id: "loam".into(),
            repository_id: "repo".into(),
            descriptor_digest: "d".into(),
            display_path: "/w".into(),
            instance_id: "instance-01".into(),
            broker_profile: "p".into(),
            broker_endpoint: "mqtts://broker.example:8883".into(),
            tls_server_name: "broker.example".into(),
            ca_ref: None,
            commit: "84be000000000000000000000000000000000001".into(),
            capabilities: crate::enrollment::CapabilityRecord {
                authentication: true,
                publish: true,
                subscribe: true,
                self_receive: true,
                verified_at: "2026-07-24T14:20:00Z".into(),
            },
            remotes: Vec::new(),
        }
    }

    fn identity() -> SessionIdentity {
        SessionIdentity {
            principal_id: "employee-42".into(),
            agent_id: "agent-7".into(),
            instance_id: "instance-01".into(),
            display_name: None,
            allowed_claims: vec!["employee-42".into()],
        }
    }

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-07-24T14:20:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    /// Derive through the real CLI path, then build through the real connector
    /// path — the round trip neither half tested on its own.
    fn round_trip(operation: &str) -> Option<(String, String)> {
        let parsed = crate::json::parse(operation).expect("operation parses");
        let derived =
            crate::federation::derive_emit(&parsed, &row(), now()).expect("the CLI derives");
        outbound_envelope(&derived.operation, &row(), &identity())
    }

    /// The same round trip, but with a session claiming an instance other than
    /// the enrolled row's.
    fn round_trip_with(
        operation: &str,
        identity: &SessionIdentity,
    ) -> Result<crate::envelope::ValidatedEnvelope, crate::envelope::Violation> {
        let parsed = crate::json::parse(operation).expect("operation parses");
        let derived =
            crate::federation::derive_emit(&parsed, &row(), now()).expect("the CLI derives");
        let (document, topic) = outbound_envelope(&derived.operation, &row(), identity)
            .expect("the connector builds an envelope");
        let claims: Vec<&str> = identity.allowed_claims.iter().map(String::as_str).collect();
        let principal =
            crate::envelope::AuthenticatedPrincipal::new(&identity.principal_id, &claims);
        crate::envelope::validate(
            document.as_bytes(),
            &topic,
            &principal,
            &ValidationConfig::default(),
            now(),
        )
    }

    #[test]
    fn a_session_claiming_another_instance_is_refused_and_ships_nothing() {
        // The negative control that makes the instance-id unification
        // load-bearing. `federation emit` derives `source` from the enrolled
        // row while the connector derives the topic origin from the session, so
        // a deployment that opened a session under any other instance id would
        // ship envelopes Slice A rejects — surfacing to a user as an
        // unexplained `connector_refused` with no hint of the cause.
        let operation = r#"{"type":"work.report","state_key":"task-7","revision":"12","summary":"ready","payload":{"state":"ready"}}"#;

        // Positive control first, in the same run: the enrolled instance id
        // validates, so the refusal below is the mismatch and not the fixture.
        assert!(
            round_trip_with(operation, &identity()).is_ok(),
            "the enrolled instance must validate"
        );

        let mut divergent = identity();
        divergent.instance_id = "instance-99".into();
        let violation = round_trip_with(operation, &divergent)
            .expect_err("a divergent instance id must be refused");
        assert_eq!(
            violation,
            crate::envelope::Violation::SourceInstanceMismatch,
            "a divergent session must be refused as a source mismatch, not accepted"
        );
    }

    #[test]
    fn the_certificate_display_name_reaches_the_envelope_and_an_absent_one_stays_absent() {
        let operation = r#"{"type":"work.report","state_key":"task-7","revision":"12","summary":"ready","payload":{"state":"ready"}}"#;
        let parsed = crate::json::parse(operation).expect("operation parses");
        let derived =
            crate::federation::derive_emit(&parsed, &row(), now()).expect("the CLI derives");

        let mut named = identity();
        named.display_name = Some("Ada Lovelace".into());
        let (document, _) =
            outbound_envelope(&derived.operation, &row(), &named).expect("envelope");
        assert!(
            document.contains("\"display_name\":\"Ada Lovelace\""),
            "the authenticated given name must reach data.from: {document}"
        );

        // Control: a certificate without a given name leaves the field absent
        // rather than sending an empty one.
        let (plain, _) =
            outbound_envelope(&derived.operation, &row(), &identity()).expect("envelope");
        assert!(!plain.contains("display_name"), "{plain}");
    }

    #[test]
    fn a_derived_work_report_builds_an_envelope_and_its_revision_survives() {
        let (document, topic) = round_trip(
            r#"{"type":"work.report","state_key":"task-7","revision":"12","summary":"ready","payload":{"state":"ready"}}"#,
        )
        .expect("the connector builds an envelope from the CLI's derived work report");
        assert_eq!(topic, "loam/v1/acme/loam/state/instance-01/task-7");
        let parsed = crate::json::parse(&document).expect("envelope parses");
        let delivery = parsed
            .get("data")
            .and_then(|data| data.get("delivery"))
            .expect("delivery");
        assert_eq!(
            delivery.get("class").and_then(Value::as_str),
            Some("latest-state")
        );
        assert_eq!(delivery.get("key").and_then(Value::as_str), Some("task-7"));
        // The caller's revision, not the derived default.
        assert_eq!(
            delivery.get("revision"),
            Some(&Value::Number("12".into())),
            "revision must survive the CLI→connector round trip: {document}"
        );
    }

    #[test]
    fn a_numeric_revision_is_read_as_the_same_revision_not_silently_defaulted() {
        // A JSON-numeric `revision` is the natural spelling; reading it only as
        // a string would coerce every such report to revision 1.
        let (document, _topic) = round_trip(
            r#"{"type":"work.report","state_key":"task-7","revision":12,"summary":"ready","payload":{"state":"ready"}}"#,
        )
        .expect("envelope");
        let parsed = crate::json::parse(&document).expect("envelope parses");
        assert_eq!(
            parsed
                .get("data")
                .and_then(|data| data.get("delivery"))
                .and_then(|delivery| delivery.get("revision")),
            Some(&Value::Number("12".into())),
            "{document}"
        );
    }

    #[test]
    fn the_reply_path_still_builds_its_inbox_envelope() {
        // The positive control that this round trip is not work.report-shaped by
        // accident: the untouched vocabulary types still build.
        let (document, topic) = round_trip(
            r#"{"type":"message.reply","causation_id":"c-1","summary":"On it.","to":[{"kind":"instance","id":"instance-02"}],"payload":{}}"#,
        )
        .expect("envelope");
        assert!(
            topic.starts_with("loam/v1/acme/loam/inbox/instance/instance-02/instance-01/"),
            "{topic}"
        );
        assert!(document.contains("\"intent\":\"response\""), "{document}");
    }
}
