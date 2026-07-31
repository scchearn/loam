#[path = "support/mqtt_broker.rs"]
mod mqtt_broker;

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
