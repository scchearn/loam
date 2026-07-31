use crate::envelope::{ValidatedEnvelope, ValidationConfig, MAX_MQTT_TOPIC_BYTES};
use rumqttc::v5::MqttOptions;

// Fixed header, remaining-length encoding, topic length, QoS 1 packet ID,
// properties length, and the message-expiry property used by this transport.
const MQTT_PUBLISH_FRAMING_BYTES: usize = 18;

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
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::EmptyBroker => "broker must not be empty",
            Self::ZeroPort => "broker port must not be zero",
            Self::EmptyClientId => "client ID must not be empty",
            Self::ZeroRequestCapacity => "request capacity must not be zero",
            Self::ZeroMaxPacketBytes => "maximum packet size must not be zero",
            Self::EnvelopeExceedsPacketLimit => {
                "maximum envelope and MQTT framing exceed the packet limit"
            }
        })
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::{AuthenticatedPrincipal, ValidationConfig};
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
}
