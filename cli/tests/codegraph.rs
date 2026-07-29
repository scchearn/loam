use std::fs;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

fn temporary_codebase() -> std::path::PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be after epoch")
        .as_nanos();
    let serial = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("loam-codegraph-{nonce}-{serial}"));
    fs::create_dir_all(path.join("src")).expect("temporary codebase should be created");
    path
}

fn loam(root: &std::path::Path, args: &[&str]) -> std::process::Output {
    let binary = std::env::var("CARGO_BIN_EXE_loam").expect("cargo should provide the loam binary");
    Command::new(binary)
        .args(args)
        .current_dir(root)
        .output()
        .expect("loam should run")
}

fn git(root: &std::path::Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .expect("git should run");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("git output should be UTF-8")
        .trim()
        .to_owned()
}

fn init_git(root: &std::path::Path) {
    git(root, &["init", "-q"]);
    git(root, &["config", "user.email", "tests@example.invalid"]);
    git(root, &["config", "user.name", "Loam Tests"]);
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

#[test]
fn codegraph_walk_uses_sha256_identity_outside_git() {
    let codebase = temporary_codebase();
    fs::write(codebase.join("src/main.rs"), "fn main() {}\n").expect("source should be written");

    let output = loam(
        &codebase,
        &["codegraph", "walk", codebase.to_str().unwrap()],
    );
    let text = String::from_utf8(output.stdout).expect("walk output should be UTF-8");
    fs::remove_dir_all(&codebase).expect("temporary codebase should be removed");

    assert!(output.status.success(), "{text}");
    assert!(text.contains(r#""content_id":"sha256:"#), "{text}");
    assert!(text.contains(r#""blob_oid":"""#), "{text}");
    assert!(text.contains(r#""source_commit":"""#), "{text}");
    assert!(text.contains(r#""source_state":"fallback""#), "{text}");
}

#[test]
fn codegraph_walk_keeps_untracked_files_and_marks_git_provenance() {
    let codebase = temporary_codebase();
    init_git(&codebase);
    fs::write(codebase.join("src/main.rs"), "fn main() {}\n").expect("source should be written");
    git(&codebase, &["add", "src/main.rs"]);
    git(&codebase, &["commit", "-qm", "base"]);
    let object_format = git(&codebase, &["rev-parse", "--show-object-format"]);
    fs::write(codebase.join("src/new.rs"), "fn new() {}\n")
        .expect("untracked source should be written");

    let output = loam(
        &codebase,
        &["codegraph", "walk", codebase.to_str().unwrap()],
    );
    let text = String::from_utf8(output.stdout).expect("walk output should be UTF-8");
    fs::remove_dir_all(&codebase).expect("temporary codebase should be removed");

    assert!(output.status.success(), "{text}");
    assert!(text.contains(r#""path":"src/main.rs""#), "{text}");
    assert!(text.contains(r#""source_state":"committed""#), "{text}");
    assert!(text.contains(r#""path":"src/new.rs""#), "{text}");
    assert!(text.contains(r#""source_state":"provisional""#), "{text}");
    let identity_prefix = format!(r#""content_id":"git:{object_format}:"#);
    assert!(text.matches(&identity_prefix).count() >= 2, "{text}");
}

#[test]
fn codegraph_walk_marks_modified_tracked_files_provisional() {
    let codebase = temporary_codebase();
    init_git(&codebase);
    fs::write(codebase.join("src/main.rs"), "fn committed() {}\n")
        .expect("source should be written");
    git(&codebase, &["add", "src/main.rs"]);
    git(&codebase, &["commit", "-qm", "base"]);
    fs::write(codebase.join("src/main.rs"), "fn modified() {}\n").expect("source should change");
    let expected_oid = git(&codebase, &["hash-object", "src/main.rs"]);

    let output = loam(
        &codebase,
        &["codegraph", "walk", codebase.to_str().unwrap()],
    );
    let text = String::from_utf8(output.stdout).expect("walk output should be UTF-8");
    fs::remove_dir_all(&codebase).expect("temporary codebase should be removed");

    assert!(output.status.success(), "{text}");
    assert!(
        text.contains(&format!(r#""blob_oid":"{expected_oid}""#)),
        "{text}"
    );
    assert!(text.contains(r#""source_state":"provisional""#), "{text}");
    assert!(text.contains(r#""source_commit":"""#), "{text}");
}

#[test]
fn codegraph_walk_uses_git_clean_filters_for_identity() {
    let codebase = temporary_codebase();
    init_git(&codebase);
    fs::write(codebase.join(".gitattributes"), "*.rs text eol=lf\n")
        .expect("attributes should be written");
    fs::write(codebase.join("src/main.rs"), b"fn main() {}\r\n").expect("source should be written");
    let expected_oid = git(
        &codebase,
        &["hash-object", "--path=src/main.rs", "src/main.rs"],
    );
    let object_format = git(&codebase, &["rev-parse", "--show-object-format"]);

    let output = loam(
        &codebase,
        &["codegraph", "walk", codebase.to_str().unwrap()],
    );
    let text = String::from_utf8(output.stdout).expect("walk output should be UTF-8");
    fs::remove_dir_all(&codebase).expect("temporary codebase should be removed");

    assert!(output.status.success(), "{text}");
    assert!(
        text.contains(&format!(
            r#""content_id":"git:{object_format}:{expected_oid}""#
        )),
        "{text}"
    );
}

#[test]
fn codegraph_walk_falls_back_when_git_is_unavailable() {
    let codebase = temporary_codebase();
    init_git(&codebase);
    fs::write(codebase.join("src/main.rs"), "fn main() {}\n").expect("source should be written");
    let binary = std::env::var("CARGO_BIN_EXE_loam").expect("cargo should provide the loam binary");

    let output = Command::new(binary)
        .args(["codegraph", "walk", codebase.to_str().unwrap()])
        .env("PATH", codebase.join("missing-bin"))
        .current_dir(&codebase)
        .output()
        .expect("loam should run");
    let text = String::from_utf8(output.stdout).expect("walk output should be UTF-8");
    fs::remove_dir_all(&codebase).expect("temporary codebase should be removed");

    assert!(output.status.success(), "{text}");
    assert!(text.contains(r#""content_id":"sha256:"#), "{text}");
    assert!(text.contains(r#""source_state":"fallback""#), "{text}");
}

#[test]
fn codegraph_walk_ref_ignores_worktree_overlays() {
    let codebase = temporary_codebase();
    init_git(&codebase);
    fs::write(codebase.join("src/main.rs"), "fn committed() {}\n")
        .expect("source should be written");
    git(&codebase, &["add", "src/main.rs"]);
    git(&codebase, &["commit", "-qm", "base"]);
    let committed_blob = git(&codebase, &["rev-parse", "HEAD:src/main.rs"]);
    fs::write(codebase.join("src/main.rs"), "fn dirty() {}\n").expect("source should change");
    fs::write(codebase.join("src/new.rs"), "fn new() {}\n")
        .expect("untracked source should be written");

    let output = loam(
        &codebase,
        &[
            "codegraph",
            "walk",
            codebase.to_str().unwrap(),
            "--ref",
            "HEAD",
        ],
    );
    let text = String::from_utf8(output.stdout).expect("walk output should be UTF-8");
    fs::remove_dir_all(&codebase).expect("temporary codebase should be removed");

    assert!(output.status.success(), "{text}");
    assert!(
        text.contains(&format!(r#""blob_oid":"{committed_blob}""#)),
        "{text}"
    );
    assert!(text.contains(r#""source_state":"committed""#), "{text}");
    assert!(!text.contains("src/new.rs"), "{text}");
}

#[test]
fn codegraph_walk_ref_rejects_an_unknown_commit() {
    let codebase = temporary_codebase();
    init_git(&codebase);

    let output = loam(
        &codebase,
        &[
            "codegraph",
            "walk",
            codebase.to_str().unwrap(),
            "--ref",
            "missing-ref",
        ],
    );
    fs::remove_dir_all(&codebase).expect("temporary codebase should be removed");

    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("cannot resolve Git ref"));
}
