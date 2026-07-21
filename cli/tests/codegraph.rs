use std::fs;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn temporary_codebase() -> std::path::PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be after epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("loam-codegraph-{nonce}"));
    fs::create_dir_all(path.join("src")).expect("temporary codebase should be created");
    path
}

#[test]
fn codegraph_walk_lists_nonempty_source_files() {
    let codebase = temporary_codebase();
    fs::write(codebase.join("src/main.rs"), "fn main() {}\n").expect("source should be written");
    fs::write(codebase.join("src/empty.rs"), "").expect("empty source should be written");

    let binary = std::env::var("CARGO_BIN_EXE_loam").expect("cargo should provide the loam binary");
    let output = Command::new(binary)
        .args(["codegraph", "walk", codebase.to_str().unwrap()])
        .output()
        .expect("loam should run");
    fs::remove_dir_all(&codebase).expect("temporary codebase should be removed");

    assert!(
        output.status.success(),
        "loam failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("walk output should be UTF-8");
    assert!(
        stdout.contains("\"path\":\"src/main.rs\""),
        "output: {stdout}"
    );
    assert!(!stdout.contains("empty.rs"), "output: {stdout}");
}

#[test]
fn codegraph_walk_summary_counts_extensions() {
    let codebase = temporary_codebase();
    fs::write(codebase.join("src/main.rs"), "fn main() {}\n").expect("source should be written");
    fs::write(codebase.join("src/CMakeLists.txt"), "project(test)\n")
        .expect("non-source config should be written");

    let binary = std::env::var("CARGO_BIN_EXE_loam").expect("cargo should provide the loam binary");
    let output = Command::new(binary)
        .args(["codegraph", "walk", codebase.to_str().unwrap(), "--summary"])
        .output()
        .expect("loam should run");
    fs::remove_dir_all(&codebase).expect("temporary codebase should be removed");

    assert!(
        output.status.success(),
        "loam failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("summary output should be UTF-8");
    assert!(stdout.contains("\"total\":1"), "output: {stdout}");
    assert!(stdout.contains("\"rs\":1"), "output: {stdout}");
    assert!(stdout.contains("\"pattern\":0"), "output: {stdout}");
}

#[test]
fn codegraph_walk_excludes_generated_marker_anywhere_in_file() {
    let codebase = temporary_codebase();
    fs::write(
        codebase.join("src/generated.rs"),
        "line 1\nline 2\nline 3\nline 4\nline 5\n// generated output\n",
    )
    .expect("generated source should be written");

    let binary = std::env::var("CARGO_BIN_EXE_loam").expect("cargo should provide the loam binary");
    let output = Command::new(binary)
        .args(["codegraph", "walk", codebase.to_str().unwrap()])
        .output()
        .expect("loam should run");
    fs::remove_dir_all(&codebase).expect("temporary codebase should be removed");

    assert!(
        output.status.success(),
        "loam failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains("generated.rs"));
}
