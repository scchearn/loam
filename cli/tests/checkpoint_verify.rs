//! `loam checkpoint verify` — behavioural contract.
//!
//! The legacy `checkpoint-verify` Bash script is the byte-for-byte oracle; see
//! `checkpoint_parity.rs` for the Linux comparison against it. These tests pin
//! the platform-neutral surface so macOS and Windows are covered too.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn temporary_directory(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be after epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("loam-chkverify-{label}-{nonce}"));
    fs::create_dir_all(&path).expect("temporary directory should be created");
    path
}

fn write(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("parent directory should be created");
    }
    fs::write(path, content).expect("file should be written");
}

fn verify(note: &Path) -> (i32, String, String) {
    let binary = std::env::var("CARGO_BIN_EXE_loam").expect("cargo should provide the loam binary");
    let output = Command::new(binary)
        .args(["checkpoint", "verify", note.to_str().unwrap()])
        .output()
        .expect("loam should run");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// A note with no pointers, so the output is identical on every platform.
fn note(body: &str) -> String {
    format!("# Checkpoint\n\n{body}\n\n## Workstreams\n\n### Sample\n- Status: ready-to-resume\n- Next: Do the next operational step.\n")
}

const HEALTHY_FRONT_MATTER: &str = "- Captured: 2026-06-23 15:04 SAST\n- Reason: pause\n- Scope: sample scope\n- Format: v1\n- Intended return: pick up the thing I named";

#[test]
fn healthy_note_reports_every_field_passing_and_exits_zero() {
    let directory = temporary_directory("healthy");
    let path = directory.join("checkpoint-2026-06-23-1504.md");
    write(&path, &note(HEALTHY_FRONT_MATTER));

    let (code, stdout, stderr) = verify(&path);

    assert_eq!(code, 0, "stdout: {stdout}");
    assert_eq!(stderr, "");
    assert_eq!(
        stdout,
        "FRONTMATTER Format: PASS\n\
         FRONTMATTER Captured: PASS\n\
         FRONTMATTER Reason: PASS\n\
         FRONTMATTER Scope: PASS\n\
         FRONTMATTER Intended return: PRESENT\n\
         WORKSTREAM Sample\n\
         \x20 WORKSTREAM Sample Status: PASS\n\
         \x20 WORKSTREAM Sample Next: PASS\n\
         \n\
         === Pointer checks ===\n\
         \x20 (no pointers found)\n"
    );
}

#[test]
fn absent_intended_return_is_reported_as_none_recorded() {
    let directory = temporary_directory("no-intent");
    let path = directory.join("note.md");
    write(
        &path,
        &note("- Captured: 2026-06-23 15:04 SAST\n- Reason: pause\n- Scope: sample scope\n- Format: v1"),
    );

    let (code, stdout, _) = verify(&path);

    assert_eq!(code, 0);
    assert!(
        stdout.contains("FRONTMATTER Intended return: none recorded\n"),
        "output: {stdout}"
    );
    assert!(
        !stdout.contains("Intended return: PRESENT"),
        "output: {stdout}"
    );
}

#[test]
fn every_frontmatter_failure_mode_still_exits_zero() {
    let directory = temporary_directory("failures");
    let path = directory.join("note.md");
    write(
        &path,
        &note("- Reason: nonsense\n- Format: v2\n- Previous: none\n- Supersedes: None"),
    );

    let (code, stdout, _) = verify(&path);

    assert_eq!(code, 0, "verify must never block a save: {stdout}");
    assert!(
        stdout.starts_with("FORMAT: unknown \u{2014} got \"v2\" (expected v1)\n"),
        "output: {stdout}"
    );
    assert!(stdout.contains("FRONTMATTER Captured: FAIL: missing\n"));
    assert!(stdout.contains(
        "FRONTMATTER Reason: FAIL: not in {shutdown,pause,handoff,context-switch}\n  WARN: nonsense\n"
    ));
    assert!(stdout.contains("FRONTMATTER Scope: FAIL: missing\n"));
    assert!(stdout
        .contains("WARN: \"Previous: none\" literal \u{2014} should be omitted, not written\n"));
    assert!(stdout
        .contains("WARN: \"Supersedes: none\" literal \u{2014} should be omitted, not written\n"));
}

#[test]
fn missing_format_field_reports_the_empty_value() {
    let directory = temporary_directory("no-format");
    let path = directory.join("note.md");
    write(&path, &note("- Captured: 2026-06-23 15:04 SAST"));

    let (code, stdout, _) = verify(&path);

    assert_eq!(code, 0);
    assert!(
        stdout.starts_with("FORMAT: unknown \u{2014} got \"\" (expected v1)\n"),
        "output: {stdout}"
    );
}

#[test]
fn every_reason_in_the_enum_passes() {
    for reason in ["shutdown", "pause", "handoff", "context-switch"] {
        let directory = temporary_directory(&format!("reason-{reason}"));
        let path = directory.join("note.md");
        write(&path, &note(&format!("- Format: v1\n- Reason: {reason}")));

        let (code, stdout, _) = verify(&path);

        assert_eq!(code, 0);
        assert!(
            stdout.contains("FRONTMATTER Reason: PASS\n"),
            "reason {reason} should pass: {stdout}"
        );
    }
}

#[test]
fn workstream_without_status_is_flagged_in_first_and_last_position() {
    // Two distinct code paths in the oracle: the flush-on-next-title branch and
    // the flush-after-loop branch.
    let directory = temporary_directory("no-status");
    let path = directory.join("note.md");
    write(
        &path,
        "# Checkpoint\n\n- Format: v1\n\n## Workstreams\n\n### First\n- Next: something\n### Last\n- Next: something else\n",
    );

    let (code, stdout, _) = verify(&path);

    assert_eq!(code, 0);
    assert!(
        stdout.contains("WORKSTREAM First\n  WORKSTREAM First Next: PASS\n  WORKSTREAM First (no status field found)\nWORKSTREAM Last\n"),
        "output: {stdout}"
    );
    assert!(
        stdout.contains("  WORKSTREAM Last (no status field found)\n"),
        "output: {stdout}"
    );
}

#[test]
fn workstream_status_enum_and_empty_next_are_reported() {
    let directory = temporary_directory("ws-enum");
    let path = directory.join("note.md");
    write(
        &path,
        "# Checkpoint\n\n- Format: v1\n\n## Workstreams\n\n### Only\n- Status: bogus\n- Next:\n",
    );

    let (code, stdout, _) = verify(&path);

    assert_eq!(code, 0);
    assert!(
        stdout.contains("  WORKSTREAM Only Status: FAIL: bogus not in enum\n"),
        "output: {stdout}"
    );
    assert!(
        stdout.contains("  WORKSTREAM Only Next: FAIL: empty or missing\n"),
        "output: {stdout}"
    );
}

#[test]
fn all_workstream_statuses_in_the_enum_pass() {
    for status in ["active", "blocked", "waiting", "ready-to-resume", "done"] {
        let directory = temporary_directory(&format!("status-{status}"));
        let path = directory.join("note.md");
        write(
            &path,
            &format!("# Checkpoint\n\n- Format: v1\n\n## Workstreams\n\n### Only\n- Status: {status}\n- Next: go\n"),
        );

        let (code, stdout, _) = verify(&path);

        assert_eq!(code, 0);
        assert!(
            stdout.contains("  WORKSTREAM Only Status: PASS\n"),
            "status {status} should pass: {stdout}"
        );
    }
}

#[test]
fn headings_after_the_workstreams_section_are_ignored() {
    let directory = temporary_directory("scoped");
    let path = directory.join("note.md");
    write(
        &path,
        "# Checkpoint\n\n- Format: v1\n\n## Workstreams\n\n### Real\n- Status: active\n- Next: go\n\n## Other\n\n### Decoy\n- Status: bogus\n",
    );

    let (code, stdout, _) = verify(&path);

    assert_eq!(code, 0);
    assert!(stdout.contains("WORKSTREAM Real\n"), "output: {stdout}");
    assert!(!stdout.contains("Decoy"), "output: {stdout}");
}

#[test]
fn comma_separated_and_indented_pointers_are_both_collected() {
    let directory = temporary_directory("pointers");
    let existing = directory.join("present.md");
    write(&existing, "# Present\n");
    let path = directory.join("note.md");
    write(
        &path,
        "# Checkpoint\n\n- Format: v1\n\n## Workstreams\n\n### Only\n- Status: active\n- Next: go\n- Pointers: ./present.md, ./absent.md\n  - ./also-absent.md\n",
    );

    let (code, stdout, _) = verify(&path);

    assert_eq!(code, 0);
    assert!(
        stdout.contains("  POINTER ./present.md: OK\n"),
        "output: {stdout}"
    );
    assert!(
        stdout.contains("  POINTER ./absent.md: MISSING: ./absent.md\n"),
        "output: {stdout}"
    );
    assert!(
        stdout.contains("  POINTER ./also-absent.md: MISSING: ./also-absent.md\n"),
        "output: {stdout}"
    );
}

#[test]
fn trailing_parenthetical_context_is_stripped_before_resolution() {
    let directory = temporary_directory("parenthetical");
    write(&directory.join("present.md"), "# Present\n");
    let path = directory.join("note.md");
    write(
        &path,
        "# Checkpoint\n\n- Format: v1\n\n## Workstreams\n\n### Only\n- Status: active\n- Next: go\n- Pointers: ./present.md (events 1-9)\n",
    );

    let (code, stdout, _) = verify(&path);

    assert_eq!(code, 0);
    assert!(
        stdout.contains("  POINTER ./present.md: OK\n"),
        "output: {stdout}"
    );
}

#[test]
fn an_unresolvable_home_pointer_is_reported_missing() {
    let directory = temporary_directory("home");
    let path = directory.join("note.md");
    write(
        &path,
        "# Checkpoint\n\n- Format: v1\n\n## Workstreams\n\n### Only\n- Status: active\n- Next: go\n- Pointers: ~/loam-does-not-exist-2f8a1c\n",
    );

    let (code, stdout, _) = verify(&path);

    assert_eq!(code, 0);
    assert!(
        stdout.contains(
            "  POINTER ~/loam-does-not-exist-2f8a1c: MISSING: ~/loam-does-not-exist-2f8a1c\n"
        ),
        "output: {stdout}"
    );
}

#[test]
fn an_absolute_pointer_that_exists_is_reported_ok() {
    let directory = temporary_directory("absolute");
    let target = directory.join("target.md");
    write(&target, "# Target\n");
    let path = directory.join("note.md");
    write(
        &path,
        &format!(
            "# Checkpoint\n\n- Format: v1\n\n## Workstreams\n\n### Only\n- Status: active\n- Next: go\n- Pointers: {}\n",
            target.display()
        ),
    );

    let (code, stdout, _) = verify(&path);

    assert_eq!(code, 0);
    assert!(
        stdout.contains(&format!("  POINTER {}: OK\n", target.display())),
        "output: {stdout}"
    );
}

#[test]
fn a_taskwarrior_uuid_pointer_is_recognised_as_a_task() {
    let directory = temporary_directory("uuid");
    let path = directory.join("note.md");
    write(
        &path,
        "# Checkpoint\n\n- Format: v1\n\n## Workstreams\n\n### Only\n- Status: active\n- Next: go\n- Pointers: 550e8400-e29b-41d4-a716-446655440000\n",
    );

    let (code, stdout, _) = verify(&path);

    assert_eq!(code, 0);
    // `task` may or may not be installed; either degradation is contractual,
    // but the pointer must be classified as a task and never as a path.
    assert!(
        stdout.contains("  POINTER task 550e8400-e29b-41d4-a716-446655440000: "),
        "output: {stdout}"
    );
    assert!(
        stdout.contains("TASK: not available\n")
            || stdout.contains("NOT FOUND\n")
            || stdout.contains(": OK\n"),
        "output: {stdout}"
    );
}

#[test]
fn an_hcom_thread_pointer_is_recognised_as_a_thread() {
    let directory = temporary_directory("hcom");
    let path = directory.join("note.md");
    write(
        &path,
        "# Checkpoint\n\n- Format: v1\n\n## Workstreams\n\n### Only\n- Status: active\n- Next: go\n- Pointers: hcom thread `alpha-1`\n",
    );

    let (code, stdout, _) = verify(&path);

    assert_eq!(code, 0);
    assert!(
        stdout.contains("  POINTER hcom thread alpha-1: "),
        "output: {stdout}"
    );
}

#[test]
fn an_unrecognised_pointer_is_reported_but_not_resolved() {
    let directory = temporary_directory("unrecognised");
    let path = directory.join("note.md");
    write(
        &path,
        "# Checkpoint\n\n- Format: v1\n\n## Workstreams\n\n### Only\n- Status: active\n- Next: go\n- Pointers: some free-form note\n",
    );

    let (code, stdout, _) = verify(&path);

    assert_eq!(code, 0);
    assert!(
        stdout.contains(
            "  POINTER some free-form note: (unrecognized format, not checked by verify)\n"
        ),
        "output: {stdout}"
    );
}

#[test]
fn a_missing_note_is_reported_without_failing() {
    let directory = temporary_directory("absent-note");
    let path = directory.join("nope.md");

    let (code, stdout, stderr) = verify(&path);

    assert_eq!(code, 0);
    assert_eq!(stderr, "");
    assert_eq!(stdout, format!("NOTE: not found: {}\n", path.display()));
}

#[test]
fn no_arguments_prints_usage_and_exits_zero() {
    let binary = std::env::var("CARGO_BIN_EXE_loam").expect("cargo should provide the loam binary");
    for arguments in [
        vec!["checkpoint", "verify"],
        vec!["checkpoint", "verify", "--help"],
        vec!["checkpoint", "verify", "-h"],
    ] {
        let output = Command::new(&binary)
            .args(&arguments)
            .output()
            .expect("loam should run");
        assert_eq!(output.status.code(), Some(0), "arguments: {arguments:?}");
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "Usage: loam checkpoint verify <note.md>\n",
            "arguments: {arguments:?}"
        );
    }
}

#[test]
fn output_is_deterministic_and_read_only() {
    let directory = temporary_directory("deterministic");
    write(&directory.join("present.md"), "# Present\n");
    let path = directory.join("note.md");
    let content = "# Checkpoint\n\n- Captured: 2026-06-23 15:04 SAST\n- Reason: pause\n- Scope: sample\n- Format: v1\n\n## Workstreams\n\n### Only\n- Status: active\n- Next: go\n- Pointers: ./present.md, ./absent.md\n";
    write(&path, content);

    let (first_code, first, _) = verify(&path);
    let (second_code, second, _) = verify(&path);

    assert_eq!(first_code, second_code);
    assert_eq!(first, second);
    assert_eq!(
        fs::read_to_string(&path).expect("note should still be readable"),
        content,
        "verify must never write to the note"
    );
}
