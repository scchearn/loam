use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const NOW: &str = "2026-07-20 12:00 +02:00";

fn temporary_workspace(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be after epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("loam-lint-{label}-{nonce}"));
    fs::create_dir_all(&path).expect("temporary workspace should be created");
    path
}

fn write(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("parent directory should be created");
    }
    fs::write(path, content).expect("file should be written");
}

fn loam(args: &[&str]) -> std::process::Output {
    let binary = std::env::var("CARGO_BIN_EXE_loam").expect("cargo should provide the loam binary");
    Command::new(binary)
        .args(args)
        .output()
        .expect("loam should run")
}

/// One defect per domain: a duplicate heading and broken wikilink (markdown),
/// a legacy `overview.md` (memory), and a goal missing from the index (work).
fn defective_workspace(label: &str) -> PathBuf {
    let workspace = temporary_workspace(label);
    write(
        &workspace.join("wiki/index.md"),
        "# Index\n\n## Overview\n\nSee [[ghost-page]].\n\n## Overview\n",
    );
    write(
        &workspace.join("wiki/overview.md"),
        "# Overview\n\nLegacy.\n",
    );
    write(
        &workspace.join("goals/sample-goal.md"),
        "---\ntitle: Sample Goal\nslug: sample-goal\nstatus: active\ncreated_at: 2026-07-19 09:00 +02:00\nupdated_at: 2026-07-19 09:00 +02:00\nreviewed_at: null\nnext_review_at: 2026-08-19 09:00 +02:00\n---\n\n# Sample Goal\n\n## Intent\n\nText.\n\n## Validation contract\n\nText.\n\n## Linked work\n\n- none\n\n## Current state\n\nText.\n\n## Reviews\n\n- none\n",
    );
    write(
        &workspace.join("goals/INDEX.md"),
        "# Goals Index\n\n| Status | Goal | Path |\n|---|---|---|\n",
    );
    workspace
}

fn domains(stdout: &str) -> Vec<String> {
    field_values(stdout, "domain")
}

fn field_values(stdout: &str, name: &str) -> Vec<String> {
    let marker = format!("\"{name}\":\"");
    stdout
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| {
            let start = line
                .find(&marker)
                .unwrap_or_else(|| panic!("finding should carry {name}: {line}"))
                + marker.len();
            let rest = &line[start..];
            rest[..rest.find('"').expect("value should be terminated")].to_owned()
        })
        .collect()
}

#[test]
fn default_run_covers_every_domain() {
    let workspace = defective_workspace("default-all");
    let output = loam(&["lint", workspace.to_str().unwrap(), "--now", NOW]);
    let stdout = String::from_utf8(output.stdout).expect("findings should be UTF-8");
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");

    assert_eq!(output.status.code(), Some(2), "stdout: {stdout}");
    let mut seen = domains(&stdout);
    seen.sort();
    seen.dedup();
    assert_eq!(
        seen,
        vec!["markdown", "memory", "work"],
        "every domain should report: {stdout}"
    );
}

#[test]
fn only_selects_exactly_one_domain() {
    let workspace = defective_workspace("only");
    let root = workspace.to_str().unwrap().to_owned();

    let mut results = Vec::new();
    for domain in ["markdown", "memory", "work"] {
        let output = loam(&["lint", "--only", domain, &root, "--now", NOW]);
        let stdout = String::from_utf8(output.stdout).expect("findings should be UTF-8");
        results.push((domain, output.status.code(), stdout));
    }
    let rejected_pair = loam(&["lint", "--only", "markdown,memory", &root]);
    let rejected_unknown = loam(&["lint", "--only", "goals", &root]);
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");

    for (domain, code, stdout) in &results {
        assert_eq!(*code, Some(2), "{domain} should report findings: {stdout}");
        let mut seen = domains(stdout);
        seen.dedup();
        assert_eq!(seen, vec![*domain], "{domain} filter leaked: {stdout}");
    }
    assert_eq!(
        rejected_pair.status.code(),
        Some(1),
        "--only takes exactly one domain"
    );
    assert_eq!(
        rejected_unknown.status.code(),
        Some(1),
        "unknown domain should be rejected"
    );
}

#[test]
fn old_command_spellings_are_gone() {
    let workspace = temporary_workspace("spellings");
    let root = workspace.to_str().unwrap().to_owned();

    let removed = [
        vec!["markdown", "lint", &root],
        vec!["lint", "markdown", &root],
        vec!["lint", "memory", &root],
    ];
    let mut survivors = Vec::new();
    for args in removed {
        let output = loam(&args);
        if output.status.code() != Some(1) {
            survivors.push(format!("{args:?} exited {:?}", output.status.code()));
        }
    }
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");

    assert!(
        survivors.is_empty(),
        "old spellings still work: {survivors:?}"
    );
}

#[test]
fn every_finding_uses_the_unified_envelope() {
    let workspace = defective_workspace("envelope");
    let output = loam(&["lint", workspace.to_str().unwrap(), "--now", NOW]);
    let stdout = String::from_utf8(output.stdout).expect("findings should be UTF-8");
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");

    for line in stdout.lines().filter(|line| !line.is_empty()) {
        for field in [
            "\"schema_version\":\"1\"",
            "\"domain\":\"",
            "\"rule\":\"",
            "\"rule_names\":[",
            "\"severity\":\"",
            "\"file\":\"",
            "\"line\":",
            "\"column\":",
            "\"end_line\":",
            "\"end_column\":",
            "\"description\":\"",
            "\"detail\":\"",
            "\"context\":\"",
            "\"evidence\":{",
            "\"target\":",
            "\"candidates\":[",
        ] {
            assert!(line.contains(field), "missing {field} in: {line}");
        }
    }

    // Markdown findings keep their range and candidate detail.
    let markdown = stdout
        .lines()
        .find(|line| line.contains("\"domain\":\"markdown\""))
        .expect("a markdown finding is expected");
    assert!(
        !markdown.contains("\"end_line\":0"),
        "markdown ranges should survive: {markdown}"
    );
}

#[test]
fn combined_output_is_byte_deterministic() {
    let workspace = defective_workspace("determinism");
    let root = workspace.to_str().unwrap().to_owned();

    let first = loam(&["lint", &root, "--now", NOW]);
    let second = loam(&["lint", &root, "--now", NOW]);
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");

    assert_eq!(first.status.code(), second.status.code());
    assert_eq!(
        String::from_utf8_lossy(&first.stdout),
        String::from_utf8_lossy(&second.stdout),
        "combined output must be byte-identical"
    );
}

#[test]
fn clean_workspace_exits_zero_and_reports_nothing() {
    let workspace = temporary_workspace("clean");
    write(
        &workspace.join("wiki/index.md"),
        "# Index\n\n## Overview\n\nHub.\n\n- [[alpha-note]]\n",
    );
    write(
        &workspace.join("wiki/alpha-note.md"),
        "# Alpha Note\n\nBody.\n",
    );

    let output = loam(&["lint", workspace.to_str().unwrap(), "--now", NOW]);
    let stdout = String::from_utf8(output.stdout).expect("findings should be UTF-8");
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");

    assert_eq!(stdout, "", "clean workspace should be silent");
    assert_eq!(output.status.code(), Some(0));
}

#[test]
fn now_requires_the_documented_timestamp_shape() {
    let workspace = temporary_workspace("now-shape");
    let root = workspace.to_str().unwrap().to_owned();

    let rejected = [
        "2026-07-20",
        "2026-07-20 12:00",
        "2026-07-20 12 +02:00",
        "2026-07-20 12:00 +0200",
        "2026-07-20 25:00 +02:00",
        "2026-07-20 12:00 02:00",
    ];
    let mut failures = Vec::new();
    for value in rejected {
        let output = loam(&["lint", &root, "--now", value]);
        if output.status.code() != Some(1) {
            failures.push(format!("{value:?} exited {:?}", output.status.code()));
        }
    }
    let accepted = loam(&["lint", &root, "--now", "2026-07-20 12:00 +02:00"]);
    let negative_offset = loam(&["lint", &root, "--now", "2026-07-20 12:00 -05:30"]);
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");

    assert!(
        failures.is_empty(),
        "accepted bad --now values: {failures:?}"
    );
    assert_eq!(accepted.status.code(), Some(0));
    assert_eq!(negative_offset.status.code(), Some(0));
}
