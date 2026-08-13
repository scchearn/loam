//! `loam federation` CLI integration tests.
//!
//! `cli` is a bin-only crate, so these drive the built binary through
//! `std::process::Command`. This suite covers the one-command `connect`
//! surface (`<workspace> <broker>`, org/project inferred from the git remote,
//! overridable with `--project`), typed JSON errors, exit codes, and a full
//! happy path against hermetic local Git repositories.

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

fn run_connect(workspace: Option<&Path>, broker: &str, extra: &[&str]) -> (i32, String, String) {
    let mut command = Command::new(binary());
    command.arg("federation").arg("connect");
    if let Some(ws) = workspace {
        command.arg(ws);
    }
    command.arg(broker);
    command.args(extra);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = command.output().expect("spawn loam");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn connect_requires_a_workspace_and_a_broker() {
    let (code, _stdout, stderr) = run_connect(None, "", &[]);
    assert_eq!(code, 64, "missing positional arguments are a usage error");
    assert!(
        stderr.contains("<workspace> and <broker> are required"),
        "got: {stderr}"
    );
}

#[test]
fn connect_rejects_a_plaintext_broker_endpoint() {
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let (code, stdout, _stderr) =
        run_connect(Some(&workspace), "mqtt://broker.example:8883", &["--json"]);
    assert_eq!(code, 65, "a plaintext endpoint is a usage/input violation");
    assert!(
        stdout.contains("descriptor_invalid_endpoint"),
        "expected typed code, got: {stdout}"
    );
}

#[test]
fn connect_rejects_a_malformed_project_override() {
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let (code, stdout, _stderr) = run_connect(
        Some(&workspace),
        "mqtts://broker.example:8883",
        &["--project", "no-slash", "--json"],
    );
    assert_eq!(code, 65);
    assert!(
        stdout.contains("descriptor_invalid_field"),
        "expected typed code, got: {stdout}"
    );
}

#[test]
fn full_happy_path_validates_against_hermetic_repos() {
    // Build an origin repo with a commit on refs/heads/main, then a workspace
    // clone whose `origin` points at it. The connect's commit must be proven
    // reachable in an isolated temp repo without mutating the workspace, and
    // org/project must be inferred from the remote URL path.
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
    // A bare repo's HEAD defaults to the unborn `master`; point it at main so
    // the workspace clone checks out a working tree.
    git(
        &[
            "-C",
            origin.to_str().unwrap(),
            "symbolic-ref",
            "HEAD",
            "refs/heads/main",
        ],
        None,
    );

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

    let (code, stdout, stderr) =
        run_connect(Some(&work), "mqtts://broker.example:8883", &["--json"]);
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
fn connect_infers_org_and_project_from_the_remote_url() {
    let root = temp_dir("infer");
    let origin = root.join("acme").join("loam.git");
    std::fs::create_dir_all(origin.parent().unwrap()).unwrap();
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
            "-C",
            origin.to_str().unwrap(),
            "symbolic-ref",
            "HEAD",
            "refs/heads/main",
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

    let (code, stdout, _stderr) =
        run_connect(Some(&work), "mqtts://broker.example:8883", &["--json"]);
    assert_eq!(code, 0, "{stdout}");
    assert!(
        stdout.contains("\"org_id\":\"acme\"") && stdout.contains("\"project_id\":\"loam\""),
        "org/project must be inferred from the remote path: {stdout}"
    );

    // The override wins over the remote inference.
    let (code, stdout, _stderr) = run_connect(
        Some(&work),
        "mqtts://broker.example:8883",
        &["--project", "other-org/other-project", "--json"],
    );
    assert_eq!(code, 0, "{stdout}");
    assert!(
        stdout.contains("\"org_id\":\"other-org\"")
            && stdout.contains("\"project_id\":\"other-project\""),
        "the --project override must win: {stdout}"
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
            "-C",
            origin.to_str().unwrap(),
            "symbolic-ref",
            "HEAD",
            "refs/heads/main",
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

    // A workspace whose HEAD is not reachable from the origin's main: the
    // reachability proof must refuse it.
    std::fs::write(work.join("unpushed.txt"), "not pushed").unwrap();
    git(&["-C", work.to_str().unwrap(), "add", "."], None);
    git(
        &[
            "-C",
            work.to_str().unwrap(),
            "commit",
            "--quiet",
            "-m",
            "unpushed",
        ],
        None,
    );
    let (code, stdout, _stderr) =
        run_connect(Some(&work), "mqtts://broker.example:8883", &["--json"]);
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
