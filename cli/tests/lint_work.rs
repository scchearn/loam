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
    let path = std::env::temp_dir().join(format!("loam-wrk-{label}-{nonce}"));
    fs::create_dir_all(path.join("goals")).expect("temporary workspace should be created");
    path
}

fn write(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("parent directory should be created");
    }
    fs::write(path, content).expect("file should be written");
}

fn goal(status: &str, updated: &str, reviewed: &str, next_review: &str, reviews: &str) -> String {
    format!(
        "---\ntitle: Sample Goal\nslug: sample-goal\nstatus: {status}\ncreated_at: 2026-01-05 09:00 +02:00\nupdated_at: {updated}\nreviewed_at: {reviewed}\nnext_review_at: {next_review}\n---\n\n# Sample Goal\n\n## Intent\n\nIntent text.\n\n## Validation contract\n\nContract text.\n\n## Linked work\n\n- none\n\n## Current state\n\nState text.\n\n## Reviews\n\n{reviews}\n"
    )
}

fn healthy_goals(workspace: &Path) {
    write(
        &workspace.join("goals/sample-goal.md"),
        &goal(
            "active",
            "2026-07-19 09:00 +02:00",
            "2026-07-19 09:00 +02:00",
            "2026-08-19 09:00 +02:00",
            "### 2026-07-19\n\n- Result: pass\n",
        ),
    );
    write(
        &workspace.join("goals/INDEX.md"),
        "# Goals Index\n\n| Status | Goal | Path | Updated | Next review |\n|---|---|---|---|---|\n| active | Sample Goal | goals/sample-goal.md | 2026-07-19 | 2026-08-19 |\n",
    );
}

fn lint(workspace: &Path) -> (i32, String) {
    let binary = std::env::var("CARGO_BIN_EXE_loam").expect("cargo should provide the loam binary");
    let output = Command::new(binary)
        .args([
            "lint",
            "--only",
            "work",
            workspace.to_str().unwrap(),
            "--now",
            NOW,
        ])
        .output()
        .expect("loam should run");
    assert!(
        output.stderr.is_empty(),
        "unexpected stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    (
        output.status.code().expect("loam should exit normally"),
        String::from_utf8(output.stdout).expect("findings should be UTF-8"),
    )
}

fn rules(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| {
            let marker = "\"rule\":\"";
            let start = line.find(marker).expect("finding should carry a rule") + marker.len();
            let rest = &line[start..];
            rest[..rest.find('"').expect("rule should be terminated")].to_owned()
        })
        .collect()
}

#[test]
fn healthy_goal_reports_nothing() {
    let workspace = temporary_workspace("healthy");
    healthy_goals(&workspace);

    let (code, stdout) = lint(&workspace);
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");

    assert_eq!(stdout, "", "healthy goal should emit no findings");
    assert_eq!(code, 0);
}

#[test]
fn missing_front_matter_and_sections_are_reported() {
    let workspace = temporary_workspace("incomplete");
    healthy_goals(&workspace);
    write(
        &workspace.join("goals/sample-goal.md"),
        "---\ntitle: Sample Goal\nslug: sample-goal\nstatus: active\ncreated_at: 2026-01-05 09:00 +02:00\nupdated_at: 2026-07-19 09:00 +02:00\nreviewed_at: 2026-07-19 09:00 +02:00\n---\n\n# Sample Goal\n\n## Intent\n\nIntent text.\n\n## Reviews\n\n### 2026-07-19\n\n- Result: pass\n",
    );

    let (code, stdout) = lint(&workspace);
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");

    assert_eq!(code, 2, "output: {stdout}");
    let found = rules(&stdout);
    assert!(found.contains(&"WRK001".to_owned()), "output: {stdout}");
    assert!(found.contains(&"WRK004".to_owned()), "output: {stdout}");
    assert!(
        stdout.contains("\"target\":\"next_review_at\""),
        "missing field should be named: {stdout}"
    );
}

#[test]
fn invalid_status_and_malformed_timestamp_are_reported() {
    let workspace = temporary_workspace("invalid");
    healthy_goals(&workspace);
    write(
        &workspace.join("goals/sample-goal.md"),
        &goal(
            "in-progress",
            "2026/07/19",
            "null",
            "null",
            "### 2026-07-19\n\n- Result: pass\n",
        ),
    );
    write(
        &workspace.join("goals/INDEX.md"),
        "# Goals Index\n\n| Status | Goal | Path | Updated | Next review |\n|---|---|---|---|---|\n| in-progress | Sample Goal | goals/sample-goal.md | 2026-07-19 | — |\n",
    );

    let (code, stdout) = lint(&workspace);
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");

    assert_eq!(code, 2, "output: {stdout}");
    let found = rules(&stdout);
    assert!(found.contains(&"WRK002".to_owned()), "output: {stdout}");
    assert!(found.contains(&"WRK003".to_owned()), "output: {stdout}");
}

#[test]
fn stale_draft_and_overdue_active_goals_are_reported() {
    let workspace = temporary_workspace("stale");
    write(
        &workspace.join("goals/stale-draft.md"),
        &goal(
            "draft",
            "2026-05-01 09:00 +02:00",
            "null",
            "null",
            "- none\n",
        )
        .replace("slug: sample-goal", "slug: stale-draft")
        .replace("title: Sample Goal", "title: Stale Draft"),
    );
    write(
        &workspace.join("goals/overdue-goal.md"),
        &goal(
            "active",
            "2026-07-01 09:00 +02:00",
            "2026-07-01 09:00 +02:00",
            "2026-07-10 09:00 +02:00",
            "### 2026-07-01\n\n- Result: pass\n",
        )
        .replace("slug: sample-goal", "slug: overdue-goal")
        .replace("title: Sample Goal", "title: Overdue Goal"),
    );
    write(
        &workspace.join("goals/paused-goal.md"),
        &goal(
            "paused",
            "2025-01-01 09:00 +02:00",
            "null",
            "null",
            "- none\n",
        )
        .replace("slug: sample-goal", "slug: paused-goal")
        .replace("title: Sample Goal", "title: Paused Goal"),
    );
    write(
        &workspace.join("goals/INDEX.md"),
        "# Goals Index\n\n| Status | Goal | Path | Updated | Next review |\n|---|---|---|---|---|\n| draft | Stale Draft | goals/stale-draft.md | 2026-05-01 | — |\n| active | Overdue Goal | goals/overdue-goal.md | 2026-07-01 | 2026-07-10 |\n| paused | Paused Goal | goals/paused-goal.md | 2025-01-01 | — |\n",
    );

    let (code, stdout) = lint(&workspace);
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");

    assert_eq!(code, 2, "output: {stdout}");
    let found = rules(&stdout);
    assert!(found.contains(&"WRK005".to_owned()), "output: {stdout}");
    assert!(found.contains(&"WRK006".to_owned()), "output: {stdout}");
    assert!(
        !stdout.contains("paused-goal"),
        "paused goals are staleness-exempt: {stdout}"
    );
}

#[test]
fn active_goal_without_next_review_goes_stale_after_ninety_days() {
    let workspace = temporary_workspace("ninety");
    write(
        &workspace.join("goals/sample-goal.md"),
        &goal(
            "active",
            "2026-01-10 09:00 +02:00",
            "2026-01-10 09:00 +02:00",
            "null",
            "### 2026-01-10\n\n- Result: pass\n",
        ),
    );
    write(
        &workspace.join("goals/INDEX.md"),
        "# Goals Index\n\n| Status | Goal | Path | Updated | Next review |\n|---|---|---|---|---|\n| active | Sample Goal | goals/sample-goal.md | 2026-01-10 | — |\n",
    );

    let (code, stdout) = lint(&workspace);
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");

    assert_eq!(code, 2, "output: {stdout}");
    assert_eq!(rules(&stdout), vec!["WRK007"], "output: {stdout}");
}

#[test]
fn missing_linked_work_paths_are_reported() {
    let workspace = temporary_workspace("linked");
    healthy_goals(&workspace);
    write(
        &workspace.join("goals/sample-goal.md"),
        &goal(
            "active",
            "2026-07-19 09:00 +02:00",
            "2026-07-19 09:00 +02:00",
            "2026-08-19 09:00 +02:00",
            "### 2026-07-19\n\n- Result: pass\n",
        )
        .replace(
            "## Linked work\n\n- none",
            "## Linked work\n\n- specs/ghost-spec.md\n- plans/ghost-plan.md",
        ),
    );

    let (code, stdout) = lint(&workspace);
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");

    assert_eq!(code, 2, "output: {stdout}");
    assert_eq!(rules(&stdout), vec!["WRK008", "WRK008"], "output: {stdout}");
    assert!(
        stdout.contains("\"target\":\"plans/ghost-plan.md\""),
        "output: {stdout}"
    );
}

#[test]
fn goal_relative_linked_work_paths_resolve() {
    let workspace = temporary_workspace("relative-links");
    healthy_goals(&workspace);
    write(&workspace.join("specs/real-spec.md"), "# Real Spec\n");
    write(
        &workspace.join("goals/sample-goal.md"),
        &goal(
            "active",
            "2026-07-19 09:00 +02:00",
            "2026-07-19 09:00 +02:00",
            "2026-08-19 09:00 +02:00",
            "### 2026-07-19\n\n- Result: pass\n",
        )
        .replace(
            "## Linked work\n\n- none",
            "## Linked work\n\n- [Real Spec](../specs/real-spec.md)\n- specs/real-spec.md",
        ),
    );

    let (code, stdout) = lint(&workspace);
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");

    assert_eq!(
        stdout, "",
        "goal-relative and repo-relative links both resolve"
    );
    assert_eq!(code, 0);
}

#[test]
fn index_rows_out_of_step_with_goal_files_are_reported() {
    let workspace = temporary_workspace("index");
    healthy_goals(&workspace);
    write(
        &workspace.join("goals/INDEX.md"),
        "# Goals Index\n\n| Status | Goal | Path | Updated | Next review |\n|---|---|---|---|---|\n| active | Ghost Goal | goals/ghost-goal.md | 2026-07-19 | — |\n",
    );

    let (code, stdout) = lint(&workspace);
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");

    assert_eq!(code, 2, "output: {stdout}");
    assert_eq!(rules(&stdout), vec!["WRK009", "WRK009"], "output: {stdout}");
    assert!(stdout.contains("goals/ghost-goal.md"), "output: {stdout}");
    assert!(stdout.contains("goals/sample-goal.md"), "output: {stdout}");
}

#[test]
fn achieved_without_passing_review_is_reported() {
    let workspace = temporary_workspace("achieved");
    write(
        &workspace.join("goals/sample-goal.md"),
        &goal(
            "achieved",
            "2026-07-19 09:00 +02:00",
            "2026-07-19 09:00 +02:00",
            "null",
            "### 2026-07-19\n\n- Result: fail\n",
        ),
    );
    write(
        &workspace.join("goals/INDEX.md"),
        "# Goals Index\n\n| Status | Goal | Path | Updated | Next review |\n|---|---|---|---|---|\n| achieved | Sample Goal | goals/sample-goal.md | 2026-07-19 | — |\n",
    );

    let (code, stdout) = lint(&workspace);
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");

    assert_eq!(code, 2, "output: {stdout}");
    assert_eq!(rules(&stdout), vec!["WRK010"], "output: {stdout}");
}

#[test]
fn reviewed_at_out_of_step_with_newest_review_is_reported() {
    let workspace = temporary_workspace("reviewed");
    write(
        &workspace.join("goals/sample-goal.md"),
        &goal(
            "active",
            "2026-07-19 09:00 +02:00",
            "2026-07-01 09:00 +02:00",
            "2026-08-19 09:00 +02:00",
            "### 2026-07-19\n\n- Result: pass\n\n### 2026-07-01\n\n- Result: pass\n",
        ),
    );
    write(
        &workspace.join("goals/INDEX.md"),
        "# Goals Index\n\n| Status | Goal | Path | Updated | Next review |\n|---|---|---|---|---|\n| active | Sample Goal | goals/sample-goal.md | 2026-07-19 | 2026-08-19 |\n",
    );

    let (code, stdout) = lint(&workspace);
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");

    assert_eq!(code, 2, "output: {stdout}");
    assert_eq!(rules(&stdout), vec!["WRK011"], "output: {stdout}");
}
