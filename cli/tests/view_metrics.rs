// Coverage for `loam state --view`'s events/metrics/signals/hints/probes and
// posture (T6). See specs/loam-view.md "Loam State View profile contract"
// (events, metric catalog, signals and posture) and the T6 addendum on
// noncanonical `±HHMM` timestamps. cli/tests/fixtures/view/README.md
// documents what each fixture actually contains.
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
    let dest = std::env::temp_dir().join(format!("loam-view-metrics-{name}-{nonce}"));
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

fn metric<'a>(snapshot: &'a str, key: &str) -> &'a str {
    let needle = format!("\"{key}\":{{");
    let start = snapshot
        .find(&needle)
        .unwrap_or_else(|| panic!("metric {key} missing from {snapshot}"));
    let body_start = start + needle.len() - 1;
    let end = snapshot[body_start..]
        .find('}')
        .expect("metric object should close")
        + body_start
        + 1;
    &snapshot[start..end]
}

#[test]
fn view_metrics_healthy_fixture_computes_the_full_catalog() {
    let workspace = copy_fixture("healthy");
    let snapshot = view_snapshot(&workspace);
    fs::remove_dir_all(&workspace).ok();

    // wiki/index.md, wiki/topics/greeting.md, wiki/code/greeter.md, wiki/code/_index.md --
    // the checkpoint is excluded by the metric's own definition.
    assert_eq!(
        metric(&snapshot, "wiki.knowledge_pages"),
        r#""wiki.knowledge_pages":{"value":4,"unit":"count","state":"ready","evidence":null}"#
    );
    assert_eq!(
        metric(&snapshot, "wiki.broken_wikilinks"),
        r#""wiki.broken_wikilinks":{"value":0,"unit":"count","state":"ready","evidence":null}"#
    );
    assert_eq!(
        metric(&snapshot, "wiki.archived_pages"),
        r#""wiki.archived_pages":{"value":0,"unit":"count","state":"ready","evidence":null}"#
    );
    assert_eq!(
        metric(&snapshot, "wiki.concepts"),
        r#""wiki.concepts":{"value":0,"unit":"count","state":"ready","evidence":null}"#
    );
    // No wiki/log.md in this fixture: never ran, so null + unavailable, never zero.
    assert_eq!(
        metric(&snapshot, "wiki.last_lint_at"),
        r#""wiki.last_lint_at":{"value":null,"unit":null,"state":"unavailable","evidence":null}"#
    );
    assert_eq!(
        metric(&snapshot, "wiki.lint_age_days"),
        r#""wiki.lint_age_days":{"value":null,"unit":"days","state":"unavailable","evidence":null}"#
    );

    // Codegraph snapshot: one candidate (src/greeter.js), one index entry
    // (wiki/code/greeter.md) whose front matter `content_id` does not match the
    // computed identity, so it reads as stale rather than current.
    assert_eq!(
        metric(&snapshot, "code.candidates"),
        r#""code.candidates":{"value":1,"unit":"count","state":"ready","evidence":null}"#
    );
    assert_eq!(
        metric(&snapshot, "code.source_backed_pages"),
        r#""code.source_backed_pages":{"value":1,"unit":"count","state":"ready","evidence":null}"#
    );
    assert_eq!(
        metric(&snapshot, "code.current"),
        r#""code.current":{"value":0,"unit":"count","state":"ready","evidence":null}"#
    );
    assert_eq!(
        metric(&snapshot, "code.stale"),
        r#""code.stale":{"value":1,"unit":"count","state":"ready","evidence":null}"#
    );
    assert_eq!(
        metric(&snapshot, "code.new"),
        r#""code.new":{"value":0,"unit":"count","state":"ready","evidence":null}"#
    );
    assert_eq!(
        metric(&snapshot, "code.orphan"),
        r#""code.orphan":{"value":0,"unit":"count","state":"ready","evidence":null}"#
    );
    assert!(
        snapshot
            .contains(r#""code.coverage_percent":{"value":0.0,"unit":"percent","state":"ready""#)
            || snapshot
                .contains(r#""code.coverage_percent":{"value":0,"unit":"percent","state":"ready""#),
        "{snapshot}"
    );

    assert_eq!(
        metric(&snapshot, "work.goals"),
        r#""work.goals":{"value":1,"unit":"count","state":"ready","evidence":null}"#
    );
    assert_eq!(
        metric(&snapshot, "work.active_goals"),
        r#""work.active_goals":{"value":1,"unit":"count","state":"ready","evidence":null}"#
    );
    assert_eq!(
        metric(&snapshot, "work.specs"),
        r#""work.specs":{"value":1,"unit":"count","state":"ready","evidence":null}"#
    );
    assert_eq!(
        metric(&snapshot, "work.approved_specs"),
        r#""work.approved_specs":{"value":1,"unit":"count","state":"ready","evidence":null}"#
    );
    assert_eq!(
        metric(&snapshot, "work.plans"),
        r#""work.plans":{"value":1,"unit":"count","state":"ready","evidence":null}"#
    );
    assert_eq!(
        metric(&snapshot, "work.active_plans"),
        r#""work.active_plans":{"value":1,"unit":"count","state":"ready","evidence":null}"#
    );

    assert_eq!(
        metric(&snapshot, "checkpoints.total"),
        r#""checkpoints.total":{"value":1,"unit":"count","state":"ready","evidence":null}"#
    );
    assert_eq!(
        metric(&snapshot, "checkpoints.actionable"),
        r#""checkpoints.actionable":{"value":1,"unit":"count","state":"ready","evidence":null}"#
    );
    assert_eq!(
        metric(&snapshot, "checkpoints.latest_at"),
        r#""checkpoints.latest_at":{"value":"2026-08-10T09:00:00+02:00","unit":null,"state":"ready","evidence":null}"#
    );
    assert_eq!(
        metric(&snapshot, "guidance.files"),
        r#""guidance.files":{"value":1,"unit":"count","state":"ready","evidence":null}"#
    );

    // Signals: stale code page -> watch; no lint marker -> watch; zero broken
    // links -> healthy; the checkpoint is 9+ days old -> watch.
    assert!(
        snapshot.contains(r#""id":"code-graph-drift","state":"watch""#),
        "{snapshot}"
    );
    assert!(
        snapshot.contains(r#""id":"memory-lint","state":"watch""#),
        "{snapshot}"
    );
    assert!(
        snapshot.contains(r#""id":"wikilink-health","state":"healthy""#),
        "{snapshot}"
    );
    assert!(
        snapshot.contains(r#""id":"checkpoint-state","state":"watch""#),
        "{snapshot}"
    );
    // No qualifying spec/plan lacks goal provenance -> neutral, omitted.
    assert!(
        !snapshot.contains(r#""id":"goal-traceability""#),
        "{snapshot}"
    );
    // No malformed fields anywhere in this fixture -> neutral, omitted.
    assert!(!snapshot.contains(r#""id":"artifact-parse""#), "{snapshot}");
    // Never emitted from count alone.
    assert!(!snapshot.contains(r#""id":"concept-layer""#), "{snapshot}");

    assert!(
        snapshot.contains(r#""posture":"needs-review""#),
        "{snapshot}"
    );

    // Hints: the (real, wall-clock) checkpoint age triggers resume_stale, the
    // stale code page triggers code_ingest_pending, and the in-progress plan
    // triggers plan_in_progress -- all reused verbatim from `loam state`'s
    // existing hint pipeline.
    assert!(snapshot.contains(r#""kind":"resume_stale""#), "{snapshot}");
    assert!(
        snapshot.contains(r#""kind":"code_ingest_pending""#),
        "{snapshot}"
    );
    assert!(
        snapshot.contains(r#""kind":"plan_in_progress""#),
        "{snapshot}"
    );

    // Events: created_at/updated_at on the goal and spec, created_at on the
    // plan (it has no updated_at front matter field), and the checkpoint's
    // Captured field. No git repo in the copied fixture, so no commits.
    for needle in [
        r#""kind":"created","title":"Improve Greeting created","artifact_id":"goals/improve-greeting.md""#,
        r#""kind":"updated","title":"Improve Greeting updated","artifact_id":"goals/improve-greeting.md""#,
        r#""kind":"created","title":"Greeting created","artifact_id":"specs/greeting-spec.md""#,
        r#""kind":"updated","title":"Greeting updated","artifact_id":"specs/greeting-spec.md""#,
        r#""kind":"created","title":"Greeting Plan created","artifact_id":"plans/greeting-plan.md""#,
        r#""kind":"checkpoint-captured","title":"Checkpoint captured: greeting feature","artifact_id":"wiki/checkpoints/checkpoint-2026-08-10-0900.md""#,
    ] {
        assert!(snapshot.contains(needle), "missing {needle} in {snapshot}");
    }
    assert!(
        !snapshot.contains(r#""artifact_id":"plans/greeting-plan.md","strength":"strong","evidence":{"path":"plans/greeting-plan.md","line":null,"section":null,"field":"updated_at""#),
        "plan has no updated_at front matter field, so no updated event should exist: {snapshot}"
    );
    assert!(!snapshot.contains(r#""kind":"commit""#), "{snapshot}");

    // Probes: one entry per attempted optional check.
    for id in ["codegraph", "git", "qmd", "wikilink-scan"] {
        let needle = format!("\"id\":\"{id}\"");
        assert!(
            snapshot.contains(&needle),
            "missing probe {id} in {snapshot}"
        );
    }

    assert_schema_valid(&snapshot);
}

#[test]
fn view_metrics_sparse_fixture_emits_empty_metrics_signals_and_a_scaffold_hint() {
    let workspace = copy_fixture("sparse");
    let snapshot = view_snapshot(&workspace);
    fs::remove_dir_all(&workspace).ok();

    assert!(
        snapshot.contains(r#""status":"not-configured""#),
        "{snapshot}"
    );
    assert!(
        snapshot.contains(r#""posture":"not-configured""#),
        "{snapshot}"
    );
    assert!(
        snapshot.contains(r#""relationships":[],"events":[],"metrics":{},"signals":[]"#),
        "{snapshot}"
    );
    assert!(snapshot.contains(r#""probes":[]"#), "{snapshot}");
    assert!(
        snapshot.contains(r#""kind":"memory_missing""#),
        "{snapshot}"
    );
    assert!(
        snapshot.contains(r#""command":"/loam::scaffolding-wiki"#),
        "{snapshot}"
    );

    assert_schema_valid(&snapshot);
}

#[test]
fn view_metrics_code_drift_fixture_computes_new_stale_and_orphan_separately() {
    let workspace = copy_fixture("code-drift");
    let snapshot = view_snapshot(&workspace);
    fs::remove_dir_all(&workspace).ok();

    assert_eq!(
        metric(&snapshot, "code.candidates"),
        r#""code.candidates":{"value":2,"unit":"count","state":"ready","evidence":null}"#
    );
    assert_eq!(
        metric(&snapshot, "code.source_backed_pages"),
        r#""code.source_backed_pages":{"value":2,"unit":"count","state":"ready","evidence":null}"#
    );
    assert_eq!(
        metric(&snapshot, "code.current"),
        r#""code.current":{"value":0,"unit":"count","state":"ready","evidence":null}"#
    );
    assert_eq!(
        metric(&snapshot, "code.stale"),
        r#""code.stale":{"value":1,"unit":"count","state":"ready","evidence":null}"#
    );
    assert_eq!(
        metric(&snapshot, "code.new"),
        r#""code.new":{"value":1,"unit":"count","state":"ready","evidence":null}"#
    );
    assert_eq!(
        metric(&snapshot, "code.orphan"),
        r#""code.orphan":{"value":1,"unit":"count","state":"ready","evidence":null}"#
    );
    assert!(
        snapshot.contains(r#""id":"code-graph-drift","state":"watch""#),
        "{snapshot}"
    );

    assert_schema_valid(&snapshot);
}

#[test]
fn view_metrics_chronicle_fixture_covers_events_reviews_supersession_and_the_noncanonical_amendment(
) {
    let workspace = copy_fixture("chronicle");
    let snapshot = view_snapshot(&workspace);
    fs::remove_dir_all(&workspace).ok();

    // wiki/index.md and wiki/concepts/pricing.md -- checkpoints are excluded.
    assert_eq!(
        metric(&snapshot, "wiki.knowledge_pages"),
        r#""wiki.knowledge_pages":{"value":2,"unit":"count","state":"ready","evidence":null}"#
    );
    assert_eq!(
        metric(&snapshot, "wiki.archived_pages"),
        r#""wiki.archived_pages":{"value":1,"unit":"count","state":"ready","evidence":null}"#
    );
    assert_eq!(
        metric(&snapshot, "wiki.concepts"),
        r#""wiki.concepts":{"value":1,"unit":"count","state":"ready","evidence":null}"#
    );
    assert_eq!(
        metric(&snapshot, "wiki.last_lint_at"),
        r#""wiki.last_lint_at":{"value":"2026-08-15","unit":null,"state":"ready","evidence":null}"#
    );

    // No wiki/code/ or src/ in this fixture: zero candidates -> the ratio is
    // unavailable, never a fabricated 100%. The raw counts stay real zeros.
    assert_eq!(
        metric(&snapshot, "code.candidates"),
        r#""code.candidates":{"value":0,"unit":"count","state":"ready","evidence":null}"#
    );
    assert!(
        snapshot.contains(
            r#""code.coverage_percent":{"value":null,"unit":"percent","state":"unavailable""#
        ),
        "{snapshot}"
    );

    assert_eq!(
        metric(&snapshot, "work.goals"),
        r#""work.goals":{"value":1,"unit":"count","state":"ready","evidence":null}"#
    );
    assert_eq!(
        metric(&snapshot, "work.active_goals"),
        r#""work.active_goals":{"value":1,"unit":"count","state":"ready","evidence":null}"#
    );
    assert_eq!(
        metric(&snapshot, "work.specs"),
        r#""work.specs":{"value":1,"unit":"count","state":"ready","evidence":null}"#
    );
    assert_eq!(
        metric(&snapshot, "work.approved_specs"),
        r#""work.approved_specs":{"value":0,"unit":"count","state":"ready","evidence":null}"#
    );

    // The 2026-08-15 checkpoint supersedes the 2026-08-01 one, so only the
    // newer one counts as actionable; latest_at picks the newer one too.
    assert_eq!(
        metric(&snapshot, "checkpoints.total"),
        r#""checkpoints.total":{"value":2,"unit":"count","state":"ready","evidence":null}"#
    );
    assert_eq!(
        metric(&snapshot, "checkpoints.actionable"),
        r#""checkpoints.actionable":{"value":1,"unit":"count","state":"ready","evidence":null}"#
    );
    assert_eq!(
        metric(&snapshot, "checkpoints.latest_at"),
        r#""checkpoints.latest_at":{"value":"2026-08-15T10:00:00+02:00","unit":null,"state":"ready","evidence":null}"#
    );

    // The noncanonical `+0200` created_at normalizes to RFC 3339 with the
    // colon and is NOT a parse error -- chronology is preserved.
    assert!(
        snapshot.contains(
            r#""path":"goals/review-goal.md","kind":"goal","title":"Review Goal","lifecycle_status":"active","created_at":"2026-08-01T09:00:00+02:00""#
        ),
        "{snapshot}"
    );
    assert!(
        !snapshot.contains("invalid created_at"),
        "the noncanonical offset must not be reported as a parse error: {snapshot}"
    );
    // But the unparseable "### not-a-date" review heading IS a real parse
    // diagnostic on the goal artifact.
    assert!(
        snapshot.contains("invalid goal review date: not-a-date"),
        "{snapshot}"
    );

    // Events: two log headings, two checkpoint captures, one valid goal
    // review (the invalid one is a diagnostic, not an event), and lifecycle
    // events for the goal/spec created_at/updated_at fields.
    for needle in [
        r#""kind":"log-entry","title":"build | Initial wiki scaffold""#,
        r#""kind":"log-entry","title":"lint-check | wiki marksman links""#,
        r#""kind":"checkpoint-captured","title":"Checkpoint captured: first pass","artifact_id":"wiki/checkpoints/checkpoint-2026-08-01-0900.md""#,
        r#""kind":"checkpoint-captured","title":"Checkpoint captured: second pass","artifact_id":"wiki/checkpoints/checkpoint-2026-08-15-1000.md""#,
        r#""kind":"goal-review","title":"Goal review: pass","artifact_id":"goals/review-goal.md""#,
    ] {
        assert!(snapshot.contains(needle), "missing {needle} in {snapshot}");
    }
    assert!(
        !snapshot.contains("not-a-date\",\"strength\""),
        "{snapshot}"
    );

    // Signals: healthy wikilink scan, a goal-less draft spec -> watch, an
    // actionable-but-stale checkpoint -> watch, a malformed review date ->
    // watch, and the noncanonical timestamp feeding memory-lint -> watch.
    assert!(
        snapshot.contains(r#""id":"wikilink-health","state":"healthy""#),
        "{snapshot}"
    );
    assert!(
        snapshot.contains(r#""id":"goal-traceability","state":"watch""#),
        "{snapshot}"
    );
    assert!(
        snapshot.contains(r#""id":"checkpoint-state","state":"watch""#),
        "{snapshot}"
    );
    assert!(
        snapshot.contains(r#""id":"artifact-parse","state":"watch""#),
        "{snapshot}"
    );
    assert!(
        snapshot.contains(r#""id":"memory-lint","state":"watch""#),
        "{snapshot}"
    );
    assert!(
        !snapshot.contains(r#""id":"code-graph-drift""#),
        "absent code graph should emit no health signal: {snapshot}"
    );

    assert!(
        snapshot.contains(r#""posture":"needs-review""#),
        "{snapshot}"
    );

    assert_schema_valid(&snapshot);
}

#[test]
fn view_metrics_up_to_100_git_commits_become_source_strength_events() {
    let workspace = copy_fixture("healthy");
    let git = |args: &[&str]| {
        let status = Command::new("git")
            .arg("-C")
            .arg(&workspace)
            .args(args)
            .env("GIT_AUTHOR_NAME", "Fixture")
            .env("GIT_AUTHOR_EMAIL", "fixture@example.com")
            .env("GIT_COMMITTER_NAME", "Fixture")
            .env("GIT_COMMITTER_EMAIL", "fixture@example.com")
            .status()
            .expect("git should run");
        assert!(status.success(), "git {args:?} should succeed");
    };
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "one real fixture commit"]);

    let snapshot = view_snapshot(&workspace);
    fs::remove_dir_all(&workspace).ok();

    assert!(snapshot.contains(r#""kind":"commit""#), "{snapshot}");
    assert!(
        snapshot.contains(r#""title":"one real fixture commit""#)
            || snapshot.contains(r#"one real fixture commit"#),
        "{snapshot}"
    );
    assert!(snapshot.contains(r#""strength":"source""#), "{snapshot}");

    assert_schema_valid(&snapshot);
}
