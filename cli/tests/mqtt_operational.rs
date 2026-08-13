//! Two provisioned instances, one broker, one project — the federation running.
//!
//! Every other tier in this slice proves a piece. This one proves the whole
//! thing opens: two `EnrolledRow`s go through the real `provision_session`, the
//! real PEM identity path, the real certificate walk, and the real roster reader,
//! and two live sessions come out and hear each other.
//!
//! The two instances **share one certificate subject** and differ only in their
//! client id. That is not a weaker substitute for two machines — it is exactly
//! the two-machine shape: one person, one email, one common name, one broker
//! principal, two nodes. It is also the sharpest available local check on the
//! client id, because a broker evicts an existing session when a second one
//! connects with the same id, so two sessions that stay live simultaneously
//! could not have been given the same client id.
//!
//! Unix-only for the same reason the harness tier is: the fixtures are
//! device/inode shaped.
#![cfg(unix)]

// The broker fixture is reused; this gate uses part of it and never edits it.
#[allow(dead_code)]
#[path = "support/mqtt_broker.rs"]
mod mqtt_broker;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use chrono::{DateTime, SecondsFormat, Utc};
use loam::connector::{ChannelRegistry, PeerRoster, ProjectSessions, SessionState, SnapshotItem};
use loam::enrollment::registry::{CapabilityRecord, EnrolledRow};
use loam::envelope::{AuthenticatedPrincipal, ValidationConfig};
use mqtt_broker::BrokerFixture;

const PROJECT: &str = "project-a";
const REPOSITORY: &str = "repo-2F8";
/// Both instances authenticate as this common name: the fixture ACL grants it
/// `readwrite` on the project, and one person's two machines share it.
const PRINCIPAL: &str = "mtls-actor";
const INSTANCE_A: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
const INSTANCE_B: &str = "01ARZ3NDEKTSV4RRFFQ69G5FBV";
/// A second principal the fixture ACL also grants, listed in the roster so the
/// origin refusal below is provably about the origin and not the principal.
const OUTSIDER_PRINCIPAL: &str = "actor-a";
const SETTLE: Duration = Duration::from_secs(6);

fn enabled() -> bool {
    if std::env::var("LOAM_MQTT_TEST").as_deref() == Ok("1") {
        return true;
    }
    eprintln!("skipped: set LOAM_MQTT_TEST=1 to require the real broker tier");
    false
}

// ---------------------------------------------------------------------------
// The two-instance fixture
// ---------------------------------------------------------------------------

struct Federation {
    broker: BrokerFixture,
    root: PathBuf,
    org: String,
}

impl Federation {
    fn provision(label: &str) -> Self {
        let broker =
            BrokerFixture::provision(label).expect("the local broker fixture should provision");
        let org = broker
            .namespace()
            .strip_prefix("loam/v1/")
            .expect("the fixture namespace carries the loam/v1 prefix")
            .to_owned();
        let root = std::env::temp_dir().join(format!(
            "loam-operational-{label}-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        std::fs::create_dir_all(&root).expect("fixture root is creatable");

        let fixture = Federation { broker, root, org };
        fixture.write_identity();
        fixture.write_roster(&[INSTANCE_A, INSTANCE_B]);
        fixture.take_over_environment();
        fixture
    }

    /// One identity directory per instance: `client.pem` + `key.pem`, exactly
    /// the shape the resolver's contract describes. Instance B's certificate
    /// carries a given name and A's does not, which is the display-name
    /// control's other half.
    fn write_identity(&self) {
        for (instance, certificate, key) in [
            (
                INSTANCE_A,
                self.broker
                    .client_certificate()
                    .expect("the fixture client certificate is readable"),
                self.broker
                    .client_key()
                    .expect("the fixture client key is readable"),
            ),
            (
                INSTANCE_B,
                self.broker
                    .named_client_certificate()
                    .expect("the named client certificate is readable"),
                self.broker
                    .named_client_key()
                    .expect("the named client key is readable"),
            ),
        ] {
            let directory = self.root.join("identity").join(instance);
            std::fs::create_dir_all(&directory).expect("identity directory is creatable");
            std::fs::write(directory.join("client.pem"), certificate)
                .expect("client certificate is writable");
            std::fs::write(directory.join("key.pem"), key).expect("client key is writable");
        }
    }

    /// One roster serves both instances: they share a principal, and listing
    /// both origins lets each admit the other. An instance's own origin is
    /// seeded by the session itself, so listing it changes nothing.
    fn write_roster(&self, origins: &[&str]) {
        let directory = self.root.join("rosters").join(&self.org);
        std::fs::create_dir_all(&directory).expect("roster directory is creatable");
        let origins = origins
            .iter()
            .map(|origin| format!("\"{origin}\""))
            .collect::<Vec<_>>()
            .join(",");
        std::fs::write(
            directory.join(format!("{PROJECT}.json")),
            format!("{{\"principals\":[\"{PRINCIPAL}\",\"{OUTSIDER_PRINCIPAL}\"],\"origins\":[{origins}]}}"),
        )
        .expect("roster is writable");
    }

    /// Point the resolver at this fixture, and neutralize the machine's Git
    /// identity.
    ///
    /// The certificate common name here is `mtls-actor`, because that is what
    /// the broker ACL grants — it is a fixture name, not an operator's email.
    /// The identity match is a real gate (a local email disagreeing with the
    /// certificate is a typed refusal, covered in the resolver's own tests), so
    /// rather than pretend, this tier removes the machine's Git identity from
    /// the picture entirely.
    fn take_over_environment(&self) {
        std::env::set_var("LOAM_FEDERATION_ROSTER_DIR", self.root.join("rosters"));
        std::env::set_var("GIT_CONFIG_GLOBAL", "/dev/null");
        std::env::set_var("GIT_CONFIG_SYSTEM", "/dev/null");
    }

    /// Point the resolver's identity path at one instance's PEM bundle. The
    /// env var is read once per `resolve()` call, so two live sessions can each
    /// resolve their own identity by switching before each attach.
    fn use_identity(&self, instance: &str) {
        std::env::set_var(
            "LOAM_FEDERATION_IDENTITY_DIR",
            self.root.join("identity").join(instance),
        );
    }

    /// An enrolled row for one instance. Only the instance id differs between
    /// the two — everything else, including the broker and the project, is
    /// shared.
    fn row(&self, instance: &str) -> EnrolledRow {
        EnrolledRow {
            identity_key: format!("unix:1:{instance}"),
            org_id: self.org.clone(),
            project_id: PROJECT.to_owned(),
            repository_id: REPOSITORY.to_owned(),
            descriptor_digest: "d".into(),
            display_path: self.root.to_string_lossy().into_owned(),
            instance_id: instance.to_owned(),
            broker_profile: "fixture".into(),
            broker_endpoint: format!("mqtts://localhost:{}", self.broker.mtls_port()),
            tls_server_name: "localhost".into(),
            ca_ref: None,
            commit: "84be000000000000000000000000000000000001".into(),
            capabilities: CapabilityRecord {
                authentication: true,
                publish: true,
                subscribe: true,
                self_receive: true,
                verified_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
            },
            remotes: Vec::new(),
        }
    }

    fn project_base(&self) -> String {
        format!("loam/v1/{}/{PROJECT}", self.org)
    }

    /// The fixture CA, so a resolved session can be pointed at it.
    fn trust_bundle(&self) -> PathBuf {
        let path = self.root.join("ca.pem");
        std::fs::write(
            &path,
            self.broker
                .ca_certificate()
                .expect("the fixture CA is readable"),
        )
        .expect("trust bundle is writable");
        path
    }

    fn scoped(&self, document: &str, now: DateTime<Utc>) -> String {
        let issued = now.to_rfc3339_opts(SecondsFormat::Secs, true);
        let expires =
            (now + chrono::Duration::minutes(30)).to_rfc3339_opts(SecondsFormat::Secs, true);
        document
            .replace("org-3A1", &self.org)
            .replace("project-7M3", PROJECT)
            .replace("repo-2F8", REPOSITORY)
            .replace(
                "\"time\": \"2026-07-24T14:20:00Z\"",
                &format!("\"time\": \"{issued}\""),
            )
            .replace(
                "\"expires_at\": \"2026-07-25T14:20:00Z\"",
                &format!("\"expires_at\": \"{expires}\""),
            )
            .replace(
                "\"expires_at\": \"2026-07-24T14:50:00Z\"",
                &format!("\"expires_at\": \"{expires}\""),
            )
    }

    /// A work-state frame published by `origin` as `principal`.
    fn work_state(
        &self,
        principal: &str,
        origin: &str,
        key: &str,
        state: &str,
        now: DateTime<Utc>,
    ) -> (String, String) {
        let topic = format!("{}/state/{origin}/{key}", self.project_base());
        let document = self
            .scoped(include_str!("fixtures/mqtt/work-state.json"), now)
            .replace(
                "urn:loam:instance:instance-01",
                &format!("urn:loam:instance:{origin}"),
            )
            .replace(
                "\"principal_id\": \"employee-184\"",
                &format!("\"principal_id\": \"{principal}\""),
            )
            .replace(
                "\"instance_id\": \"instance-01\"",
                &format!("\"instance_id\": \"{origin}\""),
            )
            .replace(
                "01K6Q6ESWMT48TPB",
                &format!("01K6Q6ESWMT4{}", suffix(origin)),
            )
            .replace(
                "\"key\": \"activity-01K6Q5\"",
                &format!("\"key\": \"{key}\""),
            )
            .replace("\"state\": \"ready\"", &format!("\"state\": \"{state}\""));
        (topic, document)
    }

    /// A typed-inbox message from `origin` to `recipient`'s instance inbox.
    fn message(
        &self,
        principal: &str,
        origin: &str,
        recipient: &str,
        summary: &str,
        now: DateTime<Utc>,
    ) -> (String, String) {
        let event = format!("01K6Q6ESWMT4{}", suffix(origin));
        let topic = format!(
            "{}/inbox/instance/{recipient}/{origin}/{event}",
            self.project_base()
        );
        let document = self
            .scoped(include_str!("fixtures/mqtt/message.json"), now)
            .replace(
                "urn:loam:instance:instance-01",
                &format!("urn:loam:instance:{origin}"),
            )
            .replace(
                "\"principal_id\": \"employee-184\"",
                &format!("\"principal_id\": \"{principal}\""),
            )
            .replace(
                "\"instance_id\": \"instance-01\"",
                &format!("\"instance_id\": \"{origin}\""),
            )
            .replace("01K6Q6ESWMT48TPC", &event)
            .replace(
                "{\"kind\": \"agent\", \"id\": \"agent-91\"}",
                &format!("{{\"kind\": \"instance\", \"id\": \"{recipient}\"}}"),
            )
            .replace(
                "\"summary\": \"Review the anchored change.\"",
                &format!("\"summary\": \"{summary}\""),
            );
        (topic, document)
    }
}

/// A short, envelope-legal suffix derived from an instance id.
fn suffix(instance: &str) -> String {
    let tail: String = instance
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .rev()
        .take(4)
        .collect::<String>()
        .to_uppercase();
    format!("{tail:X>4}")
}

/// Publish one document through a provisioned session — the connector's own
/// publish path, including the envelope gate it applies before shipping.
fn ship(
    sessions: &ProjectSessions,
    project: &str,
    document: &str,
    topic: &str,
) -> loam::connector::EmitOutcome {
    let identity = sessions
        .identity(project)
        .expect("a live session has an authenticated identity");
    let claims: Vec<&str> = identity.allowed_claims.iter().map(String::as_str).collect();
    let principal = AuthenticatedPrincipal::new(&identity.principal_id, &claims);
    let validated = loam::envelope::validate(
        document.as_bytes(),
        topic,
        &principal,
        &ValidationConfig::default(),
        Utc::now(),
    )
    .expect("the connector's own gate accepts its own envelope");
    sessions.ship(project, validated)
}

/// Wait for a snapshot to carry `count` items matching `predicate`, or give up.
fn wait_for(
    sessions: &ProjectSessions,
    project: &str,
    count: usize,
    predicate: impl Fn(&SnapshotItem) -> bool,
) -> Vec<SnapshotItem> {
    let deadline = Instant::now() + SETTLE;
    loop {
        let items: Vec<SnapshotItem> = sessions
            .snapshot(project, Utc::now())
            .into_iter()
            .filter(&predicate)
            .collect();
        if items.len() >= count || Instant::now() >= deadline {
            return items;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

// ---------------------------------------------------------------------------
// The proofs
// ---------------------------------------------------------------------------

#[test]
#[ignore = "requires LOAM_MQTT_TEST=1 and a real Mosquitto/OpenSSL installation"]
fn two_provisioned_instances_hear_each_other_in_both_directions() {
    if !enabled() {
        return;
    }
    let fixture = Federation::provision("two-instance");
    std::env::set_var("SSL_CERT_FILE", fixture.trust_bundle());

    let row_a = fixture.row(INSTANCE_A);
    let row_b = fixture.row(INSTANCE_B);

    // Both sessions come out of the real seam: the real PEM identity path, the
    // real certificate walk, the real roster file. Nothing is hand-built.
    let mut a = ProjectSessions::new(16, ChannelRegistry::new());
    let mut b = ProjectSessions::new(16, ChannelRegistry::new());
    let now = Utc::now();
    fixture.use_identity(INSTANCE_A);
    assert_eq!(
        a.attach(&row_a, loam::connector::provision_session(&row_a), now),
        SessionState::Live,
        "instance A must open a live session from its enrollment alone"
    );
    fixture.use_identity(INSTANCE_B);
    assert_eq!(
        b.attach(&row_b, loam::connector::provision_session(&row_b), now),
        SessionState::Live,
        "instance B must open a live session from its enrollment alone"
    );

    // Both remain live at once. A broker evicts an existing session when a
    // second connects with the same client id, so two simultaneous sessions
    // could not have been given the same one — this is the local half of the
    // client-id proof that a same-machine ACL cannot show.
    assert!(a.is_live(PROJECT) && b.is_live(PROJECT));

    // The identity each session reports is the one the enrollment named, and
    // the given name is the one in that instance's certificate.
    let identity_a = a.identity(PROJECT).expect("A has an identity");
    let identity_b = b.identity(PROJECT).expect("B has an identity");
    assert_eq!(identity_a.instance_id, INSTANCE_A);
    assert_eq!(identity_b.instance_id, INSTANCE_B);
    assert_eq!(identity_a.principal_id, PRINCIPAL);
    assert_eq!(identity_b.principal_id, PRINCIPAL);
    assert!(
        identity_a.display_name.is_none(),
        "A's certificate carries no given name, so its display name stays absent"
    );
    assert_eq!(
        identity_b.display_name.as_deref(),
        Some("Ada Lovelace"),
        "B's certificate carries a given name, so its display name is read from it"
    );

    // --- A to B -------------------------------------------------------------
    let (state_topic, state) = fixture.work_state(
        PRINCIPAL,
        INSTANCE_A,
        "activity-from-a",
        "ready",
        Utc::now(),
    );
    ship(&a, PROJECT, &state, &state_topic);
    let (inbox_topic, message) = fixture.message(
        PRINCIPAL,
        INSTANCE_A,
        INSTANCE_B,
        "From A to B.",
        Utc::now(),
    );
    ship(&a, PROJECT, &message, &inbox_topic);

    let heard = wait_for(&b, PROJECT, 2, |item| item.from_instance_id == INSTANCE_A);
    assert_eq!(
        heard.len(),
        2,
        "B must hear exactly A's state frame and A's message: {heard:?}"
    );
    assert!(
        heard.iter().all(|item| item.from_principal_id == PRINCIPAL),
        "every heard item must be attributed to its sender: {heard:?}"
    );
    assert!(
        heard
            .iter()
            .any(|item| item.summary.contains("From A to B.")),
        "{heard:?}"
    );

    // --- B to A, symmetrically ---------------------------------------------
    let (state_topic, state) = fixture.work_state(
        PRINCIPAL,
        INSTANCE_B,
        "activity-from-b",
        "ready",
        Utc::now(),
    );
    ship(&b, PROJECT, &state, &state_topic);
    let (inbox_topic, message) = fixture.message(
        PRINCIPAL,
        INSTANCE_B,
        INSTANCE_A,
        "From B to A.",
        Utc::now(),
    );
    ship(&b, PROJECT, &message, &inbox_topic);

    let heard = wait_for(&a, PROJECT, 2, |item| item.from_instance_id == INSTANCE_B);
    assert_eq!(
        heard.len(),
        2,
        "A must hear exactly B's state frame and B's message: {heard:?}"
    );
    assert!(
        heard
            .iter()
            .any(|item| item.summary.contains("From B to A.")),
        "{heard:?}"
    );

    a.detach(PROJECT);
    b.detach(PROJECT);
    // Teardown stops the broker and drops the roots; nothing takes a lock the
    // pump threads hold.
    fixture
        .broker
        .finish()
        .expect("broker fixture should remove only its temporary directory");
    let _ = std::fs::remove_dir_all(&fixture.root);
}

#[test]
#[ignore = "requires LOAM_MQTT_TEST=1 and a real Mosquitto/OpenSSL installation"]
fn an_origin_absent_from_the_roster_is_not_heard_and_the_same_origin_added_is() {
    if !enabled() {
        return;
    }
    // The roster refusal is an authorization decision, not a deaf client. The
    // only way to tell those apart is to change the roster and nothing else.
    let fixture = Federation::provision("roster-authorization");
    std::env::set_var("SSL_CERT_FILE", fixture.trust_bundle());

    // B's roster lists only itself, so A's origin is not admitted.
    fixture.write_roster(&[INSTANCE_B]);
    let row_b = fixture.row(INSTANCE_B);
    let mut b = ProjectSessions::new(16, ChannelRegistry::new());
    fixture.use_identity(INSTANCE_B);
    assert_eq!(
        b.attach(
            &row_b,
            loam::connector::provision_session(&row_b),
            Utc::now()
        ),
        SessionState::Live
    );

    let row_a = fixture.row(INSTANCE_A);
    let mut a = ProjectSessions::new(16, ChannelRegistry::new());
    fixture.use_identity(INSTANCE_A);
    assert_eq!(
        a.attach(
            &row_a,
            loam::connector::provision_session(&row_a),
            Utc::now()
        ),
        SessionState::Live
    );

    let (topic, document) = fixture.work_state(
        PRINCIPAL,
        INSTANCE_A,
        "activity-unlisted",
        "ready",
        Utc::now(),
    );
    ship(&a, PROJECT, &document, &topic);
    let unheard = wait_for(&b, PROJECT, 1, |item| item.from_instance_id == INSTANCE_A);
    assert!(
        unheard.is_empty(),
        "an origin absent from the roster must not be heard: {unheard:?}"
    );

    // The positive control, changing only the roster: B re-attaches with A's
    // origin listed and the same frame republished is heard. Without this the
    // absence above would be indistinguishable from a broken subscriber.
    b.detach(PROJECT);
    fixture.write_roster(&[INSTANCE_A, INSTANCE_B]);
    let mut b = ProjectSessions::new(16, ChannelRegistry::new());
    fixture.use_identity(INSTANCE_B);
    assert_eq!(
        b.attach(
            &row_b,
            loam::connector::provision_session(&row_b),
            Utc::now()
        ),
        SessionState::Live
    );
    let (topic, document) = fixture.work_state(
        PRINCIPAL,
        INSTANCE_A,
        "activity-listed",
        "ready",
        Utc::now(),
    );
    ship(&a, PROJECT, &document, &topic);
    let heard = wait_for(&b, PROJECT, 1, |item| item.from_instance_id == INSTANCE_A);
    assert_eq!(
        heard.len(),
        1,
        "the same origin, once listed, must be heard: {heard:?}"
    );

    a.detach(PROJECT);
    b.detach(PROJECT);
    fixture
        .broker
        .finish()
        .expect("broker fixture should remove only its temporary directory");
    let _ = std::fs::remove_dir_all(&fixture.root);
}

#[test]
#[ignore = "requires LOAM_MQTT_TEST=1 and a real Mosquitto/OpenSSL installation"]
fn an_unusable_roster_is_refused_and_a_thin_roster_self_admits() {
    if !enabled() {
        return;
    }
    // The refusals, driven through the same real seam as the successes above,
    // so the reason an operator sees is the reason this code produces.
    let fixture = Federation::provision("roster-refusals");
    std::env::set_var("SSL_CERT_FILE", fixture.trust_bundle());
    let row = fixture.row(INSTANCE_A);
    fixture.use_identity(INSTANCE_A);
    let directory = fixture.root.join("rosters").join(&fixture.org);
    let path = directory.join(format!("{PROJECT}.json"));

    // Self-announce re-scopes the gate: an enrolled machine always admits
    // itself, so an absent/empty/one-sided roster is the ordinary first-join
    // state — a self-only Live session — not a refusal.
    for body in [
        format!("{{\"principals\":[\"{PRINCIPAL}\"],\"origins\":[]}}"),
        format!("{{\"principals\":[],\"origins\":[\"{INSTANCE_B}\"]}}"),
        "{\"principals\":[],\"origins\":[]}".to_owned(),
    ] {
        std::fs::write(&path, &body).expect("roster is writable");
        let mut sessions = ProjectSessions::new(4, ChannelRegistry::new());
        assert_eq!(
            sessions.attach(&row, loam::connector::provision_session(&row), Utc::now()),
            SessionState::Live,
            "{body}"
        );
        sessions.detach(PROJECT);
    }

    // The refusal survives only for genuinely unusable roster data.
    for (body, expected) in [
        (
            format!("{{\"principals\":[\"*\"],\"origins\":[\"{INSTANCE_B}\"]}}"),
            "roster-wildcard",
        ),
        ("{not json".to_owned(), "roster-malformed"),
    ] {
        std::fs::write(&path, &body).expect("roster is writable");
        let mut sessions = ProjectSessions::new(4, ChannelRegistry::new());
        let state = sessions.attach(&row, loam::connector::provision_session(&row), Utc::now());
        assert_eq!(state.code(), "no-peer-roster", "{body}");
        assert_eq!(state.reason(), Some(expected), "{body}");
        assert!(!sessions.is_live(PROJECT), "{body}");
    }

    // The positive control in the same run: a usable roster does open, so every
    // refusal above is the roster and not the broker.
    fixture.write_roster(&[INSTANCE_B]);
    let mut sessions = ProjectSessions::new(4, ChannelRegistry::new());
    assert_eq!(
        sessions.attach(&row, loam::connector::provision_session(&row), Utc::now()),
        SessionState::Live
    );
    sessions.detach(PROJECT);

    // An identity path with nothing behind it refuses on the other state,
    // naming the input rather than the roster.
    let mut absent = fixture.row(INSTANCE_A);
    absent.instance_id = INSTANCE_A.to_owned();
    std::env::set_var(
        "LOAM_FEDERATION_IDENTITY_DIR",
        fixture.root.join("identity").join("no-such-instance"),
    );
    let mut sessions = ProjectSessions::new(4, ChannelRegistry::new());
    let state = sessions.attach(
        &absent,
        loam::connector::provision_session(&absent),
        Utc::now(),
    );
    assert_eq!(state.code(), "credentials-unresolved");
    assert_eq!(state.reason(), Some("identity-required"));

    fixture
        .broker
        .finish()
        .expect("broker fixture should remove only its temporary directory");
    let _ = std::fs::remove_dir_all(&fixture.root);
}

/// The roster the fixture writes is the one the resolver reads — a mismatch
/// between writer and reader would be silent, so it is asserted directly.
#[test]
#[ignore = "requires LOAM_MQTT_TEST=1 and a real Mosquitto/OpenSSL installation"]
fn the_written_roster_is_the_one_the_resolver_reads() {
    if !enabled() {
        return;
    }
    let fixture = Federation::provision("roster-path");
    let root = loam::provisioning::configured_roster_root().expect("a roster root resolves");
    assert_eq!(root, fixture.root.join("rosters"));
    let roster = loam::provisioning::read_roster(&root, &fixture.org, PROJECT)
        .expect("the fixture roster is admitted");
    assert_eq!(
        roster,
        PeerRoster {
            principals: vec![PRINCIPAL.to_owned(), OUTSIDER_PRINCIPAL.to_owned()],
            origins: vec![INSTANCE_A.to_owned(), INSTANCE_B.to_owned()],
        }
    );
    fixture
        .broker
        .finish()
        .expect("broker fixture should remove only its temporary directory");
    let _ = std::fs::remove_dir_all(&fixture.root);
}
