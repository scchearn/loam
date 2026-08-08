#[path = "support/mqtt_broker.rs"]
mod mqtt_broker;

use chrono::{DateTime, Utc};
use loam::envelope::{
    AuthenticatedPrincipal, BindingAxis, ValidatedEnvelope, ValidationConfig, Violation,
};
use loam::transport::{
    self, AuthenticatedTransportPrincipal, DeliveryProcessor, GitOracle, GitOracleError, GitScope,
    LifecycleConfig, PublicationStatus, ReceiveOutcome, RestartInspection, TransportError,
    WorkClassification, WorkStatus, WorkTracker,
};
use mqtt_broker::BrokerFixture;
use rumqttc::v5::mqttbytes::{
    v5::{ConnectReturnCode, Packet, PubAckReason, PublishProperties, SubscribeReasonCode},
    QoS,
};
use rumqttc::v5::{Client, Connection, Event, MqttOptions, RecvTimeoutError};
use rumqttc::Transport;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[test]
#[ignore = "requires LOAM_MQTT_TEST=1, Git, and a real Mosquitto/OpenSSL installation"]
fn isolation() {
    if std::env::var("LOAM_MQTT_TEST").as_deref() != Ok("1") {
        eprintln!("skipped: set LOAM_MQTT_TEST=1 to require the real broker tier");
        return;
    }

    let mut broker =
        BrokerFixture::provision("isolation").expect("the real broker fixture should provision");
    broker
        .enable_isolation()
        .expect("strict isolation ACLs should replace the broad broker-contract fixture");
    let organization_a = broker.namespace().to_owned();
    let organization_b = broker.foreign_namespace().to_owned();
    let project_a = format!("{organization_a}/project-a");
    let project_b = format!("{organization_a}/project-b");
    let other_org_project = format!("{organization_b}/project-a");
    let state_a = format!("{project_a}/state/instance-01/sentinel");
    let state_b = format!("{project_b}/state/instance-01/sentinel");
    let state_c = format!("{other_org_project}/state/instance-01/sentinel");

    let mut actor_a = TestClient::password(&broker, "isolation-actor-a")
        .expect("organization A/project A client should authenticate");
    let mut actor_b = TestClient::credentials(
        &broker,
        "isolation-actor-b",
        "actor-b",
        broker.foreign_password(),
    )
    .expect("organization A/project B client should authenticate");
    let mut actor_c = TestClient::credentials(
        &broker,
        "isolation-actor-c",
        "actor-c",
        broker.other_org_password(),
    )
    .expect("organization B/project A client should authenticate");
    let mut mtls =
        TestClient::mtls(&broker, "isolation-mtls").expect("mTLS project peer should authenticate");

    actor_a
        .subscribe(format!("{project_a}/state/#"))
        .expect("actor A should subscribe to its project state");
    actor_a
        .subscribe(format!("{project_b}/state/#"))
        .expect("broker should return a SUBACK before filtering the foreign project");
    actor_a
        .subscribe(format!("{other_org_project}/state/#"))
        .expect("broker should return a SUBACK before filtering the foreign org");
    actor_b
        .subscribe(format!("{project_b}/state/#"))
        .expect("actor B should subscribe to its project state");
    actor_c
        .subscribe(format!("{other_org_project}/state/#"))
        .expect("actor C should subscribe to its organization state");
    mtls.subscribe(format!("{project_a}/state/#"))
        .expect("mTLS peer should subscribe to project state");

    assert_publish_accepted(
        actor_a
            .publish(&state_a, b"organization-a-project-a", true, None)
            .expect("actor A should publish under its own origin"),
    );
    assert_publish_accepted(
        actor_b
            .publish(&state_b, b"organization-a-project-b", true, None)
            .expect("actor B should publish under its own project"),
    );
    assert_publish_accepted(
        actor_c
            .publish(&state_c, b"organization-b-project-a", true, None)
            .expect("actor C should publish under its own organization"),
    );
    assert_eq!(
        actor_a
            .receive(&state_a, Duration::from_secs(3))
            .expect("actor A should receive its retained control")
            .payload
            .as_ref(),
        b"organization-a-project-a"
    );
    assert_eq!(
        actor_b
            .receive(&state_b, Duration::from_secs(3))
            .expect("actor B should receive its retained control")
            .payload
            .as_ref(),
        b"organization-a-project-b"
    );
    assert_eq!(
        actor_c
            .receive(&state_c, Duration::from_secs(3))
            .expect("actor C should receive its retained control")
            .payload
            .as_ref(),
        b"organization-b-project-a"
    );
    assert!(
        actor_a
            .collect(Duration::from_secs(1))
            .iter()
            .all(|publish| {
                publish.topic.as_ref() != state_b.as_bytes()
                    && publish.topic.as_ref() != state_c.as_bytes()
            }),
        "actor A received retained state across a project or organization boundary"
    );
    assert_eq!(
        actor_a
            .publish(
                format!("{project_b}/state/instance-01/forbidden"),
                b"cross-project",
                true,
                None,
            )
            .expect("cross-project publish should return a broker reason"),
        PubAckReason::NotAuthorized
    );
    assert_eq!(
        actor_a
            .publish(
                format!("{other_org_project}/state/instance-01/forbidden"),
                b"cross-org",
                true,
                None,
            )
            .expect("cross-organization publish should return a broker reason"),
        PubAckReason::NotAuthorized
    );
    assert_eq!(
        actor_a
            .publish(
                format!("{project_a}/state/instance-02/forbidden"),
                b"cross-origin",
                true,
                None,
            )
            .expect("cross-origin publish should return a broker reason"),
        PubAckReason::NotAuthorized
    );

    let now = test_time("2026-07-24T14:21:00Z");
    let allowed_inbox = format!("{project_a}/inbox/agent/shared-42/instance-02/01K6Q6ESWMT48TPX");
    let colliding_inbox =
        format!("{project_a}/inbox/principal/shared-42/instance-02/01K6Q6ESWMT48TPX");
    actor_a
        .subscribe(&allowed_inbox)
        .expect("actor A should subscribe to its agent recipient namespace");
    actor_a
        .subscribe(&colliding_inbox)
        .expect("broker should SUBACK before filtering the colliding principal namespace");
    mtls.subscribe(&allowed_inbox)
        .expect("mTLS control should subscribe to the agent inbox");
    mtls.subscribe(&colliding_inbox)
        .expect("mTLS control should subscribe to the principal inbox");
    let inbox_frame = scoped_frame(include_bytes!("fixtures/mqtt/message.json"), &broker)
        .replace(
            "urn:loam:instance:instance-01",
            "urn:loam:instance:instance-02",
        )
        .replace(
            "\"principal_id\": \"employee-184\"",
            "\"principal_id\": \"employee-191\"",
        )
        .replace(
            "\"instance_id\": \"instance-01\"",
            "\"instance_id\": \"instance-02\"",
        )
        .replace("agent-91", "shared-42")
        .replace("01K6Q6ESWMT48TPC", "01K6Q6ESWMT48TPX");
    let inbox_principal = AuthenticatedPrincipal::new("employee-191", &[]);
    let valid_inbox = loam::envelope::validate(
        inbox_frame.as_bytes(),
        &allowed_inbox,
        &inbox_principal,
        &ValidationConfig::default(),
        now,
    )
    .expect("typed colliding-ID inbox control should validate");
    publish_validated(&mut mtls, valid_inbox.clone(), now);
    let allowed_publish = actor_a
        .receive(&allowed_inbox, Duration::from_secs(3))
        .expect("actor A should receive the authorized agent recipient");
    let allowed_control = mtls
        .receive(&allowed_inbox, Duration::from_secs(3))
        .expect("mTLS control should receive the authorized agent recipient");
    assert_eq!(allowed_publish.payload, allowed_control.payload);

    assert_publish_accepted(
        mtls.publish(
            &colliding_inbox,
            transport::encode_validated(valid_inbox.clone()),
            true,
            None,
        )
        .expect("mismatched recipient-kind probe should reach receiver validation"),
    );
    let colliding_publish = mtls
        .receive(&colliding_inbox, Duration::from_secs(3))
        .expect("mTLS control should receive the colliding principal probe");
    let claims = ["employee-191"];
    let origins = ["instance-02"];
    let identity = AuthenticatedTransportPrincipal::new(
        AuthenticatedPrincipal::new("mtls-actor", &claims),
        &origins,
    );
    let mut processor = DeliveryProcessor::new(ValidationConfig::default(), 8, 8, 8)
        .expect("bounded isolation processor should configure");
    assert!(matches!(
        processor.receive(&allowed_inbox, &allowed_control.payload, &identity, now),
        Ok(ReceiveOutcome::Accepted(_))
    ));
    assert_eq!(
        processor.receive(&colliding_inbox, &colliding_publish.payload, &identity, now),
        Err(TransportError::Validation(Violation::BindingMismatch(
            BindingAxis::RecipientKind
        )))
    );
    assert!(
        actor_a
            .collect(Duration::from_secs(1))
            .iter()
            .all(|publish| publish.topic.as_ref() != colliding_inbox.as_bytes()),
        "colliding principal inbox crossed the typed recipient ACL"
    );

    let application_oversize_topic = format!("{project_a}/event/instance-02");
    mtls.subscribe(&application_oversize_topic)
        .expect("mTLS control should subscribe to its event origin");
    let application_oversize = vec![b'x'; ValidationConfig::default().max_document_bytes + 1];
    assert_publish_accepted(
        mtls.publish(
            &application_oversize_topic,
            application_oversize,
            false,
            None,
        )
        .expect("application-quota probe should fit under the broker packet limit"),
    );
    let oversized_publish = mtls
        .receive(&application_oversize_topic, Duration::from_secs(3))
        .expect("application-quota probe should reach receiver validation");
    assert_eq!(
        processor.receive(
            &application_oversize_topic,
            &oversized_publish.payload,
            &identity,
            now
        ),
        Err(TransportError::Validation(Violation::DocumentTooLarge))
    );
    let mut broker_oversize = TestClient::password(&broker, "isolation-broker-oversize")
        .expect("broker quota probe should authenticate");
    assert_eq!(broker_oversize.server_max_packet_size(), Some(400_000));
    let broker_oversize_result = broker_oversize.publish(
        format!("{project_a}/event/instance-01"),
        vec![b'x'; 400_001],
        false,
        None,
    );
    assert!(
        broker_oversize_result.is_err(),
        "broker accepted a packet beyond its configured maximum"
    );

    for (client, topic) in [
        (&mut actor_a, &state_a),
        (&mut actor_b, &state_b),
        (&mut actor_c, &state_c),
    ] {
        assert_publish_accepted(
            client
                .publish(topic, Vec::new(), true, None)
                .expect("authorized retained cleanup should be acknowledged"),
        );
    }
    for topic in [&allowed_inbox, &colliding_inbox] {
        assert_publish_accepted(
            mtls.publish(topic, Vec::new(), true, None)
                .expect("mTLS retained inbox cleanup should be acknowledged"),
        );
    }
    // Retain a value from the origin under a bounded 1s message-expiry. Mosquitto
    // does not clear an origin's retained messages when its credential is removed,
    // so this value's later absence is driven by MQTT message expiry, not by
    // revocation. That satisfies the "cleared or expired" AC via the expiry arm.
    let revoked_topic = format!("{project_a}/state/instance-01/revoked");
    assert_publish_accepted(
        actor_a
            .publish(&revoked_topic, b"expires-by-message-expiry", true, Some(1))
            .expect("bounded-expiry retained probe should publish before revocation"),
    );
    mtls.receive(&revoked_topic, Duration::from_secs(3))
        .expect("bounded-expiry retained value must exist before it expires");

    // Baseline positive control while actor A is still connected: it writes to
    // its own origin and receives a live message an authorized peer publishes
    // into its subscribed project scope.
    let live_probe = format!("{project_a}/state/instance-02/live-access");
    assert_publish_accepted(
        actor_a
            .publish(
                format!("{project_a}/state/instance-01/pre-revoke-live"),
                b"authorized-before-revocation",
                false,
                None,
            )
            .expect("actor A should still write to its own origin before revocation"),
    );
    assert_publish_accepted(
        mtls.publish(&live_probe, b"pre-revoke-sentinel", false, None)
            .expect("mTLS control should publish the pre-revocation live sentinel"),
    );
    assert_eq!(
        actor_a
            .receive(&live_probe, Duration::from_secs(3))
            .expect("actor A should receive live project state before revocation")
            .payload
            .as_ref(),
        b"pre-revoke-sentinel"
    );

    let mut git = GitOracleFixture::provision();
    let git_before = git.snapshot();
    drop(actor_b);
    drop(actor_c);
    drop(broker_oversize);

    // Revoke actor A on its already-connected session without a broker restart:
    // strip its ACL grants and delete its password, then apply the change in
    // place with a SIGHUP reload. The reload severs actor A's live session but
    // leaves every other connection up.
    broker
        .revoke_live("actor-a")
        .expect("broker credential should be revoked live without a restart");
    assert_eq!(git.snapshot(), git_before);
    assert!(git.peer_has_object(git.base_oid()));

    // Live-session loss: an operation on the connection that was demonstrably
    // working moments earlier now fails because the broker dropped the revoked
    // session in place. The same publish succeeded before revocation, so the
    // failure is genuine live loss rather than a never-authorized origin.
    let severed = actor_a.publish(
        format!("{project_a}/state/instance-01/post-revoke-live"),
        b"denied-after-revocation",
        false,
        None,
    );
    assert!(
        severed.is_err(),
        "revoked live session unexpectedly kept publishing: {severed:?}"
    );

    // Positive control proving this was a live selective revocation and not a
    // broker restart: the unrevoked mTLS session stays connected and keeps full
    // publish/subscribe access on the same broker instance.
    let survivor_probe = format!("{project_a}/state/instance-02/survivor");
    assert_publish_accepted(
        mtls.publish(&survivor_probe, b"survivor-sentinel", false, None)
            .expect("unrevoked mTLS session must survive the live revocation"),
    );
    assert_eq!(
        mtls.receive(&survivor_probe, Duration::from_secs(3))
            .expect("surviving mTLS session must still receive live project state")
            .payload
            .as_ref(),
        b"survivor-sentinel"
    );

    drop(actor_a);
    drop(mtls);

    // The now-deleted credential cannot open a fresh session, and anonymous
    // access stays refused.
    let revoked = match TestClient::password(&broker, "isolation-revoked") {
        Ok(_) => panic!("revoked credential reconnected"),
        Err(error) => error,
    };
    assert!(
        revoked.contains("NotAuthorized") || revoked.contains("not authorised"),
        "unexpected revoked-credential refusal: {revoked}"
    );
    let anonymous = match TestClient::anonymous(&broker, "isolation-anonymous") {
        Ok(_) => panic!("anonymous connection must remain refused"),
        Err(error) => error,
    };
    assert!(
        anonymous.contains("NotAuthorized") || anonymous.contains("not authorised"),
        "unexpected anonymous refusal: {anonymous}"
    );
    std::thread::sleep(Duration::from_secs(2));

    let post_revoke_control = format!("{project_a}/state/instance-02/post-revoke");
    let mut post_revoke = TestClient::mtls(&broker, "isolation-post-revoke")
        .expect("unrevoked mTLS peer should reconnect");
    post_revoke
        .subscribe(format!("{project_a}/state/#"))
        .expect("unrevoked peer should retain its read scope");
    assert_publish_accepted(
        post_revoke
            .publish(&post_revoke_control, b"still-authorized", true, None)
            .expect("unrevoked peer should publish its retained control"),
    );
    let post_revoke_values = post_revoke.collect(Duration::from_secs(2));
    assert!(post_revoke_values.iter().any(|publish| {
        publish.topic.as_ref() == post_revoke_control.as_bytes()
            && publish.payload.as_ref() == b"still-authorized"
    }));
    // The revoked origin's retained value is gone — by MQTT message expiry
    // (1s interval + the sleep above), not because revocation cleared it.
    assert!(post_revoke_values
        .iter()
        .all(|publish| publish.topic.as_ref() != revoked_topic.as_bytes()));
    assert_publish_accepted(
        post_revoke
            .publish(&post_revoke_control, Vec::new(), true, None)
            .expect("post-revocation control should clean up"),
    );

    broker
        .wait_for_log("Denied PUBLISH")
        .expect("broker log should prove forbidden publication reached the ACL");
    broker
        .wait_for_log("not authorised")
        .expect("broker log should prove revoked or anonymous authentication refusal");
    let mut final_mtls = TestClient::mtls(&broker, "isolation-final-a")
        .expect("final organization A scan should authenticate");
    assert_retained_round_trip_and_clear(
        &mut final_mtls,
        format!("{project_a}/state/#"),
        format!("{project_a}/state/instance-02/final-control"),
    );
    assert_retained_round_trip_and_clear(
        &mut final_mtls,
        format!("{project_a}/inbox/agent/shared-42/#"),
        format!("{project_a}/inbox/agent/shared-42/instance-02/final-agent"),
    );
    assert_retained_round_trip_and_clear(
        &mut final_mtls,
        format!("{project_a}/inbox/principal/shared-42/#"),
        format!("{project_a}/inbox/principal/shared-42/instance-02/final-principal"),
    );
    let mut final_b = TestClient::credentials(
        &broker,
        "isolation-final-b",
        "actor-b",
        broker.foreign_password(),
    )
    .expect("final project B scan should authenticate");
    assert_retained_round_trip_and_clear(
        &mut final_b,
        format!("{project_b}/state/#"),
        format!("{project_b}/state/instance-01/final-control"),
    );
    let mut final_c = TestClient::credentials(
        &broker,
        "isolation-final-c",
        "actor-c",
        broker.other_org_password(),
    )
    .expect("final organization B scan should authenticate");
    assert_retained_round_trip_and_clear(
        &mut final_c,
        format!("{other_org_project}/state/#"),
        format!("{other_org_project}/state/instance-01/final-control"),
    );

    broker
        .finish()
        .expect("broker fixture should remove only its temporary directory");
    git.finish();
}

#[test]
#[ignore = "requires LOAM_MQTT_TEST=1, Git, and a real Mosquitto/OpenSSL installation"]
fn lease_tombstone() {
    if std::env::var("LOAM_MQTT_TEST").as_deref() != Ok("1") {
        eprintln!("skipped: set LOAM_MQTT_TEST=1 to require the real broker tier");
        return;
    }

    let cases = include_str!("fixtures/mqtt/lease-cases.json");
    for name in [
        "active_lease_current",
        "renewal_interval_elapsed",
        "expired_within_skew",
        "expired_beyond_skew",
        "origin_tombstone",
        "cross_origin_tombstone",
        "clean_restart",
        "dirty_restart",
        "degraded_restart",
    ] {
        assert!(cases.contains(name), "missing lease case {name}");
    }

    let lifecycle = LifecycleConfig::new(
        chrono::Duration::minutes(30),
        chrono::Duration::minutes(10),
        chrono::Duration::minutes(5),
        chrono::Duration::hours(24),
        chrono::Duration::seconds(30),
    )
    .expect("bounded lifecycle configuration should validate");
    let broker = BrokerFixture::provision("lease-tombstone")
        .expect("the real broker fixture should provision");
    let project = format!("{}/project-a", broker.namespace());
    let state_topic = format!("{project}/state/instance-01/activity-01K6Q5");
    let foreign_topic = format!("{project}/state/instance-02/activity-01K6Q5");
    let now = test_time("2026-07-24T14:21:00Z");
    let (_, active) = scoped_work_state(
        &broker,
        "instance-01",
        "employee-184",
        1,
        "active",
        "SB-42",
        now,
    );

    let mut publisher = TestClient::password(&broker, "lease-publisher")
        .expect("lease publisher should authenticate");
    let mut observer = TestClient::password(&broker, "lease-observer")
        .expect("lease observer should authenticate");
    observer
        .subscribe(format!("{project}/#"))
        .expect("lease observer should subscribe");
    transport::publish_with_lifecycle(&publisher.client, active.clone(), now, &lifecycle)
        .expect("active lease should queue for publication");
    assert_publish_accepted(
        publisher
            .wait_for_puback()
            .expect("active lease should be acknowledged"),
    );
    let active_publish = observer
        .receive(&state_topic, Duration::from_secs(3))
        .expect("active lease should cross the broker");

    let claims = ["employee-184"];
    let origins = ["instance-01"];
    let identity = AuthenticatedTransportPrincipal::new(
        AuthenticatedPrincipal::new("broker-user-7", &claims),
        &origins,
    );
    let mut processor =
        DeliveryProcessor::with_lifecycle(ValidationConfig::default(), 8, 8, 8, lifecycle.clone())
            .expect("bounded lease processor should configure");
    let received =
        accepted(processor.receive(&state_topic, &active_publish.payload, &identity, now));
    let mut tracker = WorkTracker::with_lifecycle(8, lifecycle.clone())
        .expect("bounded lease tracker should configure");
    tracker
        .observe(&received)
        .expect("active lease should be observed");
    assert_eq!(
        tracker.classification(
            "instance-01",
            "activity-01K6Q5",
            test_time("2026-07-24T14:29:59Z")
        ),
        Some(WorkClassification::Current(WorkStatus::Active))
    );
    assert!(tracker.renewal_due(
        "instance-01",
        "activity-01K6Q5",
        test_time("2026-07-24T14:30:00Z")
    ));
    assert_eq!(
        tracker.classification(
            "instance-01",
            "activity-01K6Q5",
            test_time("2026-07-24T14:50:29Z")
        ),
        Some(WorkClassification::Current(WorkStatus::Active))
    );
    assert_eq!(
        tracker.classification(
            "instance-01",
            "activity-01K6Q5",
            test_time("2026-07-24T14:50:31Z")
        ),
        Some(WorkClassification::StaleInterrupted)
    );
    assert_eq!(
        tracker.status("instance-01", "activity-01K6Q5"),
        Some(WorkStatus::Active),
        "lease expiry must not manufacture a terminal transition"
    );

    let mut within_skew =
        DeliveryProcessor::with_lifecycle(ValidationConfig::default(), 8, 8, 8, lifecycle.clone())
            .expect("within-skew processor should configure");
    assert!(matches!(
        within_skew.receive(
            &state_topic,
            &active_publish.payload,
            &identity,
            test_time("2026-07-24T14:50:29Z")
        ),
        Ok(ReceiveOutcome::Accepted(_))
    ));
    let mut beyond_skew =
        DeliveryProcessor::with_lifecycle(ValidationConfig::default(), 8, 8, 8, lifecycle.clone())
            .expect("beyond-skew processor should configure");
    assert_eq!(
        beyond_skew.receive(
            &state_topic,
            &active_publish.payload,
            &identity,
            test_time("2026-07-24T14:50:31Z")
        ),
        Err(TransportError::Validation(Violation::Expired))
    );

    let mut late = TestClient::password(&broker, "lease-late-positive")
        .expect("late retained-state control should authenticate");
    late.subscribe(&state_topic)
        .expect("late retained-state control should subscribe");
    assert_eq!(
        late.receive(&state_topic, Duration::from_secs(3))
            .expect("retained state must exist before either tombstone probe")
            .payload,
        active_publish.payload
    );

    assert_publish_accepted(
        publisher
            .publish(&foreign_topic, Vec::new(), true, None)
            .expect("foreign tombstone should reach the application boundary"),
    );
    let foreign_tombstone = observer
        .receive(&foreign_topic, Duration::from_secs(3))
        .expect("foreign tombstone should cross the broker for application rejection");
    assert_eq!(
        processor.receive(&foreign_topic, &foreign_tombstone.payload, &identity, now),
        Err(TransportError::OriginNotAuthorized)
    );

    publish_state_clear(&mut publisher, active);
    let tombstone = observer
        .receive(&state_topic, Duration::from_secs(3))
        .expect("same-origin tombstone should cross the broker");
    assert!(tombstone.payload.is_empty());
    assert_eq!(
        processor.receive(&state_topic, &tombstone.payload, &identity, now),
        Ok(ReceiveOutcome::Removed)
    );

    let mut git = GitOracleFixture::provision();
    let clean_snapshot = git.snapshot();
    assert_eq!(
        transport::inspect_restart_worktree(git.peer()),
        RestartInspection::Clean
    );
    assert_eq!(git.snapshot(), clean_snapshot);
    git.make_dirty();
    let dirty_snapshot = git.snapshot();
    assert_eq!(
        transport::inspect_restart_worktree(git.peer()),
        RestartInspection::Dirty
    );
    assert_eq!(git.snapshot(), dirty_snapshot);
    git.clean_dirty_probe();
    assert_eq!(
        transport::inspect_restart_worktree(git.wiki()),
        RestartInspection::Degraded
    );
    git.finish();

    let mut final_scan = TestClient::password(&broker, "lease-final-scan")
        .expect("final retained scan should authenticate");
    final_scan
        .subscribe(format!("{project}/#"))
        .expect("final retained scan should subscribe");
    assert!(
        final_scan.collect(Duration::from_secs(2)).is_empty(),
        "lease/tombstone test left retained values under its namespace"
    );
    broker
        .finish()
        .expect("broker fixture should remove only its temporary directory");
}

#[test]
#[ignore = "requires LOAM_MQTT_TEST=1, Git, and a real Mosquitto/OpenSSL installation"]
fn git_oracle() {
    if std::env::var("LOAM_MQTT_TEST").as_deref() != Ok("1") {
        eprintln!("skipped: set LOAM_MQTT_TEST=1 to require the real broker tier");
        return;
    }

    let cases = include_str!("fixtures/mqtt/git-transport-cases.json");
    for name in [
        "ready_only",
        "unreachable_published",
        "allowed_ref_published",
        "disallowed_ref",
        "fetch_failure",
        "dropped_hint",
        "dirty_handoff",
        "unfetchable_handoff",
    ] {
        assert!(cases.contains(name), "missing Git oracle case {name}");
    }
    let mut fixture = GitOracleFixture::provision();
    let broker =
        BrokerFixture::provision("git-oracle").expect("the real broker fixture should provision");
    let project = format!("{}/project-a", broker.namespace());
    let state_topic = format!("{project}/state/instance-01/activity-01K6Q5");
    let event_topic = format!("{project}/event/instance-01");
    let now = test_time("2026-07-24T14:21:00Z");
    let mut oracle = GitOracle::new(
        fixture.peer(),
        fixture.wiki(),
        "origin",
        git_scope(&broker),
        ["refs/heads/main"],
        ["instance-01"],
        Duration::from_secs(30),
    )
    .expect("configured Git oracle should validate");
    let ready = scoped_work_state_with_commit(
        &broker,
        "instance-01",
        "employee-184",
        1,
        "ready",
        "SB-42",
        None,
        now,
    )
    .1;
    assert_eq!(
        oracle
            .evaluate_work_state(&ready)
            .expect("ready should remain provisional"),
        PublicationStatus::Provisional
    );

    let published = scoped_work_state_with_commit(
        &broker,
        "instance-01",
        "employee-184",
        2,
        "published",
        "SB-42",
        Some(fixture.allowed_oid()),
        now,
    )
    .1;
    assert!(
        !fixture.peer_has_object(fixture.allowed_oid()),
        "allowed publication object was already present before the oracle fetch"
    );
    let before = fixture.snapshot();
    let configured_remote = oracle.configured_remote().to_owned();
    let configured_refs = oracle.configured_refs().to_vec();
    let proof = match oracle
        .evaluate_work_state(&published)
        .expect("allowed-ref publication should verify")
    {
        PublicationStatus::Verified(proof) => proof,
        PublicationStatus::Provisional => panic!("published state remained provisional"),
    };
    assert!(fixture.peer_has_object(fixture.allowed_oid()));
    assert_eq!(fixture.snapshot(), before);
    assert_eq!(oracle.configured_remote(), configured_remote);
    assert_eq!(oracle.configured_refs(), configured_refs);

    let missing = scoped_work_state_with_commit(
        &broker,
        "instance-01",
        "employee-184",
        2,
        "published",
        "SB-42",
        Some("0000000000000000000000000000000000000001"),
        now,
    )
    .1;
    assert_eq!(
        oracle.evaluate_work_state(&missing),
        Err(GitOracleError::UnreachableCommit)
    );
    let disallowed = scoped_work_state_with_commit(
        &broker,
        "instance-01",
        "employee-184",
        2,
        "published",
        "SB-42",
        Some(fixture.disallowed_oid()),
        now,
    )
    .1;
    assert_eq!(
        oracle.evaluate_work_state(&disallowed),
        Err(GitOracleError::UnreachableCommit)
    );
    let mut broken = GitOracle::new(
        fixture.peer(),
        fixture.wiki(),
        "broken",
        git_scope(&broker),
        ["refs/heads/main"],
        ["instance-01"],
        Duration::from_secs(30),
    )
    .expect("configured but unreachable remote should validate locally");
    assert_eq!(
        broken.evaluate_work_state(&disallowed),
        Err(GitOracleError::GitFailure)
    );

    let mut publisher = TestClient::password(&broker, "git-oracle-publisher")
        .expect("Git oracle publisher should authenticate");
    let mut observer = TestClient::password(&broker, "git-oracle-observer")
        .expect("Git oracle observer should authenticate");
    observer
        .subscribe(format!("{project}/#"))
        .expect("Git oracle observer should subscribe");
    let claims = ["employee-184"];
    let origins = ["instance-01"];
    let identity = AuthenticatedTransportPrincipal::new(
        AuthenticatedPrincipal::new("broker-user-7", &claims),
        &origins,
    );
    let mut processor = DeliveryProcessor::new(ValidationConfig::default(), 8, 8, 8)
        .expect("bounded Git oracle processor should configure");
    let mut tracker = WorkTracker::new(8).expect("bounded work tracker should configure");
    publish_validated(&mut publisher, ready, now);
    let ready_publish = observer
        .receive(&state_topic, Duration::from_secs(3))
        .expect("ready claim should cross the broker");
    let received_ready =
        accepted(processor.receive(&state_topic, &ready_publish.payload, &identity, now));
    tracker
        .observe(&received_ready)
        .expect("ready claim should remain provisional");
    publish_validated(&mut publisher, published.clone(), now);
    let published_frame = observer
        .receive(&state_topic, Duration::from_secs(3))
        .expect("published claim should cross the broker");
    let received_published =
        accepted(processor.receive(&state_topic, &published_frame.payload, &identity, now));
    assert_eq!(
        tracker.observe(&received_published),
        Err(TransportError::PublicationUnverified)
    );
    tracker
        .observe_verified(&received_published, &proof)
        .expect("Git proof should admit ready to published");
    assert_eq!(
        tracker.status("instance-01", "activity-01K6Q5"),
        Some(WorkStatus::Published)
    );

    let refs_changed = scoped_refs_changed(
        &broker,
        &event_topic,
        fixture.base_oid(),
        fixture.allowed_oid(),
        now,
    );
    publish_validated(&mut publisher, refs_changed, now);
    let hint_publish = observer
        .receive(&event_topic, Duration::from_secs(3))
        .expect("ref-change hint should cross the broker");
    let received_hint =
        accepted(processor.receive(&event_topic, &hint_publish.payload, &identity, now));
    let hinted = oracle
        .reconcile_ref_change(&received_hint)
        .expect("allowed ref-change hint should fetch and recompute");
    let without_hint = oracle
        .reconcile()
        .expect("dropped hint should converge through ordinary reconciliation");
    assert_eq!(hinted, without_hint);
    assert!(hinted.tips.iter().all(|tip| tip.pending_count > 0));
    assert_eq!(fixture.snapshot(), before);

    let mut short_cache = GitOracle::new(
        fixture.peer(),
        fixture.wiki(),
        "origin",
        git_scope(&broker),
        ["refs/heads/main"],
        ["instance-01"],
        Duration::from_millis(50),
    )
    .expect("short bounded reconciliation freshness should validate");
    let cached_control = short_cache
        .reconcile()
        .expect("initial reconciliation should populate the successful-result cache");
    fixture.hide_remote();
    assert_eq!(
        short_cache
            .reconcile()
            .expect("a fresh successful reconciliation should coalesce the retry"),
        cached_control
    );
    std::thread::sleep(Duration::from_millis(75));
    assert_eq!(short_cache.reconcile(), Err(GitOracleError::GitFailure));
    fixture.restore_remote();
    assert_eq!(
        short_cache
            .reconcile()
            .expect("a failed refresh must not be cached"),
        cached_control
    );

    let foreign_hint_topic = format!("{project}/event/instance-02");
    let foreign_hint = scoped_refs_changed_from(
        &broker,
        &foreign_hint_topic,
        "instance-02",
        "employee-191",
        fixture.base_oid(),
        fixture.allowed_oid(),
        now,
    );
    assert_eq!(
        oracle.reconcile_ref_change(&foreign_hint),
        Err(GitOracleError::UnauthorizedHintOrigin)
    );

    oracle
        .check_handoff(fixture.allowed_oid())
        .expect("clean fetchable handoff should pass");
    assert_eq!(
        oracle.check_handoff(fixture.disallowed_oid()),
        Err(GitOracleError::UnreachableCommit)
    );
    fixture.make_dirty();
    assert_eq!(
        oracle.check_handoff(fixture.allowed_oid()),
        Err(GitOracleError::DirtyWorktree)
    );
    fixture.clean_dirty_probe();
    assert_eq!(
        broken.check_handoff(fixture.allowed_oid()),
        Err(GitOracleError::GitFailure)
    );

    publish_state_clear(&mut publisher, published);
    let mut final_scan = TestClient::password(&broker, "git-oracle-final-scan")
        .expect("final retained scan should authenticate");
    final_scan
        .subscribe(format!("{project}/#"))
        .expect("final retained scan should subscribe");
    assert!(
        final_scan.collect(Duration::from_secs(2)).is_empty(),
        "Git oracle test left retained values under its namespace"
    );
    broker
        .finish()
        .expect("broker fixture should remove only its temporary directory");
    fixture.finish();
}

#[test]
#[ignore = "requires LOAM_MQTT_TEST=1 and a real Mosquitto/OpenSSL installation"]
fn collaboration_semantics() {
    if std::env::var("LOAM_MQTT_TEST").as_deref() != Ok("1") {
        eprintln!("skipped: set LOAM_MQTT_TEST=1 to require the real broker tier");
        return;
    }

    let broker = BrokerFixture::provision("collaboration-semantics")
        .expect("the real broker fixture should provision");
    let project = format!("{}/project-a", broker.namespace());
    let now = test_time("2026-07-24T14:21:00Z");
    let request_topic = format!("{project}/inbox/agent/agent-91/instance-01/01K6Q6ESWMT48TPC");
    let request = scoped_envelope(
        include_bytes!("fixtures/mqtt/message.json"),
        &request_topic,
        &broker,
        now,
    );
    let (response_topic, response) = scoped_response_envelope(&broker, now);
    let (ack_topic, ack) = scoped_ack_envelope(&broker, now);

    let mut sender = TestClient::password(&broker, "collaboration-sender")
        .expect("collaboration sender should authenticate");
    let mut peer = TestClient::password(&broker, "collaboration-peer")
        .expect("collaboration peer should authenticate");
    peer.subscribe(format!("{project}/#"))
        .expect("collaboration peer should subscribe");

    publish_validated(&mut sender, request.clone(), now);
    let request_publish = peer
        .receive(&request_topic, Duration::from_secs(3))
        .expect("request should cross the broker");
    let mut idle = TestClient::password(&broker, "idle-recipient")
        .expect("idle recipient should authenticate");
    idle.subscribe(&request_topic)
        .expect("idle recipient should subscribe after the PUBACK");
    assert_eq!(
        idle.receive(&request_topic, Duration::from_secs(3))
            .expect("unanswered request must remain retained")
            .payload,
        request_publish.payload
    );
    assert_eq!(
        transport::publish_inbox_tombstone_after(&sender.client, request.clone(), &request),
        Err(TransportError::SemanticReplyMismatch),
        "a transport acknowledgement must not clear an inbox request"
    );

    publish_validated(&mut sender, response.clone(), now);
    publish_validated(&mut sender, ack.clone(), now);
    let response_publish = peer
        .receive(&response_topic, Duration::from_secs(3))
        .expect("response should cross the broker");
    let ack_publish = peer
        .receive(&ack_topic, Duration::from_secs(3))
        .expect("semantic acknowledgement should cross the broker");

    let claims = ["employee-184", "employee-191"];
    let origins = ["instance-01", "instance-02"];
    let identity = AuthenticatedTransportPrincipal::new(
        AuthenticatedPrincipal::new("broker-user-7", &claims),
        &origins,
    );
    let mut processor = DeliveryProcessor::new(ValidationConfig::default(), 16, 16, 16)
        .expect("bounded collaboration processor should configure");
    let received_request =
        accepted(processor.receive(&request_topic, &request_publish.payload, &identity, now));
    let received_response =
        accepted(processor.receive(&response_topic, &response_publish.payload, &identity, now));
    let received_ack =
        accepted(processor.receive(&ack_topic, &ack_publish.payload, &identity, now));
    let response_thread = received_response
        .as_envelope()
        .data
        .thread
        .as_ref()
        .expect("response should preserve its thread");
    assert_eq!(response_thread.correlation_id, "01K6Q6ESWMT48TPC");
    assert_eq!(
        response_thread
            .causation_id
            .as_ref()
            .and_then(|id| id.as_str()),
        Some("01K6Q6ESWMT48TPC")
    );
    let ack_thread = received_ack
        .as_envelope()
        .data
        .thread
        .as_ref()
        .expect("ack should preserve its thread");
    assert_eq!(ack_thread.correlation_id, "01K6Q6ESWMT48TPC");
    assert_eq!(
        ack_thread.causation_id.as_ref().and_then(|id| id.as_str()),
        Some("01K6Q6ESWMT48TPD")
    );

    publish_semantic_clear(&mut sender, received_request, &received_response);
    publish_semantic_clear(&mut sender, received_response, &received_ack);
    assert_publish_accepted(
        sender
            .publish(&ack_topic, Vec::new(), true, None)
            .expect("observed terminal ack should be cleaned from the test namespace"),
    );

    let mut tracker = WorkTracker::new(16).expect("bounded work tracker should configure");
    let mut final_first = None;
    for (revision, status) in [(1, "active"), (2, "blocked"), (3, "active"), (4, "ready")] {
        let (topic, state) = scoped_work_state(
            &broker,
            "instance-01",
            "employee-184",
            revision,
            status,
            "SB-42",
            now,
        );
        publish_validated(&mut sender, state.clone(), now);
        let publish = peer
            .receive(&topic, Duration::from_secs(3))
            .expect("work-state transition should cross the broker");
        let received = accepted(processor.receive(&topic, &publish.payload, &identity, now));
        tracker
            .observe(&received)
            .expect("legal work-state transition should be accepted");
        final_first = Some((topic, state));
    }
    assert_eq!(
        tracker.status("instance-01", "activity-01K6Q5"),
        Some(WorkStatus::Ready)
    );

    let (second_topic, second_active) = scoped_work_state(
        &broker,
        "instance-02",
        "employee-191",
        1,
        "active",
        "SB-42",
        now,
    );
    publish_validated(&mut sender, second_active, now);
    let second_publish = peer
        .receive(&second_topic, Duration::from_secs(3))
        .expect("overlapping activity should cross the broker");
    let second_received =
        accepted(processor.receive(&second_topic, &second_publish.payload, &identity, now));
    let overlap = tracker
        .observe(&second_received)
        .expect("overlap should warn without rejecting either activity");
    assert_eq!(overlap.warnings.len(), 2);
    assert_eq!(tracker.len(), 2);

    let (_, second_abandoned) = scoped_work_state(
        &broker,
        "instance-02",
        "employee-191",
        2,
        "abandoned",
        "SB-42",
        now,
    );
    publish_validated(&mut sender, second_abandoned.clone(), now);
    let abandoned_publish = peer
        .receive(&second_topic, Duration::from_secs(3))
        .expect("explicit abandonment should cross the broker");
    let abandoned =
        accepted(processor.receive(&second_topic, &abandoned_publish.payload, &identity, now));
    assert!(tracker
        .observe(&abandoned)
        .expect("explicit abandonment should be accepted")
        .warnings
        .is_empty());
    assert_eq!(
        tracker.status("instance-02", "activity-01K6Q5"),
        Some(WorkStatus::Abandoned)
    );

    let offline_id = "01K6Q6ESWMT48TPX";
    let offline_topic = format!("{project}/inbox/agent/agent-91/instance-01/{offline_id}");
    let offline_request = scoped_message_with_id(&broker, &offline_topic, offline_id, now);
    publish_validated(&mut sender, offline_request, now);
    peer.receive(&offline_topic, Duration::from_secs(3))
        .expect("retained offline request positive control should be live before disconnect");
    let old_event_topic = format!("{project}/event/instance-01");
    publish_validated(
        &mut sender,
        scoped_event_with_id(&broker, &old_event_topic, "old-event", now),
        now,
    );
    peer.receive(&old_event_topic, Duration::from_secs(3))
        .expect("pre-disconnect event positive control should be live");
    drop(peer);

    let mut recovered = TestClient::password(&broker, "clean-session-recovery")
        .expect("clean-session peer should reconnect");
    recovered
        .subscribe(format!("{project}/#"))
        .expect("clean-session peer should resubscribe");
    let restored = recovered.collect(Duration::from_secs(2));
    let final_first = final_first.expect("ready state should have been published");
    assert!(restored
        .iter()
        .any(|publish| publish.topic.as_ref() == final_first.0.as_bytes()));
    assert!(restored
        .iter()
        .any(|publish| publish.topic.as_ref() == offline_topic.as_bytes()));
    assert!(
        restored
            .iter()
            .all(|publish| publish.topic.as_ref() != old_event_topic.as_bytes()),
        "old event replayed through a clean session: {restored:?}"
    );
    let live_event = scoped_event_with_id(&broker, &old_event_topic, "live-event", now);
    publish_validated(&mut sender, live_event, now);
    recovered
        .receive(&old_event_topic, Duration::from_secs(3))
        .expect("post-reconnect live event proves the event subscription is active");

    publish_state_clear(&mut sender, final_first.1);
    publish_state_clear(&mut sender, second_abandoned);
    assert_publish_accepted(
        sender
            .publish(&offline_topic, Vec::new(), true, None)
            .expect("test-owned unresolved request should be removed during teardown"),
    );
    let mut final_scan = TestClient::password(&broker, "collaboration-final-scan")
        .expect("final retained scan should authenticate");
    final_scan
        .subscribe(format!("{project}/#"))
        .expect("final retained scan should subscribe");
    assert!(
        final_scan.collect(Duration::from_secs(2)).is_empty(),
        "collaboration test left retained values under its namespace"
    );
    broker
        .finish()
        .expect("broker fixture should remove only its temporary directory");
}

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
    let (_, semantic_reply) = scoped_response_envelope(&broker, now);
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
    server_max_packet_size: Option<u32>,
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
            server_max_packet_size: None,
        };
        loop {
            match connected.next_packet(Duration::from_secs(5))? {
                Packet::ConnAck(ack) if ack.code == ConnectReturnCode::Success => {
                    connected.server_max_packet_size = ack
                        .properties
                        .as_ref()
                        .and_then(|properties| properties.max_packet_size);
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

    fn server_max_packet_size(&self) -> Option<u32> {
        self.server_max_packet_size
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
        .set_clean_start(true)
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

fn assert_retained_round_trip_and_clear(
    client: &mut TestClient,
    filter: impl Into<String>,
    topic: impl Into<String>,
) {
    let topic = topic.into();
    assert_eq!(
        client
            .subscribe(filter)
            .expect("retained cleanup control should subscribe"),
        SubscribeReasonCode::Success(QoS::AtLeastOnce)
    );
    assert_publish_accepted(
        client
            .publish(&topic, b"retained-cleanup-control", true, None)
            .expect("retained cleanup control should publish"),
    );
    assert_eq!(
        client
            .receive(&topic, Duration::from_secs(3))
            .unwrap_or_else(|error| panic!("retained cleanup control {topic} failed: {error}"))
            .payload
            .as_ref(),
        b"retained-cleanup-control"
    );
    assert_publish_accepted(
        client
            .publish(&topic, Vec::new(), true, None)
            .expect("retained cleanup control should clear"),
    );
    assert!(client
        .receive(&topic, Duration::from_secs(3))
        .expect("retained cleanup tombstone should be observed")
        .payload
        .is_empty());
    assert!(
        client.collect(Duration::from_secs(2)).is_empty(),
        "retained values remained after positive cleanup control under {topic}"
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

fn scoped_response_envelope(
    broker: &BrokerFixture,
    now: DateTime<Utc>,
) -> (String, ValidatedEnvelope) {
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
    let envelope = loam::envelope::validate(
        response.as_bytes(),
        &response_topic,
        &principal,
        &ValidationConfig::default(),
        now,
    )
    .expect("scoped semantic response should validate");
    (response_topic, envelope)
}

fn scoped_ack_envelope(broker: &BrokerFixture, now: DateTime<Utc>) -> (String, ValidatedEnvelope) {
    let topic = format!(
        "{}/project-a/inbox/agent/agent-91/instance-01/01K6Q6ESWMT48TPE",
        broker.namespace()
    );
    let frame = scoped_frame(include_bytes!("fixtures/mqtt/message.json"), broker)
        .replacen(
            "\"id\": \"01K6Q6ESWMT48TPC\"",
            "\"id\": \"01K6Q6ESWMT48TPE\"",
            1,
        )
        .replace("\"intent\": \"request\"", "\"intent\": \"ack\"")
        .replace(
            "\"causation_id\": null",
            "\"causation_id\": \"01K6Q6ESWMT48TPD\"",
        );
    let principal = AuthenticatedPrincipal::new("employee-184", &[]);
    let envelope = loam::envelope::validate(
        frame.as_bytes(),
        &topic,
        &principal,
        &ValidationConfig::default(),
        now,
    )
    .expect("scoped semantic ack should validate");
    (topic, envelope)
}

fn scoped_work_state(
    broker: &BrokerFixture,
    origin: &str,
    principal_id: &str,
    revision: u64,
    status: &str,
    artifact_id: &str,
    now: DateTime<Utc>,
) -> (String, ValidatedEnvelope) {
    scoped_work_state_with_commit(
        broker,
        origin,
        principal_id,
        revision,
        status,
        artifact_id,
        None,
        now,
    )
}

#[allow(clippy::too_many_arguments)]
fn scoped_work_state_with_commit(
    broker: &BrokerFixture,
    origin: &str,
    principal_id: &str,
    revision: u64,
    status: &str,
    artifact_id: &str,
    commit: Option<&str>,
    now: DateTime<Utc>,
) -> (String, ValidatedEnvelope) {
    let topic = format!(
        "{}/project-a/state/{origin}/activity-01K6Q5",
        broker.namespace()
    );
    let mut frame = scoped_frame(include_bytes!("fixtures/mqtt/work-state.json"), broker)
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
        .replace("\"state\": \"ready\"", &format!("\"state\": \"{status}\""))
        .replace("SB-42", artifact_id);
    if let Some(commit) = commit {
        frame = frame.replace(
            "\"plan_oid\": \"61af000000000000000000000000000000000001\"",
            &format!(
                "\"plan_oid\": \"61af000000000000000000000000000000000001\",\n        \"commit\": \"{commit}\""
            ),
        );
    }
    let principal = AuthenticatedPrincipal::new(principal_id, &[]);
    let envelope = loam::envelope::validate(
        frame.as_bytes(),
        &topic,
        &principal,
        &ValidationConfig::default(),
        now,
    )
    .expect("scoped work-state should validate");
    (topic, envelope)
}

fn scoped_refs_changed(
    broker: &BrokerFixture,
    topic: &str,
    old_oid: &str,
    new_oid: &str,
    now: DateTime<Utc>,
) -> ValidatedEnvelope {
    scoped_refs_changed_from(
        broker,
        topic,
        "instance-01",
        "employee-184",
        old_oid,
        new_oid,
        now,
    )
}

#[allow(clippy::too_many_arguments)]
fn scoped_refs_changed_from(
    broker: &BrokerFixture,
    topic: &str,
    origin: &str,
    principal_id: &str,
    old_oid: &str,
    new_oid: &str,
    now: DateTime<Utc>,
) -> ValidatedEnvelope {
    let frame = scoped_frame(
        include_bytes!("fixtures/mqtt/git-refs-changed.json"),
        broker,
    )
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
    .replace("84be000000000000000000000000000000000001", old_oid)
    .replace("84be000000000000000000000000000000000002", new_oid);
    let principal = AuthenticatedPrincipal::new(principal_id, &[]);
    loam::envelope::validate(
        frame.as_bytes(),
        topic,
        &principal,
        &ValidationConfig::default(),
        now,
    )
    .expect("scoped ref-change hint should validate")
}

fn scoped_message_with_id(
    broker: &BrokerFixture,
    topic: &str,
    id: &str,
    now: DateTime<Utc>,
) -> ValidatedEnvelope {
    let frame = scoped_frame(include_bytes!("fixtures/mqtt/message.json"), broker)
        .replace("01K6Q6ESWMT48TPC", id);
    let principal = AuthenticatedPrincipal::new("employee-184", &[]);
    loam::envelope::validate(
        frame.as_bytes(),
        topic,
        &principal,
        &ValidationConfig::default(),
        now,
    )
    .expect("scoped inbox message should validate")
}

fn scoped_event_with_id(
    broker: &BrokerFixture,
    topic: &str,
    id: &str,
    now: DateTime<Utc>,
) -> ValidatedEnvelope {
    let frame = scoped_frame(
        include_bytes!("fixtures/mqtt/git-refs-changed.json"),
        broker,
    )
    .replace("01K6Q6ESWMT48TPA", id);
    let principal = AuthenticatedPrincipal::new("employee-184", &[]);
    loam::envelope::validate(
        frame.as_bytes(),
        topic,
        &principal,
        &ValidationConfig::default(),
        now,
    )
    .expect("scoped event should validate")
}

fn publish_validated(client: &mut TestClient, envelope: ValidatedEnvelope, now: DateTime<Utc>) {
    transport::publish(&client.client, envelope, now)
        .expect("validated envelope should queue for publication");
    assert_publish_accepted(
        client
            .wait_for_puback()
            .expect("validated publication should be acknowledged"),
    );
}

fn publish_semantic_clear(
    client: &mut TestClient,
    request: ValidatedEnvelope,
    reply: &ValidatedEnvelope,
) {
    transport::publish_inbox_tombstone_after(&client.client, request, reply)
        .expect("validated semantic reply should queue the predecessor tombstone");
    assert_publish_accepted(
        client
            .wait_for_puback()
            .expect("semantic tombstone should be acknowledged"),
    );
}

fn publish_state_clear(client: &mut TestClient, state: ValidatedEnvelope) {
    transport::publish_tombstone(&client.client, state)
        .expect("validated state tombstone should queue");
    assert_publish_accepted(
        client
            .wait_for_puback()
            .expect("state tombstone should be acknowledged"),
    );
}

fn accepted(outcome: Result<ReceiveOutcome, TransportError>) -> ValidatedEnvelope {
    match outcome {
        Ok(ReceiveOutcome::Accepted(envelope)) => *envelope,
        other => panic!("expected accepted delivery, got {other:?}"),
    }
}

#[derive(Debug, PartialEq, Eq)]
struct RepoSnapshot {
    status: Vec<u8>,
    refs: Vec<u8>,
    fetch_head: Option<Vec<u8>>,
    plan: Vec<u8>,
    spec: Vec<u8>,
    remote_url: Vec<u8>,
}

struct GitOracleFixture {
    root: PathBuf,
    remote: PathBuf,
    peer: PathBuf,
    wiki: PathBuf,
    plan: PathBuf,
    spec: PathBuf,
    base_oid: String,
    allowed_oid: String,
    disallowed_oid: String,
    finished: bool,
}

impl GitOracleFixture {
    fn provision() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("test clock should be after the Unix epoch")
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("loam-git-oracle-{}-{nonce:x}", std::process::id()));
        let remote = root.join("origin.git");
        let seed = root.join("seed");
        let peer = root.join("peer");
        let wiki = root.join("wiki");
        fs::create_dir_all(&root).expect("Git oracle fixture root should be created");
        git_at(&root, &["init", "--bare", path_text(&remote)]);
        git_at(&root, &["init", "-b", "main", path_text(&seed)]);
        git_at(&seed, &["config", "user.name", "Loam Test"]);
        git_at(&seed, &["config", "user.email", "loam@example.invalid"]);
        git_at(&seed, &["config", "commit.gpgsign", "false"]);
        fs::write(seed.join("app.rs"), "pub fn base() {}\n")
            .expect("seed source should be written");
        git_at(&seed, &["add", "app.rs"]);
        git_at(&seed, &["commit", "-m", "base"]);
        let base_oid = git_text_at(&seed, &["rev-parse", "HEAD"]);
        git_at(&seed, &["remote", "add", "origin", path_text(&remote)]);
        git_at(&seed, &["push", "-u", "origin", "main"]);
        git_at(
            &root,
            &[
                "clone",
                "--branch",
                "main",
                path_text(&remote),
                path_text(&peer),
            ],
        );
        git_at(&peer, &["config", "user.name", "Loam Peer"]);
        git_at(&peer, &["config", "user.email", "peer@example.invalid"]);
        git_at(&peer, &["config", "commit.gpgsign", "false"]);

        git_at(&seed, &["switch", "-c", "feature/disallowed"]);
        fs::write(seed.join("disallowed.rs"), "pub fn disallowed() {}\n")
            .expect("disallowed source should be written");
        git_at(&seed, &["add", "disallowed.rs"]);
        git_at(&seed, &["commit", "-m", "disallowed"]);
        let disallowed_oid = git_text_at(&seed, &["rev-parse", "HEAD"]);
        git_at(
            &seed,
            &["push", "origin", "HEAD:refs/heads/feature/disallowed"],
        );

        git_at(&seed, &["switch", "main"]);
        fs::write(seed.join("allowed.rs"), "pub fn allowed() {}\n")
            .expect("allowed source should be written");
        git_at(&seed, &["add", "allowed.rs"]);
        git_at(&seed, &["commit", "-m", "allowed"]);
        let allowed_oid = git_text_at(&seed, &["rev-parse", "HEAD"]);
        git_at(&seed, &["push", "origin", "main"]);

        git_at(
            &peer,
            &[
                "remote",
                "add",
                "broken",
                path_text(&root.join("missing.git")),
            ],
        );
        fs::create_dir_all(wiki.join("code")).expect("external codegraph wiki should be created");
        for name in ["SCHEMA.md", "index.md", "log.md"] {
            fs::write(wiki.join(name), format!("# {name}\n"))
                .expect("wiki contract file should be written");
        }
        let plan = root.join("plan.md");
        let spec = root.join("spec.md");
        fs::write(&plan, "plan bytes stay fixed\n").expect("plan snapshot should be written");
        fs::write(&spec, "spec bytes stay fixed\n").expect("spec snapshot should be written");

        Self {
            root,
            remote,
            peer,
            wiki,
            plan,
            spec,
            base_oid,
            allowed_oid,
            disallowed_oid,
            finished: false,
        }
    }

    fn peer(&self) -> &Path {
        &self.peer
    }

    fn wiki(&self) -> &Path {
        &self.wiki
    }

    fn base_oid(&self) -> &str {
        &self.base_oid
    }

    fn allowed_oid(&self) -> &str {
        &self.allowed_oid
    }

    fn disallowed_oid(&self) -> &str {
        &self.disallowed_oid
    }

    fn peer_has_object(&self, oid: &str) -> bool {
        Command::new("git")
            .current_dir(&self.peer)
            .args(["cat-file", "-e", &format!("{oid}^{{commit}}")])
            .output()
            .is_ok_and(|output| output.status.success())
    }

    fn snapshot(&self) -> RepoSnapshot {
        RepoSnapshot {
            status: git_bytes_at(
                &self.peer,
                &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
            ),
            refs: git_bytes_at(
                &self.peer,
                &["for-each-ref", "--format=%(refname)%00%(objectname)"],
            ),
            fetch_head: fs::read(self.peer.join(".git/FETCH_HEAD")).ok(),
            plan: fs::read(&self.plan).expect("plan snapshot should remain readable"),
            spec: fs::read(&self.spec).expect("spec snapshot should remain readable"),
            remote_url: git_bytes_at(&self.peer, &["remote", "get-url", "--all", "origin"]),
        }
    }

    fn make_dirty(&self) {
        fs::write(self.peer.join("dirty-probe"), "uncommitted\n")
            .expect("dirty handoff probe should be written");
    }

    fn clean_dirty_probe(&self) {
        fs::remove_file(self.peer.join("dirty-probe"))
            .expect("dirty handoff probe should be removed");
    }

    fn hide_remote(&self) {
        fs::rename(&self.remote, self.root.join("origin.hidden"))
            .expect("Git oracle remote should be hidden for the freshness probe");
    }

    fn restore_remote(&self) {
        fs::rename(self.root.join("origin.hidden"), &self.remote)
            .expect("Git oracle remote should be restored after the freshness probe");
    }

    fn finish(&mut self) {
        let name = self
            .root
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        assert!(
            self.root.starts_with(std::env::temp_dir()) && name.starts_with("loam-git-oracle-"),
            "refusing to remove unexpected Git oracle fixture root"
        );
        fs::remove_dir_all(&self.root).expect("Git oracle fixture should be removed");
        self.finished = true;
    }
}

impl Drop for GitOracleFixture {
    fn drop(&mut self) {
        if !self.finished {
            eprintln!(
                "Git oracle fixture artifacts preserved for diagnosis: {}",
                self.root.display()
            );
        }
    }
}

fn git_at(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(root)
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(args)
        .output()
        .expect("Git fixture command should start");
    assert!(
        output.status.success(),
        "Git fixture command failed: git {}\nstdout: {}\nstderr: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_text_at(root: &Path, args: &[&str]) -> String {
    String::from_utf8(git_bytes_at(root, args))
        .expect("Git fixture text should be UTF-8")
        .trim()
        .to_owned()
}

fn git_bytes_at(root: &Path, args: &[&str]) -> Vec<u8> {
    let output = Command::new("git")
        .current_dir(root)
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(args)
        .output()
        .expect("Git fixture command should start");
    assert!(
        output.status.success(),
        "Git fixture command failed: git {}\nstderr: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn path_text(path: &Path) -> &str {
    path.to_str().expect("test path should be UTF-8")
}

fn git_scope(broker: &BrokerFixture) -> GitScope {
    GitScope::new(
        broker
            .namespace()
            .strip_prefix("loam/v1/")
            .expect("broker namespace should have a Loam v1 prefix"),
        "project-a",
        "repo-2F8",
    )
    .expect("Git oracle scope should validate")
}

fn test_time(value: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(value)
        .expect("test time should parse")
        .with_timezone(&Utc)
}
