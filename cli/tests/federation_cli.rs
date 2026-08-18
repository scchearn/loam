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
        // The org is configuration, never inferred from the remote. These
        // tests are about everything downstream of that, so they pin the org
        // rung explicitly: it keeps a developer's real config.json out of the
        // run and makes each test state which org it federates under.
        // `scope_resolution` below owns the ladder itself.
        .env("LOAM_FEDERATION_ORG", "acme")
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

/// Run `connect` with the identity/config root pinned to a throwaway directory
/// (via `LOAM_CONFIG_DIR`, the rung-1 identity-root resolver). The -global-root
/// the CLI takes drives the connector service, but the machine identity path is
/// governed by the config ladder; pinning it keeps these tests hermetic and
/// never touching the real user config.
fn run_connect_pinned(
    workspace: Option<&Path>,
    broker: &str,
    root: &Path,
    extra: &[&str],
) -> (i32, String, String) {
    let mut command = Command::new(binary());
    command.arg("federation").arg("connect");
    if let Some(ws) = workspace {
        command.arg(ws);
    }
    command.arg(broker);
    command.args(extra);
    command
        .env("LOAM_CONFIG_DIR", root)
        // See `run_connect`: the org rung is pinned, not inferred.
        .env("LOAM_FEDERATION_ORG", "acme")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // The token/auto-enroll path reads the git identity; pin it so this resolves
    // on CI (no ambient global gitconfig) and reaches the signer contact.
    pin_git_identity(&mut command);
    let output = command.output().expect("spawn loam");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// An empty HOME directory, so `git config` finds no global user.email/name and
/// the machine-side enrollment sees no local git identity to name the CSR with.
fn empty_home() -> PathBuf {
    let home = temp_dir("empty-home");
    std::fs::create_dir_all(&home).unwrap();
    home
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

// Skipped on Windows: this full-flow federation-connect integration test has
// never run there (the build did not compile before the cfg-gate fix) and needs
// real Windows validation of the git fixtures and connect resolution. See #121.
#[cfg(not(windows))]
#[test]
fn full_happy_path_validates_against_hermetic_repos() {
    // Build an origin repo with a commit on refs/heads/main, then a workspace
    // clone whose `origin` points at it. Connect resolves the workspace's git
    // binding and infers org/project from the remote URL path without mutating
    // the workspace. (No commit-reachability proof runs; commit_reachability_is_not_required
    // covers that contract.)
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

// Skipped on Windows: federation-connect integration coverage is unvalidated
// there (never compiled before the cfg-gate fix). See #121.
#[cfg(not(windows))]
#[test]
fn the_org_comes_from_configuration_and_the_project_from_the_remote() {
    let root = temp_dir("scope-ladder");
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

    // The remote is `<root>/acme/loam.git`, so the old inference would have
    // read org `acme` from it. It must not: on a real laptop that yielded the
    // repository host's account for every workspace, which is an org the
    // broker's ACL denies — silently, as denied subscribes long after connect.
    let profile = root.join("profile");
    let connect = |env: &[(&str, &str)], extra: &[&str]| -> (i32, String, String) {
        let mut command = Command::new(binary());
        command
            .arg("federation")
            .arg("connect")
            .arg(&work)
            .arg("mqtts://broker.example:8883")
            .args(extra)
            .arg("--json")
            // Pinned so the ladder under test is the only source of an org and
            // the developer's real config.json cannot answer for it.
            .env("LOAM_CONFIG_DIR", &profile)
            .env_remove("LOAM_FEDERATION_ORG")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in env {
            command.env(key, value);
        }
        let output = command.output().expect("spawn loam");
        (
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    };

    // Rung 4: nothing configured. A refusal, not a guess — and a recipe.
    let (code, stdout, stderr) = connect(&[], &[]);
    assert_eq!(
        code, 64,
        "an unconfigured org must refuse: {stdout} {stderr}"
    );
    assert!(
        stdout.contains("\"code\":\"federation_org_unconfigured\""),
        "the refusal must be typed: {stdout}"
    );
    let config_path = profile.join("config.json");
    let config_path = config_path.to_str().unwrap();
    for expected in [config_path, "LOAM_FEDERATION_ORG", "--project"] {
        assert!(
            stdout.contains(expected),
            "the refusal must name every way to fix it, missing {expected}: {stdout}"
        );
    }
    assert!(
        !stdout.contains("\"org_id\":\"acme\""),
        "the remote's path must never become the org: {stdout}"
    );

    // Rung 3: the durable machine setting. The project still comes from the
    // remote, which is the whole point of keeping that half inferred.
    std::fs::create_dir_all(&profile).unwrap();
    std::fs::write(config_path, "{\"org\": \"real-org\"}\n").unwrap();
    let (code, stdout, stderr) = connect(&[], &[]);
    assert_eq!(code, 0, "{stdout} {stderr}");
    assert!(
        stdout.contains("\"org_id\":\"real-org\"") && stdout.contains("\"project_id\":\"loam\""),
        "org from config.json, project from the remote: {stdout}"
    );

    // Rung 2: the environment beats the file.
    let (code, stdout, _stderr) = connect(&[("LOAM_FEDERATION_ORG", "env-org")], &[]);
    assert_eq!(code, 0, "{stdout}");
    assert!(
        stdout.contains("\"org_id\":\"env-org\"") && stdout.contains("\"project_id\":\"loam\""),
        "LOAM_FEDERATION_ORG must outrank config.json: {stdout}"
    );

    // Rung 1: `--project` beats both, and supplies the project too.
    let (code, stdout, _stderr) = connect(
        &[("LOAM_FEDERATION_ORG", "env-org")],
        &["--project", "other-org/other-project"],
    );
    assert_eq!(code, 0, "{stdout}");
    assert!(
        stdout.contains("\"org_id\":\"other-org\"")
            && stdout.contains("\"project_id\":\"other-project\""),
        "the --project override must win over both: {stdout}"
    );

    // A blank setting is not a setting: it must fall through, not become an
    // empty org that publishes to `loam/v1//<project>`.
    std::fs::write(config_path, "{\"org\": \"   \"}\n").unwrap();
    let (code, stdout, _stderr) = connect(&[("LOAM_FEDERATION_ORG", "  ")], &[]);
    assert_eq!(code, 64, "a blank org must refuse: {stdout}");
    assert!(
        stdout.contains("\"code\":\"federation_org_unconfigured\""),
        "{stdout}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

// Skipped on Windows: federation-connect integration coverage is unvalidated
// there (never compiled before the cfg-gate fix). See #121.
#[cfg(not(windows))]
#[test]
fn commit_reachability_is_not_required() {
    // The connect surface deliberately does not prove the HEAD commit is
    // reachable from the origin: the workspace's git state changes after
    // enrollment anyway, and the remote URL alone proves it is a git repo.
    // A workspace whose HEAD is ahead of (or missing from) origin must still
    // validate and infer org/project from the remote path.
    let root = temp_dir("unreachable");
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

    // A workspace whose HEAD is NOT reachable from the origin's main and may
    // not have been pushed: no reachability proof runs, so connect still
    // validates and infers org/project from the remote path.
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
    assert_eq!(code, 0, "{stdout}");
    assert!(
        stdout.contains("\"org_id\":\"acme\"") && stdout.contains("\"project_id\":\"loam\""),
        "the project must come from the remote path and the org from configuration: {stdout}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

// ---------------------------------------------------------------------------
// `connect --token`: the auto-enrollment surface
// ---------------------------------------------------------------------------

#[test]
fn connect_rejects_token_and_token_file_together() {
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let (code, _stdout, stderr) = run_connect(
        Some(&workspace),
        "mqtts://broker.example:8883",
        &["--token", "secret", "--token-file", "/tmp/nope"],
    );
    assert_eq!(code, 64, "mutual exclusion is a usage error: {stderr}");
    assert!(
        stderr.contains("--token and --token-file are mutually exclusive"),
        "got: {stderr}"
    );
}

#[test]
fn connect_rejects_an_unreadable_token_file() {
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let missing = temp_dir("missing-token");
    let (code, _stdout, stderr) = run_connect(
        Some(&workspace),
        "mqtts://broker.example:8883",
        &["--token-file", missing.join("absent").to_str().unwrap()],
    );
    assert_eq!(
        code, 64,
        "an unreadable token file is a usage error: {stderr}"
    );
    assert!(stderr.contains("cannot read --token-file"), "got: {stderr}");
}

/// #94: auto-enrollment failing *before* the network as `signer-unreachable`.
///
/// The pre-network steps are local file and environment work, and folding them
/// into the network refusal sent the investigation to DNS, firewalls, and the
/// broker host while the signer's journal stayed empty. Each must now name
/// itself. These run on every platform, macos-14 included — the one the report
/// came from.
fn connect_with_token(root: &Path, env: &[(&str, &str)], extra: &[&str]) -> (i32, String, String) {
    let mut command = Command::new(binary());
    command
        .arg("federation")
        .arg("connect")
        .arg(env!("CARGO_MANIFEST_DIR"))
        .arg("mqtts://broker.example:8883")
        .arg("--global-root")
        .arg(root)
        .arg("--token")
        .arg("secret")
        .arg("--json")
        .args(extra)
        .env("LOAM_CONFIG_DIR", root)
        .env("LOAM_FEDERATION_ORG", "acme")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    pin_git_identity(&mut command);
    for (key, value) in env {
        command.env(key, value);
    }
    let output = command.output().expect("spawn loam");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn an_unusable_ssl_cert_file_names_the_trust_store_not_the_signer() {
    // The most likely shape of #94: a shell that exports SSL_CERT_FILE (a
    // Homebrew or certifi setup does, and macOS is where those are common)
    // pointing at a file that is gone. Trust anchors are built before anything
    // is dialled, so the signer never sees the attempt — which is exactly the
    // reported symptom, an empty signer journal.
    let root = temp_dir("autoenroll-trust");
    let missing = root.join("no-such-bundle.pem");
    let (code, stdout, stderr) =
        connect_with_token(&root, &[("SSL_CERT_FILE", missing.to_str().unwrap())], &[]);
    assert_eq!(code, 69, "{stdout} {stderr}");
    assert!(
        stdout.contains("trust-anchors-unresolved"),
        "an unresolvable trust store must name itself: {stdout}"
    );
    assert!(
        !stdout.contains("signer-unreachable"),
        "a local trust-store failure must not read as a network one: {stdout}"
    );
    // The detail says which rung answered, which is the whole fix instruction.
    assert!(
        stdout.contains("ssl-cert-file") && stdout.contains("ca-unresolved"),
        "the detail must name the rung and the reason: {stdout}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_signer_url_that_cannot_be_dialled_is_not_an_unreachable_signer() {
    // A typo in LOAM_FEDERATION_SIGNER is a local mistake with a local fix.
    let root = temp_dir("autoenroll-url");
    let (code, stdout, stderr) = connect_with_token(
        &root,
        &[("LOAM_FEDERATION_SIGNER", "http://signer.example/v1/enroll")],
        &[],
    );
    assert_eq!(code, 69, "{stdout} {stderr}");
    assert!(
        stdout.contains("signer-url-invalid") && stdout.contains("not-https"),
        "a non-HTTPS signer URL must name itself: {stdout}"
    );
    assert!(!stdout.contains("signer-unreachable"), "{stdout}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_genuinely_unreachable_signer_still_reads_as_unreachable() {
    // The control for the two above: splitting the local failures out must not
    // have taken the network refusal with it.
    let root = temp_dir("autoenroll-still-unreachable");
    let (code, stdout, stderr) = connect_with_token(&root, &[], &[]);
    assert_eq!(code, 69, "{stdout} {stderr}");
    assert!(
        stdout.contains("signer-unreachable") || stdout.contains("signer-timeout"),
        "a broker host that does not resolve is still a network refusal: {stdout}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn connect_with_token_but_no_certificate_fails_fast_on_an_unreachable_signer() {
    // A fresh machine: no identity bundle, but the operator supplies a token.
    // Auto-enrollment engages and must report the unreachable signer as a typed
    // refusal without writing any partial identity or session state.
    let root = temp_dir("autoenroll-unreachable");
    let identity = root.join("federation").join("identity");
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let (code, stdout, stderr) = run_connect_pinned(
        Some(&workspace),
        "mqtts://broker.example:8883",
        &root,
        &[
            "--global-root",
            root.to_str().unwrap(),
            "--token",
            "secret",
            "--json",
        ],
    );
    assert_eq!(
        code, 69,
        "an unreachable signer is a typed refusal: {stdout} {stderr}"
    );
    assert!(
        stdout.contains("signer-unreachable"),
        "the refusal must name the failing input: {stdout}"
    );
    assert!(
        !identity.join("client.pem").exists(),
        "no certificate may be left behind: {identity:?}"
    );
    assert!(
        !identity.join("key.pem").exists(),
        "no private key may be left behind: {identity:?}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

// Skipped on Windows: the fixture builds the installed layout under a hardcoded
// linux-musl target with a `loam` (no `.exe`) binary, so the Windows install
// probe (host target + `loam.exe`) never finds it; unvalidated on Windows. See
// #121.
#[cfg(not(windows))]
#[test]
fn bare_connect_with_token_uses_the_installed_global_root() {
    let root = temp_dir("autoenroll-installed-root");
    let target_dir = root
        .join("bin")
        .join(env!("CARGO_PKG_VERSION"))
        .join("x86_64-unknown-linux-musl");
    std::fs::create_dir_all(&target_dir).unwrap();
    std::fs::copy(binary(), target_dir.join("loam")).unwrap();
    std::fs::write(root.join("install.json"), "{}\n").unwrap();

    let mut command = Command::new(target_dir.join("loam"));
    command
        .args([
            "federation",
            "connect",
            env!("CARGO_MANIFEST_DIR"),
            "mqtts://broker.example:8883",
            "--token",
            "secret",
            "--json",
        ])
        .env("LOAM_CONFIG_DIR", &root)
        // See `run_connect`: the org is configuration and never inferred.
        .env("LOAM_FEDERATION_ORG", "acme")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // The token/auto-enroll path reads the git identity; pin it so this resolves
    // on CI (no ambient global gitconfig) and reaches the signer contact.
    pin_git_identity(&mut command);
    let output = command.output().expect("spawn installed loam");
    let code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        code, 69,
        "bare connect must run enrollment: {stdout} {stderr}"
    );
    assert!(
        stdout.contains("signer-unreachable"),
        "the token path must contact the signer instead of validating only: {stdout}"
    );
    assert!(!stdout.contains("\"status\":\"validated\""), "{stdout}");
    assert!(!root.join("loam.sqlite3").exists());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn bare_connect_with_token_outside_an_install_requires_global_root() {
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let (code, _stdout, stderr) = run_connect(
        Some(&workspace),
        "mqtts://broker.example:8883",
        &["--token", "secret"],
    );
    assert_eq!(code, 64);
    assert!(stderr.contains("--global-root is required"), "{stderr}");
}

#[test]
fn connect_with_token_missing_git_identity_names_the_typed_refusal() {
    // A workspace with no git user.email cannot name the CSR subject; this must
    // be a typed git-identity-required refusal, not a silent partial state. The
    // machine's git must see NO identity, so HOME is pinned to an empty dir
    // (no global .gitconfig to fall back to).
    let root = temp_dir("autoenroll-noident");
    let work = root.join("work");
    git(&["init", "--quiet", work.to_str().unwrap()], None);
    // Validation infers org/project from the origin remote; give the workspace
    // one, but deliberately no user.email so the CSR subject cannot be named.
    git(
        &[
            "-C",
            work.to_str().unwrap(),
            "remote",
            "add",
            "origin",
            "git@example.org:acme/loam.git",
        ],
        None,
    );
    // Spawn with both config and HOME pinned.
    let mut command = Command::new(binary());
    let output = command
        .arg("federation")
        .arg("connect")
        .arg(&work)
        .arg("mqtts://broker.example:8883")
        .arg("--global-root")
        .arg(&root)
        .arg("--token")
        .arg("secret")
        .arg("--json")
        .env("LOAM_CONFIG_DIR", &root)
        // See `run_connect`: the org is configuration and never inferred.
        .env("LOAM_FEDERATION_ORG", "acme")
        .env("HOME", empty_home())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("spawn loam");
    let code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert_eq!(
        code,
        69,
        "{stdout} {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("git-identity-required"),
        "a missing git identity must be its own typed refusal: {stdout}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

// ---------------------------------------------------------------------------
// `loam federation emit`
// ---------------------------------------------------------------------------

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

/// Inject a deterministic git identity into a spawned command's environment.
/// Enrollment reads `git config user.email/user.name`, and setup commits read
/// the same identity — on CI, which has no ambient global gitconfig, both would
/// otherwise resolve nothing and the token/auto-enroll path would refuse with
/// `git-identity-required` instead of exercising the signer contact. `GIT_CONFIG_*`
/// entries are parsed after every config file, so they win regardless of the
/// host's global config: hermetic on CI and on a developer machine alike, and
/// without mutating the real workspace's git config.
fn pin_git_identity(command: &mut Command) {
    command
        .env("GIT_CONFIG_COUNT", "2")
        .env("GIT_CONFIG_KEY_0", "user.name")
        .env("GIT_CONFIG_VALUE_0", "Loam CI")
        .env("GIT_CONFIG_KEY_1", "user.email")
        .env("GIT_CONFIG_VALUE_1", "ci@loam.test");
}

// Only the git-fixture tests call this, and those are skipped on Windows (see
// #121). Keep it compiled — so the helpers it references stay live — but let it
// be unused on Windows.
#[cfg_attr(windows, allow(dead_code))]
fn git(args: &[&str], cwd: Option<&Path>) -> String {
    let mut command = Command::new("git");
    command.args(args);
    if let Some(dir) = cwd {
        command.current_dir(dir);
    }
    pin_git_identity(&mut command);
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
