//! Connector service integration tests.
//!
//! `cli` is bin-only, so these drive the built binary. They cover the
//! inert-by-default guarantee: `federation service run` against an unenrolled
//! machine binds no endpoint, creates no database on a read, and exits cleanly.
//! The served accept loop and dispatch are covered by the connector unit tests
//! (which do not block); a full socket round-trip needs the persisting connect
//! from T10.

use std::path::PathBuf;
use std::process::Command;

fn binary() -> PathBuf {
    let mut path = std::env::current_exe().expect("test exe path");
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    path.join(if cfg!(windows) { "loam.exe" } else { "loam" })
}

/// A short-pathed temporary global root. The endpoint is a Unix socket, and
/// `sun_path` is 104 bytes on macOS — `std::env::temp_dir()` there is a long
/// `/var/folders/...` path that alone can exceed it.
fn temp_root(label: &str) -> PathBuf {
    #[cfg(unix)]
    let base = PathBuf::from("/tmp");
    #[cfg(not(unix))]
    let base = std::env::temp_dir();
    let dir = base.join(format!(
        "loam-connector-{label}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// An isolated Loam config dir under a test's own temp root.
///
/// The registry-resolution ladder puts `LOAM_CONFIG_DIR` above the passed
/// `--global-root`, so a child that inherits the developer machine's
/// environment reads that machine's live config-dir registry: a test meaning to
/// describe an unenrolled machine describes an enrolled one instead (#130).
/// Pinning is per-spawn on the child's environment, never on the test process's,
/// so it needs no lock under `cargo test`'s threads.
fn temp_config_dir(root: &std::path::Path) -> PathBuf {
    let dir = root.join("config");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The registry a pinned child resolves. Rung 1 of the ladder returns
/// `<config dir>/federation/loam.sqlite3` unconditionally, so this — not the
/// `--global-root` database — is the store those runs create or refuse to
/// create.
fn pinned_registry(config_dir: &std::path::Path) -> PathBuf {
    config_dir.join("federation").join("loam.sqlite3")
}

/// The binary with its config dir pinned. Every spawn in this file goes through
/// here so no test can reach the live config-dir registry of the machine
/// running it.
fn loam(config_dir: &std::path::Path) -> Command {
    let mut command = Command::new(binary());
    command.env("LOAM_CONFIG_DIR", config_dir);
    command
}

#[test]
fn service_run_on_an_unenrolled_machine_is_inert() {
    let root = temp_root("inert");
    let config = temp_config_dir(&root);
    let output = loam(&config)
        .args(["federation", "service", "run"])
        .arg("--global-root")
        .arg(&root)
        .output()
        .expect("spawn service");

    assert!(
        output.status.success(),
        "inert service run should exit 0; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // No endpoint was bound, and a read did not create the database.
    #[cfg(unix)]
    assert!(
        !root.join("run").join("connector.sock").exists(),
        "no socket may exist on an unenrolled machine"
    );
    assert!(
        !root.join("loam.sqlite3").exists(),
        "a reconciliation read must not create the database"
    );
    assert!(
        !pinned_registry(&config).exists(),
        "a reconciliation read must not create the resolved config-dir database"
    );

    let _ = std::fs::remove_dir_all(&root);
}

// --- enrolled-start positive control ---
//
// Every inertness assertion here and in the hosted service smokes is an absence
// check, and an absence check needs a run where the thing really does appear.
// These seed one enrollment through the real registry API — no broker, no probe,
// no credential — and prove the same binary that stays dormant on an empty
// registry binds its endpoint and keeps serving once one exists.

/// One enrollment for `root`, carrying only non-secret projections.
fn smoke_enrollment(root: &std::path::Path) -> loam::enrollment::ValidatedEnrollment {
    use loam::enrollment::{
        PhysicalWorkspace, PlatformIdentity, ValidatedEnrollment, ValidatedRemote,
    };
    #[cfg(unix)]
    let identity = {
        use std::os::unix::fs::MetadataExt;
        let metadata = std::fs::metadata(root).expect("global root exists");
        PlatformIdentity::Unix {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    };
    #[cfg(not(unix))]
    let identity = PlatformIdentity::WindowsPath;

    ValidatedEnrollment {
        org_id: "org-smoke".into(),
        project_id: "project-smoke".into(),
        repository_id: "repository-smoke".into(),
        broker_profile: "profile-smoke".into(),
        broker_endpoint: "mqtts://broker.invalid:8883".into(),
        tls_server_name: "broker.invalid".into(),
        ca_ref: None,
        commit: "0".repeat(40),
        remotes: vec![ValidatedRemote {
            name: "origin".into(),
            url_digest: "0".repeat(64),
            allowed_refs: vec!["refs/heads/main".into()],
        }],
        workspace: PhysicalWorkspace {
            display_path: root.to_string_lossy().into_owned(),
            identity,
        },
    }
}

/// Seed one enrollment into `registry`. The registry path is explicit because
/// the two callers resolve it differently: the hosted smokes run an unpinned
/// binary that falls back to the `--global-root` database, while the in-process
/// tests pin `LOAM_CONFIG_DIR` and must seed the rung-1 path the child will read.
fn seed_enrollment(root: &std::path::Path, registry: &std::path::Path) {
    use loam::enrollment::registry::{insert_enrollment, open_writable, CapabilityRecord};
    if let Some(parent) = registry.parent() {
        std::fs::create_dir_all(parent).expect("registry directory");
    }
    let mut connection = open_writable(registry).expect("open the registry");
    let capabilities = CapabilityRecord {
        authentication: true,
        publish: true,
        subscribe: true,
        self_receive: true,
        verified_at: "2026-08-08T00:00:00Z".into(),
    };
    insert_enrollment(
        &mut connection,
        &smoke_enrollment(root),
        "instance-under-test",
        &capabilities,
        "2026-08-08T00:00:00Z",
    )
    .expect("seed one enrollment");
}

/// Seeds `LOAM_SMOKE_ROOT` for the hosted LaunchAgent/Task Scheduler smokes, so
/// they can observe a real enrolled start under the real manager. Ignored by
/// default; the smoke scripts run it by name.
#[test]
#[ignore]
fn seed_one_enrollment_for_the_service_smoke() {
    let root = PathBuf::from(
        std::env::var("LOAM_SMOKE_ROOT").expect("LOAM_SMOKE_ROOT names the smoke's global root"),
    );
    std::fs::create_dir_all(&root).expect("global root");
    seed_enrollment(&root, &root.join("loam.sqlite3"));
    assert!(root.join("loam.sqlite3").is_file(), "registry was written");
}

#[test]
fn an_enrolled_machine_starts_and_serves_instead_of_exiting() {
    let root = temp_root("enrolled-start");
    let config = temp_config_dir(&root);
    seed_enrollment(&root, &pinned_registry(&config));

    let mut child = loam(&config)
        .args(["federation", "service", "run", "--global-root"])
        .arg(&root)
        .spawn()
        .expect("spawn service");

    // The connector is inert only while the registry is empty; with one
    // enrollment it must keep serving instead of exiting. On Unix the endpoint
    // is an observable socket file; the Windows endpoint is a named pipe, so
    // there the live process is the observation.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    #[cfg(unix)]
    let served = {
        let socket = root.join("run").join("connector.sock");
        while !socket.exists() && std::time::Instant::now() < deadline {
            assert!(
                child.try_wait().expect("poll service").is_none(),
                "an enrolled connector must not exit before binding its endpoint \
                 (exit status {:?})",
                child.try_wait().expect("poll service")
            );
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        socket.exists()
    };
    #[cfg(not(unix))]
    let served = {
        let live_for = std::time::Duration::from_secs(3);
        let until = std::time::Instant::now() + live_for;
        while std::time::Instant::now() < until {
            assert!(
                child.try_wait().expect("poll service").is_none(),
                "an enrolled connector must not exit like an unenrolled one does"
            );
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let _ = deadline;
        true
    };
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&root);
    assert!(
        served,
        "an enrolled connector serves its owner-only endpoint"
    );
}

#[test]
fn service_run_requires_a_global_root() {
    let root = temp_root("usage");
    let config = temp_config_dir(&root);
    let output = loam(&config)
        .args(["federation", "service", "run"])
        .output()
        .expect("spawn service");
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(
        output.status.code(),
        Some(64),
        "missing --global-root is a usage error"
    );
}

// --- T11 disconnect / status lifecycle (through the real binary) ---
//
// The rich lifecycle matrix (intermediate/final/repair/degraded/broker-down) is
// covered by the connector `lifecycle_tests` unit module with an injected
// service runner. These two prove the read-only, inert guarantees end-to-end
// through the shipped binary: neither command creates the database or a socket.

#[test]
#[cfg(unix)]
fn lifecycle_status_on_a_missing_registry_creates_no_database() {
    let root = temp_root("lifecycle-status");
    let config = temp_config_dir(&root);
    let output = loam(&config)
        .args(["federation", "status", "--global-root"])
        .arg(&root)
        .output()
        .expect("spawn status");

    assert!(
        output.status.success(),
        "read-only status exits 0; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("0 enrolled"),
        "status reports no enrollments; got: {stdout}"
    );
    // A read-only status neither creates the store nor binds an endpoint.
    assert!(
        !root.join("loam.sqlite3").exists(),
        "status must not create the database"
    );
    assert!(
        !pinned_registry(&config).exists(),
        "status must not create the resolved config-dir database"
    );
    assert!(!root.join("run").join("connector.sock").exists());

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
#[cfg(unix)]
fn lifecycle_disconnect_on_a_dormant_machine_is_inert() {
    let root = temp_root("lifecycle-disconnect");
    let config = temp_config_dir(&root);
    // "." resolves to this git workspace, but no enrollment exists under the
    // fresh global root, so the machine is dormant: nothing to remove, no manager
    // call, and no database created.
    let output = loam(&config)
        .args(["federation", "disconnect", ".", "--global-root"])
        .arg(&root)
        .output()
        .expect("spawn disconnect");

    assert!(
        output.status.success(),
        "a dormant disconnect exits 0; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("not-enrolled") && stdout.contains("dormant"),
        "disconnect reports a dormant machine; got: {stdout}"
    );
    assert!(
        !root.join("loam.sqlite3").exists(),
        "disconnect must not create the database"
    );
    assert!(
        !pinned_registry(&config).exists(),
        "disconnect must not create the resolved config-dir database"
    );

    let _ = std::fs::remove_dir_all(&root);
}

// --- `federation list`: the human-facing enrollment inventory (#146) ---

#[test]
fn list_on_a_fresh_machine_reports_an_empty_inventory_and_creates_nothing() {
    let root = temp_root("list-empty");
    let config = temp_config_dir(&root);
    let output = loam(&config)
        .args(["federation", "list"])
        .output()
        .expect("spawn list");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "an empty inventory is not an error; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("no federation enrollments") && stdout.contains("federation connect"),
        "the empty inventory names the command that fills it; got: {stdout}"
    );
    // The empty array is the contract a --json consumer leans on hardest: no
    // enrollments must read as an empty inventory, never as an absent field or
    // a refusal it has to special-case.
    let json = loam(&config)
        .args(["federation", "list", "--json"])
        .output()
        .expect("spawn list --json");
    let json_stdout = String::from_utf8_lossy(&json.stdout);
    assert!(json.status.success(), "{json_stdout}");
    assert_eq!(
        json_stdout.trim(),
        "{\"schema\":1,\"enrollments\":[]}",
        "an empty inventory is an empty array"
    );

    assert!(
        !pinned_registry(&config).exists(),
        "a read-only inventory must not create the registry"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn list_reads_the_config_dir_registry_without_a_global_root() {
    let root = temp_root("list-enrolled");
    let config = temp_config_dir(&root);
    seed_enrollment(&root, &pinned_registry(&config));

    // No --global-root: the whole point of the command is that the config-dir
    // ladder already knows where the registry is.
    let output = loam(&config)
        .args(["federation", "list"])
        .output()
        .expect("spawn list");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "list exits 0; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    for column in [
        "PROJECT",
        "WORKSPACE",
        "BROKER",
        "LAST VERIFIED",
        "org-smoke/project-smoke",
        "mqtts://broker.invalid:8883",
        "2026-08-08T00:00:00Z",
    ] {
        assert!(stdout.contains(column), "missing {column} in: {stdout}");
    }
    assert!(
        stdout.contains(&root.to_string_lossy().into_owned()),
        "the workspace column names the enrolled workspace; got: {stdout}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// `list --json` and `status --json` describe an enrollment identically: one
/// projection, two surfaces. If they ever diverge, a JSON consumer of `list`
/// silently loses a field `status` still reports.
#[test]
fn list_json_is_the_same_per_enrollment_array_status_builds() {
    let root = temp_root("list-json");
    let config = temp_config_dir(&root);
    seed_enrollment(&root, &pinned_registry(&config));

    let listed = loam(&config)
        .args(["federation", "list", "--json"])
        .output()
        .expect("spawn list");
    assert!(listed.status.success());
    let listed = loam::json::parse(&String::from_utf8_lossy(&listed.stdout)).expect("list json");

    let status = loam(&config)
        .args(["federation", "status", "--json", "--global-root"])
        .arg(&root)
        .output()
        .expect("spawn status");
    assert!(status.status.success());
    let status = loam::json::parse(&String::from_utf8_lossy(&status.stdout)).expect("status json");

    let listed = listed.get("enrollments").expect("list enrollments");
    assert_eq!(
        listed.to_json(),
        status
            .get("enrollments")
            .expect("status enrollments")
            .to_json(),
        "list and status must project an enrollment identically"
    );
    // And the projection carries what the table shows, so a --json consumer is
    // never poorer than the human reading the same command.
    assert!(
        listed.to_json().contains("mqtts://broker.invalid:8883"),
        "the projection carries the broker endpoint: {}",
        listed.to_json()
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn list_rejects_an_unknown_argument_instead_of_ignoring_it() {
    let root = temp_root("list-badarg");
    let config = temp_config_dir(&root);
    let output = loam(&config)
        .args(["federation", "list", "--verbose"])
        .output()
        .expect("spawn list");
    assert_eq!(output.status.code().unwrap_or(-1), 64);
    assert!(String::from_utf8_lossy(&output.stderr).contains("unexpected argument"));

    let _ = std::fs::remove_dir_all(&root);
}
