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
