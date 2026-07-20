// Native coverage for `loam codegraph index|diff`, including the four wiki-root
// validation fixtures previously owned by loam-common.sh.
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

#[test]
fn index_emits_records_for_code_pages() {
    let wiki = temporary_root("index-wiki");
    let codebase = temporary_root("index-code");
    fs::create_dir_all(codebase.join("src")).expect("src should be created");
    fs::write(codebase.join("src/main.rs"), "fn main() {}\n").expect("source should be written");
    wiki_with_page(
        &wiki,
        "source_path: src/main.rs\ningested_at: \"1700000000\"\nsource_size: \"13\"\ncontent_hash: \"ABC\"\n",
        "src-main-rs",
    );

    let output = loam(&[
        "codegraph",
        "index",
        wiki.to_str().unwrap(),
        "--codebase-root",
        codebase.to_str().unwrap(),
    ]);
    let text = stdout(&output);
    fs::remove_dir_all(&wiki).ok();
    fs::remove_dir_all(&codebase).ok();

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(text.contains("\"source_path\":\"src/main.rs\""), "{text}");
    assert!(text.contains("\"slug\":\"src-main-rs\""), "{text}");
    assert!(text.contains("\"ingested_at\":\"1700000000\""), "{text}");
    assert!(text.contains("\"content_hash\":\"abc\""), "{text}");
    assert!(text.contains("\"exists\":true"), "{text}");
}

#[test]
fn index_marks_missing_sources_as_absent() {
    let wiki = temporary_root("index-missing-wiki");
    let codebase = temporary_root("index-missing-code");
    wiki_with_page(
        &wiki,
        "source_path: src/gone.rs\ningested_at: \"1700000000\"\n",
        "src-gone-rs",
    );

    let output = loam(&[
        "codegraph",
        "index",
        wiki.to_str().unwrap(),
        "--codebase-root",
        codebase.to_str().unwrap(),
    ]);
    let text = stdout(&output);
    fs::remove_dir_all(&wiki).ok();
    fs::remove_dir_all(&codebase).ok();

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(text.contains("\"exists\":false"), "{text}");
    assert!(text.contains("\"mtime\":\"\""), "{text}");
}

#[test]
fn index_skips_pages_without_source_path_or_ingested_at() {
    let wiki = temporary_root("index-partial-wiki");
    wiki_with_page(&wiki, "source_path: src/only.rs\n", "src-only-rs");

    let output = loam(&["codegraph", "index", wiki.to_str().unwrap()]);
    let text = stdout(&output);
    fs::remove_dir_all(&wiki).ok();

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert_eq!(text.trim(), "[]");
}

#[test]
fn index_validates_wiki_root_contract() {
    // Fixture 1: contract file inside wiki/ probed directly → 0.
    let one = temporary_root("wiki-fixture-one");
    fs::create_dir_all(one.join("wiki")).expect("wiki dir");
    fs::write(one.join("wiki/SCHEMA.md"), "").expect("schema");
    let output = loam(&["codegraph", "index", one.join("wiki").to_str().unwrap()]);
    assert!(output.status.success(), "fixture 1: {}", stderr(&output));

    // Fixture 2: contract file at the root itself → 0.
    let two = temporary_root("wiki-fixture-two");
    fs::write(two.join("SCHEMA.md"), "").expect("schema");
    let output = loam(&["codegraph", "index", two.to_str().unwrap()]);
    assert!(output.status.success(), "fixture 2: {}", stderr(&output));

    // Fixture 3: contract one level down, parent passed → 2 + "did you mean".
    let three = temporary_root("wiki-fixture-three");
    fs::create_dir_all(three.join("wiki")).expect("wiki dir");
    fs::write(three.join("wiki/SCHEMA.md"), "").expect("schema");
    let output = loam(&["codegraph", "index", three.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(2), "fixture 3 exit code");
    assert!(
        stderr(&output).contains("did you mean"),
        "fixture 3: {}",
        stderr(&output)
    );

    // Fixture 4: nonexistent path → 2 + "wiki root not found".
    let output = loam(&["codegraph", "index", "/nonexistent-loam-wiki-root"]);
    assert_eq!(output.status.code(), Some(2), "fixture 4 exit code");
    assert!(
        stderr(&output).contains("wiki root not found"),
        "fixture 4: {}",
        stderr(&output)
    );

    for root in [one, two, three] {
        fs::remove_dir_all(root).ok();
    }
}

#[test]
fn diff_reports_new_and_stale_entries() {
    let wiki = temporary_root("diff-wiki");
    let codebase = temporary_root("diff-code");
    fs::create_dir_all(codebase.join("src")).expect("src should be created");
    fs::write(codebase.join("src/known.rs"), "fn known() {}\n").expect("known source");
    fs::write(codebase.join("src/fresh.rs"), "fn fresh() {}\n").expect("new source");
    // ingested_at far in the past → mtime newer → stale.
    wiki_with_page(
        &wiki,
        "source_path: src/known.rs\ningested_at: \"1\"\nsource_size: \"1\"\n",
        "src-known-rs",
    );

    let output = loam(&[
        "codegraph",
        "diff",
        codebase.to_str().unwrap(),
        wiki.to_str().unwrap(),
    ]);
    let text = stdout(&output);
    fs::remove_dir_all(&wiki).ok();
    fs::remove_dir_all(&codebase).ok();

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(text.contains("\"path\":\"src/fresh.rs\""), "{text}");
    assert!(text.contains("\"reason\":\"new\""), "{text}");
    assert!(text.contains("\"path\":\"src/known.rs\""), "{text}");
    assert!(text.contains("\"reason\":\"stale\""), "{text}");
    assert!(text.contains("\"slug\":\"src-known-rs\""), "{text}");
}

#[test]
fn diff_treats_matching_hash_as_current() {
    let wiki = temporary_root("diff-hash-wiki");
    let codebase = temporary_root("diff-hash-code");
    fs::create_dir_all(codebase.join("src")).expect("src should be created");
    let body = "fn known() {}\n";
    fs::write(codebase.join("src/known.rs"), body).expect("known source");
    let hash = "ff93b8b31f63b372f27a4c10588f9fa4c5735a16b7d7ec3d059cb5066b15c344";
    wiki_with_page(
        &wiki,
        &format!(
            "source_path: src/known.rs\ningested_at: \"1\"\nsource_size: \"{}\"\ncontent_hash: \"{hash}\"\n",
            body.len()
        ),
        "src-known-rs",
    );

    let output = loam(&[
        "codegraph",
        "diff",
        codebase.to_str().unwrap(),
        wiki.to_str().unwrap(),
    ]);
    let text = stdout(&output);
    fs::remove_dir_all(&wiki).ok();
    fs::remove_dir_all(&codebase).ok();

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert_eq!(text.trim(), "[]");
}

#[test]
fn diff_strict_rehashes_regardless_of_mtime() {
    let wiki = temporary_root("diff-strict-wiki");
    let codebase = temporary_root("diff-strict-code");
    fs::create_dir_all(codebase.join("src")).expect("src should be created");
    fs::write(codebase.join("src/known.rs"), "fn changed() {}\n").expect("known source");
    // ingested_at in the far future so the non-strict path would say "current".
    wiki_with_page(
        &wiki,
        "source_path: src/known.rs\ningested_at: \"9999999999\"\nsource_size: \"16\"\ncontent_hash: \"deadbeef\"\n",
        "src-known-rs",
    );

    let relaxed = loam(&[
        "codegraph",
        "diff",
        codebase.to_str().unwrap(),
        wiki.to_str().unwrap(),
    ]);
    let strict = loam(&[
        "codegraph",
        "diff",
        codebase.to_str().unwrap(),
        wiki.to_str().unwrap(),
        "--strict",
    ]);
    let relaxed_text = stdout(&relaxed);
    let strict_text = stdout(&strict);
    fs::remove_dir_all(&wiki).ok();
    fs::remove_dir_all(&codebase).ok();

    assert_eq!(relaxed_text.trim(), "[]", "relaxed: {relaxed_text}");
    assert!(
        strict_text.contains("\"reason\":\"stale\""),
        "strict: {strict_text}"
    );
}

#[test]
fn codegraph_rejects_unknown_subcommand() {
    let output = loam(&["codegraph", "bogus"]);
    assert_eq!(output.status.code(), Some(1));
}

#[test]
fn top_level_usage_lists_every_codegraph_command() {
    let output = loam(&[]);
    let text = stderr(&output);

    assert_eq!(output.status.code(), Some(1));
    for command in [
        "loam codegraph index",
        "loam codegraph walk",
        "loam codegraph diff",
        "--strict",
    ] {
        assert!(text.contains(command), "missing {command:?} from:\n{text}");
    }
}

#[test]
fn index_scans_legacy_entities_directory() {
    let wiki = temporary_root("legacy-entities-wiki");
    fs::create_dir_all(wiki.join("entities")).expect("entities dir");
    fs::write(wiki.join("SCHEMA.md"), "").expect("schema");
    fs::write(
        wiki.join("entities/legacy-page.md"),
        "---\nsource_path: src/legacy.ts\ningested_at: \"1700000000\"\n---\n",
    )
    .expect("legacy page");

    let output = loam(&["codegraph", "index", wiki.to_str().unwrap()]);
    let text = stdout(&output);
    fs::remove_dir_all(&wiki).ok();

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(text.contains("\"source_path\":\"src/legacy.ts\""), "{text}");
    assert!(text.contains("\"slug\":\"legacy-page\""), "{text}");
}

#[test]
fn index_prefers_code_pages_over_legacy_entities() {
    let wiki = temporary_root("precedence-wiki");
    fs::create_dir_all(wiki.join("code")).expect("code dir");
    fs::create_dir_all(wiki.join("entities")).expect("entities dir");
    fs::write(wiki.join("SCHEMA.md"), "").expect("schema");
    fs::write(
        wiki.join("code/current.md"),
        "---\nsource_path: src/shared.ts\ningested_at: \"1700000000\"\n---\n",
    )
    .expect("code page");
    fs::write(
        wiki.join("entities/stranded.md"),
        "---\nsource_path: src/shared.ts\ningested_at: \"1600000000\"\n---\n",
    )
    .expect("entities page");

    let output = loam(&["codegraph", "index", wiki.to_str().unwrap()]);
    let text = stdout(&output);
    fs::remove_dir_all(&wiki).ok();

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(text.contains("\"slug\":\"current\""), "{text}");
    assert!(!text.contains("stranded"), "{text}");
    assert_eq!(text.matches("src/shared.ts").count(), 1, "{text}");
}

#[test]
fn diff_reports_stale_when_recorded_size_differs() {
    let wiki = temporary_root("size-wiki");
    let codebase = temporary_root("size-code");
    fs::create_dir_all(codebase.join("src")).expect("src");
    let body = "fn known() {}\n";
    fs::write(codebase.join("src/known.rs"), body).expect("known source");
    let hash = "ff93b8b31f63b372f27a4c10588f9fa4c5735a16b7d7ec3d059cb5066b15c344";
    // Correct hash but a stale recorded size: size mismatch short-circuits to stale.
    wiki_with_page(
        &wiki,
        &format!(
            "source_path: src/known.rs\ningested_at: \"1\"\nsource_size: \"999\"\ncontent_hash: \"{hash}\"\n"
        ),
        "src-known-rs",
    );

    let output = loam(&[
        "codegraph",
        "diff",
        codebase.to_str().unwrap(),
        wiki.to_str().unwrap(),
    ]);
    let text = stdout(&output);
    fs::remove_dir_all(&wiki).ok();
    fs::remove_dir_all(&codebase).ok();

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(text.contains("\"reason\":\"stale\""), "{text}");
}

#[test]
fn diff_reports_stale_when_ingested_at_is_not_an_epoch() {
    let wiki = temporary_root("legacy-mtime-wiki");
    let codebase = temporary_root("legacy-mtime-code");
    fs::create_dir_all(codebase.join("src")).expect("src");
    fs::write(codebase.join("src/known.rs"), "fn known() {}\n").expect("known source");
    wiki_with_page(
        &wiki,
        "source_path: src/known.rs\ningested_at: 2026-06-24 14:33 +02:00\n",
        "src-known-rs",
    );

    let output = loam(&[
        "codegraph",
        "diff",
        codebase.to_str().unwrap(),
        wiki.to_str().unwrap(),
    ]);
    let text = stdout(&output);
    fs::remove_dir_all(&wiki).ok();
    fs::remove_dir_all(&codebase).ok();

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(text.contains("\"reason\":\"stale\""), "{text}");
}

#[test]
fn diff_discovers_the_wiki_root_when_it_is_omitted() {
    let root = temporary_root("diff-implicit");
    fs::create_dir_all(root.join("src")).expect("src");
    fs::write(root.join("src/known.rs"), "fn known() {}\n").expect("known source");
    fs::write(root.join("src/fresh.rs"), "fn fresh() {}\n").expect("new source");
    wiki_with_page(
        &root.join("wiki"),
        "source_path: src/known.rs\ningested_at: \"1\"\nsource_size: \"1\"\n",
        "src-known-rs",
    );

    let implicit = loam(&["codegraph", "diff", root.to_str().unwrap()]);
    let explicit = loam(&[
        "codegraph",
        "diff",
        root.to_str().unwrap(),
        root.join("wiki").to_str().unwrap(),
    ]);
    let implicit_text = stdout(&implicit);
    let explicit_text = stdout(&explicit);
    fs::remove_dir_all(&root).ok();

    assert!(implicit.status.success(), "stderr: {}", stderr(&implicit));
    assert_eq!(
        implicit_text, explicit_text,
        "discovered wiki root must match the explicit one"
    );
    assert!(
        implicit_text.contains("\"reason\":\"new\""),
        "{implicit_text}"
    );
    assert!(
        implicit_text.contains("\"reason\":\"stale\""),
        "{implicit_text}"
    );
}

#[test]
fn diff_discovers_a_wiki_root_at_the_codebase_root_itself() {
    let root = temporary_root("diff-implicit-flat");
    fs::create_dir_all(root.join("src")).expect("src");
    fs::write(root.join("src/fresh.rs"), "fn fresh() {}\n").expect("source");
    // Contract files at the codebase root: the wiki IS the workspace.
    wiki_with_page(
        &root,
        "source_path: src/other.rs\ningested_at: \"1\"\n",
        "other",
    );

    let output = loam(&["codegraph", "diff", root.to_str().unwrap()]);
    let text = stdout(&output);
    fs::remove_dir_all(&root).ok();

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(text.contains("\"path\":\"src/fresh.rs\""), "{text}");
}

#[test]
fn diff_without_a_discoverable_wiki_root_fails_rather_than_reporting_everything_new() {
    let root = temporary_root("diff-implicit-missing");
    fs::create_dir_all(root.join("src")).expect("src");
    fs::write(root.join("src/fresh.rs"), "fn fresh() {}\n").expect("source");

    let output = loam(&["codegraph", "diff", root.to_str().unwrap()]);
    let message = stderr(&output);
    fs::remove_dir_all(&root).ok();

    // An empty index looks exactly like "nothing is stale", so this must never
    // silently succeed.
    assert_eq!(output.status.code(), Some(2), "stderr: {message}");
    assert!(message.contains("no wiki root found"), "{message}");
}

#[test]
fn diff_with_an_explicit_bad_wiki_root_still_reports_the_did_you_mean_hint() {
    let root = temporary_root("diff-explicit-bad");
    fs::create_dir_all(root.join("src")).expect("src");
    fs::write(root.join("src/fresh.rs"), "fn fresh() {}\n").expect("source");
    wiki_with_page(
        &root.join("wiki"),
        "source_path: x\ningested_at: \"1\"\n",
        "x",
    );

    // Explicitly passing the parent of the wiki keeps the old hard failure.
    let output = loam(&[
        "codegraph",
        "diff",
        root.to_str().unwrap(),
        root.to_str().unwrap(),
    ]);
    let message = stderr(&output);
    fs::remove_dir_all(&root).ok();

    assert_eq!(output.status.code(), Some(2), "stderr: {message}");
    assert!(message.contains("did you mean"), "{message}");
}
