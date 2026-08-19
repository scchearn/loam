// Coverage for `loam state --view`, the Loam View snapshot producer (T3).
// See specs/loam-view.md "Snapshot v1 shape" and "Artifact inventory and
// wikilink rules", and cli/tests/fixtures/view/README.md for what each
// fixture actually contains.
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

/// Fixtures live under version control inside this worktree, so running
/// `git status` against them in place would pick up whatever this session
/// happens to be editing. Copying each fixture into an isolated temp
/// directory keeps `workspace.git` (and the git capability) deterministic:
/// the copy is never a git repository.
fn copy_fixture(name: &str) -> PathBuf {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/view")
        .join(name);
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be after epoch")
        .as_nanos();
    let dest = std::env::temp_dir().join(format!("loam-view-inventory-{name}-{nonce}"));
    copy_dir_recursive(&source, &dest);
    dest
}

fn copy_dir_recursive(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).expect("fixture directory should be created");
    for entry in fs::read_dir(src).expect("fixture directory should be readable") {
        let entry = entry.expect("fixture entry should be readable");
        let dst_path = dst.join(entry.file_name());
        if entry
            .file_type()
            .expect("file type should be readable")
            .is_dir()
        {
            copy_dir_recursive(&entry.path(), &dst_path);
        } else {
            fs::copy(entry.path(), &dst_path).expect("fixture file should be copied");
        }
    }
}

fn loam(args: &[&str]) -> Output {
    let binary = std::env::var("CARGO_BIN_EXE_loam").expect("cargo should provide the loam binary");
    Command::new(binary)
        .args(args)
        .output()
        .expect("loam should run")
}

fn view_snapshot(workspace: &Path) -> String {
    let output = loam(&["state", "--view", workspace.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "loam state --view should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("snapshot stdout should be UTF-8")
}

/// Best-effort schema round-trip against T2's real validator
/// (view/server/validate-snapshot.mjs). Skips quietly when `node` is not on
/// PATH rather than failing the suite over an optional check; the Rust
/// assertions in each test are the actual coverage.
fn assert_schema_valid(snapshot_json: &str) {
    let script =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/validate-view-snapshot.mjs");
    let Ok(mut child) = Command::new("node")
        .arg(&script)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
    else {
        eprintln!("node not found on PATH; skipping schema round-trip");
        return;
    };
    use std::io::Write;
    child
        .stdin
        .take()
        .expect("child stdin should be piped")
        .write_all(snapshot_json.as_bytes())
        .expect("snapshot should be written to validator stdin");
    let output = child.wait_with_output().expect("validator should run");
    assert!(
        output.status.success(),
        "snapshot failed schema validation: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn count_occurrences(haystack: &str, needle: &str) -> usize {
    haystack.matches(needle).count()
}

#[test]
fn view_inventory_sparse_workspace_is_not_configured_with_honest_capabilities() {
    let workspace = copy_fixture("sparse");
    let snapshot = view_snapshot(&workspace);
    fs::remove_dir_all(&workspace).ok();

    assert!(
        snapshot.contains(r#""status":"not-configured""#),
        "{snapshot}"
    );
    assert!(
        snapshot.contains(r#""wiki":{"state":"absent","required":true,"reason":"no wiki/ directory found","evidence":null}"#),
        "{snapshot}"
    );
    assert!(
        snapshot.contains(r#""search_corpus":{"state":"absent","required":true,"reason":"no wiki/ directory found","evidence":null}"#),
        "{snapshot}"
    );
    assert!(
        snapshot.contains(
            r#""code_graph":{"state":"absent","required":false,"reason":null,"evidence":null}"#
        ),
        "{snapshot}"
    );
    assert!(
        snapshot.contains(
            r#""goals":{"state":"absent","required":false,"reason":null,"evidence":null}"#
        ),
        "{snapshot}"
    );

    // Only AGENTS.md is inventoried -- no wiki/, goals/, specs/, or plans/ exist.
    // `"parse_errors":` is an artifact-only key (relationships also carry a
    // `"kind"` field, so counting that key directly would double-count edges).
    assert_eq!(
        count_occurrences(&snapshot, "\"parse_errors\":"),
        1,
        "{snapshot}"
    );
    assert!(
        snapshot.contains(r#""path":"AGENTS.md","kind":"guidance""#),
        "{snapshot}"
    );
    assert!(snapshot.contains(
        r#""relationships":[],"events":[],"metrics":{},"signals":[],"hints":[],"probes":[]"#
    ));

    assert_schema_valid(&snapshot);
}

#[test]
fn view_inventory_healthy_workspace_is_a_complete_correctly_kinded_inventory() {
    let workspace = copy_fixture("healthy");
    let snapshot = view_snapshot(&workspace);
    fs::remove_dir_all(&workspace).ok();

    assert!(snapshot.contains(r#""status":"ready""#), "{snapshot}");
    assert!(snapshot
        .contains(r#""wiki":{"state":"ready","required":true,"reason":null,"evidence":null}"#));
    assert!(snapshot.contains(
        r#""search_corpus":{"state":"ready","required":true,"reason":null,"evidence":null}"#
    ));
    assert!(snapshot
        .contains(r#""goals":{"state":"ready","required":false,"reason":null,"evidence":null}"#));
    assert!(snapshot.contains(
        r#""checkpoints":{"state":"ready","required":false,"reason":null,"evidence":null}"#
    ));
    assert!(snapshot.contains(r#""qmd":{"state":"absent","required":false,"reason":"no qmd config found","evidence":null}"#));

    // AGENTS.md, goal, spec, plan, checkpoint, wiki-index, topic, and two code pages.
    // `"parse_errors":` is an artifact-only key (relationships also carry a
    // `"kind"` field, so counting that key directly would double-count edges).
    assert_eq!(
        count_occurrences(&snapshot, "\"parse_errors\":"),
        9,
        "{snapshot}"
    );
    assert_eq!(
        count_occurrences(&snapshot, "\"kind\":\"code\""),
        2,
        "{snapshot}"
    );
    for (path, kind) in [
        ("AGENTS.md", "guidance"),
        ("goals/improve-greeting.md", "goal"),
        ("specs/greeting-spec.md", "spec"),
        ("plans/greeting-plan.md", "plan"),
        (
            "wiki/checkpoints/checkpoint-2026-08-10-0900.md",
            "checkpoint",
        ),
        ("wiki/code/greeter.md", "code"),
        ("wiki/code/_index.md", "code"),
        ("wiki/index.md", "wiki-index"),
        ("wiki/topics/greeting.md", "topic"),
    ] {
        let needle = format!("\"path\":\"{path}\",\"kind\":\"{kind}\"");
        assert!(snapshot.contains(&needle), "missing {needle} in {snapshot}");
    }

    // Goal -> spec/plan attributes, extracted from the "## Linked work" body section.
    assert!(snapshot.contains(
        r#""attributes":{"linked_specs":["specs/greeting-spec.md"],"linked_plans":["plans/greeting-plan.md"]}"#
    ));
    // Plan -> spec/goal front matter, declared vs. observed tasks, and acceptance criteria.
    assert!(snapshot.contains(r#""spec":"specs/greeting-spec.md","goal":"goals/improve-greeting.md","task_count_declared":1,"task_count_observed":1,"task_statuses":["done"]"#));
    assert!(snapshot.contains(r#""acceptance_criteria":{"total":1,"done":1}"#));
    // Checkpoint body fields and its one workstream.
    assert!(snapshot.contains(r#""captured_at":"2026-08-10T09:00:00+02:00""#));
    assert!(snapshot.contains(r#""reason":"pause","scope":"greeting feature""#));
    assert!(
        snapshot.contains(r#""name":"Greeting spec follow-through","status":"ready-to-resume""#)
    );
    // Code page attributes: front matter's `content_hash` becomes the `source_hash` attribute,
    // and the source file exists on disk.
    assert!(snapshot.contains(r#""source_path":"src/greeter.js""#));
    assert!(snapshot.contains(r#""source_hash":"d93ba2d5e1ad3dc0e161e8aaa1869df3576d5fa9068f46a8e4ea465e8ad762d6","source_exists":true"#));
    // Every artifact's own content_hash is a real sha256 of its file bytes, and no
    // artifact in this fixture failed to parse.
    assert_eq!(
        count_occurrences(&snapshot, "\"parse_errors\":[]"),
        9,
        "{snapshot}"
    );

    assert_schema_valid(&snapshot);
}

#[test]
fn view_inventory_code_drift_fixture_records_source_existence_per_page() {
    let workspace = copy_fixture("code-drift");
    let snapshot = view_snapshot(&workspace);
    fs::remove_dir_all(&workspace).ok();

    assert!(snapshot.contains(r#""status":"ready""#));
    // stale-module.js still exists on disk (T3 does not compute drift -- that is a
    // later task's metric); orphan-module.md's source_path points at a deleted file.
    assert!(snapshot.contains(r#""path":"wiki/code/stale-module.md""#));
    assert!(snapshot.contains(r#""source_path":"src/stale-module.js""#));
    assert!(snapshot.contains(r#""source_path":"src/removed-module.js","ingested_at":"1753000000","source_size":40,"source_hash":"9f3c1b7a2e5d4c6f8091a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f7","source_exists":false"#));
    // src/new-module.js has no wiki page, so it never becomes an artifact.
    assert!(!snapshot.contains("new-module"), "{snapshot}");

    assert_schema_valid(&snapshot);
}

#[test]
fn view_inventory_broken_links_fixture_classifies_every_page_without_scanning_links() {
    let workspace = copy_fixture("broken-links");
    let snapshot = view_snapshot(&workspace);
    fs::remove_dir_all(&workspace).ok();

    // Wikilink diagnostics (broken/ambiguous/case-drift) and the relationships they
    // gate are cli/tests/view_links.rs's concern -- this test only needs a complete,
    // correctly kinded inventory of the pages involved.
    for (path, kind) in [
        ("wiki/index.md", "wiki-index"),
        ("wiki/entities/overview.md", "entity"),
        ("wiki/topics/overview.md", "topic"),
        ("wiki/topics/Setup.md", "topic"),
        ("wiki/topics/broken-links-demo.md", "topic"),
        ("AGENTS.md", "guidance"),
    ] {
        let needle = format!("\"path\":\"{path}\",\"kind\":\"{kind}\"");
        assert!(snapshot.contains(&needle), "missing {needle} in {snapshot}");
    }
    // `"parse_errors":` is an artifact-only key (relationships also carry a
    // `"kind"` field, so counting that key directly would double-count edges).
    assert_eq!(
        count_occurrences(&snapshot, "\"parse_errors\":"),
        6,
        "{snapshot}"
    );

    assert_schema_valid(&snapshot);
}

#[test]
fn view_inventory_malformed_fixture_keeps_artifacts_and_records_parse_errors() {
    let workspace = copy_fixture("malformed");
    let snapshot = view_snapshot(&workspace);
    fs::remove_dir_all(&workspace).ok();

    // A goal with an unparseable created_at and a semantically invalid (month 13,
    // hour 99) updated_at: both fields go null, but the artifact is not dropped.
    assert!(snapshot.contains(r#""path":"goals/bad-timestamps.md","kind":"goal""#));
    assert!(snapshot.contains(
        r#""title":"Bad Timestamps","lifecycle_status":"active","created_at":null,"updated_at":null"#
    ));
    assert!(snapshot.contains("invalid created_at: not-a-real-date"));
    assert!(snapshot.contains("invalid updated_at: 2026-13-45 99:99 +99:99"));

    // A topic page with unterminated YAML string/list values in front matter: still
    // present, title falls back to the body's H1, and both malformations are recorded.
    assert!(snapshot.contains(r#""path":"wiki/topics/bad-frontmatter.md","kind":"topic""#));
    assert!(snapshot.contains(r#""title":"Bad frontmatter""#));
    assert!(snapshot.contains("malformed front matter field 'title': unterminated quoted value"));
    assert!(snapshot.contains("malformed front matter field 'tags': unterminated list value"));

    assert_schema_valid(&snapshot);
}

#[test]
fn view_inventory_degraded_fixture_isolates_one_unreadable_artifact() {
    let workspace = copy_fixture("degraded");
    let snapshot = view_snapshot(&workspace);
    fs::remove_dir_all(&workspace).ok();

    // corrupt.md has invalid UTF-8 bytes: it still appears (with a real content hash
    // computed from its raw bytes) but is marked unparseable rather than dropped.
    assert!(snapshot.contains(r#""path":"wiki/code/corrupt.md","kind":"code""#));
    assert!(snapshot.contains("content is not valid UTF-8"));

    // The rest of the workspace stays fully readable and error-free.
    for path in ["wiki/index.md", "wiki/topics/status.md", "AGENTS.md"] {
        let needle = format!("\"path\":\"{path}\"");
        assert!(snapshot.contains(&needle), "missing {needle} in {snapshot}");
    }
    assert_eq!(
        count_occurrences(&snapshot, "\"parse_errors\":[]"),
        3,
        "{snapshot}"
    );

    assert_schema_valid(&snapshot);
}

#[test]
fn view_inventory_default_and_fast_outputs_are_unchanged_by_the_view_wiring() {
    let workspace = copy_fixture("healthy");

    let default_output = loam(&["state", workspace.to_str().unwrap()]);
    let fast_output = loam(&["state", "--fast", workspace.to_str().unwrap()]);
    fs::remove_dir_all(&workspace).ok();

    let default_stdout = String::from_utf8(default_output.stdout).unwrap();
    let fast_stdout = String::from_utf8(fast_output.stdout).unwrap();

    // The pre-existing default/--fast contract is untouched: no snapshot-v1 fields
    // leak in, and the legacy top-level shape is exactly as before.
    for stdout in [&default_stdout, &fast_stdout] {
        assert!(!stdout.contains("\"profile\":\"loam-view\""), "{stdout}");
        assert!(stdout.starts_with("{\"wiki_root\":"), "{stdout}");
        assert!(stdout.contains("\"qmd_ready\":"), "{stdout}");
    }
}
