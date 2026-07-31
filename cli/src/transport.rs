use crate::envelope::{
    self, AuthenticatedPrincipal, Intent, TopicDelivery, ValidatedEnvelope, ValidationConfig,
    Violation, MAX_MQTT_TOPIC_BYTES,
};
use crate::sha256::Sha256;
use chrono::{DateTime, Utc};
use rumqttc::v5::mqttbytes::{v5::PublishProperties, QoS};
use rumqttc::v5::{Client, MqttOptions};
use std::collections::VecDeque;

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
    InvalidWorkEnvelope,
    InvalidWorkTransition,
    TerminalWorkState,
    PublicationUnverified,
    WorkRevisionNotNewer,
    OriginNotAuthorized,
    ClientQueue,
    Validation(Violation),
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
            Self::InvalidWorkEnvelope => "validated envelope is not a usable work-state claim",
            Self::InvalidWorkTransition => "work-state transition is not allowed",
            Self::TerminalWorkState => "terminal work state cannot transition",
            Self::PublicationUnverified => "published work state requires Git reachability proof",
            Self::WorkRevisionNotNewer => "work-state revision must increase",
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
    let topic = topic_for(&envelope)?;
    let retain = envelope.as_envelope().data.delivery.class != "event";
    let expires_at = DateTime::parse_from_rfc3339(&envelope.as_envelope().data.expires_at)
        .map_err(|_| TransportError::InvalidExpiry)?
        .with_timezone(&Utc);
    let seconds = expires_at.signed_duration_since(now).num_seconds();
    if seconds <= 0 {
        return Err(TransportError::Expired);
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
}

pub struct DeliveryProcessor {
    validation: ValidationConfig,
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
        if event_capacity == 0 || state_capacity == 0 || inbox_capacity == 0 {
            return Err(TransportError::ZeroTrackingCapacity);
        }
        Ok(Self {
            validation,
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
        if !identity.can_use_origin(parsed_topic.delivery.origin()) {
            return Err(TransportError::OriginNotAuthorized);
        }
        if payload.is_empty() {
            return self.remove(parsed_topic.delivery);
        }

        let validated =
            envelope::validate(payload, topic, &identity.principal, &self.validation, now)
                .map_err(TransportError::Validation)?;
        let expires_at = DateTime::parse_from_rfc3339(&validated.as_envelope().data.expires_at)
            .map_err(|_| TransportError::InvalidExpiry)?
            .with_timezone(&Utc);
        self.prune(now);

        match parsed_topic.delivery {
            TopicDelivery::Event { .. } => self.receive_event(validated, expires_at),
            TopicDelivery::State { origin, key } => {
                self.receive_state(validated, payload, origin, key, expires_at)
            }
            TopicDelivery::Inbox { message_id, .. } => {
                self.receive_inbox(validated, message_id, expires_at)
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlapWarning {
    pub artifact_kind: String,
    pub artifact_id: String,
    pub activity_origin: String,
    pub activity_key: String,
    pub conflicting_origin: String,
    pub conflicting_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkObservation {
    pub status: WorkStatus,
    pub warnings: Vec<OverlapWarning>,
}

pub struct WorkTracker {
    capacity: usize,
    activities: VecDeque<TrackedWork>,
}

struct TrackedWork {
    origin: String,
    key: String,
    revision: u64,
    status: WorkStatus,
    artifacts: Vec<(String, String)>,
}

impl WorkTracker {
    pub fn new(capacity: usize) -> Result<Self, TransportError> {
        if capacity == 0 {
            return Err(TransportError::ZeroTrackingCapacity);
        }
        Ok(Self {
            capacity,
            activities: VecDeque::with_capacity(capacity),
        })
    }

    pub fn observe(
        &mut self,
        envelope: &ValidatedEnvelope,
    ) -> Result<WorkObservation, TransportError> {
        let envelope = envelope.as_envelope();
        if envelope.message_type != "io.loam.work.state"
            || envelope.data.delivery.class != "latest-state"
        {
            return Err(TransportError::InvalidWorkEnvelope);
        }
        let status = envelope
            .data
            .payload
            .get("state")
            .and_then(crate::json::Value::as_str)
            .and_then(WorkStatus::from_wire)
            .ok_or(TransportError::InvalidWorkEnvelope)?;
        if status == WorkStatus::Published {
            return Err(TransportError::PublicationUnverified);
        }
        let key = envelope
            .data
            .delivery
            .key
            .as_deref()
            .ok_or(TransportError::InvalidWorkEnvelope)?;
        let revision = envelope
            .data
            .delivery
            .revision
            .as_deref()
            .and_then(|value| value.parse::<u64>().ok())
            .ok_or(TransportError::InvalidWorkEnvelope)?;
        let origin = &envelope.data.from.instance_id;
        let previous = self
            .activities
            .iter()
            .position(|activity| activity.origin == *origin && activity.key == key);
        if let Some(index) = previous {
            if revision <= self.activities[index].revision {
                return Err(TransportError::WorkRevisionNotNewer);
            }
            let current = self.activities[index].status;
            if current.is_terminal() {
                return Err(TransportError::TerminalWorkState);
            }
            if !valid_work_transition(current, status) {
                return Err(TransportError::InvalidWorkTransition);
            }
            self.activities.remove(index);
        }
        let artifacts = envelope
            .data
            .context
            .artifacts
            .iter()
            .map(|artifact| (artifact.kind.clone(), artifact.id.clone()))
            .collect();
        push_bounded(
            &mut self.activities,
            self.capacity,
            TrackedWork {
                origin: origin.clone(),
                key: key.to_owned(),
                revision,
                status,
                artifacts,
            },
        );
        Ok(WorkObservation {
            status,
            warnings: self.overlap_warnings(origin, key),
        })
    }

    pub fn status(&self, origin: &str, key: &str) -> Option<WorkStatus> {
        self.activities
            .iter()
            .find(|activity| activity.origin == origin && activity.key == key)
            .map(|activity| activity.status)
    }

    pub fn len(&self) -> usize {
        self.activities.len()
    }

    pub fn is_empty(&self) -> bool {
        self.activities.is_empty()
    }

    fn overlap_warnings(&self, origin: &str, key: &str) -> Vec<OverlapWarning> {
        let Some(activity) = self
            .activities
            .iter()
            .find(|activity| activity.origin == origin && activity.key == key)
        else {
            return Vec::new();
        };
        if activity.status.is_terminal() {
            return Vec::new();
        }
        let mut warnings = Vec::new();
        for other in self.activities.iter().filter(|other| {
            (other.origin != activity.origin || other.key != activity.key)
                && !other.status.is_terminal()
        }) {
            for (kind, id) in activity
                .artifacts
                .iter()
                .filter(|artifact| other.artifacts.contains(artifact))
            {
                warnings.push(overlap_warning(activity, other, kind, id));
                warnings.push(overlap_warning(other, activity, kind, id));
            }
        }
        warnings
    }
}

fn valid_work_transition(current: WorkStatus, next: WorkStatus) -> bool {
    match current {
        WorkStatus::Active => matches!(
            next,
            WorkStatus::Active | WorkStatus::Blocked | WorkStatus::Ready | WorkStatus::Abandoned
        ),
        WorkStatus::Blocked => matches!(
            next,
            WorkStatus::Active | WorkStatus::Blocked | WorkStatus::Ready | WorkStatus::Abandoned
        ),
        WorkStatus::Ready => matches!(next, WorkStatus::Ready | WorkStatus::Abandoned),
        WorkStatus::Published | WorkStatus::Abandoned => false,
    }
}

fn overlap_warning(
    activity: &TrackedWork,
    other: &TrackedWork,
    artifact_kind: &str,
    artifact_id: &str,
) -> OverlapWarning {
    OverlapWarning {
        artifact_kind: artifact_kind.to_owned(),
        artifact_id: artifact_id.to_owned(),
        activity_origin: activity.origin.clone(),
        activity_key: activity.key.clone(),
        conflicting_origin: other.origin.clone(),
        conflicting_key: other.key.clone(),
    }
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
    fn collaboration_fixture_enforces_state_machine_and_overlap() {
        let cases = json::parse(include_str!(
            "../tests/fixtures/mqtt/collaboration-cases.json"
        ))
        .expect("collaboration cases should parse");
        for case in cases
            .as_array()
            .expect("collaboration cases should be an array")
        {
            let name = case
                .get("name")
                .and_then(Value::as_str)
                .expect("collaboration case should have a name");
            let expected = case
                .get("expected")
                .and_then(Value::as_str)
                .expect("collaboration case should have an expected result");
            assert_eq!(
                exercise_collaboration_case(name, case),
                expected,
                "case {name}"
            );
        }
    }

    fn exercise_collaboration_case(name: &str, case: &Value) -> &'static str {
        let now = test_time("2026-07-24T14:21:00Z");
        let mut tracker = WorkTracker::new(8).expect("bounded work tracker should configure");
        if name == "stable_artifact_overlap" {
            let first =
                validated_work_state("instance-01", "employee-184", 1, "active", "SB-42", now);
            let second =
                validated_work_state("instance-02", "employee-191", 1, "active", "SB-42", now);
            tracker
                .observe(&first)
                .expect("first activity should be accepted");
            let observed = tracker
                .observe(&second)
                .expect("overlap should warn, not reject");
            return if observed.warnings.len() == 2 && tracker.len() == 2 {
                "two_warnings"
            } else {
                "wrong_overlap"
            };
        }
        if name == "non_increasing_revision" {
            let first =
                validated_work_state("instance-01", "employee-184", 2, "active", "SB-42", now);
            let stale =
                validated_work_state("instance-01", "employee-184", 1, "blocked", "SB-42", now);
            tracker
                .observe(&first)
                .expect("first activity should be accepted");
            return match tracker.observe(&stale) {
                Err(TransportError::WorkRevisionNotNewer) => "revision_not_newer",
                other => panic!("unexpected stale work revision outcome: {other:?}"),
            };
        }

        for (index, state) in case
            .get("states")
            .and_then(Value::as_array)
            .expect("transition case should have states")
            .iter()
            .enumerate()
        {
            let state = state.as_str().expect("state should be a string");
            let envelope = validated_work_state(
                "instance-01",
                "employee-184",
                index as u64 + 1,
                state,
                "SB-42",
                now,
            );
            if let Err(error) = tracker.observe(&envelope) {
                return match error {
                    TransportError::TerminalWorkState => "terminal_transition",
                    TransportError::InvalidWorkTransition => "invalid_transition",
                    other => panic!("unexpected work transition error: {other:?}"),
                };
            }
        }
        match tracker.status("instance-01", "activity-01K6Q5") {
            Some(WorkStatus::Ready) => "ready",
            Some(WorkStatus::Abandoned) => "abandoned",
            other => panic!("unexpected final work state: {other:?}"),
        }
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

    fn validated_work_state(
        origin: &str,
        principal_id: &str,
        revision: u64,
        state: &str,
        artifact_id: &str,
        now: DateTime<Utc>,
    ) -> ValidatedEnvelope {
        let topic = format!("loam/v1/org-3A1/project-7M3/state/{origin}/activity-01K6Q5");
        let frame =
            String::from_utf8(include_bytes!("../tests/fixtures/mqtt/work-state.json").to_vec())
                .expect("work-state fixture should be UTF-8")
                .replace(
                    "urn:loam:instance:instance-01",
                    &format!("urn:loam:instance:{origin}"),
                )
                .replace(
                    "\"principal_id\": \"employee-184\"",
                    &format!("\"principal_id\": \"{principal_id}\""),
                )
                .replace(
                    "\"instance_id\": \"instance-01\"",
                    &format!("\"instance_id\": \"{origin}\""),
                )
                .replace("01K6Q6ESWMT48TPB", &format!("work-{origin}-{revision}"))
                .replace("\"revision\": 7", &format!("\"revision\": {revision}"))
                .replace("\"state\": \"ready\"", &format!("\"state\": \"{state}\""))
                .replace("SB-42", artifact_id);
        let principal = AuthenticatedPrincipal::new(principal_id, &[]);
        crate::envelope::validate(
            frame.as_bytes(),
            &topic,
            &principal,
            &ValidationConfig::default(),
            now,
        )
        .expect("generated work-state fixture should validate")
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
            Ok(ReceiveOutcome::Accepted(_)) => "accepted",
            Ok(ReceiveOutcome::DuplicateEvent) => "duplicate_event",
            Ok(ReceiveOutcome::DuplicateState) => "duplicate_state",
            Ok(ReceiveOutcome::StaleState) => "stale_state",
            Ok(ReceiveOutcome::ConflictingState) => "conflicting_state",
            Ok(ReceiveOutcome::DuplicateInbox) => "duplicate_inbox",
            Ok(ReceiveOutcome::Removed) => "removed",
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
