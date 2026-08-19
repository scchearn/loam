//! The `loam hook <harness>` read path, driven through the built
//! binary exactly as a harness invokes it — event JSON on stdin, the native
//! response envelope on stdout.
//!
//! This is the CI tier: real CLI, real workspace resolution, no broker and no
//! installed harness. The connector-live half is proven against a real broker
//! and a real installed harness in the e2e suite; the structural "this path cannot publish"
//! property is proven by the cannot-publish test.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use loam::json::Value;

const FRAMES: &str = include_str!("fixtures/mqtt/harness-hook-frames.json");

fn binary() -> PathBuf {
    let mut path = std::env::current_exe().expect("test exe path");
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    path.join(if cfg!(windows) { "loam.exe" } else { "loam" })
}

fn temp_root(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "loam-hook-{label}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The repository this test runs in: a real Git workspace, so workspace
/// canonicalization is exercised rather than stubbed.
fn workspace() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("cli/ has a parent")
        .to_path_buf()
}

/// A global root with no registry and no connector endpoint, plus a skills root
/// carrying the one file the baseline context reads.
fn installation(label: &str) -> (PathBuf, PathBuf) {
    let global_root = temp_root(label);
    std::fs::write(
        global_root.join("install.json"),
        r#"{"plugin_version":"9.9.9"}"#,
    )
    .unwrap();
    let skills_root = global_root.join("skills");
    let using = skills_root.join("loam-using");
    std::fs::create_dir_all(&using).unwrap();
    std::fs::write(
        using.join("SKILL.md"),
        "---\nname: loam-using\n---\n# Using loam\n\nSKILL-BODY-MARKER\n",
    )
    .unwrap();
    (global_root, skills_root)
}

struct Run {
    stdout: String,
    stderr: String,
    status: i32,
}

fn run_hook(harness: &str, stdin: &[u8], global_root: &Path, skills_root: &Path) -> Run {
    run_hook_env(harness, stdin, global_root, skills_root, None)
}

/// [`run_hook`] with an optional isolated `LOAM_CONFIG_DIR`. The federation
/// registry resolves through the config-dir ladder, so a test that asserts on
/// the recorded outcome must pin an empty config root — otherwise it reads the
/// running machine's live federation config (the #130 hermeticity class) and a
/// developer's enrolled laptop flips the outcome from `succeeded` to
/// `continued`. An empty config dir resolves as unenrolled: baseline renders,
/// the run records `succeeded`.
fn run_hook_env(
    harness: &str,
    stdin: &[u8],
    global_root: &Path,
    skills_root: &Path,
    config_dir: Option<&Path>,
) -> Run {
    let mut command = Command::new(binary());
    command
        .args(["hook", harness])
        .env("LOAM_HOME", global_root)
        .env("LOAM_SKILLS_ROOT", skills_root);
    if let Some(config_dir) = config_dir {
        command.env("LOAM_CONFIG_DIR", config_dir);
    }
    let mut child = command
        .current_dir(workspace())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("hook spawns");
    // A hook that refuses *before* reading stdin — an unknown harness id is
    // exactly that case — closes the pipe under this write. Losing the race is
    // the behaviour under test, not a failure; every other IO error still is.
    match child.stdin.as_mut().expect("stdin").write_all(stdin) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => {}
        Err(error) => panic!("write the hook frame: {error}"),
    }
    let output = child.wait_with_output().expect("hook completes");
    Run {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        status: output.status.code().unwrap_or(-1),
    }
}

/// A path as a JSON *string body* — escaped, without the surrounding quotes.
/// A Windows path is full of backslashes, and pasting one straight into a hand
/// built frame produces invalid escapes (`\\U`, `\\l`), so the hook correctly
/// refuses the frame as malformed and the test asserts against an empty body
/// for entirely the wrong reason. Real harnesses emit escaped JSON; the test
/// must too.
fn json_path(path: &Path) -> String {
    let quoted = Value::String(path.to_string_lossy().into_owned()).to_json();
    quoted[1..quoted.len() - 1].to_owned()
}

fn fixture() -> Value {
    loam::json::parse(FRAMES).expect("frame corpus parses")
}

fn cases<'a>(fixture: &'a Value, key: &str) -> &'a [Value] {
    fixture
        .get(key)
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("corpus has `{key}`"))
}

fn text(case: &Value, key: &str) -> String {
    case.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// Pull the context body back out of whatever envelope this harness uses.
fn body_of(harness: &Value, stdout: &str) -> String {
    match harness.get("key").and_then(Value::as_str) {
        None => stdout.trim_end().to_owned(),
        Some("additionalContext") => {
            let parsed = loam::json::parse(stdout.trim()).expect("claude envelope is JSON");
            parsed
                .get("hookSpecificOutput")
                .and_then(|inner| inner.get("additionalContext"))
                .and_then(Value::as_str)
                .expect("claude envelope carries additionalContext")
                .to_owned()
        }
        Some(key) => {
            let parsed = loam::json::parse(stdout.trim()).expect("envelope is JSON");
            parsed
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("envelope carries `{key}`"))
                .to_owned()
        }
    }
}

#[test]
fn every_harness_returns_its_native_envelope_for_every_frame_shape() {
    let corpus = fixture();
    let (global_root, skills_root) = installation("envelopes");
    let root = workspace();
    let root = json_path(&root);

    for harness in cases(&corpus, "harnesses") {
        let id = harness.get("id").and_then(Value::as_str).expect("id");
        for case in cases(&corpus, "frames") {
            let name = text(case, "name");
            let stdin = text(case, "text").replace("WORKSPACE", &root);
            let run = run_hook(id, stdin.as_bytes(), &global_root, &skills_root);
            assert_eq!(run.status, 0, "{id}/{name}: {}", run.stderr);

            let body = body_of(harness, &run.stdout);
            // The complete baseline, on every harness and every frame shape.
            assert!(body.starts_with("<LOAM_IMPORTANT>"), "{id}/{name}: {body}");
            assert!(body.ends_with("</LOAM_IMPORTANT>"), "{id}/{name}: {body}");
            assert!(body.contains("You have loam (v9.9.9)."), "{id}/{name}");
            assert!(body.contains("SKILL-BODY-MARKER"), "{id}/{name}: {body}");
            assert!(
                !body.contains("name: loam-using"),
                "{id}/{name}: frontmatter"
            );
            assert!(body.contains("Native runtime command: "), "{id}/{name}");
            assert!(body.contains("## Workspace state"), "{id}/{name}");
            assert!(body.contains("## Federation"), "{id}/{name}");
        }
    }
}

#[test]
fn the_same_snapshot_produces_one_body_across_all_four_harnesses() {
    let corpus = fixture();
    let (global_root, skills_root) = installation("identical-body");
    let root = workspace();
    let stdin = format!(r#"{{"cwd":"{}"}}"#, json_path(&root));

    let mut bodies = Vec::new();
    for harness in cases(&corpus, "harnesses") {
        let id = harness.get("id").and_then(Value::as_str).expect("id");
        let run = run_hook(id, stdin.as_bytes(), &global_root, &skills_root);
        assert_eq!(run.status, 0, "{id}: {}", run.stderr);
        bodies.push(body_of(harness, &run.stdout));
    }
    // Equality alone would pass on four identically-empty bodies.
    assert!(
        bodies[0].contains("SKILL-BODY-MARKER"),
        "no baseline in the rendered body: {:?}",
        bodies[0]
    );
    // One shared renderer: only the envelope key differs, never the text.
    assert!(
        bodies.windows(2).all(|pair| pair[0] == pair[1]),
        "harness bodies diverged: {bodies:#?}"
    );
}

#[test]
fn malformed_and_oversized_frames_are_refused_with_a_bounded_diagnostic() {
    let corpus = fixture();
    let (global_root, skills_root) = installation("malformed");

    for case in cases(&corpus, "malformed") {
        let name = text(case, "name");
        let code = text(case, "code");
        let stdin: Vec<u8> = if let Some(bytes) = case.get("bytes").and_then(Value::as_array) {
            bytes
                .iter()
                .map(|value| match value {
                    Value::Number(literal) => literal.parse::<u8>().expect("byte"),
                    _ => panic!("{name}: bytes must be numbers"),
                })
                .collect()
        } else if let Some(Value::Number(byte)) = case.get("repeat_byte") {
            let count = match case.get("repeat_count") {
                Some(Value::Number(literal)) => literal.parse::<usize>().expect("count"),
                _ => panic!("{name}: repeat_byte needs repeat_count"),
            };
            vec![byte.parse::<u8>().expect("byte"); count]
        } else {
            text(case, "text").into_bytes()
        };

        for harness in cases(&corpus, "harnesses") {
            let id = harness.get("id").and_then(Value::as_str).expect("id");
            let run = run_hook(id, &stdin, &global_root, &skills_root);
            // A refused frame never fails the session.
            assert_eq!(run.status, 0, "{id}/{name}");
            // Exactly the stable code, and nothing of the offending payload.
            assert!(
                run.stderr.contains(&code),
                "{id}/{name}: expected `{code}`, got `{}`",
                run.stderr
            );
            assert!(run.stderr.len() < 256, "{id}/{name}: unbounded diagnostic");
            assert!(
                !run.stderr.contains("not json"),
                "{id}/{name}: echoed input"
            );
            // No payload rendered at all.
            let body = body_of(harness, &run.stdout);
            assert!(body.is_empty(), "{id}/{name}: rendered `{body}`");
        }
    }
}

#[test]
fn an_unknown_harness_id_is_refused_before_anything_is_read() {
    let (global_root, skills_root) = installation("unknown-harness");
    for id in ["copilot", "", "claude-code", "../claude"] {
        let run = run_hook(id, b"{}", &global_root, &skills_root);
        assert_eq!(run.status, 1, "{id}");
        assert!(run.stdout.is_empty(), "{id}: {}", run.stdout);
        assert!(run.stderr.contains("Usage: loam hook"), "{id}");
    }
}

#[test]
fn a_workspace_with_no_connector_still_gets_its_baseline_and_says_federation_is_off() {
    // The regression the slice's sharpest risk names: retiring the Node
    // integration must not cost a session its ordinary Loam context whenever
    // federation is not available.
    let (global_root, skills_root) = installation("no-connector");
    let root = workspace();
    let stdin = format!(r#"{{"cwd":"{}"}}"#, json_path(&root));
    let run = run_hook("claude", stdin.as_bytes(), &global_root, &skills_root);
    assert_eq!(run.status, 0, "{}", run.stderr);

    let body = body_of(
        &loam::json::parse(r#"{"id":"claude","key":"additionalContext"}"#).unwrap(),
        &run.stdout,
    );
    assert!(body.contains("SKILL-BODY-MARKER"), "{body}");
    assert!(body.contains("Native runtime command: "), "{body}");
    assert!(body.contains("## Workspace state"), "{body}");
    assert!(
        body.contains(&format!("Workspace: {}", root.to_str().unwrap())),
        "{body}"
    );
    // Federation is off, and honestly so — not a fabricated live claim.
    assert!(body.contains("federation: unenrolled"), "{body}");
    assert!(!body.contains("federation: live"), "{body}");
    // The baseline is the bulk of the answer, not a federation-only stub.
    assert!(
        body.find("## Workspace state") < body.find("## Federation"),
        "{body}"
    );
    // Nothing was started and nothing was created under the global root.
    assert!(
        !global_root.join("run").exists(),
        "the hook opened an endpoint"
    );
    // #136: the read path now records itself in the hook_run ledger at
    // <global-root>/loam.sqlite3 — the hook-event DB, not the enrollment
    // registry (which lives in the config dir). This one diagnostic write is
    // the whole point of the ledger; the "federation: unenrolled" line above
    // already proves no enrollment was fabricated.
    assert!(
        global_root.join("loam.sqlite3").exists(),
        "the hook recorded its run in the ledger"
    );
}

#[test]
fn a_non_git_workspace_degrades_without_failing_the_session() {
    let (global_root, skills_root) = installation("non-git");
    let outside = temp_root("not-a-repo");
    let stdin = format!(r#"{{"cwd":"{}"}}"#, json_path(&outside));
    let run = run_hook("cursor", stdin.as_bytes(), &global_root, &skills_root);
    assert_eq!(run.status, 0, "{}", run.stderr);
    let body = body_of(
        &loam::json::parse(r#"{"id":"cursor","key":"additional_context"}"#).unwrap(),
        &run.stdout,
    );
    assert!(body.contains("SKILL-BODY-MARKER"), "{body}");
    assert!(body.contains("federation: unenrolled"), "{body}");
}

/// Query the run ledger the way an operator diagnosing a lane would: shell
/// `loam hooks list <global-root>` with the given filters, return its stdout
/// (one JSON object per matching row).
fn hooks_list(global_root: &Path, filters: &[&str]) -> String {
    let output = Command::new(binary())
        .args(["hooks", "list"])
        .arg(global_root)
        .args(filters)
        .output()
        .expect("hooks list runs");
    assert!(
        output.status.success(),
        "hooks list failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// An isolated empty config root, so the federation ladder resolves unenrolled
/// instead of reading the running machine's live config (the #130 class).
fn isolated_config(label: &str) -> PathBuf {
    temp_root(&format!("{label}-cfg"))
}

#[test]
fn a_rendered_invocation_records_a_succeeded_run() {
    // #136 acceptance: after a session where the hook fired, the ledger shows a
    // row — so a later absence is meaningful. A normal (unenrolled) workspace
    // renders the baseline and records `succeeded`, with the session id, event,
    // and runtime version the diagnosis needs.
    let (global_root, skills_root) = installation("ledger-rendered");
    let config = isolated_config("ledger-rendered");
    let stdin = format!(
        r#"{{"cwd":"{}","session_id":"sess-rendered"}}"#,
        json_path(&workspace())
    );
    let run = run_hook_env(
        "claude",
        stdin.as_bytes(),
        &global_root,
        &skills_root,
        Some(&config),
    );
    assert_eq!(run.status, 0, "{}", run.stderr);

    let rows = hooks_list(
        &global_root,
        &[
            "--harness",
            "claude",
            "--hook",
            "session_start",
            "--status",
            "succeeded",
        ],
    );
    let line = rows
        .lines()
        .next()
        .unwrap_or_else(|| panic!("no succeeded run recorded:\n{rows}"));
    let row = loam::json::parse(line).expect("row is json");
    assert_eq!(row.get("harness").and_then(Value::as_str), Some("claude"));
    assert_eq!(
        row.get("hook").and_then(Value::as_str),
        Some("session_start")
    );
    assert_eq!(row.get("status").and_then(Value::as_str), Some("succeeded"));
    assert_eq!(
        row.get("session_id").and_then(Value::as_str),
        Some("sess-rendered")
    );
    assert_eq!(
        row.get("runtime_version").and_then(Value::as_str),
        Some(env!("CARGO_PKG_VERSION"))
    );
    // Duration is recorded (finished - started), never null.
    assert!(
        line.contains("\"duration_ms\":") && !line.contains("\"duration_ms\":null"),
        "the run duration is recorded: {line}"
    );
}

#[test]
fn an_unparseable_frame_records_a_failed_run() {
    // #136: the error paths are the ones that matter for diagnosis. A frame the
    // hook cannot parse still exits 0 (fail-open render) and records a `failed`
    // row carrying the refuse code.
    let (global_root, skills_root) = installation("ledger-error");
    let config = isolated_config("ledger-error");
    let run = run_hook_env(
        "claude",
        b"this is not json",
        &global_root,
        &skills_root,
        Some(&config),
    );
    assert_eq!(
        run.status, 0,
        "a refused frame still exits 0: {}",
        run.stderr
    );

    let rows = hooks_list(&global_root, &["--harness", "claude", "--status", "failed"]);
    let line = rows
        .lines()
        .next()
        .unwrap_or_else(|| panic!("no failed run recorded:\n{rows}"));
    let row = loam::json::parse(line).expect("row is json");
    assert_eq!(row.get("status").and_then(Value::as_str), Some("failed"));
    assert!(
        row.get("detail").and_then(Value::as_str).is_some(),
        "the failed row carries the refuse code: {line}"
    );
}

#[test]
fn an_unwritable_ledger_never_fails_the_hook() {
    // #136 fail-open, end to end: a ledger the hook cannot write must not fail
    // or block the render. A directory where the DB file must be makes every
    // ledger open fail; the hook still returns its baseline envelope, exit 0.
    let (global_root, skills_root) = installation("ledger-failopen");
    let config = isolated_config("ledger-failopen");
    std::fs::create_dir_all(global_root.join("loam.sqlite3")).unwrap();
    let stdin = format!(r#"{{"cwd":"{}"}}"#, json_path(&workspace()));
    let run = run_hook_env(
        "claude",
        stdin.as_bytes(),
        &global_root,
        &skills_root,
        Some(&config),
    );
    assert_eq!(
        run.status, 0,
        "an unwritable ledger must not fail the hook: {}",
        run.stderr
    );
    assert!(
        run.stdout.contains("SKILL-BODY-MARKER"),
        "the hook still renders the baseline: {}",
        run.stdout
    );
}
