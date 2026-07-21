// The session-start hook gives `loam state --fast` a five-second hard timeout.
// This is the CI-enforced budget on a synthetic fixture whose size is recorded
// here rather than committed as thousands of files.
//
// Fixture size: 2000 source files across 20 directories, 2000 wiki code pages,
// 50 checkpoints. Sports-bridge and uwf sub-0.5s runs stay manual benchmarks.
use std::fs;
use std::process::Command;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

const SOURCE_FILES: usize = 2000;
const CODE_PAGES: usize = 2000;
const CHECKPOINTS: usize = 50;
const BUDGET_SECONDS: u64 = 5;

#[test]
fn fast_state_stays_within_the_five_second_hook_budget() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("loam-state-budget-{nonce}"));
    let wiki = root.join("wiki");
    fs::create_dir_all(wiki.join("checkpoints")).expect("checkpoints");
    fs::create_dir_all(wiki.join("code")).expect("code pages");
    fs::write(wiki.join("SCHEMA.md"), "# Schema\n").expect("schema");
    fs::write(wiki.join("index.md"), "# Index\n").expect("index");
    // A ready collection short-circuits the optional qmd CLI probe, matching a
    // real indexed workspace; the probe is a host-tool boundary, not our cost.
    fs::write(
        wiki.join(".wiki-metadata.json"),
        "{\"retrieval\":{\"status\":\"ready\",\"collection_name\":\"budget-fixture\"}}\n",
    )
    .expect("metadata");

    for directory in 0..20 {
        fs::create_dir_all(root.join(format!("src/module_{directory}"))).expect("module");
    }
    for index in 0..SOURCE_FILES {
        fs::write(
            root.join(format!("src/module_{}/file_{index}.rs", index % 20)),
            format!("fn item_{index}() {{ let value = {index}; }}\n"),
        )
        .expect("source");
    }
    for index in 0..CODE_PAGES {
        fs::write(
            wiki.join(format!("code/page-{index}.md")),
            format!(
                "---\nsource_path: src/module_{}/file_{index}.rs\ningested_at: \"1\"\n---\n\n# page\n",
                index % 20
            ),
        )
        .expect("code page");
    }
    for index in 0..CHECKPOINTS {
        fs::write(
            wiki.join(format!("checkpoints/checkpoint-2026-07-{:02}-1000.md", index % 28 + 1)),
            "# Checkpoint\n\n- Captured: 2026-07-01 10:00 +02:00\n- Scope: fixture\n\n## Workstreams\n\n### Budget fixture\n",
        )
        .expect("checkpoint");
    }

    let binary = std::env::var("CARGO_BIN_EXE_loam").expect("cargo should provide the loam binary");
    // One warmup so the page cache, not cold I/O, is what the budget measures.
    Command::new(&binary)
        .args(["state", "--fast", root.to_str().unwrap()])
        .output()
        .expect("warmup should run");

    let started = Instant::now();
    let output = Command::new(&binary)
        .args(["state", "--fast", root.to_str().unwrap()])
        .output()
        .expect("state should run");
    let elapsed = started.elapsed();
    fs::remove_dir_all(&root).ok();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("state output should be UTF-8");
    // Fast mode omits the date and codegraph probes and reports a null drift count.
    assert!(stdout.contains("\"drift_count\":null"), "{stdout}");
    assert!(!stdout.contains("date_drift_pending"), "{stdout}");
    assert!(!stdout.contains("code_ingest_pending"), "{stdout}");
    assert!(
        elapsed.as_secs() < BUDGET_SECONDS,
        "state --fast took {elapsed:?} on {SOURCE_FILES} sources / {CODE_PAGES} code pages / \
         {CHECKPOINTS} checkpoints, over the {BUDGET_SECONDS}s hook budget"
    );
}
