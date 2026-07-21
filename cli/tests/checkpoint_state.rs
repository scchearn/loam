//! `loam checkpoint state` — capture-side digest contract.
//!
//! The digest replaces a Bash script that required `jq`, GNU `date -d`, GNU
//! `date -r`, `grep -oP`, and `find -mmin`. None of those exist on stock
//! Windows, so every assertion here is platform-neutral and the optional
//! `hcom`/TaskWarrior integrations are exercised through an emptied `PATH`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn temporary_workspace(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be after epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("loam-chkstate-{label}-{nonce}"));
    fs::create_dir_all(&path).expect("temporary workspace should be created");
    path
}

fn write(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("parent directory should be created");
    }
    fs::write(path, content).expect("file should be written");
}

/// Runs with an emptied `PATH` so `hcom` and `task` are reliably absent: the
/// graceful-degradation branches are the ones a fresh machine actually hits.
fn state(workspace: &Path, extra: &[&str]) -> (i32, String, String) {
    let binary = std::env::var("CARGO_BIN_EXE_loam").expect("cargo should provide the loam binary");
    let mut arguments = vec!["checkpoint", "state"];
    arguments.extend_from_slice(extra);
    arguments.push(workspace.to_str().unwrap());
    let output = Command::new(binary)
        .args(&arguments)
        .env("PATH", "")
        .output()
        .expect("loam should run");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn section<'a>(output: &'a str, heading: &str) -> &'a str {
    let start = output
        .find(heading)
        .unwrap_or_else(|| panic!("missing section {heading} in:\n{output}"))
        + heading.len();
    let rest = &output[start..];
    match rest.find("\n=== ") {
        Some(end) => &rest[..end],
        None => rest,
    }
}

#[test]
fn the_three_sections_are_emitted_in_order_and_exit_zero() {
    let workspace = temporary_workspace("sections");
    write(&workspace.join("touched.md"), "# Touched\n");

    let (code, stdout, stderr) = state(&workspace, &[]);

    assert_eq!(code, 0, "stderr: {stderr}");
    let hcom = stdout
        .find("=== hcom threads ===\n")
        .expect("hcom section should exist");
    let task = stdout
        .find("=== taskwarrior active ===\n")
        .expect("taskwarrior section should exist");
    let files = stdout
        .find("=== files touched recently ===\n")
        .expect("files section should exist");
    assert!(hcom < task && task < files, "order drift:\n{stdout}");
    assert!(stdout.starts_with("=== hcom threads ===\n"), "{stdout}");
}

#[test]
fn absent_optional_tools_degrade_without_failing() {
    let workspace = temporary_workspace("degraded");
    write(&workspace.join("touched.md"), "# Touched\n");

    let (code, stdout, _) = state(&workspace, &[]);

    assert_eq!(code, 0);
    assert_eq!(
        section(&stdout, "=== hcom threads ===\n").trim_end(),
        "hcom: not available"
    );
    assert_eq!(
        section(&stdout, "=== taskwarrior active ===\n").trim_end(),
        "task: not available"
    );
}

#[test]
fn recently_touched_files_are_listed_with_a_local_timestamp() {
    let workspace = temporary_workspace("recent");
    write(&workspace.join("alpha.md"), "# Alpha\n");

    let (code, stdout, _) = state(&workspace, &[]);

    assert_eq!(code, 0);
    let files = section(&stdout, "=== files touched recently ===\n");
    let line = files
        .lines()
        .find(|line| line.ends_with("alpha.md"))
        .unwrap_or_else(|| panic!("alpha.md should be listed:\n{stdout}"));
    // "YYYY-MM-DD HH:MM <path>"
    let (timestamp, _) = line.split_at(16);
    assert_eq!(timestamp.len(), 16, "line: {line}");
    assert_eq!(&timestamp[4..5], "-", "line: {line}");
    assert_eq!(&timestamp[7..8], "-", "line: {line}");
    assert_eq!(&timestamp[10..11], " ", "line: {line}");
    assert_eq!(&timestamp[13..14], ":", "line: {line}");
    assert!(
        timestamp[..4].chars().all(|value| value.is_ascii_digit()),
        "line: {line}"
    );
}

#[test]
fn hidden_directories_are_excluded() {
    let workspace = temporary_workspace("hidden");
    write(&workspace.join("visible.md"), "# Visible\n");
    write(&workspace.join(".git/config"), "[core]\n");
    write(&workspace.join(".obsidian/app.json"), "{}\n");

    let (code, stdout, _) = state(&workspace, &[]);

    assert_eq!(code, 0);
    let files = section(&stdout, "=== files touched recently ===\n");
    assert!(files.contains("visible.md"), "output: {stdout}");
    assert!(!files.contains(".git"), "output: {stdout}");
    assert!(!files.contains(".obsidian"), "output: {stdout}");
}

#[test]
fn a_zero_window_reports_no_recent_files() {
    let workspace = temporary_workspace("window-zero");
    write(&workspace.join("alpha.md"), "# Alpha\n");

    let (code, stdout, _) = state(&workspace, &["--window", "0"]);

    assert_eq!(code, 0);
    assert_eq!(
        section(&stdout, "=== files touched recently ===\n").trim_end(),
        "none"
    );
}

#[test]
fn the_listing_is_capped_at_fifteen_entries() {
    let workspace = temporary_workspace("cap");
    for index in 0..25 {
        write(&workspace.join(format!("file-{index:02}.md")), "# File\n");
    }

    let (code, stdout, _) = state(&workspace, &[]);

    assert_eq!(code, 0);
    let listed = section(&stdout, "=== files touched recently ===\n")
        .lines()
        .filter(|line| line.ends_with(".md"))
        .count();
    assert_eq!(listed, 15, "output: {stdout}");
}

#[test]
fn the_listing_is_deterministic_across_runs() {
    let workspace = temporary_workspace("deterministic");
    for index in 0..20 {
        write(&workspace.join(format!("file-{index:02}.md")), "# File\n");
    }

    let (_, first, _) = state(&workspace, &[]);
    let (_, second, _) = state(&workspace, &[]);

    assert_eq!(
        section(&first, "=== files touched recently ===\n"),
        section(&second, "=== files touched recently ===\n")
    );
}

#[test]
fn the_workspace_can_come_from_the_environment() {
    let workspace = temporary_workspace("environment");
    write(&workspace.join("alpha.md"), "# Alpha\n");
    let binary = std::env::var("CARGO_BIN_EXE_loam").expect("cargo should provide the loam binary");

    let output = Command::new(binary)
        .args(["checkpoint", "state"])
        .env("PATH", "")
        .env("WORKSPACE", &workspace)
        .output()
        .expect("loam should run");

    assert_eq!(output.status.code(), Some(0));
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("alpha.md"),
        "WORKSPACE should be honoured"
    );
}

#[test]
fn a_malformed_window_is_rejected() {
    let workspace = temporary_workspace("bad-window");

    let (code, _, stderr) = state(&workspace, &["--window", "soon"]);

    assert_eq!(code, 1);
    assert!(stderr.contains("--window"), "stderr: {stderr}");
}

#[test]
fn a_missing_workspace_is_reported() {
    let workspace = temporary_workspace("absent").join("nope");

    let (code, _, stderr) = state(&workspace, &[]);

    assert_eq!(code, 1);
    assert!(stderr.contains("workspace not found"), "stderr: {stderr}");
}
