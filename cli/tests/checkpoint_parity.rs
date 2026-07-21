//! Byte-for-byte parity between `loam checkpoint verify` and the retained
//! `checkpoint-verify-legacy` Bash oracle.
//!
//! Linux-only for the same reason as `state_parity.rs`: the oracle is Bash and
//! depends on GNU `awk`, `grep -E`, and `sed`. Parity is the migration
//! contract; the cross-platform contract lives in `checkpoint_verify.rs`.
#![cfg(target_os = "linux")]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("cli/ should have a parent")
        .to_path_buf()
}

fn legacy_script() -> Option<PathBuf> {
    let path = repository_root()
        .join("skills/loam-work/loam-checkpointing/scripts/checkpoint-verify-legacy");
    path.is_file().then_some(path)
}

fn temporary_directory(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be after epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("loam-chkparity-{label}-{nonce}"));
    fs::create_dir_all(&path).expect("temporary directory should be created");
    path
}

fn run(program: &str, arguments: &[&str]) -> (i32, String) {
    let output = Command::new(program)
        .args(arguments)
        .output()
        .unwrap_or_else(|error| panic!("{program} should run: {error}"));
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
    )
}

fn assert_parity(note: &Path) {
    let Some(legacy) = legacy_script() else {
        return;
    };
    let binary = std::env::var("CARGO_BIN_EXE_loam").expect("cargo should provide the loam binary");
    let note = note.to_str().expect("fixture path should be UTF-8");

    let (legacy_code, legacy_output) = run(
        "bash",
        &[legacy.to_str().expect("oracle path should be UTF-8"), note],
    );
    let (native_code, native_output) = run(&binary, &["checkpoint", "verify", note]);

    assert_eq!(native_code, legacy_code, "exit code drift on {note}");
    assert_eq!(
        native_output, legacy_output,
        "output drift on {note}\n--- native ---\n{native_output}\n--- legacy ---\n{legacy_output}"
    );
}

fn write(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("parent directory should be created");
    }
    fs::write(path, content).expect("file should be written");
}

#[test]
fn a_healthy_note_matches_the_bash_oracle() {
    let directory = temporary_directory("healthy");
    write(&directory.join("present.md"), "# Present\n");
    let note = directory.join("note.md");
    write(
        &note,
        "# Checkpoint\n\n- Captured: 2026-06-23 15:04 SAST\n- Reason: pause\n- Scope: sample scope\n- Format: v1\n- Intended return: pick up the thing I named\n\n## Workstreams\n\n### Sample\n- Status: ready-to-resume\n- Next: Do the next operational step.\n- Pointers: ./present.md\n",
    );

    assert_parity(&note);
}

#[test]
fn every_failure_mode_at_once_matches_the_bash_oracle() {
    let directory = temporary_directory("failures");
    let note = directory.join("note.md");
    write(
        &note,
        "# Checkpoint\n\n- Reason: nonsense\n- Format: v2\n- Previous: none\n- Supersedes: None\n\n## Workstreams\n\n### Only\n- Status: bogus\n- Next:\n",
    );

    assert_parity(&note);
}

#[test]
fn mixed_pointer_patterns_match_the_bash_oracle() {
    let directory = temporary_directory("pointers");
    write(&directory.join("present.md"), "# Present\n");
    let note = directory.join("note.md");
    write(
        &note,
        &format!(
            "# Checkpoint\n\n- Format: v1\n\n## Workstreams\n\n### Sample\n- Status: active\n- Next: go\n- Pointers: {}, ./present.md, ./absent.md, ~/loam-does-not-exist-2f8a1c\n  - some free-form note\n### Second\n- Next: no status here\n- Pointers: 550e8400-e29b-41d4-a716-446655440000\n",
            directory.join("present.md").display()
        ),
    );

    assert_parity(&note);
}

#[test]
fn volatile_and_parenthetical_pointers_match_the_bash_oracle() {
    let directory = temporary_directory("volatile");
    write(&directory.join("present.md"), "# Present\n");
    let note = directory.join("note.md");
    write(
        &note,
        &format!(
            "# Checkpoint\n\n- Format: v1\n\n## Workstreams\n\n### Sample\n- Status: active\n- Next: go\n- Pointers: ./present.md (events 1-9), {} (pending: #4)\n",
            directory.join("present.md").display()
        ),
    );

    assert_parity(&note);
}

#[test]
fn a_missing_note_matches_the_bash_oracle() {
    let directory = temporary_directory("absent");
    assert_parity(&directory.join("nope.md"));
}

#[test]
fn workstreams_outside_the_section_match_the_bash_oracle() {
    let directory = temporary_directory("scoped");
    let note = directory.join("note.md");
    write(
        &note,
        "# Checkpoint\n\n- Captured: 2026-06-23 15:04 SAST\n- Format: v1\n\n## Workstreams\n\n### Real\n- Status: active\n- Next: go\n\n## Other\n\n### Decoy\n- Status: bogus\n- Next: ignored\n",
    );

    assert_parity(&note);
}
