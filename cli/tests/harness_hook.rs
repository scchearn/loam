//! Slice D T2: the `loam hook <harness>` read path, driven through the built
//! binary exactly as a harness invokes it — event JSON on stdin, the native
//! response envelope on stdout.
//!
//! This is the T1 CI tier: real CLI, real workspace resolution, no broker and no
//! installed harness. The connector-live half is proven against a real broker
//! and a real installed harness at T8; the structural "this path cannot publish"
//! property is proven at T4.

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
    let mut child = Command::new(binary())
        .args(["hook", harness])
        .env("LOAM_HOME", global_root)
        .env("LOAM_SKILLS_ROOT", skills_root)
        .current_dir(workspace())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("hook spawns");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(stdin)
        .expect("write stdin");
    let output = child.wait_with_output().expect("hook completes");
    Run {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        status: output.status.code().unwrap_or(-1),
    }
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
    let root = root.to_str().unwrap();

    for harness in cases(&corpus, "harnesses") {
        let id = harness.get("id").and_then(Value::as_str).expect("id");
        for case in cases(&corpus, "frames") {
            let name = text(case, "name");
            let stdin = text(case, "text").replace("WORKSPACE", root);
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
    let stdin = format!(r#"{{"cwd":"{}"}}"#, root.to_str().unwrap());

    let mut bodies = Vec::new();
    for harness in cases(&corpus, "harnesses") {
        let id = harness.get("id").and_then(Value::as_str).expect("id");
        let run = run_hook(id, stdin.as_bytes(), &global_root, &skills_root);
        assert_eq!(run.status, 0, "{id}: {}", run.stderr);
        bodies.push(body_of(harness, &run.stdout));
    }
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
    let stdin = format!(r#"{{"cwd":"{}"}}"#, root.to_str().unwrap());
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
    assert!(
        !global_root.join("loam.sqlite3").exists(),
        "the hook created a registry"
    );
}

#[test]
fn a_non_git_workspace_degrades_without_failing_the_session() {
    let (global_root, skills_root) = installation("non-git");
    let outside = temp_root("not-a-repo");
    let stdin = format!(r#"{{"cwd":"{}"}}"#, outside.to_str().unwrap());
    let run = run_hook("cursor", stdin.as_bytes(), &global_root, &skills_root);
    assert_eq!(run.status, 0, "{}", run.stderr);
    let body = body_of(
        &loam::json::parse(r#"{"id":"cursor","key":"additional_context"}"#).unwrap(),
        &run.stdout,
    );
    assert!(body.contains("SKILL-BODY-MARKER"), "{body}");
    assert!(body.contains("federation: unenrolled"), "{body}");
}
