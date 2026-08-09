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

#[test]
fn service_run_on_an_unenrolled_machine_is_inert() {
    let root = temp_root("inert");
    let output = Command::new(binary())
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
        credential_ref: "keychain:loam-smoke".into(),
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

fn seed_enrollment(root: &std::path::Path) {
    use loam::enrollment::registry::{insert_enrollment, open_writable, CapabilityRecord};
    let mut connection = open_writable(&root.join("loam.sqlite3")).expect("open the registry");
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
    seed_enrollment(&root);
    assert!(root.join("loam.sqlite3").is_file(), "registry was written");
}

#[test]
fn an_enrolled_machine_starts_and_serves_instead_of_exiting() {
    let root = temp_root("enrolled-start");
    seed_enrollment(&root);

    let mut child = Command::new(binary())
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
    let output = Command::new(binary())
        .args(["federation", "service", "run"])
        .output()
        .expect("spawn service");
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
    let output = Command::new(binary())
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
    assert!(!root.join("run").join("connector.sock").exists());

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
#[cfg(unix)]
fn lifecycle_disconnect_on_a_dormant_machine_is_inert() {
    let root = temp_root("lifecycle-disconnect");
    // "." resolves to this git workspace, but no enrollment exists under the
    // fresh global root, so the machine is dormant: nothing to remove, no manager
    // call, and no database created.
    let output = Command::new(binary())
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

    let _ = std::fs::remove_dir_all(&root);
}
