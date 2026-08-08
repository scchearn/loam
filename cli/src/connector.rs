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
