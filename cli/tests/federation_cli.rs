//! `loam federation` CLI integration tests.
//!
//! `cli` is a bin-only crate, so these drive the built binary through
//! `std::process::Command` and feed the descriptor on stdin. This suite covers the
//! `connect` descriptor-validation path (typed JSON errors, exit codes) and a
//! full happy path against hermetic local Git repositories.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn binary() -> PathBuf {
    // Cargo exposes the built test binary's own path; the `loam` binary sits in
    // the same target directory.
    let mut path = std::env::current_exe().expect("test exe path");
    path.pop(); // deps/
    if path.ends_with("deps") {
        path.pop();
    }
    path.join(if cfg!(windows) { "loam.exe" } else { "loam" })
}

fn run_connect(workspace: Option<&Path>, stdin: &[u8]) -> (i32, String, String) {
    let mut command = Command::new(binary());
    command.arg("federation").arg("connect");
    if let Some(ws) = workspace {
        command.arg(ws);
    }
    command.arg("--json");
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("spawn loam");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(stdin)
        .expect("write descriptor");
    let output = child.wait_with_output().expect("wait");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

const GOLDEN: &str = include_str!("fixtures/mqtt/connector-descriptor-cases.json");

fn valid_descriptor_json() -> String {
    // Reuse the same golden case the unit tests use.
    let start = GOLDEN.find("\"valid\":").expect("valid key") + "\"valid\":".len();
    // Extract the balanced object after "valid":
    extract_object(&GOLDEN[start..])
}

fn extract_object(text: &str) -> String {
    let bytes = text.as_bytes();
    let open = text.find('{').expect("object start");
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (i, &b) in bytes.iter().enumerate().skip(open) {
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return text[open..=i].to_owned();
                }
            }
            _ => {}
        }
    }
    panic!("unbalanced object");
}

#[test]
fn descriptor_unknown_field_is_rejected_with_typed_json() {
    let valid = valid_descriptor_json();
    let bad = format!("{{\"note\":\"x\",{}", &valid[1..]);
    let (code, stdout, _stderr) = run_connect(None, bad.as_bytes());
    assert_eq!(code, 65, "usage/input class for a descriptor violation");
    assert!(
        stdout.contains("descriptor_unknown_field"),
        "expected typed code, got: {stdout}"
    );
}

#[test]
fn descriptor_secret_field_is_rejected() {
    let valid = valid_descriptor_json();
    let bad = format!("{{\"token\":\"x\",{}", &valid[1..]);
    let (code, stdout, _stderr) = run_connect(None, bad.as_bytes());
    assert_eq!(code, 65);
    assert!(
        stdout.contains("descriptor_forbidden_field"),
        "got: {stdout}"
    );
}

#[test]
fn descriptor_plaintext_endpoint_is_rejected() {
    let bad = valid_descriptor_json().replace("mqtts://", "mqtt://");
    let (code, stdout, _stderr) = run_connect(None, bad.as_bytes());
    assert_eq!(code, 65);
    assert!(
        stdout.contains("descriptor_invalid_endpoint"),
        "got: {stdout}"
    );
}

#[test]
fn descriptor_oversize_is_rejected() {
    let big = vec![b' '; 64 * 1024 + 1];
    let (code, stdout, _stderr) = run_connect(None, &big);
    assert_eq!(code, 65);
    assert!(stdout.contains("descriptor_too_large"), "got: {stdout}");
}

#[test]
fn full_happy_path_validates_against_hermetic_repos() {
    // Build an origin repo with a commit on refs/heads/main, then a workspace
    // clone whose `origin` points at it. The descriptor's commit must be proven
    // reachable in an isolated temp repo without mutating the workspace.
    let root = temp_dir("happy");
    let origin = root.join("origin.git");
    let work = root.join("work");
    git(
        &["init", "--bare", "--quiet", origin.to_str().unwrap()],
        None,
    );

    // seed via a scratch checkout
    let seed = root.join("seed");
    git(&["init", "--quiet", seed.to_str().unwrap()], None);
    git(
        &["-C", seed.to_str().unwrap(), "config", "user.email", "t@t"],
        None,
    );
    git(
        &["-C", seed.to_str().unwrap(), "config", "user.name", "t"],
        None,
    );
    std::fs::write(seed.join("f.txt"), "hi").unwrap();
    git(&["-C", seed.to_str().unwrap(), "add", "."], None);
    git(
        &[
            "-C",
            seed.to_str().unwrap(),
            "commit",
            "--quiet",
            "-m",
            "init",
        ],
        None,
    );
    git(
        &["-C", seed.to_str().unwrap(), "branch", "-M", "main"],
        None,
    );
    git(
        &[
            "-C",
            seed.to_str().unwrap(),
            "push",
            "--quiet",
            origin.to_str().unwrap(),
            "main",
        ],
        None,
    );
    let commit = git(&["-C", seed.to_str().unwrap(), "rev-parse", "HEAD"], None)
        .trim()
        .to_owned();

    // workspace clone with origin -> origin.git
    git(
        &[
            "clone",
            "--quiet",
            origin.to_str().unwrap(),
            work.to_str().unwrap(),
        ],
        None,
    );

    let descriptor = valid_descriptor_json()
        .replace("0123456789abcdef0123456789abcdef01234567", &commit)
        .replace(
            r#""refs":["refs/heads/main","refs/heads/federation"]"#,
            r#""refs":["refs/heads/main"]"#,
        );

    let (code, stdout, stderr) = run_connect(Some(&work), descriptor.as_bytes());
    assert_eq!(
        code, 0,
        "happy path should validate. stderr: {stderr} stdout: {stdout}"
    );
    assert!(stdout.contains("\"status\":\"validated\""), "got: {stdout}");
    assert!(
        stdout.contains("\"url_digest\""),
        "remote digest present: {stdout}"
    );
    assert!(
        !stdout.contains(origin.to_str().unwrap()),
        "raw remote URL must not leak"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn unreachable_commit_is_rejected() {
    let root = temp_dir("unreachable");
    let origin = root.join("origin.git");
    let work = root.join("work");
    git(
        &["init", "--bare", "--quiet", origin.to_str().unwrap()],
        None,
    );
    let seed = root.join("seed");
    git(&["init", "--quiet", seed.to_str().unwrap()], None);
    git(
        &["-C", seed.to_str().unwrap(), "config", "user.email", "t@t"],
        None,
    );
    git(
        &["-C", seed.to_str().unwrap(), "config", "user.name", "t"],
        None,
    );
    std::fs::write(seed.join("f.txt"), "hi").unwrap();
    git(&["-C", seed.to_str().unwrap(), "add", "."], None);
    git(
        &[
            "-C",
            seed.to_str().unwrap(),
            "commit",
            "--quiet",
            "-m",
            "init",
        ],
        None,
    );
    git(
        &["-C", seed.to_str().unwrap(), "branch", "-M", "main"],
        None,
    );
    git(
        &[
            "-C",
            seed.to_str().unwrap(),
            "push",
            "--quiet",
            origin.to_str().unwrap(),
            "main",
        ],
        None,
    );
    git(
        &[
            "clone",
            "--quiet",
            origin.to_str().unwrap(),
            work.to_str().unwrap(),
        ],
        None,
    );

    // A syntactically valid but absent commit.
    let descriptor = valid_descriptor_json().replace(
        r#""refs":["refs/heads/main","refs/heads/federation"]"#,
        r#""refs":["refs/heads/main"]"#,
    );
    let (code, stdout, _stderr) = run_connect(Some(&work), descriptor.as_bytes());
    assert_eq!(code, 65);
    assert!(stdout.contains("commit_unreachable"), "got: {stdout}");

    let _ = std::fs::remove_dir_all(&root);
}

fn temp_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "loam-fed-{label}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn git(args: &[&str], cwd: Option<&Path>) -> String {
    let mut command = Command::new("git");
    command.args(args);
    if let Some(dir) = cwd {
        command.current_dir(dir);
    }
    let output = command.output().expect("git runs");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

// ---------------------------------------------------------------------------
// `loam federation emit`
// ---------------------------------------------------------------------------

fn run_emit(workspace: Option<&Path>, global_root: &Path, stdin: &[u8]) -> (i32, String, String) {
    let mut command = Command::new(binary());
    command.arg("federation").arg("emit");
    if let Some(ws) = workspace {
        command.arg(ws);
    }
    command
        .arg("--global-root")
        .arg(global_root)
        .arg("--json")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("emit spawns");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(stdin)
        .expect("write stdin");
    let output = child.wait_with_output().expect("emit completes");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn emit_refuses_an_extension_type_with_a_typed_error_rather_than_dispatching() {
    let root = temp_dir("emit-extension");
    let operation = br#"{"type":"com.example.deploy.request","causation_id":"c-1","summary":"Deploy please.","to":[{"kind":"instance","id":"instance-01"}],"payload":{}}"#;
    // The workspace is this repository: real, Git, and deliberately unenrolled
    // in the throwaway global root.
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let (code, stdout, _stderr) = run_emit(Some(&workspace), &root, operation);
    assert_eq!(code, 65, "{stdout}");
    assert!(
        stdout.contains("unsupported_operation_type"),
        "an extension type must be refused by name, not dispatched: {stdout}"
    );
    // The vocabulary check runs before anything is reached: no registry was
    // created and no endpoint was opened.
    assert!(!root.join("loam.sqlite3").exists());
    assert!(!root.join("run").exists());
}

#[test]
fn emit_rejects_an_unenrolled_workspace_before_it_reaches_the_connector() {
    let root = temp_dir("emit-unenrolled");
    let operation = br#"{"type":"message.ack","causation_id":"c-1","summary":"Received.","to":[{"kind":"instance","id":"instance-01"}],"payload":{"action":"collaboration.ack","params":{}}}"#;
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let (code, stdout, _stderr) = run_emit(Some(&workspace), &root, operation);
    assert_eq!(code, 78, "{stdout}");
    assert!(stdout.contains("workspace_unenrolled"), "{stdout}");
    assert!(!root.join("run").exists(), "emit opened an endpoint");
}

#[test]
fn emit_requires_a_global_root_and_reports_its_usage() {
    let mut command = Command::new(binary());
    let output = command
        .arg("federation")
        .arg("emit")
        .stdin(Stdio::null())
        .output()
        .expect("emit runs");
    assert_eq!(output.status.code(), Some(64));
    assert!(String::from_utf8_lossy(&output.stderr).contains("--global-root"));
}

#[test]
fn emit_appears_in_the_federation_usage() {
    let output = Command::new(binary())
        .arg("federation")
        .output()
        .expect("federation runs");
    let usage = String::from_utf8_lossy(&output.stderr);
    assert!(usage.contains("loam federation emit"), "{usage}");
}
