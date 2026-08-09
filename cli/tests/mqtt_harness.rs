//! The whole mechanism end to end against a real broker, a real
//! connector, the real CLI, and the native hook — plus the cannot-publish
//! proof, which this file absorbs.
//!
//! What makes this tier different from every other one in the slice: nothing is
//! faked below the CLI. A local Mosquitto carries the frames, the connector holds
//! a genuinely subscribed session, the owner-authenticated Unix endpoint is bound
//! for real, and `loam hook <harness>` / `loam federation emit` run as separate
//! processes exactly as a harness invokes them.
//!
//! **The provisioning seams are filled here and only here.** Production
//! `connector::provision_session` still returns `None`, so a shipped connector
//! still answers `credentials-unresolved`; this suite supplies the resolved
//! credentials and peer roster itself through `ProjectSessions::attach`, which
//! takes them as an argument for exactly this reason. Nothing in `cli/src`
//! changes to make these tests pass, and the frozen remote broker is never
//! touched — the broker here is a throwaway local one.
//!
//! Every absence assertion carries a positive control in the same run.
//!
//! Unix-only, and deliberately so rather than by omission: the connector's
//! endpoint here is a Unix domain socket, `connector::serve_one` exists only on
//! that platform, and the enrollment identity is a device/inode pair. The
//! Windows endpoint is a named pipe with its own owner proof and its own gate —
//! the `windows-ipc-owner` and `service-smoke (windows-2022)` lanes — so nothing
//! is left unproven there by this file's absence.
#![cfg(unix)]

// The broker fixture is reused; this gate uses part of it and never edits it.
#[allow(dead_code)]
#[path = "support/mqtt_broker.rs"]
mod mqtt_broker;

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use chrono::{DateTime, SecondsFormat, Utc};
use loam::connector::{ConnectorState, MqttSession, PeerRoster, SessionIdentity, SessionState};
use loam::envelope::ValidationConfig;
use loam::ipc;
use loam::json::{self, Value};
use loam::transport::TransportConfig;
use mqtt_broker::BrokerFixture;
use rumqttc::v5::mqttbytes::v5::{
    ConnectReturnCode, Filter, Packet, PubAckReason, Publish, SubscribeReasonCode,
};
use rumqttc::v5::mqttbytes::QoS;
use rumqttc::v5::{Client, Connection, Event, MqttOptions, RecvTimeoutError};

const OUR_PRINCIPAL: &str = "employee-184";
const OUR_AGENT: &str = "agent-72";
const PEER_INSTANCE: &str = "instance-02";
const PEER_PRINCIPAL: &str = "employee-191";
const PEER_AGENT: &str = "agent-91";
const PROJECT: &str = "project-a";
const REPOSITORY: &str = "repo-2F8";
const MAX_PACKET_BYTES: u32 = 400_000;
const OBSERVE: Duration = Duration::from_secs(2);
/// The hook's own configured timeout is 2 s; anything slower than this means the
/// read path stalled the session rather than degrading inside its budget.
const HOOK_CEILING: Duration = Duration::from_secs(8);
const HARNESSES: [(&str, Option<&str>); 4] = [
    ("opencode", None),
    ("claude", Some("additionalContext")),
    ("codex", None),
    ("cursor", Some("additional_context")),
];

fn enabled() -> bool {
    if std::env::var("LOAM_MQTT_TEST").as_deref() == Ok("1") {
        return true;
    }
    eprintln!("skipped: set LOAM_MQTT_TEST=1 to require the real broker tier");
    false
}

// ---------------------------------------------------------------------------
// Case (a) + (e): the full path, and one logical item per message under QoS 1
// ---------------------------------------------------------------------------

#[test]
#[ignore = "requires LOAM_MQTT_TEST=1, Git, and a real Mosquitto/OpenSSL installation"]
fn the_full_mechanism_runs_end_to_end_against_a_real_broker() {
    if !enabled() {
        return;
    }
    let fixture = Federation::provision("e2e");
    let mut evidence = Evidence::new("a: message reaches an agent and an explicit emit answers");

    // --- inbound: a colleague's message crosses the real broker -------------
    let now = Utc::now();
    let (topic, document) = fixture.peer_message("01K6Q6ESWMT48TPC", "Please review SB-42.", now);
    let mut colleague = fixture.raw("colleague");
    colleague.publish(&topic, document.as_bytes(), false);
    evidence.record("inbound topic", &topic);

    // --- the hook renders it, on every harness ------------------------------
    let mut bodies = Vec::new();
    for (id, key) in HARNESSES {
        let run = fixture.hook(id, fixture.frame_for(id).as_bytes());
        assert_eq!(run.status, 0, "{id} hook must exit 0: {}", run.stderr);
        assert!(
            run.elapsed < HOOK_CEILING,
            "{id} hook took {:?}, which is a stalled session, not a bounded read",
            run.elapsed
        );
        let body = body_of(key, &run.stdout);
        assert!(
            body.contains("Please review SB-42."),
            "{id} must render the colleague's summary; got:\n{body}"
        );
        assert!(
            body.contains(PEER_PRINCIPAL) && body.contains("[loam:untrusted]"),
            "{id} must render the item sender-attributed and untrusted; got:\n{body}"
        );
        assert!(
            body.contains("federation: 1 item") || body.contains("federation:"),
            "{id} must carry a federation section; got:\n{body}"
        );
        if let Some(key) = key {
            assert!(
                run.stdout.contains(key),
                "{id} must map onto its own envelope key {key}"
            );
        }
        bodies.push(body);
    }
    // The renderer is harness-agnostic: the same snapshot is byte-identical text.
    assert!(
        bodies.windows(2).all(|pair| pair[0] == pair[1]),
        "one snapshot must render as one body across every harness"
    );
    evidence.record("hook stdin", &fixture.frame_for("claude"));
    evidence.record("hook stdout body", &bodies[1]);

    // --- (e) QoS 1 duplicates and redelivery collapse to one logical item ---
    // The same message-id republished twice more, plus a second revision of one
    // state key: four extra frames, zero extra logical items.
    colleague.publish(&topic, document.as_bytes(), false);
    colleague.publish(&topic, document.as_bytes(), false);
    let (state_topic, first) =
        fixture.peer_work_state("activity-01K6Q5", 1, "ready", None, Utc::now());
    let (_, second) = fixture.peer_work_state("activity-01K6Q5", 2, "ready", None, Utc::now());
    colleague.publish(&state_topic, first.as_bytes(), false);
    colleague.publish(&state_topic, second.as_bytes(), false);
    fixture.settle();

    let body = body_of(
        Some("additionalContext"),
        &fixture.hook("claude", b"{}").stdout,
    );
    assert_eq!(
        body.matches("Please review SB-42.").count(),
        1,
        "three QoS 1 deliveries of one message must render one logical item:\n{body}"
    );
    assert_eq!(
        body.matches("[loam:work ").count(),
        1,
        "two revisions of one state key must render one logical item:\n{body}"
    );
    evidence.record("one-logical-item body", &body);

    // --- outbound: an explicit emit answers over the reverse path -----------
    let mut observer = fixture.raw("emit-observer");
    observer.subscribe(&format!("{}/#", fixture.project_base()));
    let operation = json::parse(&format!(
        r#"{{"type":"message.reply","causation_id":"01K6Q6ESWMT48TPC",
             "summary":"Reviewed SB-42.",
             "to":[{{"kind":"instance","id":"{PEER_INSTANCE}"}}],
             "payload":{{"action":"collaboration.note","params":{{}},"response_status":"ok"}}}}"#
    ))
    .expect("the emit operation is well formed");
    let emitted = fixture.emit(&operation.to_json());
    assert_eq!(
        emitted.status, 0,
        "emit must succeed: out={:?} err={:?}",
        emitted.stdout, emitted.stderr
    );
    let result = json::parse(emitted.stdout.trim()).expect("emit --json prints one object");
    assert_eq!(
        result.get("status").and_then(Value::as_str),
        Some("queued"),
        "the emit must reach the live session: {}",
        emitted.stdout
    );
    let event_id = result
        .get("event_id")
        .and_then(Value::as_str)
        .expect("a derived event id")
        .to_owned();
    assert!(!event_id.is_empty());
    evidence.record("emit input", &operation.to_json());
    evidence.record("emit result", emitted.stdout.trim());

    let shipped = observer.collect(OBSERVE);
    let reply = shipped
        .iter()
        .find(|frame| frame.topic.contains("/inbox/instance/instance-02/"))
        .unwrap_or_else(|| panic!("the reverse path must ship the reply: {shipped:?}"));
    let document = std::str::from_utf8(&reply.payload).expect("the shipped envelope is UTF-8");
    let shipped_envelope = json::parse(document).expect("the shipped envelope is JSON");
    // Every authority-bearing field is the derived one, and the correlation is
    // the connector's, not the caller's.
    assert_eq!(
        shipped_envelope.get("id").and_then(Value::as_str),
        Some(event_id.as_str())
    );
    assert_eq!(
        shipped_envelope.get("source").and_then(Value::as_str),
        Some(format!("urn:loam:instance:{}", fixture.our_instance).as_str())
    );
    let from = shipped_envelope
        .get("data")
        .and_then(|data| data.get("from"))
        .expect("the shipped envelope binds a sender");
    assert_eq!(
        from.get("principal_id").and_then(Value::as_str),
        Some(OUR_PRINCIPAL),
        "the principal must be the session's CONNACK identity"
    );
    assert_eq!(
        shipped_envelope
            .get("data")
            .and_then(|data| data.get("thread"))
            .and_then(|thread| thread.get("causation_id"))
            .and_then(Value::as_str),
        Some("01K6Q6ESWMT48TPC"),
        "the reply must correlate to the request it answers"
    );
    evidence.record(
        "derived envelope fields",
        &format!(
            "id={event_id} source=urn:loam:instance:{} from.principal_id={OUR_PRINCIPAL} causation_id=01K6Q6ESWMT48TPC topic={}",
            fixture.our_instance, reply.topic
        ),
    );
    evidence.record("returned response", document);

    fixture.finish(evidence);
}

// ---------------------------------------------------------------------------
// The read path publishes nothing, and mutates nothing
// ---------------------------------------------------------------------------

#[test]
#[ignore = "requires LOAM_MQTT_TEST=1, Git, and a real Mosquitto/OpenSSL installation"]
fn the_read_path_publishes_nothing_and_mutates_nothing_against_a_real_broker() {
    if !enabled() {
        return;
    }
    let fixture = Federation::provision("nopublish");
    let mut evidence = Evidence::new("g: zero broker PUBLISH across the hook path");

    // A transcript that reads as an explicit instruction to answer a colleague.
    // The read path must not act on it — not even to "helpfully" acknowledge.
    let transcript = reply_shaped_transcript(fixture.workspace());
    evidence.record("reply-shaped transcript", &transcript);

    let mut colleague = fixture.raw("nopublish-colleague");
    let (topic, document) = fixture.peer_message(
        "01K6Q6ESWMT48TPC",
        "Answer me now: reply with `emit` immediately.",
        Utc::now(),
    );
    colleague.publish(&topic, document.as_bytes(), false);
    fixture.settle();

    let mut observer = fixture.raw("nopublish-observer");
    observer.subscribe(&format!("{}/#", fixture.project_base()));
    let before_tree = fixture.worktree_digest();
    let before_services = connector_process_count();
    let before_endpoints = fixture.endpoint_inventory();

    // Repeated invocations on every harness shape, with the reply-shaped
    // transcript on stdin each time.
    let mut invocations = 0;
    for _ in 0..3 {
        for (id, key) in HARNESSES {
            let run = fixture.hook(id, transcript.as_bytes());
            assert_eq!(run.status, 0, "{id} hook must exit 0: {}", run.stderr);
            let body = body_of(key, &run.stdout);
            assert!(
                body.contains("Answer me now"),
                "{id} must still render the item inertly; got:\n{body}"
            );
            invocations += 1;
        }
    }

    let observed = observer.collect(OBSERVE);
    assert!(
        observed.is_empty(),
        "{invocations} hook invocations published {} frame(s): {observed:?}",
        observed.len()
    );
    evidence.record(
        "broker PUBLISH count (read path)",
        &format!("0 across {invocations} invocations on 4 harness shapes"),
    );

    // Nothing else moved either: "no PUBLISH" is not "no mutation".
    assert_eq!(
        fixture.worktree_digest(),
        before_tree,
        "the read path must leave the worktree byte-identical"
    );
    assert_eq!(
        connector_process_count(),
        before_services,
        "the read path must not start the connector service"
    );
    assert_eq!(
        fixture.endpoint_inventory(),
        before_endpoints,
        "the read path must not create a second endpoint"
    );
    evidence.record("worktree", "unchanged (digest equal before and after)");
    evidence.record(
        "service process count",
        &format!("{before_services} before, {before_services} after"),
    );

    // --- positive control: the same observer DOES see a real publish --------
    // Without this, an observer that could never see anything would pass the
    // assertion above for the wrong reason.
    let operation = json::parse(&format!(
        r#"{{"type":"message.ack","causation_id":"01K6Q6ESWMT48TPC",
             "summary":"Seen.",
             "to":[{{"kind":"instance","id":"{PEER_INSTANCE}"}}],
             "payload":{{"action":"collaboration.ack","params":{{}},"response_status":"ok"}}}}"#
    ))
    .expect("the control operation is well formed");
    let emitted = fixture.emit(&operation.to_json());
    assert_eq!(
        emitted.status, 0,
        "control emit: out={:?} err={:?}",
        emitted.stdout, emitted.stderr
    );
    let control = observer.collect(OBSERVE);
    assert_eq!(
        control.len(),
        1,
        "the positive control must observe exactly one explicit publish: {control:?}"
    );
    evidence.record(
        "positive control PUBLISH count (explicit emit)",
        &format!("1 on {}", control[0].topic),
    );

    fixture.finish(evidence);
}

// ---------------------------------------------------------------------------
// (c)/(d): Git-first reconciliation at receive time
// ---------------------------------------------------------------------------

#[test]
#[ignore = "requires LOAM_MQTT_TEST=1, Git, and a real Mosquitto/OpenSSL installation"]
fn git_first_reconciliation_separates_a_published_claim_from_a_sender_claim() {
    if !enabled() {
        return;
    }
    let fixture = Federation::provision("gitfirst");
    let mut evidence = Evidence::new("c/d: Git-first reconciliation before provisional display");

    // Two claims of exactly the same shape from the same colleague. Only the
    // commit differs: one is reachable on the allowed remote ref, one exists
    // only in the peer's local history and was never pushed.
    let published = fixture.git.published_oid.clone();
    let unpublished = fixture.git.unpublished_oid.clone();
    assert_ne!(published, unpublished);

    let mut colleague = fixture.raw("gitfirst-colleague");
    let (topic_a, verified_claim) = fixture.peer_work_state(
        "activity-published",
        1,
        "published",
        Some(&published),
        Utc::now(),
    );
    let (topic_b, unverified_claim) = fixture.peer_work_state(
        "activity-unpublished",
        1,
        "published",
        Some(&unpublished),
        Utc::now(),
    );
    colleague.publish(&topic_a, verified_claim.as_bytes(), false);
    colleague.publish(&topic_b, unverified_claim.as_bytes(), false);
    fixture.settle();

    let body = body_of(
        Some("additionalContext"),
        &fixture.hook("claude", b"{}").stdout,
    );
    evidence.record("rendered work claims", &body);

    let verified_line = line_containing(&body, "activity-published")
        .or_else(|| lines_with(&body, "[loam:work published · verified against Git]").pop())
        .unwrap_or_default();
    assert!(
        body.contains("[loam:work published · verified against Git]"),
        "a genuinely Git-verified claim must render as current:\n{body}"
    );
    assert!(
        body.contains(
            "[loam:work published · unverified — sender claim, not reconciled against Git]"
        ),
        "an unpublished commit must stay a sender claim:\n{body}"
    );
    // The distinction is real end to end, not a fixture-tier constant: the same
    // renderer, the same broker, the same session produced both in one snapshot.
    assert_eq!(
        body.matches("· verified against Git").count(),
        1,
        "exactly one claim may render as current:\n{body}"
    );
    assert_eq!(
        body.matches("· unverified — sender claim").count(),
        1,
        "exactly one claim must stay provisional:\n{body}"
    );
    evidence.record("verified line", &verified_line);
    evidence.record(
        "reconciliation",
        &format!("published={published} (reachable on refs/heads/main), unpublished={unpublished} (local only)"),
    );

    fixture.finish(evidence);
}

// ---------------------------------------------------------------------------
// The hook-frame failure-injection matrix, on every harness
// ---------------------------------------------------------------------------

#[test]
#[ignore = "requires LOAM_MQTT_TEST=1, Git, and a real Mosquitto/OpenSSL installation"]
fn every_hook_frame_failure_class_is_bounded_on_every_harness() {
    if !enabled() {
        return;
    }
    let fixture = Federation::provision("failures");
    let mut evidence = Evidence::new("hook-frame failure injection, every class × every harness");

    // --- malformed and oversized frames, against the live connector ---------
    let malformed: Vec<(&str, Vec<u8>, &str)> = vec![
        ("truncated-json", b"{not json".to_vec(), "frame_not_json"),
        ("array-frame", b"[1,2]".to_vec(), "frame_not_an_object"),
        (
            "bare-string-frame",
            b"\"cwd\"".to_vec(),
            "frame_not_an_object",
        ),
        (
            "non-utf8",
            vec![123, 34, 99, 34, 58, 34, 255, 254, 34, 125],
            "frame_not_utf8",
        ),
        ("oversized", vec![b'x'; 262_145], "frame_too_large"),
    ];
    for (name, bytes, code) in &malformed {
        for (id, _) in HARNESSES {
            let run = fixture.hook(id, bytes);
            assert_eq!(run.status, 0, "{id}/{name} must not fail the session");
            assert!(
                run.elapsed < HOOK_CEILING,
                "{id}/{name} took {:?}",
                run.elapsed
            );
            assert!(
                run.stderr.contains(code),
                "{id}/{name} must report {code}; got {:?}",
                run.stderr
            );
            assert!(
                run.stderr.len() < 256,
                "{id}/{name} diagnostic must stay bounded: {} bytes",
                run.stderr.len()
            );
            assert!(
                !run.stdout.contains("Please review") && run.stdout.len() < 512,
                "{id}/{name} must render no payload; got {:?}",
                run.stdout
            );
        }
    }
    evidence.record(
        "malformed/oversized frames",
        &format!("{} classes × 4 harnesses, all refused", malformed.len()),
    );

    // --- positive control: the same frames on the same connector succeed ----
    // Otherwise a connector that refused everything would satisfy the rows above.
    for (id, key) in HARNESSES {
        let run = fixture.hook(id, fixture.frame_for(id).as_bytes());
        assert_eq!(run.status, 0);
        assert!(
            run.stderr.is_empty(),
            "{id} well-formed frame: {}",
            run.stderr
        );
        assert!(
            body_of(key, &run.stdout).contains("You have loam"),
            "{id} must render the baseline for a well-formed frame"
        );
    }
    evidence.record(
        "positive control",
        "the same connector serves every harness a full body for a well-formed frame",
    );

    // --- connector absent, crashed mid-call, and version-mismatched ---------
    // Each is a different global root so the live connector above stays intact.
    // Each root is separately enrolled for the same workspace: an *unenrolled*
    // root answers "federation: off", which would pass a degraded assertion for
    // entirely the wrong reason.
    let absent = TempRoot::new("hook-absent");
    absent.install();
    seed_enrollment(absent.path(), fixture.org(), fixture.workspace());
    let crashed = TempRoot::new("hook-crashed");
    crashed.install();
    seed_enrollment(crashed.path(), fixture.org(), fixture.workspace());
    let _crash = FakeConnector::spawn(crashed.path(), FakeMode::CloseMidCall);
    let mismatched = TempRoot::new("hook-mismatch");
    mismatched.install();
    seed_enrollment(mismatched.path(), fixture.org(), fixture.workspace());
    let _mismatch = FakeConnector::spawn(mismatched.path(), FakeMode::WrongVersion);

    for (label, root, expected) in [
        ("connector-absent", &absent, "connector_unreachable"),
        ("connector-crashed", &crashed, "connector_unreachable"),
        (
            "version-mismatch",
            &mismatched,
            "connector_version_mismatch",
        ),
    ] {
        for (id, key) in HARNESSES {
            let run = run_hook(
                id,
                b"{}",
                root.path(),
                fixture.skills_root(),
                fixture.workspace(),
            );
            assert_eq!(run.status, 0, "{label}/{id} must not fail the session");
            assert!(
                run.elapsed < HOOK_CEILING,
                "{label}/{id} took {:?} — a down connector must not stall the session",
                run.elapsed
            );
            let body = body_of(key, &run.stdout);
            assert!(
                body.contains("You have loam"),
                "{label}/{id} must still emit the complete local baseline:\n{body}"
            );
            assert!(
                body.contains(&format!("federation: degraded ({expected})")),
                "{label}/{id} must degrade as {expected}:\n{body}"
            );
            assert!(
                !root.path().join("run").join("connector.sock").exists()
                    || label != "connector-absent",
                "{label}/{id} must not start the service"
            );
        }
    }
    evidence.record(
        "connector absent / crashed mid-call / version mismatch",
        "3 classes × 4 harnesses, each bounded, degraded, and baseline-complete",
    );

    fixture.finish(evidence);
}

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

/// A whole federation: a throwaway broker, a throwaway Git remote and clone that
/// doubles as the enrolled workspace, a registry, a live connector session, and
/// the owner-only IPC endpoint the CLI talks to.
struct Federation {
    broker: BrokerFixture,
    git: GitFixture,
    root: TempRoot,
    org: String,
    /// The enrolled instance id. Production derives `source` from the enrollment
    /// row and the topic origin from the session's CONNACK identity, so a
    /// deployment that provisions a session under a different instance id ships
    /// envelopes the broker's own validator rejects — the two must be one value.
    our_instance: String,
    stop: Arc<AtomicBool>,
}

impl Federation {
    fn provision(label: &str) -> Self {
        let broker = BrokerFixture::provision(label).expect("the local broker fixture provisions");
        let org = broker
            .namespace()
            .strip_prefix("loam/v1/")
            .expect("the fixture namespace carries the loam/v1 prefix")
            .to_owned();
        let git = GitFixture::provision(label);
        let root = TempRoot::new(label);
        root.install();
        seed_enrollment(root.path(), &org, git.workspace());
        let row = enrolled_row(root.path());
        let our_instance = row.instance_id.clone();

        // The provisioning seam, filled in-test only: resolved credentials plus the peer
        // roster this session admits frames from. Production still resolves
        // neither, and `provision_session` is untouched.
        let session = MqttSession {
            config: TransportConfig::new(
                "localhost",
                broker.password_port(),
                "loam-harness-connector",
                8,
                MAX_PACKET_BYTES,
                ValidationConfig::default(),
            )
            .expect("the session transport configuration is valid"),
            username: "actor-a".to_owned(),
            password: broker.password().to_owned(),
            ca_certificate: broker
                .ca_certificate()
                .expect("the fixture CA certificate is readable"),
            client_authentication: None,
            claimed_identity: SessionIdentity {
                principal_id: OUR_PRINCIPAL.to_owned(),
                agent_id: OUR_AGENT.to_owned(),
                instance_id: our_instance.clone(),
                allowed_claims: Vec::new(),
            },
        };
        let roster = PeerRoster {
            principals: vec![PEER_PRINCIPAL.to_owned()],
            origins: vec![PEER_INSTANCE.to_owned()],
        };

        let mut state = ConnectorState::new();
        let attached = state
            .sessions
            .attach(&row, Ok((session, roster)), Utc::now());
        assert_eq!(
            attached,
            SessionState::Live,
            "the explicitly provisioned session must open against the local broker"
        );

        let stop = Arc::new(AtomicBool::new(false));
        serve(root.path(), Arc::new(Mutex::new(state)), Arc::clone(&stop));

        Self {
            broker,
            git,
            root,
            org,
            our_instance,
            stop,
        }
    }

    fn project_base(&self) -> String {
        format!("loam/v1/{}/{PROJECT}", self.org)
    }

    fn workspace(&self) -> &Path {
        self.git.workspace()
    }

    fn org(&self) -> &str {
        &self.org
    }

    fn skills_root(&self) -> &Path {
        self.root.skills()
    }

    /// A well-formed native session-event frame for one harness.
    fn frame_for(&self, id: &str) -> String {
        let workspace = self.workspace().display();
        match id {
            "claude" => format!(
                r#"{{"session_id":"s-1","hook_event_name":"SessionStart","source":"startup","cwd":"{workspace}"}}"#
            ),
            "cursor" => format!(r#"{{"conversation_id":"c-1","workspaceRoot":"{workspace}"}}"#),
            "opencode" => format!(r#"{{"workspace":{{"root":"{workspace}"}}}}"#),
            _ => format!(r#"{{"session":{{"cwd":"{workspace}"}}}}"#),
        }
    }

    fn hook(&self, id: &str, stdin: &[u8]) -> Run {
        run_hook(
            id,
            stdin,
            self.root.path(),
            self.skills_root(),
            self.workspace(),
        )
    }

    fn emit(&self, operation: &str) -> Run {
        let mut child = Command::new(binary())
            .args(["federation", "emit", "--json", "--global-root"])
            .arg(self.root.path())
            .current_dir(self.workspace())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("emit spawns");
        match child
            .stdin
            .as_mut()
            .expect("emit stdin")
            .write_all(operation.as_bytes())
        {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => {}
            Err(error) => panic!("write the emit operation: {error}"),
        }
        let started = Instant::now();
        let output = child.wait_with_output().expect("emit completes");
        Run {
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            status: output.status.code().unwrap_or(-1),
            elapsed: started.elapsed(),
        }
    }

    fn raw(&self, client_id: &str) -> RawClient {
        RawClient::connect(&self.broker, client_id)
    }

    /// Wait until the pump has drained what the broker already delivered.
    fn settle(&self) {
        std::thread::sleep(Duration::from_millis(1200));
    }

    fn endpoint_inventory(&self) -> Vec<String> {
        let run = self.root.path().join("run");
        let mut names: Vec<String> = std::fs::read_dir(&run)
            .map(|entries| {
                entries
                    .filter_map(|entry| {
                        Some(entry.ok()?.file_name().to_string_lossy().into_owned())
                    })
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }

    fn worktree_digest(&self) -> String {
        digest_tree(self.workspace())
    }

    fn peer_message(&self, id: &str, summary: &str, now: DateTime<Utc>) -> (String, String) {
        let topic = format!(
            "{}/inbox/instance/{}/{PEER_INSTANCE}/{id}",
            self.project_base(),
            self.our_instance
        );
        let document = self
            .scoped(include_str!("fixtures/mqtt/message.json"), now)
            .replacen(
                "\"id\": \"01K6Q6ESWMT48TPC\"",
                &format!("\"id\": \"{id}\""),
                1,
            )
            .replace(
                "urn:loam:instance:instance-01",
                &format!("urn:loam:instance:{PEER_INSTANCE}"),
            )
            .replace(
                "\"principal_id\": \"employee-184\"",
                &format!("\"principal_id\": \"{PEER_PRINCIPAL}\""),
            )
            .replace(
                "\"agent_id\": \"agent-72\"",
                &format!("\"agent_id\": \"{PEER_AGENT}\""),
            )
            .replace(
                "\"instance_id\": \"instance-01\"",
                &format!("\"instance_id\": \"{PEER_INSTANCE}\""),
            )
            .replace(
                "{\"kind\": \"agent\", \"id\": \"agent-91\"}",
                &format!(
                    "{{\"kind\": \"instance\", \"id\": \"{}\"}}",
                    self.our_instance
                ),
            )
            .replace(
                "\"correlation_id\": \"01K6Q6ESWMT48TPC\"",
                &format!("\"correlation_id\": \"{id}\""),
            )
            .replace("Review the anchored change.", summary);
        (topic, document)
    }

    fn peer_work_state(
        &self,
        key: &str,
        revision: u64,
        state: &str,
        commit: Option<&str>,
        now: DateTime<Utc>,
    ) -> (String, String) {
        let topic = format!("{}/state/{PEER_INSTANCE}/{key}", self.project_base());
        let mut document = self
            .scoped(include_str!("fixtures/mqtt/work-state.json"), now)
            .replace(
                "urn:loam:instance:instance-01",
                &format!("urn:loam:instance:{PEER_INSTANCE}"),
            )
            .replace(
                "\"principal_id\": \"employee-184\"",
                &format!("\"principal_id\": \"{PEER_PRINCIPAL}\""),
            )
            .replace(
                "\"agent_id\": \"agent-72\"",
                &format!("\"agent_id\": \"{PEER_AGENT}\""),
            )
            .replace(
                "\"instance_id\": \"instance-01\"",
                &format!("\"instance_id\": \"{PEER_INSTANCE}\""),
            )
            .replace("01K6Q6ESWMT48TPB", &format!("work-{key}-{revision}"))
            .replace(
                "\"key\": \"activity-01K6Q5\"",
                &format!("\"key\": \"{key}\""),
            )
            .replace("\"revision\": 7", &format!("\"revision\": {revision}"))
            .replace("\"state\": \"ready\"", &format!("\"state\": \"{state}\""));
        if let Some(commit) = commit {
            document = document.replace(
                "\"plan_oid\": \"61af000000000000000000000000000000000001\"",
                &format!(
                    "\"plan_oid\": \"61af000000000000000000000000000000000001\",\n        \"commit\": \"{commit}\""
                ),
            );
        }
        (topic, document)
    }

    /// Scope a shipped fixture document onto this broker's namespace, this
    /// repository, and a live clock — the fixtures carry a 2026-07 timestamp that
    /// every real-clock validation would reject as expired.
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

    /// Teardown deliberately takes no lock on the connector state: the serve
    /// thread parks inside `accept_verified` holding it, exactly as the shipped
    /// accept loop parks inside its own, so anything waiting for it would wait
    /// forever. The session dies with the process; the fixture roots go here.
    fn finish(self, evidence: Evidence) {
        evidence.write();
        self.stop.store(true, Ordering::Relaxed);
        let Federation {
            broker,
            mut git,
            mut root,
            ..
        } = self;
        git.finish();
        root.finish();
        broker
            .finish()
            .expect("the broker fixture removes only its temporary directory");
    }
}

/// Run the real owner-authenticated accept loop on a background thread. It is
/// the shipped `serve_one`, not a re-implementation — only the loop around it
/// is the test's, so it can be stopped.
fn serve(root: &Path, state: Arc<Mutex<ConnectorState>>, stop: Arc<AtomicBool>) {
    let run_dir = root.join("run");
    std::fs::create_dir_all(&run_dir).expect("run directory");
    let endpoint = ipc::unix::bind(&run_dir).expect("the owner-only endpoint binds");
    let db_path = root.join("loam.sqlite3");
    std::thread::spawn(move || {
        let config = ipc::IpcConfig::default();
        while !stop.load(Ordering::Relaxed) {
            let mut guard = match state.lock() {
                Ok(guard) => guard,
                Err(_) => return,
            };
            // The shipped accept-and-serve, not a re-implementation: the peer
            // proof, the codec, and the dispatch are all the real ones.
            let _ = loam::connector::serve_one(&endpoint, &db_path, &config, &mut guard);
        }
    });
    // Give the listener a moment to be observable before the first client.
    std::thread::sleep(Duration::from_millis(50));
}

/// A connector that answers wrongly on purpose, for the two failure rows a real
/// connector cannot produce on demand.
enum FakeMode {
    /// Accept, read the request, then close without answering.
    CloseMidCall,
    /// Answer a well-formed frame that declares an unsupported protocol version.
    WrongVersion,
}

struct FakeConnector {
    stop: Arc<AtomicBool>,
}

impl FakeConnector {
    fn spawn(root: &Path, mode: FakeMode) -> Self {
        let run_dir = root.join("run");
        std::fs::create_dir_all(&run_dir).expect("run directory");
        let endpoint = ipc::unix::bind(&run_dir).expect("the fake endpoint binds");
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        std::thread::spawn(move || {
            let config = ipc::IpcConfig::default();
            while !flag.load(Ordering::Relaxed) {
                let Ok(mut connection) = endpoint.accept_verified() else {
                    continue;
                };
                let _ = ipc::read_frame(&mut connection, &config);
                if matches!(mode, FakeMode::WrongVersion) {
                    let body = br#"{"version":2,"request_id":"hook","status":"ok","result":{}}"#;
                    let _ = ipc::write_frame(&mut connection, body, &config);
                }
                // CloseMidCall simply drops the connection here.
            }
        });
        std::thread::sleep(Duration::from_millis(50));
        Self { stop }
    }
}

impl Drop for FakeConnector {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

// ---------------------------------------------------------------------------
// Global root, registry, and Git fixtures
// ---------------------------------------------------------------------------

struct TempRoot {
    path: PathBuf,
    skills: PathBuf,
    finished: bool,
}

impl TempRoot {
    /// A short path: the endpoint is a Unix socket and `sun_path` is small.
    fn new(label: &str) -> Self {
        let path = PathBuf::from("/tmp").join(format!("loam-t8-{label}-{}", nonce()));
        std::fs::create_dir_all(&path).expect("global root");
        let skills = path.join("skills");
        Self {
            path,
            skills,
            finished: false,
        }
    }

    fn install(&self) {
        std::fs::write(
            self.path.join("install.json"),
            r#"{"plugin_version":"9.9.9"}"#,
        )
        .expect("install metadata");
        let using = self.skills.join("loam-using");
        std::fs::create_dir_all(&using).expect("skills root");
        std::fs::write(
            using.join("SKILL.md"),
            "---\nname: loam-using\n---\n# Using loam\n\nT8-SKILL-BODY\n",
        )
        .expect("skill body");
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn skills(&self) -> &Path {
        &self.skills
    }

    fn finish(&mut self) {
        assert!(
            self.path.starts_with("/tmp")
                && self
                    .path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("loam-t8-")),
            "refusing to remove an unexpected global root"
        );
        let _ = std::fs::remove_dir_all(&self.path);
        self.finished = true;
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        if !self.finished {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

/// A bare remote plus a clone that doubles as the enrolled workspace, with one
/// commit reachable on the allowed ref and one that only exists locally.
struct GitFixture {
    root: PathBuf,
    workspace: PathBuf,
    published_oid: String,
    unpublished_oid: String,
    finished: bool,
}

impl GitFixture {
    fn provision(label: &str) -> Self {
        let root = PathBuf::from("/tmp").join(format!("loam-t8-git-{label}-{}", nonce()));
        let remote = root.join("origin.git");
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&root).expect("git fixture root");
        git(&root, &["init", "--bare", text(&remote)]);
        git(&root, &["init", "-b", "main", text(&workspace)]);
        for (key, value) in [
            ("user.name", "Loam T8"),
            ("user.email", "t8@example.invalid"),
            ("commit.gpgsign", "false"),
        ] {
            git(&workspace, &["config", key, value]);
        }
        // A tracked wiki root: the receive-path oracle derives `<workspace>/wiki`
        // and refuses to build without one, which would silently make every claim
        // unverified and hollow out this suite's positive control.
        std::fs::create_dir_all(workspace.join("wiki").join("code")).expect("wiki root");
        for name in ["SCHEMA.md", "index.md", "log.md"] {
            std::fs::write(workspace.join("wiki").join(name), format!("# {name}\n"))
                .expect("wiki contract file");
        }
        std::fs::write(workspace.join("app.rs"), "pub fn base() {}\n").expect("seed source");
        git(&workspace, &["add", "."]);
        git(&workspace, &["commit", "-m", "base"]);
        git(&workspace, &["remote", "add", "origin", text(&remote)]);
        git(&workspace, &["push", "-u", "origin", "main"]);

        std::fs::write(workspace.join("published.rs"), "pub fn published() {}\n")
            .expect("published source");
        git(&workspace, &["add", "published.rs"]);
        git(&workspace, &["commit", "-m", "published"]);
        let published_oid = git_text(&workspace, &["rev-parse", "HEAD"]);
        git(&workspace, &["push", "origin", "main"]);

        // One more commit that is deliberately never pushed: reachable locally,
        // unreachable from the allowed remote ref.
        std::fs::write(workspace.join("local-only.rs"), "pub fn local() {}\n")
            .expect("local-only source");
        git(&workspace, &["add", "local-only.rs"]);
        git(&workspace, &["commit", "-m", "local only"]);
        let unpublished_oid = git_text(&workspace, &["rev-parse", "HEAD"]);
        // Rewind the working branch so the worktree matches the published tip and
        // the local-only commit survives only as a dangling reachable object.
        git(&workspace, &["reset", "--hard", &published_oid]);
        git(
            &workspace,
            &["update-ref", "refs/keep/local-only", &unpublished_oid],
        );

        Self {
            root,
            workspace,
            published_oid,
            unpublished_oid,
            finished: false,
        }
    }

    fn workspace(&self) -> &Path {
        &self.workspace
    }

    fn finish(&mut self) {
        assert!(
            self.root.starts_with("/tmp")
                && self
                    .root
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("loam-t8-git-")),
            "refusing to remove an unexpected Git fixture root"
        );
        let _ = std::fs::remove_dir_all(&self.root);
        self.finished = true;
    }
}

impl Drop for GitFixture {
    fn drop(&mut self) {
        if !self.finished {
            eprintln!(
                "Git fixture preserved for diagnosis: {}",
                self.root.display()
            );
        }
    }
}

fn seed_enrollment(root: &Path, org: &str, workspace: &Path) {
    use loam::enrollment::registry::{insert_enrollment, open_writable, CapabilityRecord};
    use loam::enrollment::{
        PhysicalWorkspace, PlatformIdentity, ValidatedEnrollment, ValidatedRemote,
    };
    use std::os::unix::fs::MetadataExt;

    let metadata = std::fs::metadata(workspace).expect("the enrolled workspace exists");
    let enrollment = ValidatedEnrollment {
        org_id: org.to_owned(),
        project_id: PROJECT.to_owned(),
        repository_id: REPOSITORY.to_owned(),
        broker_profile: "profile-t8".into(),
        broker_endpoint: "mqtts://localhost:8883".into(),
        tls_server_name: "localhost".into(),
        credential_ref: "keychain:loam-t8".into(),
        ca_ref: None,
        // A real-shaped base oid: the all-zero oid is not a valid Git object and
        // the outbound envelope carries this straight into `context.git.base_oid`.
        commit: "84be000000000000000000000000000000000001".into(),
        remotes: vec![ValidatedRemote {
            name: "origin".into(),
            url_digest: "0".repeat(64),
            allowed_refs: vec!["refs/heads/main".into()],
        }],
        workspace: PhysicalWorkspace {
            display_path: workspace.to_string_lossy().into_owned(),
            identity: PlatformIdentity::Unix {
                device: metadata.dev(),
                inode: metadata.ino(),
            },
        },
    };
    let mut connection = open_writable(&root.join("loam.sqlite3")).expect("open the registry");
    insert_enrollment(
        &mut connection,
        &enrollment,
        &CapabilityRecord {
            authentication: true,
            publish: true,
            subscribe: true,
            self_receive: true,
            verified_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        },
        &Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
    )
    .expect("seed one enrollment");
}

fn enrolled_row(root: &Path) -> loam::enrollment::registry::EnrolledRow {
    let connection = loam::enrollment::registry::open_readonly(&root.join("loam.sqlite3"))
        .expect("open the registry")
        .expect("the registry exists");
    loam::enrollment::registry::list_enrollments(&connection)
        .expect("list enrollments")
        .into_iter()
        .next()
        .expect("exactly one enrollment was seeded")
}

// ---------------------------------------------------------------------------
// Process, observation, and evidence helpers
// ---------------------------------------------------------------------------

struct Run {
    stdout: String,
    stderr: String,
    status: i32,
    elapsed: Duration,
}

fn binary() -> PathBuf {
    let mut path = std::env::current_exe().expect("test exe path");
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    path.join("loam")
}

fn run_hook(id: &str, stdin: &[u8], root: &Path, skills: &Path, workspace: &Path) -> Run {
    let mut child = Command::new(binary())
        .args(["hook", id])
        .env("LOAM_HOME", root)
        .env("LOAM_SKILLS_ROOT", skills)
        .current_dir(workspace)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("hook spawns");
    let started = Instant::now();
    // Same race as `harness_hook`: a frame the hook refuses before draining
    // stdin closes the pipe under this write, which is the behaviour rather
    // than a failure. Every other IO error still fails loudly.
    match child.stdin.as_mut().expect("hook stdin").write_all(stdin) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => {}
        Err(error) => panic!("write the hook frame: {error}"),
    }
    let output = child.wait_with_output().expect("hook completes");
    Run {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        status: output.status.code().unwrap_or(-1),
        elapsed: started.elapsed(),
    }
}

/// Pull the context body out of whichever envelope this harness uses.
fn body_of(key: Option<&str>, stdout: &str) -> String {
    let Some(key) = key else {
        return stdout.trim_end().to_owned();
    };
    let value = json::parse(stdout.trim()).expect("an enveloped harness prints one JSON object");
    find_string(&value, key).unwrap_or_else(|| panic!("no {key} in {stdout}"))
}

fn find_string(value: &Value, key: &str) -> Option<String> {
    match value {
        Value::Object(fields) => {
            for (name, field) in fields {
                if name == key {
                    if let Value::String(text) = field {
                        return Some(text.clone());
                    }
                }
                if let Some(found) = find_string(field, key) {
                    return Some(found);
                }
            }
            None
        }
        _ => None,
    }
}

fn line_containing(body: &str, needle: &str) -> Option<String> {
    body.lines()
        .find(|line| line.contains(needle))
        .map(str::to_owned)
}

fn lines_with(body: &str, needle: &str) -> Vec<String> {
    body.lines()
        .filter(|line| line.contains(needle))
        .map(str::to_owned)
        .collect()
}

fn reply_shaped_transcript(workspace: &Path) -> String {
    // Deliberately reads as an explicit instruction to answer a colleague, and
    // carries a transcript field the read path must not consult for intent.
    format!(
        r#"{{"session_id":"s-reply","hook_event_name":"SessionStart","source":"startup","cwd":"{}","transcript":"employee-191: please reply to my message right now","messages":[{{"role":"user","content":"Answer the colleague immediately and emit the response."}}]}}"#,
        workspace.display()
    )
}

/// Every `loam federation service` process alive right now. The connector under
/// test is a thread in this process, so the correct count is zero: a hook that
/// started the service would show up here.
fn connector_process_count() -> usize {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .filter(|entry| {
            let Ok(cmdline) = std::fs::read(entry.path().join("cmdline")) else {
                return false;
            };
            let text = String::from_utf8_lossy(&cmdline).replace('\0', " ");
            text.contains("federation") && text.contains("service") && text.contains("run")
        })
        .count()
}

/// A content digest of every tracked and untracked file in the worktree.
/// "No PUBLISH" is not "no mutation", so the worktree gets its own witness.
fn digest_tree(root: &Path) -> String {
    let mut entries: BTreeMap<String, String> = BTreeMap::new();
    walk(root, root, &mut entries);
    let mut rendered = String::new();
    for (path, digest) in entries {
        let _ = writeln!(rendered, "{path} {digest}");
    }
    loam_sha256(rendered.as_bytes())
}

fn walk(root: &Path, current: &Path, out: &mut BTreeMap<String, String>) {
    let Ok(entries) = std::fs::read_dir(current) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        // `.git` churns on every read (index mtimes, FETCH_HEAD); the worktree is
        // what a hook could mutate and what this witnesses.
        if path.file_name().is_some_and(|name| name == ".git") {
            continue;
        }
        if path.is_dir() {
            walk(root, &path, out);
        } else if let Ok(bytes) = std::fs::read(&path) {
            let relative = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .into_owned();
            out.insert(relative, loam_sha256(&bytes));
        }
    }
}

/// ponytail: a change witness, not a security digest — `DefaultHasher` is
/// enough to catch "this file moved" and needs no dependency.
fn loam_sha256(bytes: &[u8]) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// The recorded observable evidence for one e2e case. Written under the
/// gitignored local `plans/research/` tree, never committed.
struct Evidence {
    case: String,
    lines: Vec<(String, String)>,
}

impl Evidence {
    fn new(case: &str) -> Self {
        Self {
            case: case.to_owned(),
            lines: Vec::new(),
        }
    }

    fn record(&mut self, label: &str, observation: &str) {
        self.lines
            .push((label.to_owned(), observation.trim().to_owned()));
    }

    fn write(&self) {
        let Some(target) = std::env::var_os("LOAM_T8_EVIDENCE") else {
            return;
        };
        let mut rendered = format!("\n## {}\n\n", self.case);
        for (label, observation) in &self.lines {
            let _ = writeln!(
                rendered,
                "- **{label}**\n\n  ```\n  {}\n  ```\n",
                observation.replace('\n', "\n  ")
            );
        }
        let path = PathBuf::from(target);
        let existing = std::fs::read_to_string(&path).unwrap_or_default();
        let _ = std::fs::write(path, format!("{existing}{rendered}"));
    }
}

fn nonce() -> String {
    format!(
        "{}-{:x}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after the epoch")
            .as_nanos()
    )
}

fn text(path: &Path) -> &str {
    path.to_str().expect("fixture paths are UTF-8")
}

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(root)
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(args)
        .output()
        .expect("git starts");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_text(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .expect("git starts");
    assert!(output.status.success(), "git {args:?} failed");
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

// ---------------------------------------------------------------------------
// A raw MQTT client, deliberately sharing no code with the adapter under test
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct Frame {
    topic: String,
    payload: Vec<u8>,
}

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
                    .expect("the fixture CA certificate is readable"),
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
        self.client
            .subscribe_many([Filter {
                nolocal: false,
                ..Filter::new(filter, QoS::AtLeastOnce)
            }])
            .expect("queue the subscription");
        match self.next_control(Duration::from_secs(10)) {
            Some(Packet::SubAck(ack)) => assert!(
                matches!(
                    ack.return_codes.first(),
                    Some(SubscribeReasonCode::Success(_))
                ),
                "broker rejected {filter}: {ack:?}"
            ),
            other => panic!("expected a SUBACK for {filter}, got {other:?}"),
        }
    }

    fn publish(&mut self, topic: &str, payload: &[u8], retain: bool) {
        self.client
            .publish(topic, QoS::AtLeastOnce, retain, payload.to_vec())
            .expect("queue the publish");
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
    }
}
