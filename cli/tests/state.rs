use std::fs;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn temporary_workspace() -> std::path::PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be after epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("loam-state-{nonce}"));
    fs::create_dir(&path).expect("temporary workspace should be created");
    path
}

fn plan_with(criteria: &str) -> String {
    format!(
        "---\nstatus: in-progress\n---\n\n## Acceptance criteria\n\n{criteria}\n\n## Tasks\n\n### T1\n\n- **Status:** [x]\n"
    )
}

fn state_hints(plan: &str) -> String {
    let workspace = temporary_workspace();
    // Without a resolvable wiki root `aggregate` short-circuits to minimal
    // state and no workflow hint is reached at all.
    fs::create_dir(workspace.join("wiki")).expect("wiki directory should be created");
    fs::write(workspace.join("wiki/index.md"), "# Index\n").expect("index should be written");
    fs::create_dir(workspace.join("plans")).expect("plans directory should be created");
    fs::write(workspace.join("plans/demo.md"), plan).expect("plan should be written");
    let binary = std::env::var("CARGO_BIN_EXE_loam").expect("cargo should provide the loam binary");
    let output = Command::new(binary)
        .args(["state", "--fast", workspace.to_str().unwrap()])
        .output()
        .expect("loam should run");
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");
    String::from_utf8(output.stdout).expect("state output should be UTF-8")
}

#[test]
fn plan_with_complete_tasks_and_open_criteria_is_reconcilable() {
    // [>] and the unrecognized [~] count as open; [-] counts as resolved.
    let stdout = state_hints(&plan_with(
        "- [x] met. Evidence: tests.\n- [-] superseded.\n- [ ] pending.\n- [>] needs re-check.\n- [~] unrecognized.",
    ));
    assert!(
        stdout.contains(r#""kind":"plan_reconcilable""#),
        "expected reconcilable hint, got {stdout}"
    );
    assert!(
        stdout.contains(r#""kind":"plan_reconcilable","group":"workflow","severity":"info","message":"Every task is complete but acceptance criteria are unresolved.","command":"/loam::amending-plan","evidence":{"plans":["plans/demo.md"]}"#),
        "unexpected hint shape in {stdout}"
    );
}

#[test]
fn many_reconcilable_plans_collapse_into_one_hint() {
    let workspace = temporary_workspace();
    fs::create_dir(workspace.join("wiki")).expect("wiki directory should be created");
    fs::write(workspace.join("wiki/index.md"), "# Index\n").expect("index should be written");
    fs::create_dir(workspace.join("plans")).expect("plans directory should be created");
    for name in ["alpha.md", "beta.md", "gamma.md"] {
        fs::write(
            workspace.join("plans").join(name),
            plan_with("- [ ] pending."),
        )
        .expect("plan should be written");
    }
    let binary = std::env::var("CARGO_BIN_EXE_loam").expect("cargo should provide the loam binary");
    let output = Command::new(binary)
        .args(["state", "--fast", workspace.to_str().unwrap()])
        .output()
        .expect("loam should run");
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");
    let stdout = String::from_utf8(output.stdout).expect("state output should be UTF-8");

    assert_eq!(
        stdout.matches("plan_reconcilable").count(),
        1,
        "three drifting plans must produce one hint, got {stdout}"
    );
    assert!(
        stdout.contains(r#""plans":["plans/alpha.md","plans/beta.md","plans/gamma.md"]"#),
        "every affected plan must be listed, got {stdout}"
    );
}

#[test]
fn plan_with_all_criteria_resolved_is_not_reconcilable() {
    let stdout = state_hints(&plan_with("- [x] met. Evidence: tests.\n- [-] superseded."));
    assert!(
        !stdout.contains("plan_reconcilable"),
        "resolved plan should emit no hint, got {stdout}"
    );
}

#[test]
fn plan_with_pending_tasks_is_not_reconcilable() {
    let plan = plan_with("- [ ] pending.").replace("- **Status:** [x]", "- **Status:** [ ]");
    let stdout = state_hints(&plan);
    assert!(
        !stdout.contains("plan_reconcilable"),
        "unfinished plan should emit no hint, got {stdout}"
    );
}

#[test]
fn state_fast_without_wiki_returns_minimal_fallback() {
    let workspace = temporary_workspace();
    let binary = std::env::var("CARGO_BIN_EXE_loam").expect("cargo should provide the loam binary");
    let output = Command::new(binary)
        .args(["state", "--fast", workspace.to_str().unwrap()])
        .output()
        .expect("loam should run");
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");

    assert!(
        output.status.success(),
        "loam failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout)
            .expect("state output should be UTF-8")
            .trim(),
        r#"{"wiki_root":"","exists":false,"qmd_ready":false,"latest_checkpoint":null,"recent_checkpoints":[],"checkpoint_count":0,"git_status":null,"drift_count":null,"hints":[{"kind":"memory_missing","group":"maintenance","severity":"info","message":"No memory substrate found; scaffold a wiki to begin.","command":"/loam::scaffolding-wiki <goal>","evidence":{}}]}"#
    );
}
