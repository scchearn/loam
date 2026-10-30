//! `loam loams list` — fan-out view over the machine-local loams store.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
#[cfg(unix)]
use std::time::Instant;

struct Fixture {
    root: PathBuf,
}

/// `fs::canonicalize` returns verbatim `\\?\C:\...` paths on Windows; the store
/// records the plain `C:\...` form, so fixtures use it too.
fn plain(path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy();
    match text.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC") => PathBuf::from(rest),
        _ => path,
    }
}

impl Fixture {
    fn new(label: &str) -> Fixture {
        let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("loams-{label}"));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let root = plain(fs::canonicalize(root).unwrap());
        Fixture { root }
    }

    fn config(&self) -> PathBuf {
        self.root.join("config")
    }

    fn home(&self) -> PathBuf {
        self.root.join("home")
    }

    fn store(&self) -> PathBuf {
        self.config().join("loams")
    }

    /// A workspace with a wiki marker and `count` checkpoints, captured on
    /// consecutive days, with file names that sort opposite to capture order
    /// for one of them to prove the sort uses `Captured`.
    fn workspace(&self, name: &str, count: usize) -> PathBuf {
        let path = self.root.join(name);
        fs::create_dir_all(path.join("checkpoints")).unwrap();
        fs::write(path.join("index.md"), "# index\n").unwrap();
        for day in 1..=count {
            // Name order is reversed against capture order for day 1.
            let stamp = if day == 1 { 9 } else { day };
            fs::write(
                path.join("checkpoints")
                    .join(format!("checkpoint-2026-01-0{stamp}-{day}.md")),
                format!(
                    "# Checkpoint\n\n- Captured: 2026-02-0{day} 10:00 +02:00\n- Scope: scope-{day}\n\n### Title {day}\n"
                ),
            )
            .unwrap();
        }
        path
    }

    fn entry(&self, key: &str, workspace: &Path, seen: &str) {
        let volume = volume_of(workspace.parent().unwrap());
        let json = format!(
            "{{\"schema\":1,\"workspace\":{ws:?},\"wiki_root\":{ws:?},\"volume\":{volume:?},\"registered_at\":\"{seen}\",\"last_seen_at\":\"{seen}\"}}\n",
            ws = workspace.display().to_string(),
        );
        fs::create_dir_all(self.store()).unwrap();
        fs::write(self.store().join(format!("{key}.json")), json).unwrap();
    }

    fn run(&self, args: &[&str]) -> (i32, String, String) {
        let binary = env!("CARGO_BIN_EXE_loam");
        let output = Command::new(binary)
            .arg("loams")
            .args(args)
            .env("LOAM_CONFIG_DIR", self.config())
            .env("LOAM_HOME", self.home())
            .output()
            .expect("loam should run");
        (
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    }
}

#[cfg(unix)]
fn volume_of(path: &Path) -> String {
    use std::os::unix::fs::MetadataExt;
    format!("dev:{}", fs::metadata(path).unwrap().dev())
}

#[cfg(windows)]
fn volume_of(path: &Path) -> String {
    use std::path::Component;
    match path.components().next() {
        Some(Component::Prefix(prefix)) => {
            format!("prefix:{}", prefix.as_os_str().to_string_lossy())
        }
        _ => String::new(),
    }
}

/// Three entries: live with 5 checkpoints, live with none, deleted.
fn three(fixture: &Fixture) {
    let busy = fixture.workspace("busy", 5);
    let quiet = fixture.workspace("quiet", 0);
    let gone = fixture.root.join("gone");
    fixture.entry("aaa", &busy, "2026-03-01 09:00:00+02:00");
    fixture.entry("bbb", &quiet, "2026-03-02 09:00:00+02:00");
    fixture.entry("ccc", &gone, "2026-03-03 09:00:00+02:00");
}

fn status_of(json: &loam::json::Value, key: &str) -> String {
    json.get("loams")
        .and_then(|loams| loams.as_array())
        .unwrap()
        .iter()
        .find(|item| item.get("key").and_then(|v| v.as_str()) == Some(key))
        .and_then(|item| item.get("status"))
        .and_then(|v| v.as_str())
        .unwrap()
        .to_owned()
}

fn parse(stdout: &str) -> loam::json::Value {
    loam::json::parse(stdout).unwrap_or_else(|_| panic!("not json: {stdout}"))
}

#[test]
fn list_json_reports_statuses_and_the_three_newest_checkpoints() {
    let fixture = Fixture::new("json");
    three(&fixture);
    let (code, stdout, stderr) = fixture.run(&["list", "--checkpoints", "--json"]);
    assert_eq!(code, 0, "{stderr}");
    let json = parse(&stdout);
    assert!(matches!(json.get("schema"), Some(loam::json::Value::Number(n)) if n == "1"));
    assert_eq!(status_of(&json, "aaa"), "live");
    assert_eq!(status_of(&json, "bbb"), "live");
    assert_eq!(status_of(&json, "ccc"), "missing");

    let loams = json.get("loams").unwrap().as_array().unwrap();
    // Newest checkpoint first: busy, then the rest by last_seen_at (desc).
    let keys: Vec<_> = loams
        .iter()
        .map(|l| l.get("key").unwrap().as_str().unwrap())
        .collect();
    assert_eq!(keys, ["aaa", "ccc", "bbb"]);

    let busy = &loams[0];
    for field in ["workspace", "wiki_root", "registered_at", "last_seen_at"] {
        assert!(busy.get(field).is_some(), "{field}");
    }
    let checkpoints = busy.get("checkpoints").unwrap().as_array().unwrap();
    let captured: Vec<_> = checkpoints
        .iter()
        .map(|c| c.get("captured_at").unwrap().as_str().unwrap())
        .collect();
    assert_eq!(
        captured,
        [
            "2026-02-05 10:00 +02:00",
            "2026-02-04 10:00 +02:00",
            "2026-02-03 10:00 +02:00"
        ]
    );
    let first = &checkpoints[0];
    assert_eq!(first.get("title").unwrap().as_str(), Some("Title 5"));
    assert_eq!(first.get("scope").unwrap().as_str(), Some("scope-5"));
    assert!(first.get("path").is_some());
    assert_eq!(
        loams[2]
            .get("checkpoints")
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn list_without_checkpoints_flag_omits_the_field() {
    let fixture = Fixture::new("nocp");
    three(&fixture);
    let (code, stdout, _) = fixture.run(&["list", "--json"]);
    assert_eq!(code, 0);
    let json = parse(&stdout);
    for item in json.get("loams").unwrap().as_array().unwrap() {
        assert!(item.get("checkpoints").is_none());
    }
}

#[test]
fn list_table_shows_statuses_and_three_newest() {
    let fixture = Fixture::new("table");
    three(&fixture);
    let (code, stdout, _) = fixture.run(&["list", "--checkpoints"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("live"), "{stdout}");
    assert!(stdout.contains("missing"), "{stdout}");
    for day in [5, 4, 3] {
        assert!(stdout.contains(&format!("Title {day}")), "{stdout}");
    }
    for day in [2, 1] {
        assert!(!stdout.contains(&format!("Title {day}")), "{stdout}");
    }
}

#[test]
fn limit_zero_shows_all_checkpoints() {
    let fixture = Fixture::new("limit0");
    three(&fixture);
    let (_, stdout, _) = fixture.run(&["list", "--checkpoints", "--limit", "0", "--json"]);
    let json = parse(&stdout);
    let busy = &json.get("loams").unwrap().as_array().unwrap()[0];
    assert_eq!(
        busy.get("checkpoints").unwrap().as_array().unwrap().len(),
        5
    );
    let (_, table, _) = fixture.run(&["list", "--checkpoints", "--limit", "0"]);
    assert!(table.contains("Title 1"), "{table}");
}

#[test]
fn malformed_entry_is_invalid_with_key_and_status_only() {
    let fixture = Fixture::new("invalid");
    three(&fixture);
    fs::write(fixture.store().join("zzz.json"), "{not json").unwrap();
    // Temp files are skipped entirely.
    fs::write(fixture.store().join("zzz.json.tmp-1-abc"), "{").unwrap();
    let (code, stdout, _) = fixture.run(&["list", "--checkpoints", "--json"]);
    assert_eq!(code, 0);
    let json = parse(&stdout);
    assert_eq!(status_of(&json, "zzz"), "invalid");
    let loams = json.get("loams").unwrap().as_array().unwrap();
    assert_eq!(loams.len(), 4);
    let invalid = loams
        .iter()
        .find(|l| l.get("key").unwrap().as_str() == Some("zzz"))
        .unwrap();
    assert!(invalid.get("workspace").is_none());
    assert!(invalid.get("checkpoints").is_none());
}

#[test]
fn absent_store_is_empty_and_exits_zero() {
    let fixture = Fixture::new("absent");
    let (code, stdout, _) = fixture.run(&["list", "--json"]);
    assert_eq!(code, 0);
    assert!(parse(&stdout)
        .get("loams")
        .unwrap()
        .as_array()
        .unwrap()
        .is_empty());
    let (code, stdout, _) = fixture.run(&["list"]);
    assert_eq!(code, 0);
    assert!(stdout.trim().is_empty());
}

#[test]
fn store_path_that_is_a_file_exits_one_with_path_and_error() {
    let fixture = Fixture::new("unreadable");
    fs::create_dir_all(fixture.config()).unwrap();
    fs::write(fixture.store(), "not a directory").unwrap();
    let (code, _, stderr) = fixture.run(&["list"]);
    assert_eq!(code, 1);
    assert!(
        stderr.contains(&fixture.store().display().to_string()),
        "{stderr}"
    );
}

#[test]
fn relative_config_root_exits_one_with_reason() {
    let fixture = Fixture::new("relcfg");
    let output = Command::new(env!("CARGO_BIN_EXE_loam"))
        .args(["loams", "list"])
        .env("LOAM_CONFIG_DIR", "relative/dir")
        .current_dir(&fixture.root)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("config_root_relative"));
}

#[test]
fn bad_arguments_print_usage() {
    let fixture = Fixture::new("args");
    let (code, _, stderr) = fixture.run(&["list", "--limit", "x"]);
    assert_eq!(code, 1);
    assert!(stderr.contains("Usage"));
}

#[cfg(unix)]
#[test]
fn hung_checkpoint_read_times_out_while_other_loams_report() {
    let fixture = Fixture::new("fifo");
    three(&fixture);
    let hung = fixture.workspace("hung", 1);
    fixture.entry("ddd", &hung, "2026-03-04 09:00:00+02:00");
    let fifo = hung.join("checkpoints").join("checkpoint-zzz.md");
    let made = Command::new("mkfifo").arg(&fifo).status().unwrap();
    assert!(made.success());

    let started = Instant::now();
    let (code, stdout, stderr) = fixture.run(&["list", "--checkpoints", "--json"]);
    let elapsed = started.elapsed();
    assert_eq!(code, 0, "{stderr}");
    assert!(elapsed < Duration::from_secs(5), "took {elapsed:?}");
    let json = parse(&stdout);
    assert_eq!(status_of(&json, "ddd"), "timeout");
    let hung_row = json
        .get("loams")
        .and_then(|loams| loams.as_array())
        .unwrap()
        .iter()
        .find(|item| item.get("key").and_then(|v| v.as_str()) == Some("ddd"))
        .unwrap();
    for field in ["workspace", "wiki_root", "registered_at", "last_seen_at"] {
        assert!(hung_row.get(field).is_some(), "timeout row lacks {field}");
    }
    assert_eq!(
        hung_row.get("workspace").and_then(|v| v.as_str()),
        Some(hung.display().to_string().as_str())
    );
    assert_eq!(status_of(&json, "aaa"), "live");
    assert_eq!(status_of(&json, "bbb"), "live");
    assert_eq!(status_of(&json, "ccc"), "missing");
}

#[cfg(unix)]
#[test]
fn unreadable_checkpoint_dir_and_file_report_errors_not_live() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new("readerr");
    let locked = fixture.workspace("locked", 2);
    let unreadable = fixture.workspace("unreadable", 1);
    fixture.entry("aaa", &locked, "2026-03-01 09:00:00+02:00");
    fixture.entry("bbb", &unreadable, "2026-03-02 09:00:00+02:00");
    let dir = locked.join("checkpoints");
    let file = unreadable
        .join("checkpoints")
        .join("checkpoint-2026-01-09-1.md");
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o000)).unwrap();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o000)).unwrap();
    let restore = || {
        let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o755));
        let _ = fs::set_permissions(&file, fs::Permissions::from_mode(0o644));
    };
    // Permission bits do not bind root; skip rather than assert a false pass.
    if fs::read_dir(&dir).is_ok() {
        restore();
        return;
    }
    let (code, stdout, stderr) = fixture.run(&["list", "--checkpoints", "--json"]);
    restore();
    assert_eq!(code, 0, "{stderr}");
    let json = parse(&stdout);
    assert!(status_of(&json, "aaa").starts_with("error:"), "{stdout}");
    assert!(status_of(&json, "bbb").starts_with("error:"), "{stdout}");
}

#[cfg(unix)]
#[test]
fn directory_named_like_a_checkpoint_is_skipped() {
    let fixture = Fixture::new("cpdir");
    let workspace = fixture.workspace("dirs", 1);
    fs::create_dir(workspace.join("checkpoints").join("checkpoint-dir.md")).unwrap();
    fixture.entry("aaa", &workspace, "2026-03-01 09:00:00+02:00");
    let (code, stdout, _) = fixture.run(&["list", "--checkpoints", "--json"]);
    assert_eq!(code, 0);
    let json = parse(&stdout);
    assert_eq!(status_of(&json, "aaa"), "live");
}

// APFS (macOS) refuses non-UTF-8 file names, so these run where they can exist.
#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn non_utf8_entry_name_is_invalid_and_does_not_fail_the_listing() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    let fixture = Fixture::new("badname");
    three(&fixture);
    let name = OsStr::from_bytes(b"bad\xff.json");
    fs::write(fixture.store().join(name), "{}").unwrap();
    let (code, stdout, stderr) = fixture.run(&["list", "--json"]);
    assert_eq!(code, 0, "{stderr}");
    let json = parse(&stdout);
    let loams = json.get("loams").unwrap().as_array().unwrap();
    assert_eq!(loams.len(), 4);
    assert!(loams
        .iter()
        .any(|l| l.get("status").and_then(|v| v.as_str()) == Some("invalid")));
}

// ---------------------------------------------------------------------------
// add / import / prune
// ---------------------------------------------------------------------------

/// Every file under `roots` with its bytes, for before/after comparison.
fn snapshot(roots: &[&Path]) -> Vec<(PathBuf, Vec<u8>)> {
    fn walk(dir: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
        for item in fs::read_dir(dir).unwrap() {
            let path = item.unwrap().path();
            if path.is_dir() {
                out.push((path.clone(), Vec::new()));
                walk(&path, out);
            } else {
                out.push((path.clone(), fs::read(&path).unwrap()));
            }
        }
    }
    let mut out = Vec::new();
    for root in roots {
        walk(root, &mut out);
    }
    out.sort();
    out
}

fn store_names(fixture: &Fixture) -> Vec<String> {
    let mut names: Vec<String> = match fs::read_dir(fixture.store()) {
        Ok(items) => items
            .map(|item| item.unwrap().file_name().to_string_lossy().into_owned())
            .collect(),
        Err(_) => Vec::new(),
    };
    names.sort();
    names
}

#[test]
fn add_records_a_loam_then_reports_unchanged() {
    let fixture = Fixture::new("add");
    let workspace = fixture.workspace("one", 1);
    let before = snapshot(&[&workspace]);

    let (code, stdout, stderr) = fixture.run(&["add", workspace.to_str().unwrap()]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("created"), "{stdout}");
    assert_eq!(store_names(&fixture).len(), 1);

    let (code, stdout, _) = fixture.run(&["add", workspace.to_str().unwrap()]);
    assert_eq!(code, 0);
    assert!(stdout.contains("unchanged"), "{stdout}");
    assert_eq!(snapshot(&[&workspace]), before);
}

#[test]
fn add_on_a_non_loam_exits_one_naming_the_skip() {
    let fixture = Fixture::new("addno");
    let plain = fixture.root.join("plain");
    fs::create_dir_all(&plain).unwrap();
    let before = snapshot(&[&plain]);
    let (code, _, stderr) = fixture.run(&["add", plain.to_str().unwrap()]);
    assert_eq!(code, 1);
    assert!(stderr.contains("not_loam"), "{stderr}");
    assert!(!fixture.store().exists());
    assert_eq!(snapshot(&[&plain]), before);

    let (code, _, stderr) = fixture.run(&["add"]);
    assert_eq!(code, 1);
    assert!(stderr.contains("Usage"), "{stderr}");
}

/// Build a real hook ledger by driving `loam hooks begin` per workspace.
fn ledger_with(fixture: &Fixture, workspaces: &[&Path]) -> PathBuf {
    let home = fixture.home();
    for workspace in workspaces {
        let output = Command::new(env!("CARGO_BIN_EXE_loam"))
            .args(["hooks", "begin", home.to_str().unwrap()])
            .args(["--harness", "claude", "--hook", "session-start"])
            .args(["--workspace", workspace.to_str().unwrap()])
            .args(["--plugin-version", "1.0.0"])
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    home.join("loam.sqlite3")
}

#[test]
fn import_records_only_loams_and_is_idempotent() {
    let fixture = Fixture::new("import");
    let loam_ws = fixture.workspace("real", 2);
    let plain = fixture.root.join("plain");
    fs::create_dir_all(&plain).unwrap();
    let gone = fixture.root.join("gone");
    let temp_ws = std::env::temp_dir().join(format!(
        "loam-loams-import-{}",
        fixture.root.file_name().unwrap().to_string_lossy()
    ));
    fs::create_dir_all(&temp_ws).unwrap();
    fs::write(temp_ws.join("index.md"), "# index\n").unwrap();
    let ledger = ledger_with(&fixture, &[&loam_ws, &plain, &gone, &temp_ws]);
    let ledger_before = fs::read(&ledger).unwrap();
    let trees_before = snapshot(&[&loam_ws, &plain]);

    let (code, stdout, stderr) = fixture.run(&["import", "--ledger", ledger.to_str().unwrap()]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("recorded: "), "{stdout}");
    assert!(stdout.contains("skipped: temp"), "{stdout}");
    assert!(
        stdout.contains("1 recorded, 0 unchanged, 3 skipped"),
        "{stdout}"
    );
    assert_eq!(store_names(&fixture).len(), 1, "{stdout}");
    let entry = fs::read_to_string(fixture.store().join(&store_names(&fixture)[0])).unwrap();
    assert!(entry.contains(&loam_ws.display().to_string()), "{entry}");

    let (code, stdout, _) = fixture.run(&["import", "--ledger", ledger.to_str().unwrap()]);
    assert_eq!(code, 0);
    let _ = fs::remove_dir_all(&temp_ws);
    assert!(!stdout.contains("recorded: "), "{stdout}");
    assert!(stdout.contains("unchanged: "), "{stdout}");
    assert!(
        stdout.contains("0 recorded, 1 unchanged, 3 skipped"),
        "{stdout}"
    );
    assert_eq!(store_names(&fixture).len(), 1);

    assert_eq!(fs::read(&ledger).unwrap(), ledger_before);
    assert_eq!(snapshot(&[&loam_ws, &plain]), trees_before);
}

#[test]
fn import_defaults_to_the_global_root_ledger_and_tolerates_none() {
    let fixture = Fixture::new("importdef");
    let (code, stdout, _) = fixture.run(&["import"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("no hook ledger"), "{stdout}");
    assert!(!fixture.store().exists());

    let workspace = fixture.workspace("real", 0);
    ledger_with(&fixture, &[&workspace]);
    let (code, stdout, stderr) = fixture.run(&["import"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("recorded"), "{stdout}");
    assert_eq!(store_names(&fixture).len(), 1);
}

/// Live, missing and invalid entries plus a stale and a fresh temp file.
fn prune_fixture(fixture: &Fixture) -> (PathBuf, PathBuf) {
    let live = fixture.workspace("live", 1);
    fixture.entry("live", &live, "2026-03-01 09:00:00+02:00");
    fixture.entry(
        "gone",
        &fixture.root.join("gone"),
        "2026-03-02 09:00:00+02:00",
    );
    fs::write(fixture.store().join("broken.json"), "{not json").unwrap();
    let stale = fixture.store().join("live.json.tmp-1-old");
    let fresh = fixture.store().join("live.json.tmp-2-new");
    fs::write(&stale, "{").unwrap();
    fs::write(&fresh, "{").unwrap();
    let old = std::time::SystemTime::now() - Duration::from_secs(2 * 3600);
    fs::File::options()
        .write(true)
        .open(&stale)
        .unwrap()
        .set_modified(old)
        .unwrap();
    (stale, fresh)
}

#[test]
fn prune_without_yes_lists_and_deletes_nothing() {
    let fixture = Fixture::new("prunedry");
    let (_, _) = prune_fixture(&fixture);
    let live = fixture.root.join("live");
    let store_before = snapshot(&[&fixture.store()]);
    let tree_before = snapshot(&[&live]);

    let (code, stdout, stderr) = fixture.run(&["prune"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("would remove"), "{stdout}");
    assert!(stdout.contains("gone.json"), "{stdout}");
    assert!(stdout.contains("broken.json"), "{stdout}");
    assert!(stdout.contains("keep"), "{stdout}");
    assert_eq!(snapshot(&[&fixture.store()]), store_before);
    assert_eq!(snapshot(&[&live]), tree_before);
}

#[test]
fn prune_yes_removes_missing_invalid_and_stale_temp_only() {
    let fixture = Fixture::new("pruneyes");
    let (stale, fresh) = prune_fixture(&fixture);
    let live = fixture.root.join("live");
    // An unreachable entry (workspace on another volume) must survive.
    let other = fixture.root.join("gone-elsewhere");
    fixture.entry("elsewhere", &other, "2026-03-03 09:00:00+02:00");
    let path = fixture.store().join("elsewhere.json");
    let text = fs::read_to_string(&path)
        .unwrap()
        .replace(&volume_of(&fixture.root), "other-volume");
    fs::write(&path, text).unwrap();
    let tree_before = snapshot(&[&live]);

    let (code, stdout, stderr) = fixture.run(&["prune", "--yes"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("removed"), "{stdout}");
    assert_eq!(
        store_names(&fixture),
        ["elsewhere.json", "live.json", "live.json.tmp-2-new"]
    );
    assert!(fresh.exists() && !stale.exists());
    assert_eq!(snapshot(&[&live]), tree_before);
}

#[test]
fn prune_on_an_absent_store_is_a_quiet_success() {
    let fixture = Fixture::new("pruneabsent");
    let (code, _, _) = fixture.run(&["prune", "--yes"]);
    assert_eq!(code, 0);
    assert!(!fixture.store().exists());
}

// ---------------------------------------------------------------------------
// identity is the wiki; review findings
// ---------------------------------------------------------------------------

/// `<root>/<name>/wiki/index.md`, returning the workspace directory.
fn wiki_workspace(fixture: &Fixture, name: &str) -> PathBuf {
    let path = fixture.root.join(name);
    fs::create_dir_all(path.join("wiki")).unwrap();
    fs::write(path.join("wiki").join("index.md"), "# index\n").unwrap();
    path
}

fn only_entry(fixture: &Fixture) -> loam::json::Value {
    let names = store_names(fixture);
    assert_eq!(names.len(), 1, "{names:?}");
    loam::json::parse(&fs::read_to_string(fixture.store().join(&names[0])).unwrap()).unwrap()
}

#[test]
fn add_of_a_workspace_and_its_wiki_dir_is_one_loam() {
    let fixture = Fixture::new("idwiki");
    let ws = wiki_workspace(&fixture, "x");
    let (code, _, stderr) = fixture.run(&["add", ws.to_str().unwrap()]);
    assert_eq!(code, 0, "{stderr}");
    let (code, stdout, stderr) = fixture.run(&["add", ws.join("wiki").to_str().unwrap()]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("unchanged"), "{stdout}");
    let entry = only_entry(&fixture);
    assert_eq!(
        entry.get("workspace").and_then(|v| v.as_str()),
        Some(ws.display().to_string().as_str())
    );
}

#[test]
fn a_wiki_at_the_root_records_itself_as_the_workspace() {
    let fixture = Fixture::new("idroot");
    let ws = fixture.workspace("y", 0);
    let (code, _, stderr) = fixture.run(&["add", ws.to_str().unwrap()]);
    assert_eq!(code, 0, "{stderr}");
    let entry = only_entry(&fixture);
    assert_eq!(
        entry.get("workspace").and_then(|v| v.as_str()),
        Some(ws.display().to_string().as_str())
    );
}

#[test]
fn import_of_a_ledger_with_both_forms_gives_one_entry() {
    let fixture = Fixture::new("idimport");
    let ws = wiki_workspace(&fixture, "x");
    let ledger = ledger_with(&fixture, &[&ws, &ws.join("wiki")]);
    let (code, stdout, stderr) = fixture.run(&["import", "--ledger", ledger.to_str().unwrap()]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("1 recorded, 1 unchanged"), "{stdout}");
    assert_eq!(store_names(&fixture).len(), 1);
}

#[test]
fn add_echoes_the_canonical_workspace() {
    let fixture = Fixture::new("addecho");
    let ws = fixture.workspace("one", 0);
    let raw = fixture.root.join("one").join("..").join("one");
    let (code, stdout, stderr) = fixture.run(&["add", raw.to_str().unwrap()]);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stdout.trim(), format!("created: {}", ws.display()));
}

#[test]
fn import_fails_up_front_when_the_store_cannot_be_resolved() {
    let fixture = Fixture::new("importnostore");
    let output = Command::new(env!("CARGO_BIN_EXE_loam"))
        .args(["loams", "import", "--ledger", "/nonexistent/ledger.sqlite3"])
        .env("LOAM_CONFIG_DIR", "relative/config")
        .env("LOAM_HOME", fixture.home())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("config_root_relative"));
}

#[test]
fn import_exits_one_and_counts_failed_when_a_register_errors() {
    let fixture = Fixture::new("importfail");
    let ws = fixture.workspace("real", 0);
    let ledger = ledger_with(&fixture, &[&ws]);
    fs::create_dir_all(fixture.config()).unwrap();
    fs::write(fixture.store(), "a file where the store should be").unwrap();
    let (code, stdout, _) = fixture.run(&["import", "--ledger", ledger.to_str().unwrap()]);
    assert_eq!(code, 1, "{stdout}");
    assert!(stdout.contains("1 failed"), "{stdout}");
    assert!(stdout.contains("0 skipped"), "{stdout}");
}

#[test]
fn a_ledger_without_a_hook_run_table_means_no_hook_ledger() {
    let fixture = Fixture::new("importnotable");
    let path = fixture.root.join("empty.sqlite3");
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection.execute("CREATE TABLE other (x)", []).unwrap();
    drop(connection);
    let (code, stdout, stderr) = fixture.run(&["import", "--ledger", path.to_str().unwrap()]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("no hook ledger"), "{stdout}");
}

#[test]
fn a_non_utf8_workspace_row_is_skipped_not_fatal() {
    let fixture = Fixture::new("importbadrow");
    let ws = fixture.workspace("real", 0);
    let ledger = ledger_with(&fixture, &[&ws, &fixture.root.join("other")]);
    let connection = rusqlite::Connection::open(&ledger).unwrap();
    connection
        .execute(
            "UPDATE hook_run SET workspace = CAST(x'2fff' AS TEXT) WHERE workspace LIKE '%other'",
            [],
        )
        .unwrap();
    drop(connection);
    let (code, stdout, stderr) = fixture.run(&["import", "--ledger", ledger.to_str().unwrap()]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("skipped: non-utf8"), "{stdout}");
    assert!(stdout.contains("1 recorded"), "{stdout}");
}

// APFS (macOS) refuses non-UTF-8 file names, so these run where they can exist.
#[cfg(all(unix, not(target_os = "macos")))]
fn bad_name() -> std::ffi::OsString {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::OsStr::from_bytes(b"bad\xff.json").to_owned()
}

// APFS (macOS) refuses non-UTF-8 file names, so these run where they can exist.
#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn prune_removes_a_symlink_but_never_what_it_points_at_inside_the_store() {
    let fixture = Fixture::new("prunelink");
    let live = fixture.workspace("live", 0);
    fixture.entry("live", &live, "2026-03-01 09:00:00+02:00");
    let link = fixture.store().join(bad_name());
    std::os::unix::fs::symlink(fixture.store().join("live.json"), &link).unwrap();
    let (code, stdout, stderr) = fixture.run(&["prune", "--yes"]);
    assert_eq!(code, 0, "{stdout}{stderr}");
    assert!(link.symlink_metadata().is_err(), "link survived");
    assert!(fixture.store().join("live.json").is_file());
}

// APFS (macOS) refuses non-UTF-8 file names, so these run where they can exist.
#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn prune_removes_a_symlink_but_never_what_it_points_at_outside_the_store() {
    let fixture = Fixture::new("prunelinkout");
    let outside = fixture.root.join("outside.json");
    fs::write(&outside, "precious").unwrap();
    fs::create_dir_all(fixture.store()).unwrap();
    let link = fixture.store().join(bad_name());
    std::os::unix::fs::symlink(&outside, &link).unwrap();
    let (code, stdout, stderr) = fixture.run(&["prune", "--yes"]);
    assert_eq!(code, 0, "{stdout}{stderr}");
    assert!(link.symlink_metadata().is_err(), "link survived");
    assert_eq!(fs::read_to_string(&outside).unwrap(), "precious");
}

#[test]
fn prune_skips_directories_named_like_entries_without_ending_the_run() {
    let fixture = Fixture::new("prunedirs");
    let (stale, _) = prune_fixture(&fixture);
    fs::create_dir_all(fixture.store().join("dir.json")).unwrap();
    let tmp_dir = fixture.store().join("dir.json.tmp-1-old");
    fs::create_dir_all(&tmp_dir).unwrap();
    let old = std::time::SystemTime::now() - Duration::from_secs(2 * 3600);
    fs::File::open(&tmp_dir).unwrap().set_modified(old).unwrap();
    let (code, stdout, stderr) = fixture.run(&["prune", "--yes"]);
    assert_eq!(code, 0, "{stdout}{stderr}");
    assert!(fixture.store().join("dir.json").is_dir());
    assert!(tmp_dir.is_dir());
    assert!(!stale.exists());
    assert!(!fixture.store().join("gone.json").exists());
}

#[cfg(unix)]
#[test]
fn prune_keeps_an_entry_whose_read_hangs() {
    let fixture = Fixture::new("prunehang");
    let live = fixture.workspace("live", 0);
    fixture.entry("live", &live, "2026-03-01 09:00:00+02:00");
    fixture.entry(
        "gone",
        &fixture.root.join("gone"),
        "2026-03-02 09:00:00+02:00",
    );
    let fifo = fixture.store().join("hung.json");
    assert!(Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .unwrap()
        .success());
    let started = Instant::now();
    let (code, stdout, stderr) = fixture.run(&["prune", "--yes"]);
    assert_eq!(code, 0, "{stdout}{stderr}");
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(fifo.symlink_metadata().is_ok(), "hung entry was deleted");
    assert!(!fixture.store().join("gone.json").exists());
}
