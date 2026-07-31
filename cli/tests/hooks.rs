use rusqlite::{params, Connection};
use std::collections::HashSet;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

fn temporary_root(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be after epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("loam-hooks-{label}-{nonce}"))
}

fn loam(args: &[&str]) -> Output {
    Command::new(std::env::var("CARGO_BIN_EXE_loam").expect("cargo should provide loam"))
        .args(args)
        .output()
        .expect("loam should run")
}

fn begin(root: &Path, harness: &str, hook: &str, workspace: &Path) -> Output {
    begin_with_session(root, harness, hook, workspace, None)
}

fn begin_with_session(
    root: &Path,
    harness: &str,
    hook: &str,
    workspace: &Path,
    session_id: Option<&str>,
) -> Output {
    let mut args = vec![
        "hooks",
        "begin",
        root.to_str().unwrap(),
        "--harness",
        harness,
        "--hook",
        hook,
        "--workspace",
        workspace.to_str().unwrap(),
        "--plugin-version",
        "0.9.5",
    ];
    if let Some(session_id) = session_id {
        args.extend(["--session-id", session_id]);
    }
    loam(&args)
}

fn begin_id(root: &Path, harness: &str, hook: &str, workspace: &Path) -> i64 {
    let output = begin(root, harness, hook, workspace);
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

fn begin_id_with_session(
    root: &Path,
    harness: &str,
    hook: &str,
    workspace: &Path,
    session_id: &str,
) -> i64 {
    let output = begin_with_session(root, harness, hook, workspace, Some(session_id));
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

fn finish(root: &Path, id: i64, status: &str, detail: Option<&str>) -> Output {
    let action = (status == "succeeded").then_some("skip");
    let reason = (status == "succeeded").then_some("nothing_to_do");
    finish_result(root, id, status, action, reason, detail)
}

fn finish_result(
    root: &Path,
    id: i64,
    status: &str,
    action: Option<&str>,
    reason: Option<&str>,
    detail: Option<&str>,
) -> Output {
    let id = id.to_string();
    let mut args = vec![
        "hooks",
        "finish",
        root.to_str().unwrap(),
        "--id",
        &id,
        "--status",
        status,
    ];
    if let Some(action) = action {
        args.extend(["--action", action]);
    }
    if let Some(reason) = reason {
        args.extend(["--reason", reason]);
    }
    if let Some(detail) = detail {
        args.extend(["--detail", detail]);
    }
    loam(&args)
}

fn worker_start(root: &Path, id: i64, session_id: Option<&str>) -> Output {
    worker_start_with_origin(root, id, None, session_id)
}

fn worker_start_with_origin(
    root: &Path,
    id: i64,
    origin: Option<&str>,
    session_id: Option<&str>,
) -> Output {
    let id = id.to_string();
    let mut args = vec!["hooks", "worker-start", root.to_str().unwrap(), "--id", &id];
    if let Some(origin) = origin {
        args.extend(["--origin", origin]);
    }
    if let Some(session_id) = session_id {
        args.extend(["--session-id", session_id]);
    }
    loam(&args)
}

fn worker_finish(root: &Path, id: i64, status: &str, reason: &str, detail: Option<&str>) -> Output {
    worker_finish_with_origin(root, id, status, reason, None, None, detail)
}

fn worker_finish_with_origin(
    root: &Path,
    id: i64,
    status: &str,
    reason: &str,
    origin: Option<&str>,
    session_id: Option<&str>,
    detail: Option<&str>,
) -> Output {
    let id = id.to_string();
    let mut args = vec![
        "hooks",
        "worker-finish",
        root.to_str().unwrap(),
        "--id",
        &id,
        "--status",
        status,
        "--reason",
        reason,
    ];
    if let Some(origin) = origin {
        args.extend(["--origin", origin]);
    }
    if let Some(session_id) = session_id {
        args.extend(["--session-id", session_id]);
    }
    if let Some(detail) = detail {
        args.extend(["--detail", detail]);
    }
    loam(&args)
}

fn record_event(
    root: &Path,
    id: i64,
    event: &str,
    phase: Option<&str>,
    outcome: &str,
    fields: &[&str],
) -> Output {
    let id = id.to_string();
    let mut args = vec![
        "hooks",
        "event",
        root.to_str().unwrap(),
        "--id",
        &id,
        "--event",
        event,
    ];
    if let Some(phase) = phase {
        args.extend(["--phase", phase]);
    }
    args.extend(["--outcome", outcome]);
    args.extend(fields);
    loam(&args)
}

fn create_v1_store(root: &Path) {
    fs::create_dir_all(root).unwrap();
    let connection = Connection::open(root.join("loam.sqlite3")).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE hook_run (
                id INTEGER PRIMARY KEY,
                started_at_ms INTEGER NOT NULL,
                finished_at_ms INTEGER,
                harness TEXT NOT NULL,
                hook TEXT NOT NULL,
                status TEXT NOT NULL CHECK (status IN ('started', 'succeeded', 'failed')),
                detail TEXT,
                session_id TEXT,
                workspace TEXT NOT NULL,
                plugin_version TEXT NOT NULL,
                runtime_version TEXT NOT NULL
            );
            INSERT INTO hook_run VALUES (7, 1000, 1250, 'codex', 'stop', 'succeeded', NULL, 'old-session', '/', '0.9.5', '0.9.2');
            PRAGMA user_version = 1;",
        )
        .unwrap();
}

fn create_complete_v1_store(root: &Path) {
    fs::create_dir_all(root).unwrap();
    let connection = Connection::open(root.join("loam.sqlite3")).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE hook_run (
                id INTEGER PRIMARY KEY,
                started_at_ms INTEGER NOT NULL,
                finished_at_ms INTEGER,
                harness TEXT NOT NULL,
                hook TEXT NOT NULL,
                status TEXT NOT NULL CHECK (status IN ('started', 'succeeded', 'failed')),
                detail TEXT,
                action TEXT,
                reason TEXT,
                session_id TEXT,
                workspace TEXT NOT NULL,
                plugin_version TEXT NOT NULL,
                runtime_version TEXT NOT NULL,
                worker_status TEXT CHECK (worker_status IN ('requested', 'running', 'succeeded', 'skipped', 'failed')),
                worker_started_at_ms INTEGER,
                worker_finished_at_ms INTEGER,
                worker_reason TEXT,
                worker_detail TEXT,
                worker_session_id TEXT
            );
            INSERT INTO hook_run VALUES
                (1, 100, NULL, 'claude', 'stop', 'started', NULL, NULL, NULL, NULL, '/', '0.9.5', '0.9.5', NULL, NULL, NULL, NULL, NULL, NULL),
                (2, 200, 250, 'codex', 'stop', 'succeeded', NULL, 'spawn_worker', NULL, 'parent', '/', '0.9.5', '0.9.5', 'succeeded', 210, 240, 'ok', NULL, 'child'),
                (3, 300, 350, 'opencode', 'session_idle', 'failed', 'boom', NULL, NULL, NULL, '/', '0.9.5', '0.9.5', NULL, NULL, NULL, NULL, NULL, NULL);
            PRAGMA user_version = 1;",
        )
        .unwrap();
}

#[test]
fn listing_a_missing_store_is_empty_and_creates_nothing() {
    let root = temporary_root("missing");
    let output = loam(&["hooks", "list", root.to_str().unwrap()]);

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(output.stdout, b"");
    assert!(!root.exists());
}

#[test]
fn begin_lazily_creates_one_private_started_run() {
    let root = temporary_root("begin");
    let workspace = temporary_root("workspace");
    let id = begin_id(&root, "claude", "stop", &workspace);
    let listed = loam(&["hooks", "list", root.to_str().unwrap()]);

    assert_eq!(id, 1);
    assert_eq!(listed.status.code(), Some(0));
    let line = String::from_utf8(listed.stdout).unwrap();
    assert_eq!(line.lines().count(), 1);
    assert!(line.contains("\"status\":\"started\""), "stdout: {line}");
    assert!(line.contains("\"finished_at\":null"), "stdout: {line}");
    assert!(line.contains("\"detail\":null"), "stdout: {line}");
    assert!(line.contains("\"session_id\":null"), "stdout: {line}");
    assert!(line.contains("\"harness\":\"claude\""), "stdout: {line}");
    assert!(line.contains("\"hook\":\"stop\""), "stdout: {line}");
    assert!(line.contains("\"schema\":2"), "stdout: {line}");
    assert!(line.contains("\"worker_origin\":null"), "stdout: {line}");
    assert!(line.contains("\"events\":[]"), "stdout: {line}");
    assert!(line.contains(concat!(
        "\"runtime_version\":\"",
        env!("CARGO_PKG_VERSION"),
        "\""
    )));

    let database = root.join("loam.sqlite3");
    let connection = Connection::open(&database).unwrap();
    assert_eq!(
        connection
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        2
    );
    assert_eq!(
        connection
            .query_row("PRAGMA journal_mode", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "delete"
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT group_concat(name, ',') FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
        "hook_run,hook_event"
    );
    drop(connection);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn begin_preserves_and_list_filters_by_session_id() {
    let root = temporary_root("session");
    let workspace = temporary_root("workspace");
    begin_id_with_session(&root, "claude", "stop", &workspace, "session α \"one\"");
    begin_id_with_session(&root, "codex", "stop", &workspace, "session-two");

    let listed = loam(&[
        "hooks",
        "list",
        root.to_str().unwrap(),
        "--session-id",
        "session α \"one\"",
    ]);
    let line = String::from_utf8(listed.stdout).unwrap();
    assert_eq!(listed.status.code(), Some(0));
    assert_eq!(line.lines().count(), 1);
    assert!(
        line.contains("\"session_id\":\"session α \\\"one\\\"\""),
        "stdout: {line}"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn finish_updates_only_one_started_run() {
    let root = temporary_root("finish");
    let workspace = temporary_root("workspace");
    let id = begin_id(&root, "codex", "stop", &workspace);

    let finished = finish(&root, id, "failed", Some("hook exited with code 1 \"bad\""));
    let repeated = finish(&root, id, "succeeded", None);
    let missing = finish(&root, id + 1, "succeeded", None);
    let listed = loam(&["hooks", "list", root.to_str().unwrap()]);

    assert_eq!(finished.status.code(), Some(0));
    assert_eq!(finished.stdout, b"");
    assert_eq!(repeated.status.code(), Some(1));
    assert_eq!(missing.status.code(), Some(1));
    let line = String::from_utf8(listed.stdout).unwrap();
    assert!(line.contains("\"status\":\"failed\""), "stdout: {line}");
    assert!(line.contains("\"finished_at\":\""), "stdout: {line}");
    assert!(
        line.contains("\"detail\":\"hook exited with code 1 \\\"bad\\\"\""),
        "stdout: {line}"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn finish_validates_status_and_bounded_detail() {
    let root = temporary_root("finish-invalid");
    let workspace = temporary_root("workspace");
    let id = begin_id(&root, "codex", "stop", &workspace);

    assert_eq!(finish(&root, id, "started", None).status.code(), Some(1));
    assert_eq!(finish(&root, id, "failed", None).status.code(), Some(1));
    assert_eq!(
        finish_result(&root, id, "succeeded", None, None, None)
            .status
            .code(),
        Some(1)
    );
    assert_eq!(
        finish(&root, id, "failed", Some(&"x".repeat(1025)))
            .status
            .code(),
        Some(1)
    );
    assert_eq!(
        finish_result(
            &root,
            id,
            "succeeded",
            Some("skip"),
            Some("nothing_to_do"),
            Some("no actionable files")
        )
        .status
        .code(),
        Some(0)
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn list_filters_newest_first_and_honors_limit() {
    let root = temporary_root("filters");
    let workspace = temporary_root("workspace");
    let claude = begin_id(&root, "claude", "stop", &workspace);
    let codex = begin_id_with_session(&root, "codex", "stop", &workspace, "codex-session");
    let opencode = begin_id(&root, "opencode", "session_idle", &workspace);
    assert_eq!(
        finish(&root, claude, "succeeded", None).status.code(),
        Some(0)
    );
    assert_eq!(
        finish(&root, codex, "failed", Some("failed")).status.code(),
        Some(0)
    );
    assert_eq!(
        finish(&root, opencode, "succeeded", None).status.code(),
        Some(0)
    );

    let newest = loam(&["hooks", "list", root.to_str().unwrap(), "--limit", "1"]);
    let failed = loam(&[
        "hooks",
        "list",
        root.to_str().unwrap(),
        "--harness",
        "codex",
        "--hook",
        "stop",
        "--status",
        "failed",
        "--session-id",
        "codex-session",
    ]);
    let newest = String::from_utf8(newest.stdout).unwrap();
    let failed = String::from_utf8(failed.stdout).unwrap();
    assert_eq!(newest.lines().count(), 1);
    assert!(newest.contains("\"harness\":\"opencode\""));
    assert_eq!(failed.lines().count(), 1);
    assert!(failed.contains("\"harness\":\"codex\""));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn invalid_inputs_do_not_create_storage() {
    let root = temporary_root("invalid");
    let workspace = temporary_root("workspace");
    assert_eq!(
        begin(&root, "Claude Code", "stop", &workspace)
            .status
            .code(),
        Some(1)
    );
    assert_eq!(
        begin(&root, "claude", "stop", Path::new("relative"))
            .status
            .code(),
        Some(1)
    );
    assert_eq!(
        begin_with_session(&root, "claude", "stop", &workspace, Some("bad\nsession"))
            .status
            .code(),
        Some(1)
    );
    assert_eq!(
        begin_with_session(&root, "claude", "stop", &workspace, Some(&"x".repeat(257)))
            .status
            .code(),
        Some(1)
    );
    assert_eq!(loam(&["hooks", "list", "relative"]).status.code(), Some(1));
    assert!(!root.exists());
}

#[test]
fn list_infers_only_a_valid_installed_runtime_root() {
    let source =
        PathBuf::from(std::env::var("CARGO_BIN_EXE_loam").expect("cargo should provide loam"));
    let root = source
        .parent()
        .unwrap()
        .join(temporary_root("inferred").file_name().unwrap());
    let workspace = temporary_root("workspace");
    begin_id(&root, "codex", "stop", &workspace);

    let target_dir = root
        .join("bin")
        .join(env!("CARGO_PKG_VERSION"))
        .join("x86_64-unknown-linux-musl");
    fs::create_dir_all(&target_dir).unwrap();
    let installed = target_dir.join(if cfg!(windows) { "loam.exe" } else { "loam" });
    fs::hard_link(source, &installed).unwrap();

    let inferred = Command::new(installed)
        .args(["hooks", "list"])
        .output()
        .expect("installed loam should run");
    assert_eq!(
        inferred.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&inferred.stderr)
    );
    assert_eq!(
        String::from_utf8(inferred.stdout).unwrap().lines().count(),
        1
    );
    assert_eq!(loam(&["hooks", "list"]).status.code(), Some(1));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn an_unknown_schema_is_rejected_without_mutation() {
    let root = temporary_root("schema");
    let workspace = temporary_root("workspace");
    fs::create_dir_all(&root).unwrap();
    let database = root.join("loam.sqlite3");
    let connection = Connection::open(&database).unwrap();
    connection
        .execute_batch(
            "PRAGMA user_version = 3; CREATE TABLE sentinel (value TEXT); INSERT INTO sentinel VALUES ('kept');",
        )
        .unwrap();
    drop(connection);

    assert_eq!(
        begin(&root, "claude", "stop", &workspace).status.code(),
        Some(1)
    );
    let connection = Connection::open(database).unwrap();
    assert_eq!(
        connection
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        3
    );
    assert_eq!(
        connection
            .query_row("SELECT value FROM sentinel", [], |row| row
                .get::<_, String>(0))
            .unwrap(),
        "kept"
    );
    drop(connection);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn earlier_v1_listing_is_read_only_and_exposes_null_result_fields() {
    let root = temporary_root("schema-v1-list");
    create_v1_store(&root);

    let listed = loam(&["hooks", "list", root.to_str().unwrap()]);
    assert_eq!(listed.status.code(), Some(0));
    let line = String::from_utf8(listed.stdout).unwrap();
    assert!(line.contains("\"schema\":2"), "stdout: {line}");
    assert!(line.contains("\"action\":null"), "stdout: {line}");
    assert!(line.contains("\"reason\":null"), "stdout: {line}");
    assert!(line.contains("\"duration_ms\":250"), "stdout: {line}");
    assert!(line.contains("\"worker_status\":null"), "stdout: {line}");
    assert!(
        line.contains("\"worker_duration_ms\":null"),
        "stdout: {line}"
    );
    assert!(line.contains("\"worker_origin\":null"), "stdout: {line}");
    assert!(line.contains("\"events\":[]"), "stdout: {line}");

    let connection = Connection::open(root.join("loam.sqlite3")).unwrap();
    assert_eq!(
        connection
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );
    drop(connection);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn the_next_write_migrates_sparse_v1_and_preserves_old_rows() {
    let root = temporary_root("schema-v1-write");
    let workspace = temporary_root("workspace");
    create_v1_store(&root);

    assert_eq!(
        begin(&root, "claude", "stop", &workspace).status.code(),
        Some(0)
    );
    let connection = Connection::open(root.join("loam.sqlite3")).unwrap();
    assert_eq!(
        connection
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        2
    );
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM hook_run", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        2
    );
    assert_eq!(
        connection
            .query_row("SELECT action FROM hook_run WHERE id = 7", [], |row| {
                row.get::<_, Option<String>>(0)
            })
            .unwrap(),
        None
    );
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM hook_event", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    drop(connection);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn immediate_results_and_worker_lifecycle_are_guarded_and_listed() {
    let root = temporary_root("worker-lifecycle");
    let workspace = temporary_root("workspace");
    let id = begin_id(&root, "opencode", "session_idle", &workspace);

    assert_eq!(worker_start(&root, id, None).status.code(), Some(1));
    assert_eq!(
        finish_result(&root, id, "succeeded", Some("spawn_worker"), None, None)
            .status
            .code(),
        Some(0)
    );
    assert_eq!(
        worker_start(&root, id, Some("worker-session"))
            .status
            .code(),
        Some(0)
    );
    assert_eq!(worker_start(&root, id, None).status.code(), Some(1));
    assert_eq!(
        worker_finish(&root, id, "succeeded", "ok", None)
            .status
            .code(),
        Some(0)
    );
    assert_eq!(
        worker_finish(&root, id, "failed", "unavailable", Some("late"))
            .status
            .code(),
        Some(1)
    );

    let listed = loam(&["hooks", "list", root.to_str().unwrap()]);
    let line = String::from_utf8(listed.stdout).unwrap();
    assert!(
        line.contains("\"action\":\"spawn_worker\""),
        "stdout: {line}"
    );
    assert!(line.contains("\"reason\":null"), "stdout: {line}");
    assert!(
        line.contains("\"worker_status\":\"succeeded\""),
        "stdout: {line}"
    );
    assert!(
        line.contains("\"worker_origin\":\"direct\""),
        "stdout: {line}"
    );
    assert!(line.contains("\"worker_reason\":\"ok\""), "stdout: {line}");
    assert!(
        line.contains("\"worker_session_id\":\"worker-session\""),
        "stdout: {line}"
    );
    assert!(line.contains("\"duration_ms\":"), "stdout: {line}");
    assert!(line.contains("\"worker_duration_ms\":"), "stdout: {line}");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn worker_finish_accepts_requested_and_validates_result_mapping() {
    let root = temporary_root("worker-requested");
    let workspace = temporary_root("workspace");
    let id = begin_id(&root, "claude", "stop", &workspace);
    assert_eq!(
        finish_result(&root, id, "succeeded", Some("spawn_worker"), None, None)
            .status
            .code(),
        Some(0)
    );
    assert_eq!(
        worker_finish(&root, id, "succeeded", "busy", None)
            .status
            .code(),
        Some(1)
    );
    assert_eq!(
        worker_finish(&root, id, "skipped", "busy", Some("lease held"))
            .status
            .code(),
        Some(0)
    );
    let listed =
        String::from_utf8(loam(&["hooks", "list", root.to_str().unwrap()]).stdout).unwrap();
    assert!(listed.contains("\"worker_started_at\":null"));
    assert!(listed.contains("\"worker_status\":\"skipped\""));
    assert!(listed.contains("\"worker_reason\":\"busy\""));
    assert!(listed.contains("\"worker_detail\":\"lease held\""));
    assert!(listed.contains("\"worker_duration_ms\":null"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn continued_and_delegated_worker_lanes_require_causal_proof() {
    let root = temporary_root("delegated");
    let workspace = temporary_root("workspace");
    let external = begin_id(&root, "codex", "stop", &workspace);

    assert_eq!(
        finish_result(
            &root,
            external,
            "succeeded",
            Some("request_worker"),
            None,
            None,
        )
        .status
        .code(),
        Some(1)
    );
    assert_eq!(
        finish_result(
            &root,
            external,
            "continued",
            Some("spawn_worker"),
            None,
            None,
        )
        .status
        .code(),
        Some(1)
    );
    assert_eq!(
        finish_result(
            &root,
            external,
            "continued",
            Some("request_worker"),
            None,
            None,
        )
        .status
        .code(),
        Some(0)
    );
    assert_eq!(worker_start(&root, external, None).status.code(), Some(1));
    assert_eq!(
        worker_start_with_origin(&root, external, Some("external"), Some("agent-1"))
            .status
            .code(),
        Some(1)
    );
    assert_eq!(
        record_event(
            &root,
            external,
            "codex_native",
            Some("continuation"),
            "returned",
            &["--visibility", "native"],
        )
        .status
        .code(),
        Some(0)
    );
    assert_eq!(
        record_event(
            &root,
            external,
            "subagent",
            Some("start"),
            "observed",
            &["--agent-type", "loam_ingestor", "--session-id", "agent-1"],
        )
        .status
        .code(),
        Some(0)
    );
    assert_eq!(
        worker_start_with_origin(&root, external, Some("fallback"), None)
            .status
            .code(),
        Some(1)
    );
    assert_eq!(
        record_event(
            &root,
            external,
            "codex_native",
            Some("fallback"),
            "taken",
            &["--visibility", "native"],
        )
        .status
        .code(),
        Some(1)
    );
    assert_eq!(
        worker_start_with_origin(&root, external, Some("external"), Some("agent-1"))
            .status
            .code(),
        Some(0)
    );
    let delegated_digest = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    assert_eq!(
        record_event(
            &root,
            external,
            "ingest_preparation",
            None,
            "admitted",
            &[
                "--launch-mode",
                "codex_native",
                "--lease-id",
                "lease-external",
                "--actionable-digest",
                delegated_digest,
                "--actionable-count",
                "1",
                "--deadline-ms",
                "1785492000000",
            ],
        )
        .status
        .code(),
        Some(0)
    );
    assert_eq!(
        record_event(
            &root,
            external,
            "ingest_finalization",
            None,
            "ok",
            &[
                "--lease-id",
                "lease-external",
                "--pre-digest",
                delegated_digest,
                "--post-digest",
                delegated_digest,
                "--actionable-count",
                "1",
                "--failure-count",
                "0",
            ],
        )
        .status
        .code(),
        Some(0)
    );
    assert_eq!(
        worker_finish_with_origin(
            &root,
            external,
            "succeeded",
            "ok",
            Some("external"),
            Some("agent-1"),
            None,
        )
        .status
        .code(),
        Some(0)
    );

    let fallback = begin_id(&root, "codex", "stop", &workspace);
    assert_eq!(
        finish_result(
            &root,
            fallback,
            "continued",
            Some("request_worker"),
            None,
            None,
        )
        .status
        .code(),
        Some(0)
    );
    assert_eq!(
        worker_finish_with_origin(
            &root,
            fallback,
            "succeeded",
            "ok",
            Some("fallback"),
            None,
            None,
        )
        .status
        .code(),
        Some(1)
    );
    assert_eq!(
        record_event(
            &root,
            fallback,
            "codex_native",
            Some("fallback"),
            "taken",
            &["--visibility", "native"],
        )
        .status
        .code(),
        Some(0)
    );
    assert_eq!(
        record_event(
            &root,
            fallback,
            "codex_native",
            Some("fallback"),
            "taken",
            &["--visibility", "native"],
        )
        .status
        .code(),
        Some(1)
    );
    assert_eq!(
        record_event(
            &root,
            fallback,
            "subagent",
            Some("start"),
            "observed",
            &[
                "--agent-type",
                "loam_ingestor",
                "--session-id",
                "late-agent",
            ],
        )
        .status
        .code(),
        Some(1)
    );
    assert_eq!(
        worker_finish_with_origin(
            &root,
            fallback,
            "succeeded",
            "ok",
            Some("fallback"),
            None,
            None,
        )
        .status
        .code(),
        Some(0)
    );

    let listed =
        String::from_utf8(loam(&["hooks", "list", root.to_str().unwrap()]).stdout).unwrap();
    assert!(listed.contains("\"status\":\"continued\""));
    assert!(listed.contains("\"action\":\"request_worker\""));
    assert!(listed.contains("\"worker_origin\":\"external\""));
    assert!(listed.contains("\"worker_origin\":\"fallback\""));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn typed_visibility_events_enforce_shape_order_and_bounds() {
    let root = temporary_root("visibility-events");
    let workspace = temporary_root("workspace");
    let id = begin_id(&root, "opencode", "session_idle", &workspace);
    assert_eq!(
        finish_result(&root, id, "succeeded", Some("spawn_worker"), None, None)
            .status
            .code(),
        Some(0)
    );

    let visibility = ["--visibility", "toast", "--launch-mode", "opencode"];
    assert_eq!(
        record_event(
            &root,
            id + 100,
            "ingest_visibility",
            Some("launch"),
            "started",
            &visibility,
        )
        .status
        .code(),
        Some(1)
    );
    assert_eq!(
        record_event(
            &root,
            id,
            "visibility_delivery",
            Some("launch"),
            "ok",
            &visibility,
        )
        .status
        .code(),
        Some(1)
    );
    let oversized_detail = "x".repeat(1025);
    assert_eq!(
        record_event(
            &root,
            id,
            "visibility_delivery",
            Some("launch"),
            "failed",
            &[
                "--visibility",
                "toast",
                "--launch-mode",
                "opencode",
                "--detail",
                oversized_detail.as_str(),
            ],
        )
        .status
        .code(),
        Some(1)
    );
    assert_eq!(
        record_event(
            &root,
            id,
            "ingest_visibility",
            Some("terminal"),
            "ok",
            &visibility,
        )
        .status
        .code(),
        Some(1)
    );
    assert_eq!(
        record_event(
            &root,
            id,
            "ingest_visibility",
            Some("launch"),
            "started",
            &visibility,
        )
        .status
        .code(),
        Some(0)
    );
    assert_eq!(
        record_event(
            &root,
            id,
            "ingest_visibility",
            Some("launch"),
            "started",
            &visibility,
        )
        .status
        .code(),
        Some(1)
    );
    assert_eq!(
        record_event(
            &root,
            id,
            "visibility_delivery",
            Some("terminal"),
            "aborted",
            &[
                "--visibility",
                "toast",
                "--launch-mode",
                "opencode",
                "--detail",
                "250 ms deadline",
            ],
        )
        .status
        .code(),
        Some(1)
    );
    assert_eq!(
        record_event(
            &root,
            id,
            "ingest_visibility",
            Some("terminal"),
            "partial",
            &visibility,
        )
        .status
        .code(),
        Some(0)
    );
    assert_eq!(
        record_event(
            &root,
            id,
            "visibility_delivery",
            Some("terminal"),
            "aborted",
            &[
                "--visibility",
                "toast",
                "--launch-mode",
                "opencode",
                "--detail",
                "250 ms deadline",
            ],
        )
        .status
        .code(),
        Some(0)
    );
    assert_eq!(
        record_event(
            &root,
            id,
            "visibility_delivery",
            Some("launch"),
            "emitted",
            &[
                "--visibility",
                "toast",
                "--launch-mode",
                "opencode",
                "--detail",
                "not admitted",
            ],
        )
        .status
        .code(),
        Some(1)
    );
    assert_eq!(
        record_event(
            &root,
            id,
            "ingest_visibility",
            Some("launch"),
            "started",
            &["--visibility", "silent", "--launch-mode", "opencode",],
        )
        .status
        .code(),
        Some(1)
    );

    let line = String::from_utf8(loam(&["hooks", "list", root.to_str().unwrap()]).stdout).unwrap();
    assert!(line.contains("\"events\":[{"));
    assert!(line.contains("\"outcome\":\"partial\""));
    assert!(line.contains("\"outcome\":\"aborted\""));
    assert!(line.contains("\"detail\":\"250 ms deadline\""));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn preparation_and_finalization_admit_only_bounded_causal_telemetry() {
    const PRE: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const POST: &str = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";
    let root = temporary_root("worker-phase-events");
    let workspace = temporary_root("workspace");
    let id = begin_id(&root, "opencode", "session_idle", &workspace);
    assert_eq!(
        finish_result(&root, id, "succeeded", Some("spawn_worker"), None, None)
            .status
            .code(),
        Some(0)
    );

    let preparation = [
        "--launch-mode",
        "opencode",
        "--lease-id",
        "lease-1",
        "--actionable-digest",
        PRE,
        "--actionable-count",
        "3",
        "--deadline-ms",
        "1785492000000",
    ];
    assert_eq!(
        record_event(
            &root,
            id,
            "ingest_preparation",
            None,
            "admitted",
            &preparation,
        )
        .status
        .code(),
        Some(1)
    );
    assert_eq!(worker_start(&root, id, None).status.code(), Some(0));
    assert_eq!(
        record_event(
            &root,
            id,
            "ingest_preparation",
            None,
            "admitted",
            &[
                "--launch-mode",
                "opencode",
                "--lease-id",
                "lease-1",
                "--actionable-digest",
                "ABCDEF0123456789abcdef0123456789abcdef0123456789abcdef0123456789",
                "--actionable-count",
                "3",
                "--deadline-ms",
                "1785492000000",
            ],
        )
        .status
        .code(),
        Some(1)
    );
    assert_eq!(
        record_event(
            &root,
            id,
            "ingest_preparation",
            None,
            "admitted",
            &[
                "--launch-mode",
                "opencode",
                "--lease-id",
                "lease-1",
                "--actionable-digest",
                PRE,
                "--actionable-count",
                "-1",
                "--deadline-ms",
                "1785492000000",
            ],
        )
        .status
        .code(),
        Some(1)
    );
    assert_eq!(
        record_event(
            &root,
            id,
            "ingest_preparation",
            None,
            "admitted",
            &[
                "--launch-mode",
                "opencode",
                "--lease-id",
                "lease-1",
                "--actionable-digest",
                PRE,
                "--actionable-count",
                "3",
                "--deadline-ms",
                "1785492000000",
                "--workspace-files",
                "/secret/path",
            ],
        )
        .status
        .code(),
        Some(1)
    );
    assert_eq!(
        record_event(
            &root,
            id,
            "ingest_finalization",
            None,
            "partial",
            &[
                "--lease-id",
                "lease-1",
                "--pre-digest",
                PRE,
                "--post-digest",
                POST,
                "--actionable-count",
                "3",
                "--failure-count",
                "1",
                "--backoff-until-ms",
                "1785492060000",
            ],
        )
        .status
        .code(),
        Some(1)
    );
    assert_eq!(
        record_event(
            &root,
            id,
            "ingest_preparation",
            None,
            "admitted",
            &preparation,
        )
        .status
        .code(),
        Some(0)
    );
    assert_eq!(
        record_event(
            &root,
            id,
            "ingest_finalization",
            None,
            "partial",
            &[
                "--lease-id",
                "lease-1",
                "--pre-digest",
                POST,
                "--post-digest",
                POST,
                "--actionable-count",
                "3",
                "--failure-count",
                "1",
            ],
        )
        .status
        .code(),
        Some(1)
    );
    assert_eq!(
        record_event(
            &root,
            id,
            "ingest_finalization",
            None,
            "partial",
            &[
                "--lease-id",
                "lease-1",
                "--pre-digest",
                PRE,
                "--post-digest",
                POST,
                "--actionable-count",
                "3",
                "--failure-count",
                "1",
                "--backoff-until-ms",
                "1785492060000",
            ],
        )
        .status
        .code(),
        Some(0)
    );

    let skipped = begin_id(&root, "claude", "stop", &workspace);
    assert_eq!(
        finish_result(
            &root,
            skipped,
            "succeeded",
            Some("spawn_worker"),
            None,
            None,
        )
        .status
        .code(),
        Some(0)
    );
    assert_eq!(worker_start(&root, skipped, None).status.code(), Some(0));
    assert_eq!(
        record_event(
            &root,
            skipped,
            "ingest_preparation",
            None,
            "skipped",
            &["--reason", "too_soon"],
        )
        .status
        .code(),
        Some(0)
    );

    let listed =
        String::from_utf8(loam(&["hooks", "list", root.to_str().unwrap()]).stdout).unwrap();
    assert!(listed.contains("\"event\":\"ingest_preparation\""));
    assert!(listed.contains("\"event\":\"ingest_finalization\""));
    assert!(listed.contains("\"outcome\":\"partial\""));
    assert!(listed.contains(&format!("\"actionable_digest\":\"{PRE}\"")));
    assert!(listed.contains("\"actionable_count\":3"));
    assert!(listed.contains("\"failure_count\":1"));
    assert!(listed.contains("\"backoff_until_ms\":1785492060000"));
    assert!(!listed.contains("/secret/path"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn claude_events_reject_foreign_missing_and_out_of_order_facts() {
    let root = temporary_root("claude-events");
    let workspace = temporary_root("workspace");
    let spawned = begin_id(&root, "claude", "stop", &workspace);
    assert_eq!(
        finish_result(
            &root,
            spawned,
            "succeeded",
            Some("spawn_worker"),
            None,
            None,
        )
        .status
        .code(),
        Some(0)
    );

    assert_eq!(
        record_event(
            &root,
            spawned,
            "subagent",
            Some("stop"),
            "aborted",
            &["--agent-type", "loam:ingestor", "--session-id", "claude-1"],
        )
        .status
        .code(),
        Some(1)
    );
    let oversized_identity = "x".repeat(257);
    assert_eq!(
        record_event(
            &root,
            spawned,
            "subagent",
            Some("start"),
            "observed",
            &[
                "--agent-type",
                "loam:ingestor",
                "--session-id",
                oversized_identity.as_str(),
            ],
        )
        .status
        .code(),
        Some(1)
    );
    assert_eq!(
        record_event(
            &root,
            spawned,
            "subagent",
            Some("start"),
            "observed",
            &["--agent-type", "foreign", "--session-id", "claude-1"],
        )
        .status
        .code(),
        Some(1)
    );
    assert_eq!(
        record_event(
            &root,
            spawned,
            "subagent",
            Some("start"),
            "observed",
            &[
                "--agent-type",
                "loam:ingestor",
                "--session-id",
                "bad\nsession"
            ],
        )
        .status
        .code(),
        Some(1)
    );
    assert_eq!(
        record_event(
            &root,
            spawned,
            "claude_agent_profile",
            None,
            "selected",
            &[
                "--agent-type",
                "loam:ingestor",
                "--launch-mode",
                "claude_bg",
                "--manager-name",
                "loam-ingest-1234567890",
                "--manager-id",
                "manager-1",
            ],
        )
        .status
        .code(),
        Some(1)
    );
    assert_eq!(
        record_event(
            &root,
            spawned,
            "claude_agent_profile",
            None,
            "selected",
            &[
                "--agent-type",
                "loam:ingestor",
                "--launch-mode",
                "claude_bg",
                "--manager-name",
                "loam-ingest-1234567890",
                "--manager-id",
                "manager-1",
                "--lease-id",
                "lease-1",
            ],
        )
        .status
        .code(),
        Some(0)
    );
    assert_eq!(
        record_event(
            &root,
            spawned,
            "claude_agent_view",
            None,
            "fallback",
            &[
                "--reason",
                "unavailable",
                "--visibility",
                "silent",
                "--launch-mode",
                "claude_bg",
                "--fallback-launch-mode",
                "claude_print",
                "--lease-id",
                "lease-1",
                "--require-visible-worker",
                "false",
            ],
        )
        .status
        .code(),
        Some(1)
    );
    assert_eq!(
        record_event(
            &root,
            spawned,
            "claude_agent_view",
            None,
            "fallback",
            &[
                "--reason",
                "agent_view_unavailable",
                "--visibility",
                "silent",
                "--launch-mode",
                "claude_bg",
                "--fallback-launch-mode",
                "claude_print",
                "--lease-id",
                "lease-1",
                "--require-visible-worker",
                "false",
            ],
        )
        .status
        .code(),
        Some(0)
    );
    assert_eq!(
        record_event(
            &root,
            spawned,
            "subagent",
            Some("start"),
            "observed",
            &["--agent-type", "loam:ingestor", "--session-id", "claude-1"],
        )
        .status
        .code(),
        Some(0)
    );
    assert_eq!(
        record_event(
            &root,
            spawned,
            "subagent",
            Some("stop"),
            "succeeded",
            &["--agent-type", "loam:ingestor", "--session-id", "claude-1"],
        )
        .status
        .code(),
        Some(0)
    );

    let skipped = begin_id(&root, "claude", "stop", &workspace);
    assert_eq!(
        finish_result(
            &root,
            skipped,
            "succeeded",
            Some("skip"),
            Some("disabled"),
            None,
        )
        .status
        .code(),
        Some(0)
    );
    assert_eq!(
        record_event(
            &root,
            skipped,
            "claude_recursion_guard",
            None,
            "refused",
            &["--agent-type", "loam:ingestor"],
        )
        .status
        .code(),
        Some(0)
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn complete_v1_migration_is_lossless_race_safe_and_accepts_v2() {
    let root = temporary_root("schema-v1-race");
    let workspace = temporary_root("workspace");
    create_complete_v1_store(&root);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(root.join("loam.sqlite3"), fs::Permissions::from_mode(0o600)).unwrap();
    }
    let barrier = Arc::new(Barrier::new(2));
    let writers: Vec<_> = (0..2)
        .map(|_| {
            let root = root.clone();
            let workspace = workspace.clone();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                begin_id(&root, "codex", "stop", &workspace)
            })
        })
        .collect();
    let ids: HashSet<_> = writers
        .into_iter()
        .map(|writer| writer.join().unwrap())
        .collect();
    assert_eq!(ids.len(), 2);

    let database = root.join("loam.sqlite3");
    let connection = Connection::open(&database).unwrap();
    assert_eq!(
        connection
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        2
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT status || ':' || COALESCE(action, '-') || ':' || COALESCE(worker_origin, '-') || ':' || COALESCE(worker_session_id, '-') FROM hook_run WHERE id = 2",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
        "succeeded:spawn_worker:direct:child"
    );
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM hook_run", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        5
    );
    drop(connection);

    let continued = begin_id(&root, "codex", "stop", &workspace);
    assert_eq!(
        finish_result(
            &root,
            continued,
            "continued",
            Some("request_worker"),
            None,
            None,
        )
        .status
        .code(),
        Some(0)
    );
    assert_eq!(
        record_event(
            &root,
            continued,
            "codex_native",
            Some("continuation"),
            "returned",
            &["--visibility", "native"],
        )
        .status
        .code(),
        Some(0)
    );
    #[cfg(unix)]
    assert_eq!(
        fs::metadata(database).unwrap().permissions().mode() & 0o777,
        0o600
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn failed_v1_rebuild_rolls_back_every_schema_change() {
    let root = temporary_root("schema-v1-rollback");
    let workspace = temporary_root("workspace");
    create_complete_v1_store(&root);
    let database = root.join("loam.sqlite3");
    let connection = Connection::open(&database).unwrap();
    connection
        .execute("UPDATE hook_run SET action = 'invalid' WHERE id = 1", [])
        .unwrap();
    drop(connection);

    assert_eq!(
        begin(&root, "codex", "stop", &workspace).status.code(),
        Some(1)
    );
    let connection = Connection::open(database).unwrap();
    assert_eq!(
        connection
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT group_concat(name, ',') FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
        "hook_run"
    );
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM hook_run", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        3
    );
    drop(connection);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn v1_rebuild_handles_the_full_retention_cap() {
    let root = temporary_root("schema-v1-cap");
    let workspace = temporary_root("workspace");
    create_complete_v1_store(&root);
    let database = root.join("loam.sqlite3");
    let connection = Connection::open(&database).unwrap();
    connection
        .execute_batch(
            "WITH RECURSIVE n(id) AS (
                 VALUES(4) UNION ALL SELECT id + 1 FROM n WHERE id < 10000
             )
             INSERT INTO hook_run (
                 id, started_at_ms, harness, hook, status, workspace,
                 plugin_version, runtime_version
             )
             SELECT id, id, 'codex', 'stop', 'started', '/', '0.9.5', '0.9.5' FROM n;",
        )
        .unwrap();
    drop(connection);

    assert_eq!(
        begin(&root, "codex", "stop", &workspace).status.code(),
        Some(0)
    );
    let connection = Connection::open(database).unwrap();
    assert_eq!(
        connection
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        2
    );
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM hook_run", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        10_000
    );
    drop(connection);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn begin_prunes_only_old_hook_runs() {
    let root = temporary_root("retention");
    let workspace = temporary_root("workspace");
    begin_id(&root, "claude", "stop", &workspace);
    let database = root.join("loam.sqlite3");
    let mut connection = Connection::open(&database).unwrap();
    connection
        .execute("CREATE TABLE sentinel (value TEXT)", [])
        .unwrap();
    connection
        .execute("INSERT INTO sentinel VALUES ('kept')", [])
        .unwrap();
    let transaction = connection.transaction().unwrap();
    for index in 0..10_005_i64 {
        transaction
            .execute(
                "INSERT INTO hook_run (started_at_ms, finished_at_ms, harness, hook, status, detail, session_id, workspace, plugin_version, runtime_version) VALUES (?1, NULL, 'seed', 'seed', 'started', NULL, NULL, '/', '0.0.0', '0.0.0')",
                params![index],
            )
            .unwrap();
    }
    transaction
        .execute(
            "INSERT INTO hook_event (hook_run_id, occurred_at_ms, event, phase, outcome)
             VALUES (2, 1, 'codex_native', 'continuation', 'returned'),
                    (10006, 2, 'codex_native', 'continuation', 'returned')",
            [],
        )
        .unwrap();
    transaction.commit().unwrap();
    drop(connection);

    begin_id(&root, "codex", "stop", &workspace);
    let connection = Connection::open(database).unwrap();
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM hook_run", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        10_000
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT group_concat(hook_run_id, ',') FROM hook_event",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
        "10006"
    );
    assert_eq!(
        connection
            .query_row("SELECT value FROM sentinel", [], |row| row
                .get::<_, String>(0))
            .unwrap(),
        "kept"
    );
    drop(connection);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn concurrent_begins_get_distinct_ids() {
    let root = temporary_root("concurrent");
    let workspace = temporary_root("workspace");
    // Repeated rounds amplify lock races at the supported three-harness boundary.
    const WRITERS_PER_ROUND: usize = 3;
    const ROUNDS: usize = 20;
    let mut ids = HashSet::new();
    for _ in 0..ROUNDS {
        let barrier = Arc::new(Barrier::new(WRITERS_PER_ROUND));
        let writers: Vec<_> = (0..WRITERS_PER_ROUND)
            .map(|_| {
                let root = root.clone();
                let workspace = workspace.clone();
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    begin_id(&root, "codex", "stop", &workspace)
                })
            })
            .collect();
        ids.extend(writers.into_iter().map(|writer| writer.join().unwrap()));
    }
    let expected = (WRITERS_PER_ROUND * ROUNDS) as i64;
    assert_eq!(ids.len(), expected as usize);
    let connection = Connection::open(root.join("loam.sqlite3")).unwrap();
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM hook_run", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        expected
    );
    drop(connection);
    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn a_new_store_is_private() {
    use std::os::unix::fs::PermissionsExt;

    let root = temporary_root("permissions");
    let workspace = temporary_root("workspace");
    begin_id(&root, "claude", "stop", &workspace);
    assert_eq!(
        fs::metadata(&root).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(root.join("loam.sqlite3"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    fs::remove_dir_all(root).unwrap();
}
