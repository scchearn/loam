#![cfg(target_os = "linux")]

use std::fs;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn temporary_workspace() -> std::path::PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be after epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("loam-state-parity-{nonce}"));
    fs::create_dir_all(path.join("wiki")).expect("temporary workspace should be created");
    path
}

fn state_script(repo_root: &std::path::Path) -> std::path::PathBuf {
    let scripts = repo_root.join("skills/loam-using/scripts");
    let legacy = scripts.join("loamstate-legacy.sh");
    if legacy.is_file() {
        legacy
    } else {
        scripts.join("loamstate.sh")
    }
}

#[test]
fn native_fast_state_matches_bash_on_qmd_and_workflow_fixture() {
    let workspace = temporary_workspace();
    fs::create_dir_all(workspace.join("wiki/checkpoints")).expect("checkpoints should be created");
    fs::create_dir_all(workspace.join("wiki/code")).expect("code graph should be created");
    fs::create_dir_all(workspace.join("src")).expect("source tree should be created");
    fs::write(workspace.join("wiki/SCHEMA.md"), "# Schema\n").expect("schema should be written");
    fs::write(workspace.join("wiki/overview.md"), "# Overview\n")
        .expect("overview should be written");
    fs::write(
        workspace.join("wiki/log.md"),
        (0..501).map(|_| "x\n").collect::<String>(),
    )
    .expect("log should be written");
    fs::write(
        workspace.join("wiki/.wiki-metadata.json"),
        "{\"retrieval\":{\"status\":\"ready\",\"collection_name\":\"fixture\"}}\n",
    )
    .expect("metadata should be written");
    fs::write(
        workspace.join("wiki/checkpoints/checkpoint-2026-07-17-1000.md"),
        "# Checkpoint\n\n- Captured: 2026-01-01 00:00 +00:00\n- Scope: fixture\n\n## Workstreams\n\n### Fixture checkpoint\n",
    )
    .expect("checkpoint should be written");
    fs::create_dir_all(workspace.join("specs")).expect("specs should be created");
    fs::create_dir_all(workspace.join("plans")).expect("plans should be created");
    fs::write(
        workspace.join("specs/ready.md"),
        "---\nstatus: approved\napproved_at: 2026-07-17 10:00 +02:00\n---\n",
    )
    .expect("spec should be written");
    fs::write(
        workspace.join("plans/pending.md"),
        "---\nstatus: pending\n---\n",
    )
    .expect("plan should be written");
    fs::write(workspace.join("src/main.rs"), "fn main() {}\n").expect("source should be written");
    fs::write(
        workspace.join("wiki/note.md"),
        "---\nupdated_at: 2026-07-17 10:00\n---\n",
    )
    .expect("drifted note should be written");

    let binary = std::env::var("CARGO_BIN_EXE_loam").expect("cargo should provide the loam binary");
    let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("cli should be under the repository root");
    for fast in [true, false] {
        let mut args = vec!["state".to_owned()];
        if fast {
            args.push("--fast".to_owned());
        }
        args.push(workspace.to_str().unwrap().to_owned());
        let native = Command::new(&binary)
            .args(&args)
            .output()
            .expect("native state should run");
        let shell = Command::new("bash")
            .arg(state_script(repo_root))
            .args(args.iter().skip(1))
            .output()
            .expect("bash state should run");

        assert!(native.status.success());
        assert!(shell.status.success());
        assert_eq!(native.stdout, shell.stdout);
    }
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");
}

#[test]
fn native_fast_state_matches_bash_on_adversarial_checkpoint_fixture() {
    let workspace = temporary_workspace();
    fs::create_dir_all(workspace.join("wiki/checkpoints")).expect("checkpoints should be created");
    fs::write(workspace.join("wiki/SCHEMA.md"), "# Schema\n").expect("schema should be written");
    fs::write(
        workspace.join("wiki/checkpoints/checkpoint-2026-07-03-1100.md"),
        "# Checkpoint\n\n- Captured: 2026-07-03 11:00 +02:00\n\n## Workstreams\n\n### Worker Cancellation Guard\n",
    )
    .expect("latest checkpoint should be written");
    fs::write(
        workspace.join("wiki/checkpoints/checkpoint-2026-07-02-1000.md"),
        "# Checkpoint\n\n- Captured: 2026-07-02 10:00 +02:00\n\n## Workstreams\n\n### Pipeline \"cancel\" bug\n",
    )
    .expect("quoted checkpoint should be written");
    Command::new("git")
        .args(["-C", workspace.to_str().unwrap(), "init", "-q"])
        .status()
        .expect("git should initialize the fixture");

    let binary = std::env::var("CARGO_BIN_EXE_loam").expect("cargo should provide the loam binary");
    let native = Command::new(&binary)
        .args(["state", "--fast", workspace.to_str().unwrap()])
        .output()
        .expect("native state should run");
    let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("cli should be under the repository root");
    let script = state_script(repo_root);
    let shell = Command::new("bash")
        .args([
            script.to_str().unwrap(),
            "--fast",
            workspace.to_str().unwrap(),
        ])
        .output()
        .expect("bash state should run");
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");

    assert!(native.status.success());
    assert!(shell.status.success());
    assert_eq!(native.stdout, shell.stdout);
}
