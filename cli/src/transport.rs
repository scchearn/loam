use crate::envelope::{
    self, AuthenticatedPrincipal, Intent, TopicDelivery, ValidatedEnvelope, ValidationConfig,
    Violation, MAX_MQTT_TOPIC_BYTES,
};
use crate::sha256::Sha256;
use chrono::{DateTime, Utc};
use rumqttc::v5::mqttbytes::{v5::PublishProperties, QoS};
use rumqttc::v5::{Client, MqttOptions};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

// Conservative room for the MQTT header, packet ID, property lengths, expiry,
// payload-format indicator, and application/json content type.
const MQTT_PUBLISH_FRAMING_BYTES: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportConfig {
    broker: String,
    port: u16,
    client_id: String,
    request_capacity: usize,
    max_packet_bytes: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportError {
    EmptyBroker,
    ZeroPort,
    EmptyClientId,
    ZeroRequestCapacity,
    ZeroMaxPacketBytes,
    EnvelopeExceedsPacketLimit,
    ZeroTrackingCapacity,
    InvalidExpiry,
    Expired,
    MissingInboxRecipient,
    MissingStateKey,
    InvalidStateRevision,
    EventTombstone,
    SemanticReplyMismatch,
    InvalidLifecycleDuration,
    OriginNotAuthorized,
    ClientQueue,
    Validation(Violation),
}

impl TransportError {
    /// A stable, content-free name for the refusal, for breadcrumbs and
    /// diagnostics (#103).
    ///
    /// The guard that keeps this content-free, stated precisely because an
    /// earlier version of this comment stated it wrongly: it is *not* that
    /// `TransportError` is `Copy`. `&'static str` is `Copy` and so is any
    /// `&str`, so a `Copy` enum can hold borrowed frame content perfectly well.
    /// What actually holds is that every arm below returns a literal, and the
    /// one arm that interpolates formats a `Violation` whose only payload is a
    /// fieldless `BindingAxis`. Adding a variant that carries a string — or
    /// giving `BindingAxis` a payload — is what would break this, and neither is
    /// prevented by the type system. The `Display` text is prose for a human
    /// reading one error; this is the token a log is grepped by.
    pub fn code(&self) -> String {
        let name = match self {
            Self::EmptyBroker => "empty_broker",
            Self::ZeroPort => "zero_port",
            Self::EmptyClientId => "empty_client_id",
            Self::ZeroRequestCapacity => "zero_request_capacity",
            Self::ZeroMaxPacketBytes => "zero_max_packet_bytes",
            Self::EnvelopeExceedsPacketLimit => "envelope_exceeds_packet_limit",
            Self::ZeroTrackingCapacity => "zero_tracking_capacity",
            Self::InvalidExpiry => "invalid_expiry",
            Self::Expired => "expired",
            Self::MissingInboxRecipient => "missing_inbox_recipient",
            Self::MissingStateKey => "missing_state_key",
            Self::InvalidStateRevision => "invalid_state_revision",
            Self::EventTombstone => "event_tombstone",
            Self::SemanticReplyMismatch => "semantic_reply_mismatch",
            Self::InvalidLifecycleDuration => "invalid_lifecycle_duration",
            Self::OriginNotAuthorized => "origin_not_authorized",
            Self::ClientQueue => "client_queue",
            // The violation is the whole diagnostic value here: "validation"
            // alone would not distinguish the #143 near-miss (a numeric
            // revision, refused as `MissingLatestStateRevision`) from a expired
            // frame. `Violation::code` rather than its `Debug` name so a
            // breadcrumb and the `federation emit` refusal an operator reads
            // next to it name the same rule the same way (#102).
            Self::Validation(violation) => return format!("validation:{}", violation.code()),
        };
        name.to_owned()
    }
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::EmptyBroker => "broker must not be empty",
            Self::ZeroPort => "broker port must not be zero",
            Self::EmptyClientId => "client ID must not be empty",
            Self::ZeroRequestCapacity => "request capacity must not be zero",
            Self::ZeroMaxPacketBytes => "maximum packet size must not be zero",
            Self::EnvelopeExceedsPacketLimit => {
                "maximum envelope and MQTT framing exceed the packet limit"
            }
            Self::ZeroTrackingCapacity => "delivery tracking capacities must not be zero",
            Self::InvalidExpiry => "envelope expiry cannot be represented by MQTT",
            Self::Expired => "envelope expired before publication",
            Self::MissingInboxRecipient => "inbox envelope has no direct recipient",
            Self::MissingStateKey => "latest-state envelope has no state key",
            Self::InvalidStateRevision => "state revision is not an unsigned integer",
            Self::EventTombstone => "event delivery cannot be tombstoned",
            Self::SemanticReplyMismatch => {
                "inbox clearing requires a correlated semantic response or acknowledgement"
            }
            Self::InvalidLifecycleDuration => "lifecycle durations must be positive and bounded",
            Self::OriginNotAuthorized => "authenticated transport identity cannot use topic origin",
            Self::ClientQueue => "MQTT client request queue is unavailable",
            Self::Validation(violation) => {
                return write!(formatter, "envelope rejected: {violation:?}")
            }
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for TransportError {}

/// The durations the delivery path applies to a frame: how long a class of
/// message stays valid, and how much clock disagreement between two machines is
/// tolerated before a frame is treated as expired.
///
/// A `renewal_interval` lived here until #148. Nothing renewed anything: it was
/// read only by the unwired `WorkTracker`, so it was a knob for a behavior the
/// running system did not have.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LifecycleConfig {
    /// Caps the published message-expiry of a non-terminal work state, so a
    /// producer that stops reporting stops being current on its own.
    lease_duration: chrono::Duration,
    event_expiry: chrono::Duration,
    inbox_expiry: chrono::Duration,
    clock_skew_tolerance: chrono::Duration,
}

impl LifecycleConfig {
    pub fn new(
        lease_duration: chrono::Duration,
        event_expiry: chrono::Duration,
        inbox_expiry: chrono::Duration,
        clock_skew_tolerance: chrono::Duration,
    ) -> Result<Self, TransportError> {
        if [lease_duration, event_expiry, inbox_expiry]
            .into_iter()
            .any(|duration| duration.num_seconds() <= 0)
            || clock_skew_tolerance < chrono::Duration::zero()
            || clock_skew_tolerance > lease_duration
        {
            return Err(TransportError::InvalidLifecycleDuration);
        }
        Ok(Self {
            lease_duration,
            event_expiry,
            inbox_expiry,
            clock_skew_tolerance,
        })
    }
}

impl Default for LifecycleConfig {
    fn default() -> Self {
        Self {
            lease_duration: chrono::Duration::minutes(30),
            event_expiry: chrono::Duration::minutes(5),
            inbox_expiry: chrono::Duration::hours(24),
            clock_skew_tolerance: chrono::Duration::seconds(30),
        }
    }
}

impl TransportConfig {
    pub fn new(
        broker: impl Into<String>,
        port: u16,
        client_id: impl Into<String>,
        request_capacity: usize,
        max_packet_bytes: u32,
        validation: ValidationConfig,
    ) -> Result<Self, TransportError> {
        let broker = broker.into();
        if broker.is_empty() {
            return Err(TransportError::EmptyBroker);
        }
        if port == 0 {
            return Err(TransportError::ZeroPort);
        }
        let client_id = client_id.into();
        if client_id.is_empty() {
            return Err(TransportError::EmptyClientId);
        }
        if request_capacity == 0 {
            return Err(TransportError::ZeroRequestCapacity);
        }
        if max_packet_bytes == 0 {
            return Err(TransportError::ZeroMaxPacketBytes);
        }
        let required_packet_bytes = validation
            .max_document_bytes
            .checked_add(MAX_MQTT_TOPIC_BYTES)
            .and_then(|size| size.checked_add(MQTT_PUBLISH_FRAMING_BYTES));
        if required_packet_bytes.is_none_or(|size| size > max_packet_bytes as usize) {
            return Err(TransportError::EnvelopeExceedsPacketLimit);
        }
        Ok(Self {
            broker,
            port,
            client_id,
            request_capacity,
            max_packet_bytes,
        })
    }

    pub fn mqtt_options(&self) -> MqttOptions {
        let mut options = MqttOptions::new(&self.client_id, &self.broker, self.port);
        options
            .set_request_channel_capacity(self.request_capacity)
            .set_max_packet_size(Some(self.max_packet_bytes));
        options
    }
}

pub fn encode_validated(envelope: ValidatedEnvelope) -> Vec<u8> {
    envelope.into_envelope().to_json().into_bytes()
}

pub struct PreparedPublish {
    topic: String,
    payload: Vec<u8>,
    qos: QoS,
    retain: bool,
    properties: Option<PublishProperties>,
}

pub fn prepare_publish(
    envelope: ValidatedEnvelope,
    now: DateTime<Utc>,
) -> Result<PreparedPublish, TransportError> {
    prepare_publish_with_lifecycle(envelope, now, &LifecycleConfig::default())
}

pub fn prepare_publish_with_lifecycle(
    envelope: ValidatedEnvelope,
    now: DateTime<Utc>,
    lifecycle: &LifecycleConfig,
) -> Result<PreparedPublish, TransportError> {
    let topic = topic_for(&envelope)?;
    let data = &envelope.as_envelope().data;
    let retain = data.delivery.class != "event";
    let expires_at = DateTime::parse_from_rfc3339(&envelope.as_envelope().data.expires_at)
        .map_err(|_| TransportError::InvalidExpiry)?
        .with_timezone(&Utc);
    let mut seconds = expires_at.signed_duration_since(now).num_seconds();
    if seconds <= 0 {
        return Err(TransportError::Expired);
    }
    let configured_expiry = match data.delivery.class.as_str() {
        "event" => Some(lifecycle.event_expiry),
        "inbox" => Some(lifecycle.inbox_expiry),
        "latest-state"
            if data
                .payload
                .get("state")
                .and_then(crate::json::Value::as_str)
                .and_then(WorkStatus::from_wire)
                .is_some_and(|status| !status.is_terminal()) =>
        {
            Some(lifecycle.lease_duration)
        }
        _ => None,
    };
    if let Some(configured_expiry) = configured_expiry {
        seconds = seconds.min(configured_expiry.num_seconds());
    }
    let message_expiry_interval =
        u32::try_from(seconds).map_err(|_| TransportError::InvalidExpiry)?;
    let payload = encode_validated(envelope);
    Ok(PreparedPublish {
        topic,
        payload,
        qos: QoS::AtLeastOnce,
        retain,
        properties: Some(expiry_properties(message_expiry_interval)),
    })
}

pub fn publish(
    client: &Client,
    envelope: ValidatedEnvelope,
    now: DateTime<Utc>,
) -> Result<(), TransportError> {
    send_prepared(client, prepare_publish(envelope, now)?)
}

pub fn publish_with_lifecycle(
    client: &Client,
    envelope: ValidatedEnvelope,
    now: DateTime<Utc>,
    lifecycle: &LifecycleConfig,
) -> Result<(), TransportError> {
    send_prepared(
        client,
        prepare_publish_with_lifecycle(envelope, now, lifecycle)?,
    )
}

pub fn publish_tombstone(
    client: &Client,
    envelope: ValidatedEnvelope,
) -> Result<(), TransportError> {
    match envelope.as_envelope().data.delivery.class.as_str() {
        "event" => return Err(TransportError::EventTombstone),
        "inbox" => return Err(TransportError::SemanticReplyMismatch),
        _ => {}
    }
    send_tombstone(client, envelope)
}

fn send_tombstone(client: &Client, envelope: ValidatedEnvelope) -> Result<(), TransportError> {
    let topic = topic_for(&envelope)?;
    send_prepared(
        client,
        PreparedPublish {
            topic,
            payload: Vec::new(),
            qos: QoS::AtLeastOnce,
            retain: true,
            properties: None,
        },
    )
}

pub fn publish_inbox_tombstone_after(
    client: &Client,
    request: ValidatedEnvelope,
    reply: &ValidatedEnvelope,
) -> Result<(), TransportError> {
    validate_semantic_clear(&request, reply)?;
    send_tombstone(client, request)
}

fn validate_semantic_clear(
    request: &ValidatedEnvelope,
    reply: &ValidatedEnvelope,
) -> Result<(), TransportError> {
    let request = request.as_envelope();
    let reply = reply.as_envelope();
    let Some(request_thread) = request.data.thread.as_ref() else {
        return Err(TransportError::SemanticReplyMismatch);
    };
    let Some(reply_thread) = reply.data.thread.as_ref() else {
        return Err(TransportError::SemanticReplyMismatch);
    };
    let references_request = [&reply_thread.causation_id, &reply_thread.reply_to]
        .into_iter()
        .flatten()
        .any(|id| id.as_str() == Some(request.id.as_str()));
    let responder_was_addressed =
        request
            .data
            .to
            .iter()
            .any(|recipient| match recipient.kind.as_str() {
                "principal" => recipient.id == reply.data.from.principal_id,
                "agent" => recipient.id == reply.data.from.agent_id,
                "instance" => recipient.id == reply.data.from.instance_id,
                _ => false,
            });
    let sender_is_addressed = reply
        .data
        .to
        .iter()
        .any(|recipient| match recipient.kind.as_str() {
            "principal" => recipient.id == request.data.from.principal_id,
            "agent" => recipient.id == request.data.from.agent_id,
            "instance" => recipient.id == request.data.from.instance_id,
            _ => false,
        });
    if request.message_type != "io.loam.message"
        || request.data.delivery.class != "inbox"
        || !matches!(request.data.intent, Intent::Request | Intent::Response)
        || reply.message_type != "io.loam.message"
        || reply.data.delivery.class != "inbox"
        || !matches!(reply.data.intent, Intent::Response | Intent::Ack)
        || request.data.context.org_id != reply.data.context.org_id
        || request.data.context.project_id != reply.data.context.project_id
        || request_thread.id != reply_thread.id
        || reply_thread.correlation_id != request_thread.correlation_id
        || !references_request
        || !responder_was_addressed
        || !sender_is_addressed
    {
        return Err(TransportError::SemanticReplyMismatch);
    }
    Ok(())
}

fn send_prepared(client: &Client, prepared: PreparedPublish) -> Result<(), TransportError> {
    let result = if let Some(properties) = prepared.properties {
        client.publish_with_properties(
            prepared.topic,
            prepared.qos,
            prepared.retain,
            prepared.payload,
            properties,
        )
    } else {
        client.publish(
            prepared.topic,
            prepared.qos,
            prepared.retain,
            prepared.payload,
        )
    };
    result.map_err(|_| TransportError::ClientQueue)
}

fn expiry_properties(message_expiry_interval: u32) -> PublishProperties {
    PublishProperties {
        payload_format_indicator: Some(1),
        message_expiry_interval: Some(message_expiry_interval),
        topic_alias: None,
        response_topic: None,
        correlation_data: None,
        user_properties: Vec::new(),
        subscription_identifiers: Vec::new(),
        content_type: Some("application/json".to_owned()),
    }
}

fn topic_for(envelope: &ValidatedEnvelope) -> Result<String, TransportError> {
    let envelope = envelope.as_envelope();
    let data = &envelope.data;
    let prefix = format!(
        "loam/v1/{}/{}",
        data.context.org_id, data.context.project_id
    );
    match data.delivery.class.as_str() {
        "event" => Ok(format!("{prefix}/event/{}", data.from.instance_id)),
        "latest-state" => {
            let key = data
                .delivery
                .key
                .as_deref()
                .ok_or(TransportError::MissingStateKey)?;
            data.delivery
                .revision
                .as_deref()
                .and_then(|revision| revision.parse::<u64>().ok())
                .ok_or(TransportError::InvalidStateRevision)?;
            Ok(format!("{prefix}/state/{}/{}", data.from.instance_id, key))
        }
        "inbox" => {
            let recipient = data
                .to
                .iter()
                .find(|recipient| {
                    matches!(recipient.kind.as_str(), "agent" | "principal" | "instance")
                })
                .ok_or(TransportError::MissingInboxRecipient)?;
            Ok(format!(
                "{prefix}/inbox/{}/{}/{}/{}",
                recipient.kind, recipient.id, data.from.instance_id, envelope.id
            ))
        }
        _ => Err(TransportError::Validation(Violation::InvalidDeliveryClass)),
    }
}

pub struct AuthenticatedTransportPrincipal<'a> {
    principal: AuthenticatedPrincipal<'a>,
    allowed_origins: &'a [&'a str],
}

impl<'a> AuthenticatedTransportPrincipal<'a> {
    pub fn new(principal: AuthenticatedPrincipal<'a>, allowed_origins: &'a [&'a str]) -> Self {
        Self {
            principal,
            allowed_origins,
        }
    }

    fn can_use_origin(&self, origin: &str) -> bool {
        self.allowed_origins.contains(&origin)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ReceiveOutcome {
    Accepted(Box<ValidatedEnvelope>),
    DuplicateEvent,
    DuplicateState,
    StaleState,
    ConflictingState,
    DuplicateInbox,
    Removed,
    /// A retained broker membership payload for a project: the raw bytes the
    /// connector writes to the local roster file. Delivered outside the
    /// envelope validator on purpose — the membership topic is a broker-track
    /// contract, not a loam envelope.
    Membership(Vec<u8>),
    /// A retained self-announced member card: the raw bytes the connector
    /// writes to the local member-card cache. Like [`ReceiveOutcome::Membership`],
    /// delivered outside the envelope validator — the members topic is a
    /// broker-track contract, not a loam envelope.
    MemberCard {
        instance_id: String,
        payload: Vec<u8>,
    },
}

impl ReceiveOutcome {
    /// A stable, content-free name for the outcome. Breadcrumbs and diagnostics
    /// need to say what happened to a frame without carrying any part of it, so
    /// this returns the vocabulary and nothing else (#103). The delivery fixture
    /// corpus pins these names, which makes the corpus the single definition of
    /// the admission vocabulary the runtime logs.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Accepted(_) => "accepted",
            Self::DuplicateEvent => "duplicate_event",
            Self::DuplicateState => "duplicate_state",
            Self::StaleState => "stale_state",
            Self::ConflictingState => "conflicting_state",
            Self::DuplicateInbox => "duplicate_inbox",
            Self::Removed => "removed",
            Self::Membership(_) => "membership",
            Self::MemberCard { .. } => "member_card",
        }
    }
}

pub struct DeliveryProcessor {
    validation: ValidationConfig,
    lifecycle: LifecycleConfig,
    event_capacity: usize,
    state_capacity: usize,
    inbox_capacity: usize,
    events: VecDeque<TrackedId>,
    states: VecDeque<TrackedState>,
    inbox: VecDeque<TrackedId>,
}

struct TrackedId {
    id: String,
    expires_at: DateTime<Utc>,
}

struct TrackedState {
    origin: String,
    key: String,
    revision: u64,
    digest: String,
    expires_at: DateTime<Utc>,
}

impl DeliveryProcessor {
    pub fn new(
        validation: ValidationConfig,
        event_capacity: usize,
        state_capacity: usize,
        inbox_capacity: usize,
    ) -> Result<Self, TransportError> {
        Self::with_lifecycle(
            validation,
            event_capacity,
            state_capacity,
            inbox_capacity,
            LifecycleConfig::default(),
        )
    }

    pub fn with_lifecycle(
        validation: ValidationConfig,
        event_capacity: usize,
        state_capacity: usize,
        inbox_capacity: usize,
        lifecycle: LifecycleConfig,
    ) -> Result<Self, TransportError> {
        if event_capacity == 0 || state_capacity == 0 || inbox_capacity == 0 {
            return Err(TransportError::ZeroTrackingCapacity);
        }
        Ok(Self {
            validation,
            lifecycle,
            event_capacity,
            state_capacity,
            inbox_capacity,
            events: VecDeque::with_capacity(event_capacity),
            states: VecDeque::with_capacity(state_capacity),
            inbox: VecDeque::with_capacity(inbox_capacity),
        })
    }

    pub fn receive(
        &mut self,
        topic: &str,
        payload: &[u8],
        identity: &AuthenticatedTransportPrincipal<'_>,
        now: DateTime<Utc>,
    ) -> Result<ReceiveOutcome, TransportError> {
        if payload.len() > self.validation.max_document_bytes {
            return Err(TransportError::Validation(Violation::DocumentTooLarge));
        }
        let parsed_topic = envelope::parse_topic(topic).map_err(TransportError::Validation)?;
        // The broker-served membership and member-card topics are broker-track
        // contracts, not loam envelopes: the payload is the roster/card JSON
        // verbatim, delivered outside the envelope validator. They have no
        // origin (the membership/members ACL grants read on the topic, not a
        // per-instance origin write), so they are exempt from the origin check.
        // A member card may be a tombstone (empty payload) clearing a departed
        // instance's card, which `remove` reports so the connector drops the
        // cached card.
        match parsed_topic.delivery {
            TopicDelivery::Membership => {
                return Ok(ReceiveOutcome::Membership(payload.to_vec()));
            }
            TopicDelivery::MemberCard { instance_id } => {
                if payload.is_empty() {
                    return Ok(ReceiveOutcome::Removed);
                }
                return Ok(ReceiveOutcome::MemberCard {
                    instance_id: instance_id.to_owned(),
                    payload: payload.to_vec(),
                });
            }
            _ => {}
        }
        if !identity.can_use_origin(parsed_topic.delivery.origin()) {
            return Err(TransportError::OriginNotAuthorized);
        }
        if payload.is_empty() {
            return self.remove(parsed_topic.delivery);
        }

        let validation_now = now
            .checked_sub_signed(self.lifecycle.clock_skew_tolerance)
            .ok_or(TransportError::InvalidExpiry)?;
        let mut validation = self.validation.clone();
        validation.max_future_expiry = validation
            .max_future_expiry
            .checked_add(&self.lifecycle.clock_skew_tolerance)
            .ok_or(TransportError::InvalidExpiry)?;
        let validated = envelope::validate(
            payload,
            topic,
            &identity.principal,
            &validation,
            validation_now,
        )
        .map_err(TransportError::Validation)?;
        let expires_at = DateTime::parse_from_rfc3339(&validated.as_envelope().data.expires_at)
            .map_err(|_| TransportError::InvalidExpiry)?
            .with_timezone(&Utc);
        self.prune(validation_now);

        match parsed_topic.delivery {
            TopicDelivery::Event { .. } => self.receive_event(validated, expires_at),
            TopicDelivery::State { origin, key } => {
                self.receive_state(validated, payload, origin, key, expires_at)
            }
            TopicDelivery::Inbox { message_id, .. } => {
                self.receive_inbox(validated, message_id, expires_at)
            }
            // Unreachable: the membership read-path returns before the
            // envelope validator runs.
            TopicDelivery::Membership | TopicDelivery::MemberCard { .. } => {
                Err(TransportError::Validation(Violation::MalformedTopic))
            }
        }
    }

    fn receive_event(
        &mut self,
        validated: ValidatedEnvelope,
        expires_at: DateTime<Utc>,
    ) -> Result<ReceiveOutcome, TransportError> {
        let id = &validated.as_envelope().id;
        if self.events.iter().any(|tracked| tracked.id == *id) {
            return Ok(ReceiveOutcome::DuplicateEvent);
        }
        push_bounded(
            &mut self.events,
            self.event_capacity,
            TrackedId {
                id: id.clone(),
                expires_at,
            },
        );
        Ok(ReceiveOutcome::Accepted(Box::new(validated)))
    }

    fn receive_inbox(
        &mut self,
        validated: ValidatedEnvelope,
        message_id: &str,
        expires_at: DateTime<Utc>,
    ) -> Result<ReceiveOutcome, TransportError> {
        if self.inbox.iter().any(|tracked| tracked.id == message_id) {
            return Ok(ReceiveOutcome::DuplicateInbox);
        }
        push_bounded(
            &mut self.inbox,
            self.inbox_capacity,
            TrackedId {
                id: message_id.to_owned(),
                expires_at,
            },
        );
        Ok(ReceiveOutcome::Accepted(Box::new(validated)))
    }

    fn receive_state(
        &mut self,
        validated: ValidatedEnvelope,
        payload: &[u8],
        origin: &str,
        key: &str,
        expires_at: DateTime<Utc>,
    ) -> Result<ReceiveOutcome, TransportError> {
        let revision = validated
            .as_envelope()
            .data
            .delivery
            .revision
            .as_deref()
            .and_then(|revision| revision.parse::<u64>().ok())
            .ok_or(TransportError::InvalidStateRevision)?;
        let digest = digest(payload);
        if let Some(index) = self
            .states
            .iter()
            .position(|state| state.origin == origin && state.key == key)
        {
            let previous = &self.states[index];
            if revision < previous.revision {
                return Ok(ReceiveOutcome::StaleState);
            }
            if revision == previous.revision {
                return if digest == previous.digest {
                    Ok(ReceiveOutcome::DuplicateState)
                } else {
                    Ok(ReceiveOutcome::ConflictingState)
                };
            }
            self.states.remove(index);
        }
        push_bounded(
            &mut self.states,
            self.state_capacity,
            TrackedState {
                origin: origin.to_owned(),
                key: key.to_owned(),
                revision,
                digest,
                expires_at,
            },
        );
        Ok(ReceiveOutcome::Accepted(Box::new(validated)))
    }

    fn remove(&mut self, topic: TopicDelivery<'_>) -> Result<ReceiveOutcome, TransportError> {
        match topic {
            TopicDelivery::Event { .. } => Err(TransportError::EventTombstone),
            TopicDelivery::State { origin, key } => {
                self.states
                    .retain(|state| state.origin != origin || state.key != key);
                Ok(ReceiveOutcome::Removed)
            }
            TopicDelivery::Inbox { message_id, .. } => {
                self.inbox.retain(|tracked| tracked.id != message_id);
                Ok(ReceiveOutcome::Removed)
            }
            // An empty membership payload is the broker's way of clearing the
            // roster; the connector refuses to write an empty roster rather
            // than treating it as membership. Member cards are handled by the
            // read-path's own tombstone branch before `remove` (which carries
            // no instance id), so this arm is defensive only.
            TopicDelivery::Membership | TopicDelivery::MemberCard { .. } => {
                Ok(ReceiveOutcome::Removed)
            }
        }
    }

    fn prune(&mut self, now: DateTime<Utc>) {
        self.events.retain(|tracked| tracked.expires_at > now);
        self.states.retain(|tracked| tracked.expires_at > now);
        self.inbox.retain(|tracked| tracked.expires_at > now);
    }
}

fn push_bounded<T>(queue: &mut VecDeque<T>, capacity: usize, value: T) {
    if queue.len() == capacity {
        queue.pop_front();
    }
    queue.push_back(value);
}

fn digest(payload: &[u8]) -> String {
    let mut sha256 = Sha256::default();
    sha256.update(payload);
    sha256.finish()
}

/// The work states a `latest-state` payload may report. Read on the publish
/// path, which caps a non-terminal state's message expiry at the lease duration
/// so a producer that goes quiet stops being current.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkStatus {
    Active,
    Blocked,
    Ready,
    Published,
    Abandoned,
}

impl WorkStatus {
    fn from_wire(value: &str) -> Option<Self> {
        match value {
            "active" => Some(Self::Active),
            "blocked" => Some(Self::Blocked),
            "ready" => Some(Self::Ready),
            "published" => Some(Self::Published),
            "abandoned" => Some(Self::Abandoned),
            _ => None,
        }
    }

    fn is_terminal(self) -> bool {
        matches!(self, Self::Published | Self::Abandoned)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestartInspection {
    Clean,
    Dirty,
    Degraded,
}

pub fn inspect_restart_worktree(repository: impl AsRef<Path>) -> RestartInspection {
    let output = Command::new("git")
        .current_dir(repository.as_ref())
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(["status", "--porcelain=v1", "-z", "--untracked-files=all"])
        .output();
    match output {
        Ok(output) if output.status.success() && output.stdout.is_empty() => {
            RestartInspection::Clean
        }
        Ok(output) if output.status.success() => RestartInspection::Dirty,
        Ok(_) | Err(_) => RestartInspection::Degraded,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitOracleError {
    InvalidRepository,
    InvalidWikiRoot,
    InvalidRemote,
    InvalidAllowedRef,
    InvalidAllowedOrigin,
    InvalidWorkClaim,
    UnauthorizedHintOrigin,
    HintDoesNotMatchRemote,
    NoAllowedRemoteRefs,
    AdvertisedOidFetchUnsupported,
    UnreachableCommit,
    DirtyWorktree,
    GitFailure,
    DerivedStateFailure,
    ForbiddenMutation,
    InvalidReconciliationFreshness,
}

impl std::fmt::Display for GitOracleError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::InvalidRepository => "Git repository is not available",
            Self::InvalidWikiRoot => "codegraph wiki root is not available",
            Self::InvalidRemote => "configured Git remote is invalid",
            Self::InvalidAllowedRef => "configured allowed Git ref is invalid",
            Self::InvalidAllowedOrigin => "configured ref-change origin is invalid",
            Self::InvalidWorkClaim => "envelope is not a ready or published work-state claim",
            Self::UnauthorizedHintOrigin => "ref-change hint origin is not allowed",
            Self::HintDoesNotMatchRemote => "ref-change hint does not match an allowed remote tip",
            Self::NoAllowedRemoteRefs => "configured remote advertised no allowed refs",
            Self::AdvertisedOidFetchUnsupported => {
                "configured Git server refuses fetching an advertised tip by object ID"
            }
            Self::UnreachableCommit => "claimed commit is not reachable from an allowed remote ref",
            Self::DirtyWorktree => "handoff requires a clean worktree",
            Self::GitFailure => "configured Git operation failed",
            Self::DerivedStateFailure => "read-only codegraph recomputation failed",
            Self::ForbiddenMutation => "Git verification changed a worktree or ref",
            Self::InvalidReconciliationFreshness => {
                "Git reconciliation freshness must be between one millisecond and five minutes"
            }
        })
    }
}

impl std::error::Error for GitOracleError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicationProof {
    commit: String,
    scope: GitScope,
    pub remote: String,
    pub reference: String,
    pub tip: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PublicationStatus {
    Provisional,
    Verified(PublicationProof),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivedTip {
    pub reference: String,
    pub oid: String,
    pub pending_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reconciliation {
    pub tips: Vec<DerivedTip>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitScope {
    organization_id: String,
    project_id: String,
    repository_id: String,
}

impl GitScope {
    pub fn new(
        organization_id: &str,
        project_id: &str,
        repository_id: &str,
    ) -> Result<Self, GitOracleError> {
        if [organization_id, project_id, repository_id]
            .into_iter()
            .any(|value| !valid_scope_id(value))
        {
            return Err(GitOracleError::InvalidWorkClaim);
        }
        Ok(Self {
            organization_id: organization_id.to_owned(),
            project_id: project_id.to_owned(),
            repository_id: repository_id.to_owned(),
        })
    }
}

pub struct GitOracle {
    repository: PathBuf,
    wiki_root: PathBuf,
    remote: String,
    scope: GitScope,
    allowed_refs: Vec<String>,
    allowed_origins: Vec<String>,
    reconciliation_freshness: Duration,
    cached_reconciliation: Option<CachedReconciliation>,
}

struct CachedReconciliation {
    observed_at: Instant,
    value: Reconciliation,
}

impl GitOracle {
    pub fn new<R, RF, O, OF>(
        repository: impl Into<PathBuf>,
        wiki_root: impl Into<PathBuf>,
        remote: &str,
        scope: GitScope,
        allowed_refs: R,
        allowed_origins: O,
        reconciliation_freshness: Duration,
    ) -> Result<Self, GitOracleError>
    where
        R: IntoIterator<Item = RF>,
        RF: AsRef<str>,
        O: IntoIterator<Item = OF>,
        OF: AsRef<str>,
    {
        let repository = repository.into();
        let wiki_root = wiki_root.into();
        if !repository.is_dir() || !git_success(&repository, &["rev-parse", "--git-dir"]) {
            return Err(GitOracleError::InvalidRepository);
        }
        if !wiki_root.is_dir() {
            return Err(GitOracleError::InvalidWikiRoot);
        }
        if !valid_config_atom(remote)
            || !git_success(&repository, &["remote", "get-url", "--all", remote])
        {
            return Err(GitOracleError::InvalidRemote);
        }
        let allowed_refs = allowed_refs
            .into_iter()
            .map(|value| value.as_ref().to_owned())
            .collect::<Vec<_>>();
        if allowed_refs.is_empty()
            || allowed_refs.len() > 64
            || allowed_refs
                .iter()
                .any(|reference| !valid_ref_pattern(reference))
        {
            return Err(GitOracleError::InvalidAllowedRef);
        }
        let allowed_origins = allowed_origins
            .into_iter()
            .map(|value| value.as_ref().to_owned())
            .collect::<Vec<_>>();
        if allowed_origins.is_empty()
            || allowed_origins.len() > 64
            || allowed_origins.iter().any(|origin| {
                origin.is_empty() || origin.len() > 255 || origin.chars().any(char::is_control)
            })
        {
            return Err(GitOracleError::InvalidAllowedOrigin);
        }
        if reconciliation_freshness.is_zero()
            || reconciliation_freshness > Duration::from_secs(5 * 60)
        {
            return Err(GitOracleError::InvalidReconciliationFreshness);
        }
        Ok(Self {
            repository,
            wiki_root,
            remote: remote.to_owned(),
            scope,
            allowed_refs,
            allowed_origins,
            reconciliation_freshness,
            cached_reconciliation: None,
        })
    }

    pub fn evaluate_work_state(
        &mut self,
        envelope: &ValidatedEnvelope,
    ) -> Result<PublicationStatus, GitOracleError> {
        let envelope = envelope.as_envelope();
        if envelope.message_type != "io.loam.work.state" {
            return Err(GitOracleError::InvalidWorkClaim);
        }
        self.validate_scope(&envelope.data.context)?;
        match envelope
            .data
            .payload
            .get("state")
            .and_then(crate::json::Value::as_str)
        {
            Some("ready") => Ok(PublicationStatus::Provisional),
            Some("published") => {
                let commit = envelope
                    .data
                    .context
                    .git
                    .as_ref()
                    .and_then(|git| git.commit.as_deref())
                    .ok_or(GitOracleError::InvalidWorkClaim)?;
                self.verify_commit(commit).map(PublicationStatus::Verified)
            }
            _ => Err(GitOracleError::InvalidWorkClaim),
        }
    }

    pub fn reconcile(&mut self) -> Result<Reconciliation, GitOracleError> {
        if let Some(cached) = &self.cached_reconciliation {
            if cached.observed_at.elapsed() <= self.reconciliation_freshness {
                return Ok(cached.value.clone());
            }
        }
        let before = self.snapshot()?;
        let tips = self.remote_tips()?;
        for tip in &tips {
            let output = self.git_output(&[
                "fetch",
                "--quiet",
                "--no-write-fetch-head",
                "--no-tags",
                &self.remote,
                &tip.oid,
            ])?;
            if !output.status.success() {
                return Err(classify_advertised_oid_fetch_failure(&output.stderr));
            }
        }
        let derived = tips
            .into_iter()
            .map(|tip| {
                let pending_count = crate::codegraph::pending_count_at_ref(
                    &self.repository,
                    &self.wiki_root,
                    &tip.oid,
                )
                .ok_or(GitOracleError::DerivedStateFailure)?;
                Ok(DerivedTip {
                    reference: tip.reference,
                    oid: tip.oid,
                    pending_count,
                })
            })
            .collect::<Result<Vec<_>, GitOracleError>>()?;
        if self.snapshot()? != before {
            return Err(GitOracleError::ForbiddenMutation);
        }
        let reconciliation = Reconciliation { tips: derived };
        self.cached_reconciliation = Some(CachedReconciliation {
            observed_at: Instant::now(),
            value: reconciliation.clone(),
        });
        Ok(reconciliation)
    }

    pub fn reconcile_ref_change(
        &mut self,
        envelope: &ValidatedEnvelope,
    ) -> Result<Reconciliation, GitOracleError> {
        let envelope = envelope.as_envelope();
        if envelope.message_type != "io.loam.git.refs.changed" {
            return Err(GitOracleError::InvalidWorkClaim);
        }
        self.validate_scope(&envelope.data.context)?;
        if !self
            .allowed_origins
            .contains(&envelope.data.from.instance_id)
        {
            return Err(GitOracleError::UnauthorizedHintOrigin);
        }
        let new_oid = envelope
            .data
            .payload
            .get("new_oid")
            .and_then(crate::json::Value::as_str)
            .ok_or(GitOracleError::InvalidWorkClaim)?;
        let reconciliation = self.reconcile()?;
        if reconciliation.tips.iter().all(|tip| tip.oid != new_oid) {
            return Err(GitOracleError::HintDoesNotMatchRemote);
        }
        Ok(reconciliation)
    }

    pub fn check_handoff(&mut self, commit: &str) -> Result<PublicationProof, GitOracleError> {
        if !self.status_bytes()?.is_empty() {
            return Err(GitOracleError::DirtyWorktree);
        }
        self.verify_commit(commit)
    }

    pub fn configured_remote(&self) -> &str {
        &self.remote
    }

    pub fn configured_refs(&self) -> &[String] {
        &self.allowed_refs
    }

    fn verify_commit(&mut self, commit: &str) -> Result<PublicationProof, GitOracleError> {
        if !valid_oid(commit) {
            return Err(GitOracleError::InvalidWorkClaim);
        }
        let reconciliation = self.reconcile()?;
        if !git_success(
            &self.repository,
            &["cat-file", "-e", &format!("{commit}^{{commit}}")],
        ) {
            return Err(GitOracleError::UnreachableCommit);
        }
        for tip in reconciliation.tips {
            let output = self.git_output(&["merge-base", "--is-ancestor", commit, &tip.oid])?;
            if output.status.success() {
                return Ok(PublicationProof {
                    commit: commit.to_owned(),
                    scope: self.scope.clone(),
                    remote: self.remote.clone(),
                    reference: tip.reference,
                    tip: tip.oid,
                });
            }
            if output.status.code() != Some(1) {
                return Err(GitOracleError::GitFailure);
            }
        }
        Err(GitOracleError::UnreachableCommit)
    }

    fn remote_tips(&self) -> Result<Vec<RemoteTip>, GitOracleError> {
        let mut args = vec!["ls-remote", "--refs", self.remote.as_str()];
        args.extend(self.allowed_refs.iter().map(String::as_str));
        let output = self.git(&args)?;
        let text = std::str::from_utf8(&output.stdout).map_err(|_| GitOracleError::GitFailure)?;
        let mut tips = Vec::new();
        for line in text.lines() {
            let Some((oid, reference)) = line.split_once('\t') else {
                return Err(GitOracleError::GitFailure);
            };
            if !valid_oid(oid)
                || !self
                    .allowed_refs
                    .iter()
                    .any(|pattern| ref_matches(pattern, reference))
            {
                return Err(GitOracleError::GitFailure);
            }
            tips.push(RemoteTip {
                reference: reference.to_owned(),
                oid: oid.to_owned(),
            });
            if tips.len() > 128 {
                return Err(GitOracleError::GitFailure);
            }
        }
        tips.sort_by(|left, right| left.reference.cmp(&right.reference));
        tips.dedup_by(|left, right| left.reference == right.reference && left.oid == right.oid);
        if tips.is_empty() {
            return Err(GitOracleError::NoAllowedRemoteRefs);
        }
        Ok(tips)
    }

    fn validate_scope(&self, context: &crate::envelope::Context) -> Result<(), GitOracleError> {
        if context.org_id != self.scope.organization_id
            || context.project_id != self.scope.project_id
            || context.repository_id != self.scope.repository_id
        {
            return Err(GitOracleError::InvalidWorkClaim);
        }
        Ok(())
    }

    fn snapshot(&self) -> Result<GitSnapshot, GitOracleError> {
        Ok(GitSnapshot {
            status: self.status_bytes()?,
            refs: self
                .git(&["for-each-ref", "--format=%(refname)%00%(objectname)"])?
                .stdout,
        })
    }

    fn status_bytes(&self) -> Result<Vec<u8>, GitOracleError> {
        Ok(self
            .git(&["status", "--porcelain=v1", "-z", "--untracked-files=all"])?
            .stdout)
    }

    fn git(&self, args: &[&str]) -> Result<Output, GitOracleError> {
        let output = self.git_output(args)?;
        if output.status.success() {
            Ok(output)
        } else {
            Err(GitOracleError::GitFailure)
        }
    }

    fn git_output(&self, args: &[&str]) -> Result<Output, GitOracleError> {
        Command::new("git")
            .current_dir(&self.repository)
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("GIT_TERMINAL_PROMPT", "0")
            .args(args)
            .output()
            .map_err(|_| GitOracleError::GitFailure)
    }
}

struct RemoteTip {
    reference: String,
    oid: String,
}

#[derive(PartialEq, Eq)]
struct GitSnapshot {
    status: Vec<u8>,
    refs: Vec<u8>,
}

fn git_success(repository: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .current_dir(repository)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(args)
        .output()
        .is_ok_and(|output| output.status.success())
}

fn classify_advertised_oid_fetch_failure(stderr: &[u8]) -> GitOracleError {
    let stderr = String::from_utf8_lossy(stderr).to_ascii_lowercase();
    if [
        "not our ref",
        "unadvertised object",
        "does not allow request for unadvertised object",
        "is not a valid object",
    ]
    .iter()
    .any(|message| stderr.contains(message))
    {
        GitOracleError::AdvertisedOidFetchUnsupported
    } else {
        GitOracleError::GitFailure
    }
}

fn valid_config_atom(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 255
        && !value.starts_with('-')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn valid_ref_pattern(value: &str) -> bool {
    value.starts_with("refs/heads/")
        && value.len() <= 1024
        && !value.ends_with('/')
        && !value.ends_with('.')
        && !value.ends_with(".lock")
        && !value.contains("..")
        && !value.contains("//")
        && !value.contains("/.")
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b'_' | b'-' | b'*')
        })
        && value.bytes().filter(|byte| *byte == b'*').count() <= 1
}

fn ref_matches(pattern: &str, reference: &str) -> bool {
    match pattern.split_once('*') {
        Some((prefix, suffix)) => reference.starts_with(prefix) && reference.ends_with(suffix),
        None => pattern == reference,
    }
}

fn valid_oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn valid_scope_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 255
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::{
        AuthenticatedPrincipal, BindingAxis, ValidatedEnvelope, ValidationConfig, Violation,
    };
    use crate::json::{self, Value};
    use chrono::{DateTime, Utc};

    #[test]
    fn configuration_rejects_unbounded_or_missing_values() {
        let validation = ValidationConfig::default();
        assert_eq!(
            TransportConfig::new("", 8883, "client-1", 8, 400_000, validation.clone()),
            Err(TransportError::EmptyBroker)
        );
        assert_eq!(
            TransportConfig::new(
                "broker.example",
                0,
                "client-1",
                8,
                400_000,
                validation.clone(),
            ),
            Err(TransportError::ZeroPort)
        );
        assert_eq!(
            TransportConfig::new("broker.example", 8883, "", 8, 400_000, validation.clone(),),
            Err(TransportError::EmptyClientId)
        );
        assert_eq!(
            TransportConfig::new(
                "broker.example",
                8883,
                "client-1",
                0,
                400_000,
                validation.clone(),
            ),
            Err(TransportError::ZeroRequestCapacity)
        );
        assert_eq!(
            TransportConfig::new("broker.example", 8883, "client-1", 8, 0, validation.clone(),),
            Err(TransportError::ZeroMaxPacketBytes)
        );
        assert_eq!(
            TransportConfig::new(
                "broker.example",
                8883,
                "client-1",
                8,
                65_536,
                validation.clone(),
            ),
            Err(TransportError::EnvelopeExceedsPacketLimit)
        );

        let config =
            TransportConfig::new("broker.example", 8883, "client-1", 8, 400_000, validation)
                .expect("bounded configuration should validate");
        let options = config.mqtt_options();
        assert_eq!(options.request_channel_capacity(), 8);
        assert_eq!(options.max_packet_size(), Some(400_000));
    }

    #[test]
    fn advertised_oid_fetch_refusal_is_typed() {
        assert_eq!(
            classify_advertised_oid_fetch_failure(
                b"fatal: remote error: upload-pack: not our ref 0123456789abcdef"
            ),
            GitOracleError::AdvertisedOidFetchUnsupported
        );
        assert_eq!(
            classify_advertised_oid_fetch_failure(b"fatal: unable to access remote"),
            GitOracleError::GitFailure
        );
    }

    #[test]
    fn lifecycle_configuration_bounds_wire_expiry() {
        let now = test_time("2026-07-24T14:21:00Z");
        let lifecycle = LifecycleConfig::new(
            chrono::Duration::minutes(3),
            chrono::Duration::seconds(45),
            chrono::Duration::seconds(90),
            chrono::Duration::seconds(10),
        )
        .expect("bounded lifecycle should validate");
        let event = prepare_publish_with_lifecycle(
            validated_fixture(
                include_bytes!("../tests/fixtures/mqtt/git-refs-changed.json"),
                "loam/v1/org-3A1/project-7M3/event/instance-01",
                now,
            ),
            now,
            &lifecycle,
        )
        .expect("event should prepare with configured expiry");
        let inbox = prepare_publish_with_lifecycle(
            validated_fixture(
                include_bytes!("../tests/fixtures/mqtt/message.json"),
                "loam/v1/org-3A1/project-7M3/inbox/agent/agent-91/instance-01/01K6Q6ESWMT48TPC",
                now,
            ),
            now,
            &lifecycle,
        )
        .expect("inbox should prepare with configured expiry");
        assert_eq!(
            event
                .properties
                .as_ref()
                .and_then(|properties| properties.message_expiry_interval),
            Some(45)
        );
        assert_eq!(
            inbox
                .properties
                .as_ref()
                .and_then(|properties| properties.message_expiry_interval),
            Some(90)
        );
        let max_future_frame = String::from_utf8(
            include_bytes!("../tests/fixtures/mqtt/git-refs-changed.json").to_vec(),
        )
        .expect("event fixture should be UTF-8")
        .replace("2026-07-24T14:25:00Z", "2026-07-31T14:21:00Z");
        let mut processor = DeliveryProcessor::with_lifecycle(
            ValidationConfig::default(),
            2,
            2,
            2,
            lifecycle.clone(),
        )
        .expect("skew-aware processor should configure");
        assert!(matches!(
            processor.receive(
                "loam/v1/org-3A1/project-7M3/event/instance-01",
                max_future_frame.as_bytes(),
                &transport_identity(),
                now,
            ),
            Ok(ReceiveOutcome::Accepted(_))
        ));
        // The remaining bound worth pinning: a skew tolerance larger than the
        // lease it is tolerated against is not a configuration, it is an expiry
        // that can never fire.
        assert_eq!(
            LifecycleConfig::new(
                chrono::Duration::minutes(3),
                chrono::Duration::seconds(45),
                chrono::Duration::seconds(90),
                chrono::Duration::minutes(4),
            ),
            Err(TransportError::InvalidLifecycleDuration)
        );
    }

    #[test]
    fn transport_boundary_consumes_only_a_validated_envelope() {
        let claims = ["employee-184"];
        let principal = AuthenticatedPrincipal::new("broker-user-7", &claims);
        let now = DateTime::parse_from_rfc3339("2026-07-24T14:21:00Z")
            .expect("test time should parse")
            .with_timezone(&Utc);
        let validated = crate::envelope::validate(
            include_bytes!("../tests/fixtures/mqtt/message.json"),
            "loam/v1/org-3A1/project-7M3/inbox/agent/agent-91/instance-01/01K6Q6ESWMT48TPC",
            &principal,
            &ValidationConfig::default(),
            now,
        )
        .expect("fixture should validate");
        let expected = validated.as_envelope().to_json().into_bytes();

        assert_eq!(encode_validated(validated), expected);
    }

    #[test]
    fn inbox_clears_only_after_a_correlated_semantic_reply() {
        let now = test_time("2026-07-24T14:21:00Z");
        let request_topic =
            "loam/v1/org-3A1/project-7M3/inbox/agent/agent-91/instance-01/01K6Q6ESWMT48TPC";
        let request = validated_fixture(
            include_bytes!("../tests/fixtures/mqtt/message.json"),
            request_topic,
            now,
        );
        let response_topic =
            "loam/v1/org-3A1/project-7M3/inbox/principal/employee-184/instance-02/01K6Q6ESWMT48TPD";
        let response_frame =
            String::from_utf8(include_bytes!("../tests/fixtures/mqtt/message.json").to_vec())
                .expect("message fixture should be UTF-8")
                .replacen(
                    "\"id\": \"01K6Q6ESWMT48TPC\"",
                    "\"id\": \"01K6Q6ESWMT48TPD\"",
                    1,
                )
                .replace(
                    "urn:loam:instance:instance-01",
                    "urn:loam:instance:instance-02",
                )
                .replace("\"intent\": \"request\"", "\"intent\": \"response\"")
                .replace(
                    "\"principal_id\": \"employee-184\"",
                    "\"principal_id\": \"employee-191\"",
                )
                .replace("\"agent_id\": \"agent-72\"", "\"agent_id\": \"agent-91\"")
                .replace(
                    "\"instance_id\": \"instance-01\"",
                    "\"instance_id\": \"instance-02\"",
                )
                .replace(
                    "{\"kind\": \"agent\", \"id\": \"agent-91\"}",
                    "{\"kind\": \"principal\", \"id\": \"employee-184\"}",
                )
                .replace(
                    "\"causation_id\": null",
                    "\"causation_id\": \"01K6Q6ESWMT48TPC\"",
                );
        let response_principal = AuthenticatedPrincipal::new("employee-191", &[]);
        let response = crate::envelope::validate(
            response_frame.as_bytes(),
            response_topic,
            &response_principal,
            &ValidationConfig::default(),
            now,
        )
        .expect("correlated response should validate");

        assert_eq!(validate_semantic_clear(&request, &response), Ok(()));
        assert_eq!(
            validate_semantic_clear(&request, &request),
            Err(TransportError::SemanticReplyMismatch)
        );
    }

    #[test]
    fn delivery_fixture_corpus_has_exact_bounded_verdicts() {
        let cases = json::parse(include_str!("../tests/fixtures/mqtt/transport-cases.json"))
            .expect("transport cases should parse");
        for case in cases
            .as_array()
            .expect("transport cases should be an array")
        {
            let name = case
                .get("name")
                .and_then(Value::as_str)
                .expect("transport case should have a name");
            let expected = case
                .get("expected")
                .and_then(Value::as_str)
                .expect("transport case should have an expected verdict");
            assert_eq!(exercise_transport_case(name), expected, "case {name}");
        }
    }

    fn exercise_transport_case(name: &str) -> &'static str {
        let now = test_time("2026-07-24T14:21:00Z");
        let event_topic = "loam/v1/org-3A1/project-7M3/event/instance-01";
        let state_topic = "loam/v1/org-3A1/project-7M3/state/instance-01/activity-01K6Q5";
        let inbox_topic =
            "loam/v1/org-3A1/project-7M3/inbox/agent/agent-91/instance-01/01K6Q6ESWMT48TPC";
        let identity = transport_identity();

        match name {
            "event_publish" => {
                let prepared = prepare_publish(
                    validated_fixture(
                        include_bytes!("../tests/fixtures/mqtt/git-refs-changed.json"),
                        event_topic,
                        now,
                    ),
                    now,
                )
                .expect("event should prepare");
                if prepared.qos == rumqttc::v5::mqttbytes::QoS::AtLeastOnce && !prepared.retain {
                    "event_nonretained_qos1"
                } else {
                    "wrong_publish_options"
                }
            }
            "state_publish" => {
                let prepared = prepare_publish(
                    validated_fixture(
                        include_bytes!("../tests/fixtures/mqtt/work-state.json"),
                        state_topic,
                        now,
                    ),
                    now,
                )
                .expect("state should prepare");
                if prepared.qos == rumqttc::v5::mqttbytes::QoS::AtLeastOnce && prepared.retain {
                    "state_retained_qos1"
                } else {
                    "wrong_publish_options"
                }
            }
            "inbox_publish" => {
                let prepared = prepare_publish(
                    validated_fixture(
                        include_bytes!("../tests/fixtures/mqtt/message.json"),
                        inbox_topic,
                        now,
                    ),
                    now,
                )
                .expect("inbox should prepare");
                if prepared.qos == rumqttc::v5::mqttbytes::QoS::AtLeastOnce && prepared.retain {
                    "inbox_retained_qos1"
                } else {
                    "wrong_publish_options"
                }
            }
            "duplicate_event" => {
                let mut processor = processor();
                let frame = include_bytes!("../tests/fixtures/mqtt/git-refs-changed.json");
                processor
                    .receive(event_topic, frame, &identity, now)
                    .expect("first event should be accepted");
                outcome_name(processor.receive(event_topic, frame, &identity, now))
            }
            "stale_state" => {
                let mut processor = processor();
                let first = state_frame(7, "01K6Q6ESWMT48TPB", "ready locally");
                let stale = state_frame(6, "01K6Q6ESWMT48TP6", "stale");
                processor
                    .receive(state_topic, first.as_bytes(), &identity, now)
                    .expect("first state should be accepted");
                outcome_name(processor.receive(state_topic, stale.as_bytes(), &identity, now))
            }
            "equal_state" => {
                let mut processor = processor();
                let frame = state_frame(7, "01K6Q6ESWMT48TPB", "ready locally");
                processor
                    .receive(state_topic, frame.as_bytes(), &identity, now)
                    .expect("first state should be accepted");
                outcome_name(processor.receive(state_topic, frame.as_bytes(), &identity, now))
            }
            "conflicting_state" => {
                let mut processor = processor();
                let first = state_frame(7, "01K6Q6ESWMT48TPB", "ready locally");
                let conflict = state_frame(7, "01K6Q6ESWMT48TPZ", "different bytes");
                processor
                    .receive(state_topic, first.as_bytes(), &identity, now)
                    .expect("first state should be accepted");
                outcome_name(processor.receive(state_topic, conflict.as_bytes(), &identity, now))
            }
            "new_state" => {
                let mut processor = processor();
                let first = state_frame(7, "01K6Q6ESWMT48TPB", "ready locally");
                let new = state_frame(8, "01K6Q6ESWMT48TP8", "newer");
                processor
                    .receive(state_topic, first.as_bytes(), &identity, now)
                    .expect("first state should be accepted");
                outcome_name(processor.receive(state_topic, new.as_bytes(), &identity, now))
            }
            "expired_state" => outcome_name(processor().receive(
                state_topic,
                include_bytes!("../tests/fixtures/mqtt/work-state.json"),
                &identity,
                test_time("2026-07-24T14:51:00Z"),
            )),
            "expired_inbox" => outcome_name(processor().receive(
                inbox_topic,
                include_bytes!("../tests/fixtures/mqtt/message.json"),
                &identity,
                test_time("2026-07-25T14:21:00Z"),
            )),
            "wrong_origin_tombstone" => outcome_name(processor().receive(
                "loam/v1/org-3A1/project-7M3/state/instance-02/activity-01K6Q5",
                &[],
                &identity,
                now,
            )),
            "colliding_recipient_kind" => {
                let frame = String::from_utf8(
                    include_bytes!("../tests/fixtures/mqtt/message.json").to_vec(),
                )
                .expect("message fixture should be UTF-8")
                .replace("agent-91", "shared-42");
                outcome_name(processor().receive(
                    "loam/v1/org-3A1/project-7M3/inbox/principal/shared-42/instance-01/01K6Q6ESWMT48TPC",
                    frame.as_bytes(),
                    &identity,
                    now,
                ))
            }
            _ => panic!("unmapped transport case {name}"),
        }
    }

    fn processor() -> DeliveryProcessor {
        DeliveryProcessor::new(ValidationConfig::default(), 2, 2, 2)
            .expect("bounded processor configuration should validate")
    }

    fn transport_identity() -> AuthenticatedTransportPrincipal<'static> {
        static CLAIMS: [&str; 1] = ["employee-184"];
        static ORIGINS: [&str; 1] = ["instance-01"];
        AuthenticatedTransportPrincipal::new(
            AuthenticatedPrincipal::new("broker-user-7", &CLAIMS),
            &ORIGINS,
        )
    }

    fn validated_fixture(input: &[u8], topic: &str, now: DateTime<Utc>) -> ValidatedEnvelope {
        let identity = transport_identity();
        crate::envelope::validate(
            input,
            topic,
            &identity.principal,
            &ValidationConfig::default(),
            now,
        )
        .expect("fixture should validate")
    }

    fn state_frame(revision: u64, id: &str, summary: &str) -> String {
        String::from_utf8(include_bytes!("../tests/fixtures/mqtt/work-state.json").to_vec())
            .expect("state fixture should be UTF-8")
            .replace("01K6Q6ESWMT48TPB", id)
            .replace("\"revision\": 7", &format!("\"revision\": {revision}"))
            .replace(
                "Implementation is ready locally; publication is pending.",
                summary,
            )
    }

    fn outcome_name(outcome: Result<ReceiveOutcome, TransportError>) -> &'static str {
        match outcome {
            // Through the production accessor on purpose: the fixture's expected
            // names then pin the exact vocabulary the connector's breadcrumbs
            // emit, so the two can never drift apart unnoticed.
            Ok(outcome) => outcome.code(),
            Err(TransportError::OriginNotAuthorized) => "origin_not_authorized",
            Err(TransportError::Validation(Violation::Expired)) => "expired",
            Err(TransportError::Validation(Violation::BindingMismatch(
                BindingAxis::RecipientKind,
            ))) => "recipient_kind_mismatch",
            other => panic!("unexpected transport outcome: {other:?}"),
        }
    }

    fn test_time(value: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(value)
            .expect("test time should parse")
            .with_timezone(&Utc)
    }
}
