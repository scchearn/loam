#[path = "support/mqtt_broker.rs"]
mod mqtt_broker;

use chrono::{DateTime, Utc};
use loam::envelope::{AuthenticatedPrincipal, ValidatedEnvelope, ValidationConfig, Violation};
use loam::transport::{
    self, AuthenticatedTransportPrincipal, DeliveryProcessor, ReceiveOutcome, TransportError,
};
use mqtt_broker::BrokerFixture;
use rumqttc::v5::mqttbytes::{
    v5::{ConnectReturnCode, Packet, PubAckReason, PublishProperties, SubscribeReasonCode},
    QoS,
};
use rumqttc::v5::{Client, Connection, Event, MqttOptions, RecvTimeoutError};
use rumqttc::Transport;
use std::time::{Duration, Instant};

#[test]
#[ignore = "requires LOAM_MQTT_TEST=1 and a real Mosquitto/OpenSSL installation"]
fn delivery_classes() {
    if std::env::var("LOAM_MQTT_TEST").as_deref() != Ok("1") {
        eprintln!("skipped: set LOAM_MQTT_TEST=1 to require the real broker tier");
        return;
    }

    let broker = BrokerFixture::provision("delivery-classes")
        .expect("the real broker fixture should provision");
    let project = format!("{}/project-a", broker.namespace());
    let event_topic = format!("{project}/event/instance-01");
    let state_topic = format!("{project}/state/instance-01/activity-01K6Q5");
    let inbox_topic = format!("{project}/inbox/agent/agent-91/instance-01/01K6Q6ESWMT48TPC");
    let now = test_time("2026-07-24T14:21:00Z");
    let event = scoped_envelope(
        include_bytes!("fixtures/mqtt/git-refs-changed.json"),
        &event_topic,
        &broker,
        now,
    );
    let state = scoped_envelope(
        include_bytes!("fixtures/mqtt/work-state.json"),
        &state_topic,
        &broker,
        now,
    );
    let inbox = scoped_envelope(
        include_bytes!("fixtures/mqtt/message.json"),
        &inbox_topic,
        &broker,
        now,
    );

    let mut observer = TestClient::password(&broker, "delivery-observer")
        .expect("delivery observer should authenticate");
    observer
        .subscribe(format!("{project}/#"))
        .expect("delivery observer should subscribe");
    let mut publisher = TestClient::password(&broker, "delivery-publisher")
        .expect("delivery publisher should authenticate");

    for envelope in [event.clone(), event.clone(), state.clone(), inbox.clone()] {
        transport::publish(&publisher.client, envelope, now)
            .expect("validated envelope should queue for publication");
        assert_publish_accepted(
            publisher
                .wait_for_puback()
                .expect("validated publication should be acknowledged"),
        );
    }

    let event_one = observer
        .receive(&event_topic, Duration::from_secs(3))
        .expect("first event should traverse the broker");
    let event_two = observer
        .receive(&event_topic, Duration::from_secs(3))
        .expect("QoS 1 duplicate event should traverse the broker");
    assert!(!event_one.retain && !event_two.retain);
    let state_publish = observer
        .receive(&state_topic, Duration::from_secs(3))
        .expect("state should traverse the broker");
    let inbox_publish = observer
        .receive(&inbox_topic, Duration::from_secs(3))
        .expect("inbox should traverse the broker");

    let claims = ["employee-184"];
    let origins = ["instance-01"];
    let identity = AuthenticatedTransportPrincipal::new(
        AuthenticatedPrincipal::new("broker-user-7", &claims),
        &origins,
    );
    let mut processor = DeliveryProcessor::new(ValidationConfig::default(), 8, 8, 8)
        .expect("bounded delivery processor should configure");
    assert!(matches!(
        processor.receive(&event_topic, &event_one.payload, &identity, now),
        Ok(ReceiveOutcome::Accepted(_))
    ));
    assert_eq!(
        processor.receive(&event_topic, &event_two.payload, &identity, now),
        Ok(ReceiveOutcome::DuplicateEvent)
    );
    assert!(matches!(
        processor.receive(&state_topic, &state_publish.payload, &identity, now),
        Ok(ReceiveOutcome::Accepted(_))
    ));
    assert!(matches!(
        processor.receive(&inbox_topic, &inbox_publish.payload, &identity, now),
        Ok(ReceiveOutcome::Accepted(_))
    ));

    let collision_topic =
        format!("{project}/inbox/principal/shared-42/instance-01/01K6Q6ESWMT48TPC");
    let collision = scoped_frame(include_bytes!("fixtures/mqtt/message.json"), &broker)
        .replace("agent-91", "shared-42");
    assert_publish_accepted(
        publisher
            .publish(&collision_topic, collision, false, None)
            .expect("collision probe should traverse the same broker ACL"),
    );
    let collision = observer
        .receive(&collision_topic, Duration::from_secs(3))
        .expect("collision probe should reach the validating receiver");
    assert_eq!(
        processor.receive(&collision_topic, &collision.payload, &identity, now),
        Err(TransportError::Validation(Violation::BindingMismatch(
            loam::envelope::BindingAxis::RecipientKind
        )))
    );

    let mut late = TestClient::password(&broker, "delivery-late-observer")
        .expect("late observer should authenticate");
    late.subscribe(format!("{project}/#"))
        .expect("late observer should subscribe");
    let retained = late.collect(Duration::from_secs(2));
    assert!(retained
        .iter()
        .any(|publish| { publish.topic.as_ref() == state_topic.as_bytes() && publish.retain }));
    assert!(retained
        .iter()
        .any(|publish| { publish.topic.as_ref() == inbox_topic.as_bytes() && publish.retain }));
    assert!(
        retained
            .iter()
            .all(|publish| publish.topic.as_ref() != event_topic.as_bytes()),
        "non-retained event was replayed to a late subscriber: {retained:?}"
    );

    transport::publish_tombstone(&publisher.client, state)
        .expect("validated state should queue its tombstone");
    assert_publish_accepted(
        publisher
            .wait_for_puback()
            .expect("validated state tombstone should be acknowledged"),
    );
    let semantic_reply = scoped_response_envelope(&broker, now);
    transport::publish_inbox_tombstone_after(&publisher.client, inbox, &semantic_reply)
        .expect("correlated semantic reply should clear the retained inbox request");
    assert_publish_accepted(
        publisher
            .wait_for_puback()
            .expect("validated inbox tombstone should be acknowledged"),
    );
    let state_tombstone = observer
        .receive(&state_topic, Duration::from_secs(3))
        .expect("state tombstone should traverse the broker");
    let inbox_tombstone = observer
        .receive(&inbox_topic, Duration::from_secs(3))
        .expect("inbox tombstone should traverse the broker");
    assert!(state_tombstone.payload.is_empty() && inbox_tombstone.payload.is_empty());
    assert_eq!(
        processor.receive(&state_topic, &state_tombstone.payload, &identity, now),
        Ok(ReceiveOutcome::Removed)
    );
    assert_eq!(
        processor.receive(&inbox_topic, &inbox_tombstone.payload, &identity, now),
        Ok(ReceiveOutcome::Removed)
    );

    let mut final_scan = TestClient::password(&broker, "delivery-final-scan")
        .expect("final retained scan should authenticate");
    final_scan
        .subscribe(format!("{project}/#"))
        .expect("final retained scan should subscribe");
    assert!(
        final_scan.collect(Duration::from_secs(2)).is_empty(),
        "delivery class test left retained values under its run namespace"
    );
    broker
        .finish()
        .expect("broker fixture should remove only its temporary directory");
}

#[test]
#[ignore = "requires LOAM_MQTT_TEST=1 and a real Mosquitto/OpenSSL installation"]
fn broker_contract() {
    if std::env::var("LOAM_MQTT_TEST").as_deref() != Ok("1") {
        eprintln!("skipped: set LOAM_MQTT_TEST=1 to require the real broker tier");
        return;
    }

    let mut broker = BrokerFixture::provision("broker-contract")
        .expect("the real broker fixture should provision");
    let project = format!("{}/project-a", broker.namespace());
    let password_topic = format!("{project}/state/password-sentinel");
    let mtls_topic = format!("{project}/state/mtls-sentinel");
    let expired_topic = format!("{project}/expiry/expired");
    let live_topic = format!("{project}/expiry/live");
    let expiry_filter = format!("{project}/expiry/#");
    let foreign_topic = format!("{}/project-b/state/forbidden", broker.namespace());

    {
        let mut password = TestClient::password(&broker, "password-client")
            .expect("password client should authenticate over TLS");
        password
            .subscribe(&password_topic)
            .expect("same-project password subscription should succeed");
        assert_eq!(
            password
                .publish(&password_topic, b"before-restart", true, None)
                .expect("same-project password publish should be acknowledged"),
            PubAckReason::Success
        );
        let received = password
            .receive(&password_topic, Duration::from_secs(3))
            .expect("password client should receive its retained sentinel");
        assert_eq!(received.payload.as_ref(), b"before-restart");

        assert_eq!(
            password
                .publish(&foreign_topic, b"forbidden", false, None)
                .expect("broker should return a reason code for forbidden publish"),
            PubAckReason::NotAuthorized
        );
        assert_eq!(
            password
                .subscribe(&foreign_topic)
                .expect("broker should acknowledge the foreign filter before read ACL filtering"),
            SubscribeReasonCode::Success(QoS::AtLeastOnce)
        );

        let mut foreign = TestClient::credentials(
            &broker,
            "foreign-control",
            "actor-b",
            broker.foreign_password(),
        )
        .expect("foreign-scope control client should authenticate");
        foreign
            .subscribe(&foreign_topic)
            .expect("foreign-scope control should subscribe to its own topic");
        assert_eq!(
            foreign
                .publish(&foreign_topic, b"foreign-control", true, None)
                .expect("foreign-scope control should publish to its own topic"),
            PubAckReason::Success
        );
        assert_eq!(
            foreign
                .receive(&foreign_topic, Duration::from_secs(3))
                .expect("foreign-scope control should receive its retained publish")
                .payload
                .as_ref(),
            b"foreign-control"
        );
        assert!(
            password
                .collect(Duration::from_secs(1))
                .iter()
                .all(|publish| publish.topic.as_ref() != foreign_topic.as_bytes()),
            "actor-a received a publication from actor-b's forbidden scope"
        );

        assert_publish_accepted(
            password
                .publish(&expired_topic, b"must-expire", true, Some(1))
                .expect("expiring retained publish should be acknowledged"),
        );
    }

    {
        let mut mtls = TestClient::mtls(&broker, "mtls-client")
            .expect("client certificate should authenticate over TLS");
        mtls.subscribe(&mtls_topic)
            .expect("same-project mTLS subscription should succeed");
        assert_eq!(
            mtls.publish(&mtls_topic, b"mtls-ok", true, None)
                .expect("same-project mTLS publish should be acknowledged"),
            PubAckReason::Success
        );
        assert_eq!(
            mtls.receive(&mtls_topic, Duration::from_secs(3))
                .expect("mTLS client should receive its retained sentinel")
                .payload
                .as_ref(),
            b"mtls-ok"
        );
    }

    let anonymous = match TestClient::anonymous(&broker, "anonymous-client") {
        Ok(_) => panic!("anonymous connection must be refused"),
        Err(error) => error,
    };
    assert!(
        anonymous.contains("NotAuthorized") || anonymous.contains("not authorised"),
        "unexpected anonymous refusal: {anonymous}"
    );
    broker
        .wait_for_log("Denied PUBLISH")
        .expect("broker log should prove the foreign publish reached the ACL");
    broker
        .wait_for_log("not authorised")
        .expect("broker log should prove anonymous authentication was refused");

    std::thread::sleep(Duration::from_secs(2));
    {
        let mut password = TestClient::password(&broker, "expiry-publisher")
            .expect("expiry control publisher should authenticate");
        assert_publish_accepted(
            password
                .publish(&live_topic, b"still-live", true, None)
                .expect("non-expiring retained control should publish"),
        );
    }
    {
        let mut observer = TestClient::password(&broker, "expiry-observer")
            .expect("expiry observer should authenticate");
        observer
            .subscribe(&expiry_filter)
            .expect("expiry observer should subscribe");
        let retained = observer.collect(Duration::from_secs(2));
        assert!(
            retained.iter().any(|publish| {
                publish.topic.as_ref() == live_topic.as_bytes()
                    && publish.payload.as_ref() == b"still-live"
            }),
            "non-expiring retained control was not delivered: {retained:?}"
        );
        assert!(
            retained
                .iter()
                .all(|publish| publish.topic.as_ref() != expired_topic.as_bytes()),
            "expired retained value was still delivered: {retained:?}"
        );
    }

    broker
        .restart()
        .expect("broker should restart against the same persistence directory");
    {
        let mut restored = TestClient::password(&broker, "restored-observer")
            .expect("password client should reconnect after restart");
        restored
            .subscribe(&password_topic)
            .expect("restored observer should subscribe");
        let publish = restored
            .receive(&password_topic, Duration::from_secs(3))
            .expect("retained sentinel should survive the broker restart");
        assert!(publish.retain);
        assert_eq!(publish.payload.as_ref(), b"before-restart");
    }

    {
        let mut cleanup = TestClient::password(&broker, "cleanup-client")
            .expect("cleanup client should authenticate");
        for topic in [&password_topic, &mtls_topic, &expired_topic, &live_topic] {
            assert_publish_accepted(
                cleanup
                    .publish(topic, Vec::new(), true, None)
                    .expect("same-origin retained tombstone should be acknowledged"),
            );
        }
    }
    {
        let mut foreign_cleanup = TestClient::credentials(
            &broker,
            "foreign-cleanup",
            "actor-b",
            broker.foreign_password(),
        )
        .expect("foreign cleanup client should authenticate");
        assert_publish_accepted(
            foreign_cleanup
                .publish(&foreign_topic, Vec::new(), true, None)
                .expect("foreign retained tombstone should be acknowledged"),
        );
        foreign_cleanup
            .subscribe(format!("{}/project-b/#", broker.namespace()))
            .expect("foreign retained scan should subscribe successfully");
        assert!(
            foreign_cleanup.collect(Duration::from_secs(2)).is_empty(),
            "broker fixture left retained values under the foreign run scope"
        );
    }
    {
        let mut final_scan = TestClient::password(&broker, "final-retained-scan")
            .expect("final retained scan should authenticate");
        final_scan
            .subscribe(format!("{project}/#"))
            .expect("final retained scan should subscribe successfully");
        assert!(
            final_scan.collect(Duration::from_secs(2)).is_empty(),
            "broker fixture left retained values under its run namespace"
        );
    }

    broker
        .finish()
        .expect("broker fixture should stop and remove only its temporary directory");
}

struct TestClient {
    client: Client,
    connection: Connection,
    pending: Vec<rumqttc::v5::mqttbytes::v5::Publish>,
}

impl TestClient {
    fn password(broker: &BrokerFixture, client_id: &str) -> Result<Self, String> {
        Self::credentials(broker, client_id, "actor-a", broker.password())
    }

    fn credentials(
        broker: &BrokerFixture,
        client_id: &str,
        username: &str,
        password: &str,
    ) -> Result<Self, String> {
        let mut options = tls_options(broker, client_id, broker.password_port(), None)?;
        options.set_credentials(username, password);
        Self::connect(options)
    }

    fn mtls(broker: &BrokerFixture, client_id: &str) -> Result<Self, String> {
        let authentication = Some((broker.client_certificate()?, broker.client_key()?));
        let options = tls_options(broker, client_id, broker.mtls_port(), authentication)?;
        Self::connect(options)
    }

    fn anonymous(broker: &BrokerFixture, client_id: &str) -> Result<Self, String> {
        let options = tls_options(broker, client_id, broker.password_port(), None)?;
        Self::connect(options)
    }

    fn connect(options: MqttOptions) -> Result<Self, String> {
        let (client, connection) = Client::new(options, 8);
        let mut connected = Self {
            client,
            connection,
            pending: Vec::new(),
        };
        loop {
            match connected.next_packet(Duration::from_secs(5))? {
                Packet::ConnAck(ack) if ack.code == ConnectReturnCode::Success => {
                    return Ok(connected);
                }
                Packet::ConnAck(ack) => {
                    return Err(format!("broker refused connection: {:?}", ack.code));
                }
                Packet::Publish(publish) => connected.pending.push(publish),
                _ => {}
            }
        }
    }

    fn subscribe(&mut self, topic: impl Into<String>) -> Result<SubscribeReasonCode, String> {
        self.client
            .subscribe(topic, QoS::AtLeastOnce)
            .map_err(|error| format!("queue MQTT subscription: {error}"))?;
        loop {
            match self.next_packet(Duration::from_secs(5))? {
                Packet::SubAck(ack) => {
                    return ack
                        .return_codes
                        .first()
                        .copied()
                        .ok_or_else(|| "broker returned an empty SUBACK".to_owned());
                }
                Packet::Publish(publish) => self.pending.push(publish),
                _ => {}
            }
        }
    }

    fn publish(
        &mut self,
        topic: impl Into<String>,
        payload: impl Into<Vec<u8>>,
        retain: bool,
        expiry_seconds: Option<u32>,
    ) -> Result<PubAckReason, String> {
        let topic = topic.into();
        let payload = payload.into();
        if let Some(expiry_seconds) = expiry_seconds {
            self.client
                .publish_with_properties(
                    topic,
                    QoS::AtLeastOnce,
                    retain,
                    payload,
                    PublishProperties {
                        payload_format_indicator: None,
                        message_expiry_interval: Some(expiry_seconds),
                        topic_alias: None,
                        response_topic: None,
                        correlation_data: None,
                        user_properties: Vec::new(),
                        subscription_identifiers: Vec::new(),
                        content_type: None,
                    },
                )
                .map_err(|error| format!("queue MQTT publish with expiry: {error}"))?;
        } else {
            self.client
                .publish(topic, QoS::AtLeastOnce, retain, payload)
                .map_err(|error| format!("queue MQTT publish: {error}"))?;
        }
        self.wait_for_puback()
    }

    fn wait_for_puback(&mut self) -> Result<PubAckReason, String> {
        loop {
            match self.next_packet(Duration::from_secs(5))? {
                Packet::PubAck(ack) => return Ok(ack.reason),
                Packet::Publish(publish) => self.pending.push(publish),
                _ => {}
            }
        }
    }

    fn receive(
        &mut self,
        topic: &str,
        timeout: Duration,
    ) -> Result<rumqttc::v5::mqttbytes::v5::Publish, String> {
        if let Some(index) = self
            .pending
            .iter()
            .position(|publish| publish.topic.as_ref() == topic.as_bytes())
        {
            return Ok(self.pending.remove(index));
        }

        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(format!("timed out waiting for MQTT topic {topic:?}"));
            }
            match self.next_packet(remaining)? {
                Packet::Publish(publish) if publish.topic.as_ref() == topic.as_bytes() => {
                    return Ok(publish);
                }
                Packet::Publish(publish) => self.pending.push(publish),
                _ => {}
            }
        }
    }

    fn collect(&mut self, timeout: Duration) -> Vec<rumqttc::v5::mqttbytes::v5::Publish> {
        let mut received = std::mem::take(&mut self.pending);
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return received;
            }
            match self.connection.recv_timeout(remaining) {
                Ok(Ok(Event::Incoming(Packet::Publish(publish)))) => received.push(publish),
                Ok(Ok(_)) => {}
                Ok(Err(error)) => panic!("MQTT connection failed during collection: {error}"),
                Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => return received,
            }
        }
    }

    fn next_packet(&mut self, timeout: Duration) -> Result<Packet, String> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err("timed out waiting for an MQTT packet".to_owned());
            }
            match self.connection.recv_timeout(remaining) {
                Ok(Ok(Event::Incoming(packet))) => return Ok(packet),
                Ok(Ok(Event::Outgoing(_))) => {}
                Ok(Err(error)) => return Err(format!("MQTT connection failed: {error}")),
                Err(RecvTimeoutError::Timeout) => {
                    return Err("timed out waiting for an MQTT packet".to_owned());
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err("MQTT request channel disconnected".to_owned());
                }
            }
        }
    }
}

fn tls_options(
    broker: &BrokerFixture,
    client_id: &str,
    port: u16,
    client_authentication: Option<(Vec<u8>, Vec<u8>)>,
) -> Result<MqttOptions, String> {
    let mut options = MqttOptions::new(client_id, "localhost", port);
    options
        .set_transport(Transport::tls(
            broker.ca_certificate()?,
            client_authentication,
            None,
        ))
        .set_keep_alive(Duration::from_secs(5))
        .set_max_packet_size(Some(400_000));
    Ok(options)
}

fn assert_publish_accepted(reason: PubAckReason) {
    assert!(
        matches!(
            reason,
            PubAckReason::Success | PubAckReason::NoMatchingSubscribers
        ),
        "broker rejected publish with {reason:?}"
    );
}

fn scoped_envelope(
    fixture: &[u8],
    topic: &str,
    broker: &BrokerFixture,
    now: DateTime<Utc>,
) -> ValidatedEnvelope {
    let claims = ["employee-184"];
    let principal = AuthenticatedPrincipal::new("broker-user-7", &claims);
    let frame = scoped_frame(fixture, broker);
    loam::envelope::validate(
        frame.as_bytes(),
        topic,
        &principal,
        &ValidationConfig::default(),
        now,
    )
    .expect("scoped MQTT fixture should validate")
}

fn scoped_frame(fixture: &[u8], broker: &BrokerFixture) -> String {
    let organization = broker
        .namespace()
        .strip_prefix("loam/v1/")
        .expect("broker namespace should use the Loam v1 prefix");
    String::from_utf8(fixture.to_vec())
        .expect("MQTT fixture should be UTF-8")
        .replace("org-3A1", organization)
        .replace("project-7M3", "project-a")
}

fn scoped_response_envelope(broker: &BrokerFixture, now: DateTime<Utc>) -> ValidatedEnvelope {
    let response_topic = format!(
        "{}/project-a/inbox/principal/employee-184/instance-02/01K6Q6ESWMT48TPD",
        broker.namespace()
    );
    let response = scoped_frame(include_bytes!("fixtures/mqtt/message.json"), broker)
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
    let principal = AuthenticatedPrincipal::new("employee-191", &[]);
    loam::envelope::validate(
        response.as_bytes(),
        &response_topic,
        &principal,
        &ValidationConfig::default(),
        now,
    )
    .expect("scoped semantic response should validate")
}

fn test_time(value: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(value)
        .expect("test time should parse")
        .with_timezone(&Utc)
}
