// Coverage for the Loam View wikilink scanner and relationship derivation
// (T4). See specs/loam-view.md "Artifact inventory and wikilink rules" and
// the "V1 relationship rules are limited to ..." paragraph, and
// cli/tests/fixtures/view/README.md for what each fixture contains.
//
// All test names carry a `view_links_` prefix so `cargo test view_links`
// (the task's verify command) reliably selects them regardless of how
// cargo's substring filter matches integration-test function names.
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

fn copy_fixture(name: &str) -> PathBuf {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/view")
        .join(name);
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be after epoch")
        .as_nanos();
    let dest = std::env::temp_dir().join(format!("loam-view-links-{name}-{nonce}"));
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

/// Best-effort schema round-trip against T2's real validator, mirroring
/// cli/tests/view_inventory.rs. Skips quietly when `node` is not on PATH.
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
fn view_links_healthy_fixture_produces_the_expected_explicit_and_derived_edge_set() {
    let workspace = copy_fixture("healthy");
    let snapshot = view_snapshot(&workspace);
    fs::remove_dir_all(&workspace).ok();

    // Every relationship entry carries a `"from":"..."` key; artifacts never do,
    // so this is an unambiguous count of relationship entries.
    assert_eq!(
        count_occurrences(&snapshot, "\"from\":\""),
        12,
        "{snapshot}"
    );

    // Rule 1 -- every resolved wikilink anywhere becomes an explicit edge.
    for (from, to) in [
        ("wiki/index.md", "wiki/topics/greeting.md"),
        ("wiki/index.md", "wiki/code/_index.md"),
        ("wiki/topics/greeting.md", "wiki/code/greeter.md"),
        ("wiki/code/_index.md", "wiki/code/greeter.md"),
        ("wiki/code/greeter.md", "wiki/topics/greeting.md"),
    ] {
        let needle =
            format!(r#""from":"{from}","to":"{to}","kind":"wikilink","origin":"explicit""#);
        assert!(snapshot.contains(&needle), "missing {needle} in {snapshot}");
    }

    // Rule 6 -- a wikilink under a code page's "## Callers" section also derives
    // a typed code-caller edge layered on top of the explicit wikilink above.
    assert!(snapshot.contains(
        r#""from":"wiki/code/greeter.md","to":"wiki/topics/greeting.md","kind":"code-caller","origin":"derived""#
    ));
    // The "## Dependencies" section is empty ("- none"), so no code-dependency edge.
    assert!(
        !snapshot.contains("\"kind\":\"code-dependency\""),
        "{snapshot}"
    );

    // Rule 2 -- goal's "## Linked work" section derives goal -> spec / goal -> plan.
    assert!(snapshot.contains(
        r#""from":"goals/improve-greeting.md","to":"specs/greeting-spec.md","kind":"goal-linked-spec","origin":"derived""#
    ));
    assert!(snapshot.contains(
        r#""from":"goals/improve-greeting.md","to":"plans/greeting-plan.md","kind":"goal-linked-plan","origin":"derived""#
    ));

    // Rule 3 -- spec's `goal:` front matter derives spec -> goal provenance.
    assert!(snapshot.contains(
        r#""from":"specs/greeting-spec.md","to":"goals/improve-greeting.md","kind":"spec-goal","origin":"derived","evidence":{"path":"specs/greeting-spec.md","line":null,"section":null,"field":"goal""#
    ));

    // Rule 4 -- plan's `spec:`/`goal:` front matter derives plan -> spec / plan -> goal.
    assert!(snapshot.contains(
        r#""from":"plans/greeting-plan.md","to":"specs/greeting-spec.md","kind":"plan-spec","origin":"derived""#
    ));
    assert!(snapshot.contains(
        r#""from":"plans/greeting-plan.md","to":"goals/improve-greeting.md","kind":"plan-goal","origin":"derived""#
    ));

    // Rule 7 -- the plan's one touched file that uniquely maps to a code artifact's
    // `source_path` (src/greeter.js -> wiki/code/greeter.md) derives an edge; the
    // touched wiki path itself (wiki/code/greeter.md) maps to no code artifact's
    // source_path, so it stays plain evidence only and derives no edge.
    assert!(snapshot.contains(
        r#""from":"plans/greeting-plan.md","to":"wiki/code/greeter.md","kind":"plan-touched-file","origin":"derived""#
    ));
    assert_eq!(
        count_occurrences(&snapshot, "\"kind\":\"plan-touched-file\""),
        1,
        "{snapshot}"
    );

    // Rule 5 -- no checkpoint in this fixture sets Previous/Supersedes.
    assert!(!snapshot.contains("checkpoint-previous"), "{snapshot}");
    assert!(!snapshot.contains("checkpoint-supersedes"), "{snapshot}");

    // Every relationship id is a 64-char lowercase hex SHA-256, and every derived
    // relationship carries a rule object; explicit wikilinks carry `"rule":null`.
    assert_eq!(
        count_occurrences(&snapshot, "\"rule\":null"),
        5,
        "{snapshot}"
    );

    assert_schema_valid(&snapshot);
}

/// The 64-hex-char `id` of every relationship entry, sorted. `rule.generated_at`
/// legitimately varies run to run (it mirrors the snapshot's own timestamp), so
/// this checks the actual "stable across runs" contract -- the id itself --
/// rather than comparing the full relationships blob.
fn relationship_ids(snapshot: &str) -> Vec<String> {
    let mut ids: Vec<String> = snapshot
        .split("\"id\":\"")
        .skip(1)
        .filter_map(|rest| rest.get(0..64))
        .filter(|candidate| candidate.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .map(str::to_owned)
        .collect();
    ids.sort();
    ids
}

#[test]
fn view_links_relationship_ids_are_stable_across_runs() {
    let workspace = copy_fixture("healthy");
    let first = view_snapshot(&workspace);
    let second = view_snapshot(&workspace);
    fs::remove_dir_all(&workspace).ok();

    let first_ids = relationship_ids(&first);
    let second_ids = relationship_ids(&second);
    assert_eq!(first_ids.len(), 12, "{first}");
    assert_eq!(first_ids, second_ids);
}

#[test]
fn view_links_broken_links_fixture_never_turns_an_unresolved_target_into_an_edge() {
    let workspace = copy_fixture("broken-links");
    let snapshot = view_snapshot(&workspace);
    fs::remove_dir_all(&workspace).ok();

    // Exactly 5 edges: 4 explicit path-form links from wiki/index.md, plus the
    // one bare `[[setup]]` link that resolves uniquely case-insensitively.
    assert_eq!(count_occurrences(&snapshot, "\"from\":\""), 5, "{snapshot}");

    for (from, to) in [
        ("wiki/index.md", "wiki/topics/broken-links-demo.md"),
        ("wiki/index.md", "wiki/topics/overview.md"),
        ("wiki/index.md", "wiki/topics/Setup.md"),
        ("wiki/index.md", "wiki/entities/overview.md"),
    ] {
        let needle =
            format!(r#""from":"{from}","to":"{to}","kind":"wikilink","origin":"explicit""#);
        assert!(snapshot.contains(&needle), "missing {needle} in {snapshot}");
    }

    // The noncanonical-case `[[setup]]` link still resolves (to wiki/topics/Setup.md)
    // and becomes an edge, at its real one-based source line.
    assert!(snapshot.contains(
        r#""from":"wiki/topics/broken-links-demo.md","to":"wiki/topics/Setup.md","kind":"wikilink","origin":"explicit","evidence":{"path":"wiki/topics/broken-links-demo.md","line":7"#
    ));

    // `[[does-not-exist]]` (broken) and `[[overview]]` (ambiguous -- matches both
    // wiki/topics/overview.md and wiki/entities/overview.md) never become edges.
    assert!(!snapshot.contains("does-not-exist"), "{snapshot}");
    assert!(
        !snapshot.contains(
            r#""from":"wiki/topics/broken-links-demo.md","to":"wiki/topics/overview.md""#
        ),
        "{snapshot}"
    );
    assert!(
        !snapshot.contains(
            r#""from":"wiki/topics/broken-links-demo.md","to":"wiki/entities/overview.md""#
        ),
        "{snapshot}"
    );

    // Content inside a fenced code block or an inline code span is never scanned.
    assert!(!snapshot.contains("also-does-not-exist"), "{snapshot}");
    assert!(!snapshot.contains("inline-code-not-a-link"), "{snapshot}");

    assert_schema_valid(&snapshot);
}

#[test]
fn view_links_sparse_workspace_has_no_relationships() {
    let workspace = copy_fixture("sparse");
    let snapshot = view_snapshot(&workspace);
    fs::remove_dir_all(&workspace).ok();

    assert!(snapshot.contains("\"relationships\":[]"), "{snapshot}");
    assert_schema_valid(&snapshot);
}
