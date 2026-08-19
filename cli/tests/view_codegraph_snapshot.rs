// Native coverage for `loam codegraph snapshot`, the one-pass {walk, index, diff}
// probe for Loam View. See specs/loam-view.md "Codegraph snapshot and formulas".
use std::fs;
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

fn temporary_root(label: &str) -> std::path::PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be after epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("loam-{label}-{nonce}"));
    fs::create_dir_all(&path).expect("temporary root should be created");
    path
}

fn loam(args: &[&str]) -> Output {
    let binary = std::env::var("CARGO_BIN_EXE_loam").expect("cargo should provide the loam binary");
    Command::new(binary)
        .args(args)
        .output()
        .expect("loam should run")
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("output should be UTF-8")
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).expect("stderr should be UTF-8")
}

/// wiki root with a `code/` page describing `src/main.rs`.
fn wiki_with_page(root: &std::path::Path, frontmatter: &str, slug: &str) {
    fs::create_dir_all(root.join("code")).expect("code dir should be created");
    fs::write(root.join("SCHEMA.md"), "# schema\n").expect("schema should be written");
    fs::write(
        root.join("code").join(format!("{slug}.md")),
        format!("---\n{frontmatter}---\n\n# page\n"),
    )
    .expect("code page should be written");
}

/// A fixture with one stale page (`src/known.rs`, legacy record with no content_id)
/// and one uncovered file (`src/fresh.rs`), matching the diff fixtures elsewhere in
/// this crate so snapshot's diff half exercises both `new` and `stale`.
fn fixture(label: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let wiki = temporary_root(&format!("{label}-wiki"));
    let codebase = temporary_root(&format!("{label}-code"));
    fs::create_dir_all(codebase.join("src")).expect("src should be created");
    fs::write(codebase.join("src/known.rs"), "fn known() {}\n").expect("known source");
    fs::write(codebase.join("src/fresh.rs"), "fn fresh() {}\n").expect("new source");
    wiki_with_page(
        &wiki,
        "source_path: src/known.rs\ningested_at: \"1\"\nsource_size: \"1\"\n",
        "src-known-rs",
    );
    (codebase, wiki)
}

#[test]
fn codegraph_snapshot_equals_separate_walk_index_diff_runs() {
    let (codebase, wiki) = fixture("snapshot-equivalence");

    let walk = loam(&["codegraph", "walk", codebase.to_str().unwrap()]);
    let index = loam(&[
        "codegraph",
        "index",
        wiki.to_str().unwrap(),
        "--codebase-root",
        codebase.to_str().unwrap(),
    ]);
    let diff = loam(&[
        "codegraph",
        "diff",
        codebase.to_str().unwrap(),
        wiki.to_str().unwrap(),
    ]);
    let snapshot = loam(&[
        "codegraph",
        "snapshot",
        codebase.to_str().unwrap(),
        wiki.to_str().unwrap(),
    ]);

    let walk_text = stdout(&walk);
    let index_text = stdout(&index);
    let diff_text = stdout(&diff);
    let snapshot_text = stdout(&snapshot);
    fs::remove_dir_all(&wiki).ok();
    fs::remove_dir_all(&codebase).ok();

    assert!(walk.status.success(), "walk stderr: {}", stderr(&walk));
    assert!(index.status.success(), "index stderr: {}", stderr(&index));
    assert!(diff.status.success(), "diff stderr: {}", stderr(&diff));
    assert!(
        snapshot.status.success(),
        "snapshot stderr: {}",
        stderr(&snapshot)
    );

    let expected = format!(
        "{{\"walk\":{},\"index\":{},\"diff\":{}}}",
        walk_text.trim(),
        index_text.trim(),
        diff_text.trim()
    );
    assert_eq!(snapshot_text.trim(), expected, "snapshot: {snapshot_text}");

    // Sanity: the fixture actually exercises both diff reasons, so the equality
    // assertion above is not vacuously true against two empty arrays.
    assert!(diff_text.contains("\"reason\":\"new\""), "{diff_text}");
    assert!(diff_text.contains("\"reason\":\"stale\""), "{diff_text}");
}

#[test]
fn codegraph_snapshot_requires_both_roots() {
    let output = loam(&["codegraph", "snapshot"]);
    assert_eq!(output.status.code(), Some(1));

    let codebase = temporary_root("snapshot-missing-wiki-code");
    let output = loam(&["codegraph", "snapshot", codebase.to_str().unwrap()]);
    fs::remove_dir_all(&codebase).ok();
    assert_eq!(output.status.code(), Some(1));
}

#[test]
fn codegraph_snapshot_validates_the_wiki_root_contract() {
    let (codebase, _wiki) = fixture("snapshot-bad-wiki");
    let bad_wiki = temporary_root("snapshot-bad-wiki-root");

    let output = loam(&[
        "codegraph",
        "snapshot",
        codebase.to_str().unwrap(),
        bad_wiki.to_str().unwrap(),
    ]);
    let message = stderr(&output);
    fs::remove_dir_all(&codebase).ok();
    fs::remove_dir_all(&bad_wiki).ok();

    assert_eq!(output.status.code(), Some(2), "stderr: {message}");
    assert!(
        message.contains("wiki root contract not found"),
        "{message}"
    );
}

#[test]
fn codegraph_snapshot_rejects_an_unusable_codebase_root() {
    let wiki = temporary_root("snapshot-missing-codebase-wiki");
    fs::write(wiki.join("SCHEMA.md"), "# schema\n").expect("schema should be written");

    let output = loam(&[
        "codegraph",
        "snapshot",
        "/nonexistent-loam-codegraph-snapshot-root",
        wiki.to_str().unwrap(),
    ]);
    let message = stderr(&output);
    fs::remove_dir_all(&wiki).ok();

    assert_eq!(output.status.code(), Some(2), "stderr: {message}");
    assert!(message.contains("codebase root not found"), "{message}");
}
