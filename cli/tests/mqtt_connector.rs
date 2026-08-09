//! The real-broker gate for the connector adapter.
//!
//! The stub suite (`connector::probe_tests`) proves the ordered probe contract
//! deterministically; this proves the same contract against a real Mosquitto:
//! accepted authentication, SUBACK on every required filter, PUBACK, the exact
//! unique self-delivery (which is only possible with No Local unset), typed
//! inbox axis binding, and a probe that leaves nothing retained — with a
//! positive retained control in the same run so the absence is meaningful.

// The shared broker fixture is reused; this gate uses only part of it and never
// edits it, so its unused surface is allowed here rather than trimmed there.
#[allow(dead_code)]
#[path = "support/mqtt_broker.rs"]
mod mqtt_broker;

use chrono::Utc;
use loam::connector::{run_probe, MqttSession, MqttTransport, ProbeContext, SessionIdentity};
use loam::envelope::ValidationConfig;
use loam::transport::TransportConfig;
use mqtt_broker::BrokerFixture;
use rumqttc::v5::mqttbytes::v5::{
    ConnectReturnCode, Filter, Packet, PubAckReason, Publish, SubscribeReasonCode,
};
use rumqttc::v5::mqttbytes::QoS;
use rumqttc::v5::{Client, Connection, Event, MqttOptions, RecvTimeoutError};
use std::time::{Duration, Instant};

const INSTANCE: &str = "instance-01";
const MAX_PACKET_BYTES: u32 = 400_000;
const OBSERVE: Duration = Duration::from_secs(2);

#[test]
#[ignore = "requires LOAM_MQTT_TEST=1 and a real Mosquitto/OpenSSL installation"]
fn enrollment_round_trip() {
    if std::env::var("LOAM_MQTT_TEST").as_deref() != Ok("1") {
        eprintln!("skipped: set LOAM_MQTT_TEST=1 to require the real broker tier");
        return;
    }

    let broker =
        BrokerFixture::provision("connector").expect("the real broker fixture should provision");
    let organization = broker
        .namespace()
        .strip_prefix("loam/v1/")
        .expect("the broker namespace should carry the loam/v1 prefix")
        .to_owned();
    // The fixture ACL grants actor-a read/write on `<namespace>/project-a/#`.
    let base = format!("loam/v1/{organization}/project-a");

    let validation = ValidationConfig::default();
    let session = MqttSession {
        config: TransportConfig::new(
            "localhost",
            broker.password_port(),
            "loam-connector-probe",
            8,
            MAX_PACKET_BYTES,
            validation.clone(),
        )
        .expect("the probe transport configuration should be valid"),
        username: "actor-a".to_owned(),
        password: broker.password().to_owned(),
        ca_certificate: broker
            .ca_certificate()
            .expect("the fixture CA certificate should be readable"),
        client_authentication: None,
        claimed_identity: SessionIdentity {
            principal_id: "employee-184".to_owned(),
            agent_id: "agent-72".to_owned(),
            instance_id: INSTANCE.to_owned(),
            allowed_claims: Vec::new(),
        },
    };
    let context = ProbeContext {
        org_id: organization,
        project_id: "project-a".to_owned(),
        repository_id: "repo-2F8".to_owned(),
        base_oid: "84be000000000000000000000000000000000001".to_owned(),
        plan_oid: "61af000000000000000000000000000000000001".to_owned(),
    };

    let now = Utc::now();
    let mut transport = MqttTransport::new(session, validation.clone(), now)
        .expect("the adapter should build its delivery processor");
    // Authentication, three SUBACKs, one PUBACK, and the exact validated
    // self-event. The echo can only arrive because No Local stays unset on
    // every verified filter.
    let evidence = run_probe(
        &mut transport,
        &context,
        &validation,
        Duration::from_secs(10),
        now,
    )
    .expect("the real broker should complete the enrollment probe");
    assert!(
        evidence.authentication && evidence.subscribe && evidence.publish && evidence.self_receive,
        "the real broker probe must observe all four capabilities: {evidence:?}"
    );
    transport.disconnect();

    assert_probe_left_nothing_retained(&broker, &base);
    assert_typed_inbox_axes_bind(&broker, &base);

    broker
        .finish()
        .expect("broker fixture should remove only its temporary directory");
}

#[test]
#[ignore = "requires LOAM_MQTT_TEST=1 and a real Mosquitto/OpenSSL installation"]
fn no_local_set_suppresses_the_self_delivery_the_probe_depends_on() {
    if std::env::var("LOAM_MQTT_TEST").as_deref() != Ok("1") {
        eprintln!("skipped: set LOAM_MQTT_TEST=1 to require the real broker tier");
        return;
    }

    // The committed negative control for `enrollment_round_trip`: the probe's
    // self-receive proof only means something if this broker *would* have
    // withheld the echo with No Local set. Same broker, same topic, same
    // client — only the No Local flag differs.
    let broker =
        BrokerFixture::provision("nolocal").expect("the real broker fixture should provision");
    let base = format!(
        "loam/v1/{}/project-a",
        broker
            .namespace()
            .strip_prefix("loam/v1/")
            .expect("the broker namespace should carry the loam/v1 prefix")
    );
    let topic = format!("{base}/event/{INSTANCE}");

    let mut suppressed = RawClient::connect(&broker, "connector-no-local-set");
    suppressed.subscribe_no_local(&topic);
    suppressed.publish(&topic, b"self-echo", false);
    assert!(
        suppressed.collect(OBSERVE).is_empty(),
        "No Local set must suppress this client's own publication"
    );

    let mut delivered = RawClient::connect(&broker, "connector-no-local-unset");
    delivered.subscribe(&topic);
    delivered.publish(&topic, b"self-echo", false);
    let received = delivered.collect(OBSERVE);
    assert_eq!(
        received
            .iter()
            .filter(|frame| frame.topic == topic && frame.payload == b"self-echo")
            .count(),
        1,
        "No Local unset must deliver this client's own publication: {received:?}"
    );

    broker
        .finish()
        .expect("broker fixture should remove only its temporary directory");
}

/// The probe is non-retained: a subscriber arriving after it sees nothing. The
/// positive control in the same run proves this observation *can* see a
/// retained value, so the absence is evidence rather than a silent subscriber.
fn assert_probe_left_nothing_retained(broker: &BrokerFixture, base: &str) {
    let filter = format!("{base}/event/+");
    let mut observer = RawClient::connect(broker, "connector-retention-observer");
    observer.subscribe(&filter);
    assert!(
        observer.collect(OBSERVE).is_empty(),
        "the enrollment probe must leave no retained message on the broker"
    );

    let sentinel = format!("{base}/event/retained-control");
    let mut publisher = RawClient::connect(broker, "connector-retention-control");
    publisher.publish(&sentinel, b"retained-control", true);

    let mut control = RawClient::connect(broker, "connector-retention-proof");
    control.subscribe(&filter);
    let received = control.collect(OBSERVE);
    assert_eq!(
        received
            .iter()
            .filter(|frame| frame.topic == sentinel && frame.retain)
            .count(),
        1,
        "the positive control must observe its own retained sentinel: {received:?}"
    );

    publisher.publish(&sentinel, b"", true);
    let mut cleared = RawClient::connect(broker, "connector-retention-cleared");
    cleared.subscribe(&filter);
    assert!(
        cleared.collect(OBSERVE).is_empty(),
        "the retained control must be cleared before the fixture is torn down"
    );
}

/// The connector's typed inbox filter binds both axes:
/// `…/inbox/{recipient-kind}/{recipient-id}/{origin}/{message-id}`. A message
/// for another recipient kind or another recipient id is not delivered.
fn assert_typed_inbox_axes_bind(broker: &BrokerFixture, base: &str) {
    let mut inbox = RawClient::connect(broker, "connector-inbox-observer");
    inbox.subscribe(&format!("{base}/inbox/instance/{INSTANCE}/+/+"));

    let mut sender = RawClient::connect(broker, "connector-inbox-sender");
    sender.publish(
        &format!("{base}/inbox/instance/{INSTANCE}/instance-02/01AAAAAAAAAAAAAAAAAAAAAAAA"),
        b"bound",
        false,
    );
    sender.publish(
        &format!("{base}/inbox/agent/{INSTANCE}/instance-02/01AAAAAAAAAAAAAAAAAAAAAAAB"),
        b"other-kind",
        false,
    );
    sender.publish(
        &format!("{base}/inbox/instance/instance-99/instance-02/01AAAAAAAAAAAAAAAAAAAAAAAC"),
        b"other-recipient",
        false,
    );

    let received = inbox.collect(OBSERVE);
    let payloads: Vec<&[u8]> = received
        .iter()
        .map(|frame| frame.payload.as_slice())
        .collect();
    assert_eq!(
        payloads,
        vec![b"bound".as_slice()],
        "the typed inbox filter must isolate both the recipient kind and id: {received:?}"
    );
}

#[derive(Debug)]
struct Frame {
    topic: String,
    payload: Vec<u8>,
    retain: bool,
}

/// A minimal raw MQTT client for the absence and axis-binding observations.
/// It deliberately shares no code with the adapter under test.
struct RawClient {
    client: Client,
    connection: Connection,
    pending: Vec<Publish>,
}

impl RawClient {
    fn connect(broker: &BrokerFixture, client_id: &str) -> Self {
        let mut options = MqttOptions::new(client_id, "localhost", broker.password_port());
        options
            .set_transport(rumqttc::Transport::tls(
                broker
                    .ca_certificate()
                    .expect("the fixture CA certificate should be readable"),
                None,
                None,
            ))
            .set_credentials("actor-a", broker.password())
            .set_keep_alive(Duration::from_secs(5))
            .set_clean_start(true)
            .set_max_packet_size(Some(MAX_PACKET_BYTES));
        let (client, connection) = Client::new(options, 8);
        let mut raw = Self {
            client,
            connection,
            pending: Vec::new(),
        };
        match raw.next_control(Duration::from_secs(10)) {
            Some(Packet::ConnAck(ack)) if ack.code == ConnectReturnCode::Success => raw,
            other => panic!("broker refused the observation client {client_id}: {other:?}"),
        }
    }

    fn subscribe(&mut self, filter: &str) {
        self.subscribe_with(filter, false);
    }

    /// Subscribe with MQTT 5 No Local set, so the broker must not deliver this
    /// client's own publications back to it.
    fn subscribe_no_local(&mut self, filter: &str) {
        self.subscribe_with(filter, true);
    }

    fn subscribe_with(&mut self, filter: &str, no_local: bool) {
        self.client
            .subscribe_many([Filter {
                nolocal: no_local,
                ..Filter::new(filter, QoS::AtLeastOnce)
            }])
            .expect("the observation client should queue its subscription");
        match self.next_control(Duration::from_secs(10)) {
            Some(Packet::SubAck(ack)) => assert!(
                matches!(
                    ack.return_codes.first(),
                    Some(SubscribeReasonCode::Success(_))
                ),
                "broker rejected the observation filter {filter}: {ack:?}"
            ),
            other => panic!("expected a SUBACK for {filter}, got {other:?}"),
        }
    }

    fn publish(&mut self, topic: &str, payload: &[u8], retain: bool) {
        self.client
            .publish(topic, QoS::AtLeastOnce, retain, payload.to_vec())
            .expect("the observation client should queue its publish");
        match self.next_control(Duration::from_secs(10)) {
            Some(Packet::PubAck(ack)) => assert!(
                matches!(
                    ack.reason,
                    PubAckReason::Success | PubAckReason::NoMatchingSubscribers
                ),
                "broker rejected the publish to {topic}: {:?}",
                ack.reason
            ),
            other => panic!("expected a PUBACK for {topic}, got {other:?}"),
        }
    }

    /// Every frame delivered within `window`, in arrival order.
    fn collect(&mut self, window: Duration) -> Vec<Frame> {
        let deadline = Instant::now() + window;
        let mut frames: Vec<Frame> = std::mem::take(&mut self.pending)
            .into_iter()
            .map(frame)
            .collect();
        while let Some(packet) = self.poll(deadline) {
            if let Packet::Publish(publish) = packet {
                frames.push(frame(publish));
            }
        }
        frames
    }

    fn next_control(&mut self, timeout: Duration) -> Option<Packet> {
        let deadline = Instant::now() + timeout;
        loop {
            match self.poll(deadline)? {
                Packet::Publish(publish) => self.pending.push(publish),
                packet => return Some(packet),
            }
        }
    }

    fn poll(&mut self, deadline: Instant) -> Option<Packet> {
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return None;
            }
            match self.connection.recv_timeout(remaining) {
                Ok(Ok(Event::Incoming(packet))) => return Some(packet),
                Ok(Ok(Event::Outgoing(_))) => {}
                Ok(Err(_)) | Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => {
                    return None;
                }
            }
        }
    }
}

fn frame(publish: Publish) -> Frame {
    Frame {
        topic: String::from_utf8_lossy(&publish.topic).into_owned(),
        payload: publish.payload.to_vec(),
        retain: publish.retain,
    }
}
