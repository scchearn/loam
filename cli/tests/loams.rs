//! `loam loams list` — fan-out view over the machine-local loams store.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(label: &str) -> Fixture {
        let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("loams-{label}"));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let root = fs::canonicalize(root).unwrap();
        Fixture { root }
    }

    fn config(&self) -> PathBuf {
        self.root.join("config")
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

#[cfg(unix)]
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
