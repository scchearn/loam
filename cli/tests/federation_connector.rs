//! Slice C connector service integration tests (T9).
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

#[cfg(unix)]
fn temp_root(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
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
#[cfg(unix)]
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
