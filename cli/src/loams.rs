//! Machine-local loams store: one small JSON entry per loam workspace under
//! `<configRoot>/loams/`. Nothing here ever writes under a workspace, and
//! nothing here reads federation state.
// `add`, `import` and `prune` (T4) will use the remaining helpers.
#![allow(dead_code)]

use crate::json::{self, Value};
use crate::sha256::Sha256;
use chrono::{DateTime, FixedOffset};
use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

/// Local time with a canonical `±HH:MM` offset.
pub(crate) const TIMESTAMP_FORMAT: &str = "%Y-%m-%d %H:%M:%S%:z";

const SCHEMA: &str = "1";

/// Why a workspace was not registered. Skips are not failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Skip {
    /// `LOAM_REGISTRY=off`.
    Disabled,
    /// The workspace sits under the system temp dir.
    Temp,
    /// `resolve_wiki_root` found no wiki marker.
    NotLoam,
    /// No config root resolves.
    ConfigRootUnresolved,
    /// The config root is not absolute (hooks run with cwd = the workspace).
    ConfigRootRelative,
}

/// A workspace that passed the gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Gated {
    pub canonical: PathBuf,
    pub wiki_root: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Outcome {
    Created,
    Rewritten,
    Unchanged,
    Skipped(Skip),
}

/// One store entry (schema 1). Timestamps are `TIMESTAMP_FORMAT` strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Entry {
    pub workspace: String,
    pub wiki_root: String,
    pub volume: String,
    pub registered_at: String,
    pub last_seen_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Status {
    Live,
    Missing,
    Unreachable,
    Error(ErrorKind),
    Invalid,
}

/// Stat input for `classify`, as plain data so tests can inject device ids.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Stat {
    /// When set, replaces the measured volume of the nearest existing
    /// ancestor of a vanished workspace.
    pub ancestor_volume: Option<String>,
}

/// `<configRoot>/loams`.
pub(crate) fn store_dir() -> Result<PathBuf, &'static str> {
    let root = crate::provisioning::configured_config_root().ok_or("config_root_unresolved")?;
    if !root.is_absolute() {
        return Err("config_root_relative");
    }
    Ok(root.join("loams"))
}

/// Canonicalize, reduced to the path every tool accepts.
fn canonical_path(path: &Path) -> std::io::Result<PathBuf> {
    fs::canonicalize(path).map(crate::enrollment::dos_compatible)
}

/// The SessionStart gate: not disabled, outside the temp root, a loam.
pub(crate) fn gate(workspace: &Path, temp_root: &Path) -> Result<Gated, Skip> {
    if std::env::var("LOAM_REGISTRY").is_ok_and(|value| value.trim() == "off") {
        return Err(Skip::Disabled);
    }
    // A workspace that cannot be canonicalized cannot be a loam either.
    let canonical = canonical_path(workspace).map_err(|_| Skip::NotLoam)?;
    let temp = canonical_path(temp_root)
        .unwrap_or_else(|_| crate::enrollment::dos_compatible(temp_root.to_path_buf()));
    if canonical.starts_with(&temp) {
        return Err(Skip::Temp);
    }
    let wiki_root = crate::state::resolve_wiki_root(&canonical).ok_or(Skip::NotLoam)?;
    Ok(Gated {
        canonical,
        wiki_root,
    })
}

/// Lowercase hex sha256 of the canonical path. Folded to ASCII lowercase on
/// Windows only, where paths compare case-insensitively.
pub(crate) fn key(canonical: &Path) -> String {
    let text = canonical.to_string_lossy();
    #[cfg(windows)]
    let text = std::borrow::Cow::Owned(text.to_ascii_lowercase());
    let mut hasher = Sha256::default();
    hasher.update(text.as_bytes());
    hasher.finish()
}

/// Volume identity: `st_dev` on Unix, the drive or UNC share on Windows.
pub(crate) fn volume(canonical: &Path) -> std::io::Result<String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(format!("dev:{}", fs::metadata(canonical)?.dev()))
    }
    #[cfg(windows)]
    {
        use std::path::Component;
        match canonical.components().next() {
            Some(Component::Prefix(prefix)) => {
                Ok(format!("prefix:{}", prefix.as_os_str().to_string_lossy()))
            }
            _ => Err(ErrorKind::NotFound.into()),
        }
    }
}

impl Entry {
    pub(crate) fn to_json(&self) -> String {
        let text = |value: &str| Value::String(value.to_owned());
        let mut out = Value::Object(vec![
            ("schema".to_owned(), Value::Number(SCHEMA.to_owned())),
            ("workspace".to_owned(), text(&self.workspace)),
            ("wiki_root".to_owned(), text(&self.wiki_root)),
            ("volume".to_owned(), text(&self.volume)),
            ("registered_at".to_owned(), text(&self.registered_at)),
            ("last_seen_at".to_owned(), text(&self.last_seen_at)),
        ])
        .to_json();
        out.push('\n');
        out
    }

    /// Unknown fields are ignored; a missing required field is an error.
    pub(crate) fn parse(text: &str) -> Result<Entry, &'static str> {
        let value = json::parse(text).map_err(|_| "not_json")?;
        match value.get("schema") {
            Some(Value::Number(schema)) if schema == SCHEMA => {}
            _ => return Err("schema"),
        }
        let field = |name: &'static str| {
            value
                .get(name)
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or(name)
        };
        Ok(Entry {
            workspace: field("workspace")?,
            wiki_root: field("wiki_root")?,
            volume: field("volume")?,
            registered_at: field("registered_at")?,
            last_seen_at: field("last_seen_at")?,
        })
    }

    fn seen_on(&self, now: &DateTime<FixedOffset>) -> bool {
        DateTime::parse_from_str(&self.last_seen_at, TIMESTAMP_FORMAT)
            .is_ok_and(|seen| seen.date_naive() == now.date_naive())
    }
}

/// Record `workspace` in the store. Never writes under the workspace.
pub(crate) fn register(
    workspace: &Path,
    now: DateTime<FixedOffset>,
    temp_root: &Path,
) -> Result<Outcome, String> {
    let gated = match gate(workspace, temp_root) {
        Ok(gated) => gated,
        Err(skip) => return Ok(Outcome::Skipped(skip)),
    };
    let dir = match store_dir() {
        Ok(dir) => dir,
        Err("config_root_relative") => return Ok(Outcome::Skipped(Skip::ConfigRootRelative)),
        Err(_) => return Ok(Outcome::Skipped(Skip::ConfigRootUnresolved)),
    };
    let volume = volume(&gated.canonical).map_err(|error| format!("volume: {}", error.kind()))?;
    let mut entry = Entry {
        workspace: gated.canonical.display().to_string(),
        wiki_root: gated.wiki_root.display().to_string(),
        volume,
        registered_at: now.format(TIMESTAMP_FORMAT).to_string(),
        last_seen_at: now.format(TIMESTAMP_FORMAT).to_string(),
    };
    let path = dir.join(format!("{}.json", key(&gated.canonical)));
    let outcome = match fs::read_to_string(&path) {
        Ok(text) => match Entry::parse(&text) {
            Ok(existing) => {
                let same = existing.workspace == entry.workspace
                    && existing.wiki_root == entry.wiki_root
                    && existing.volume == entry.volume;
                if same && existing.seen_on(&now) {
                    return Ok(Outcome::Unchanged);
                }
                entry.registered_at = existing.registered_at;
                Outcome::Rewritten
            }
            Err(_) => Outcome::Rewritten,
        },
        Err(error) if error.kind() == ErrorKind::NotFound => Outcome::Created,
        Err(error) => return Err(format!("read: {}", error.kind())),
    };
    write_atomic(&dir, &path, &entry.to_json())
        .map_err(|error| format!("write: {}", error.kind()))?;
    Ok(outcome)
}

/// A name unique across threads and processes without the process id: time,
/// a per-process counter, and per-process random hasher keys.
fn unique_suffix() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::BuildHasher;
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = chrono::Local::now().timestamp_nanos_opt().unwrap_or(0);
    let random = RandomState::new().hash_one((nanos, COUNTER.fetch_add(1, Ordering::Relaxed)));
    format!("{nanos}-{random:016x}")
}

/// Temp file in the same directory, then `rename`. Directory 0700 and file
/// 0600 on Unix. Readers ignore `*.tmp-*`.
fn write_atomic(dir: &Path, path: &Path, content: &str) -> std::io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir)?;

    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temp = dir.join(format!("{file_name}.tmp-{}", unique_suffix()));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let written = options.open(&temp).and_then(|mut file| {
        file.write_all(content.as_bytes())?;
        file.sync_all()
    });
    let renamed = written.and_then(|()| rename_into_place(&temp, path));
    if renamed.is_err() {
        let _ = fs::remove_file(&temp);
    }
    renamed
}

#[cfg(not(windows))]
fn rename_into_place(from: &Path, to: &Path) -> std::io::Result<()> {
    fs::rename(from, to)
}

/// Windows refuses to replace a file another process holds open; retry
/// briefly (3 attempts, 50 ms apart) inside the caller's deadline.
#[cfg(windows)]
fn rename_into_place(from: &Path, to: &Path) -> std::io::Result<()> {
    let mut attempt = 0;
    loop {
        match fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(_) if attempt < 2 => {
                attempt += 1;
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(error) => return Err(error),
        }
    }
}

/// Classify one entry file against the live filesystem.
pub(crate) fn classify(entry_path: &Path, stat: &Stat) -> Status {
    let text = match fs::read_to_string(entry_path) {
        Ok(text) => text,
        Err(error) => return Status::Error(error.kind()),
    };
    let Ok(entry) = Entry::parse(&text) else {
        return Status::Invalid;
    };
    let workspace = Path::new(&entry.workspace);
    match fs::metadata(workspace) {
        Ok(_) => {
            if crate::state::resolve_wiki_root(workspace).is_some() {
                Status::Live
            } else {
                Status::Missing
            }
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {
            let ancestor = workspace
                .ancestors()
                .skip(1)
                .find(|candidate| fs::metadata(candidate).is_ok());
            let Some(ancestor) = ancestor else {
                return Status::Unreachable;
            };
            let measured = match &stat.ancestor_volume {
                Some(volume) => Some(volume.clone()),
                None => volume(ancestor).ok(),
            };
            if fs::read_dir(ancestor).is_ok() && measured.as_deref() == Some(entry.volume.as_str())
            {
                Status::Missing
            } else {
                Status::Unreachable
            }
        }
        Err(error) => Status::Error(error.kind()),
    }
}

// ---------------------------------------------------------------------------
// `loam loams`
// ---------------------------------------------------------------------------

use crate::state::Checkpoint;
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// One overall deadline for every per-entry worker.
const FANOUT_DEADLINE: Duration = Duration::from_secs(2);
const DEFAULT_LIMIT: usize = 3;

fn usage() {
    eprintln!(
        "Usage:\n  loam loams list [--checkpoints] [--limit <N>] [--json]\n  loam loams add <path>\n  loam loams import [--ledger <path>]\n  loam loams prune [--yes]"
    );
}

pub(crate) fn run(mut args: impl Iterator<Item = String>) -> i32 {
    match args.next().as_deref() {
        Some("list") => list(args),
        Some("add" | "import" | "prune") => {
            eprintln!("loam loams: this subcommand is not yet implemented");
            1
        }
        _ => {
            usage();
            1
        }
    }
}

struct ListOptions {
    checkpoints: bool,
    limit: usize,
    json: bool,
}

fn parse_list_args(mut args: impl Iterator<Item = String>) -> Option<ListOptions> {
    let mut options = ListOptions {
        checkpoints: false,
        limit: DEFAULT_LIMIT,
        json: false,
    };
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--checkpoints" => options.checkpoints = true,
            "--json" => options.json = true,
            "--limit" => options.limit = args.next()?.parse().ok()?,
            _ => return None,
        }
    }
    Some(options)
}

/// What one worker found for one entry file.
struct Report {
    key: String,
    status: Option<Status>,
    entry: Option<Entry>,
    checkpoints: Vec<Checkpoint>,
}

/// A row after collection; `status: None` means the worker never answered.
struct Row {
    key: String,
    status: Option<Status>,
    entry: Option<Entry>,
    checkpoints: Vec<Checkpoint>,
}

fn status_text(status: Option<&Status>) -> String {
    match status {
        Some(Status::Live) => "live".to_owned(),
        Some(Status::Missing) => "missing".to_owned(),
        Some(Status::Unreachable) => "unreachable".to_owned(),
        Some(Status::Error(kind)) => format!("error: {kind}"),
        Some(Status::Invalid) => "invalid".to_owned(),
        None => "timeout".to_owned(),
    }
}

/// `captured_at` as written by checkpoints: `YYYY-MM-DD HH:MM ±HH:MM` is the
/// norm; seconds, RFC 3339 and offset-less stamps (read as UTC) are tolerated.
fn parse_captured(text: &str) -> Option<DateTime<FixedOffset>> {
    let text = text.trim();
    for format in [
        "%Y-%m-%d %H:%M %:z",
        "%Y-%m-%d %H:%M:%S %:z",
        "%Y-%m-%d %H:%M:%S%:z",
        "%Y-%m-%d %H:%M%:z",
    ] {
        if let Ok(parsed) = DateTime::parse_from_str(text, format) {
            return Some(parsed);
        }
    }
    if let Ok(parsed) = DateTime::parse_from_rfc3339(text) {
        return Some(parsed);
    }
    let utc = FixedOffset::east_opt(0)?;
    for format in ["%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M"] {
        if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(text, format) {
            return naive.and_local_timezone(utc).single();
        }
    }
    chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d")
        .ok()?
        .and_hms_opt(0, 0, 0)?
        .and_local_timezone(utc)
        .single()
}

fn checkpoint_time(checkpoint: &Checkpoint) -> Option<DateTime<FixedOffset>> {
    checkpoint.captured_at.as_deref().and_then(parse_captured)
}

/// Newest first by parsed `captured_at`; unparseable stamps sort after parsed
/// ones, and ties fall back to the file name, descending.
fn sort_checkpoints(checkpoints: &mut [Checkpoint]) {
    checkpoints.sort_by(|left, right| {
        checkpoint_time(right)
            .cmp(&checkpoint_time(left))
            .then_with(|| right.path.cmp(&left.path))
    });
}

/// Checkpoint files by name only. Unlike `state::read_checkpoints` this does
/// not stat the file type, so an entry that cannot be read still reaches
/// `parse_checkpoint` and is caught by the deadline.
fn checkpoint_files(wiki_root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(wiki_root.join("checkpoints")) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("checkpoint-") && name.ends_with(".md"))
        })
        .collect()
}

fn inspect(path: &Path, key: String, with_checkpoints: bool) -> Report {
    let status = classify(path, &Stat::default());
    let entry = fs::read_to_string(path)
        .ok()
        .and_then(|text| Entry::parse(&text).ok());
    let mut checkpoints = Vec::new();
    if with_checkpoints && status == Status::Live {
        if let Some(entry) = &entry {
            checkpoints = checkpoint_files(Path::new(&entry.wiki_root))
                .into_iter()
                .map(crate::state::parse_checkpoint)
                .collect();
        }
    }
    Report {
        key,
        status: Some(status),
        entry,
        checkpoints,
    }
}

fn entry_files(dir: &Path) -> std::io::Result<Vec<(String, PathBuf)>> {
    let mut files = Vec::new();
    for item in fs::read_dir(dir)? {
        let path = item?.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if name.contains(".tmp-") {
            continue;
        }
        if let Some(key) = name.strip_suffix(".json") {
            files.push((key.to_owned(), path.clone()));
        }
    }
    files.sort();
    Ok(files)
}

/// Spawn one detached worker per entry and collect under a single deadline.
/// Workers are never joined: a hung read must not hang the command.
fn collect_rows(files: Vec<(String, PathBuf)>, with_checkpoints: bool) -> Vec<Row> {
    let (sender, receiver) = mpsc::channel();
    let total = files.len();
    let keys: Vec<String> = files.iter().map(|(key, _)| key.clone()).collect();
    let deadline = Instant::now() + FANOUT_DEADLINE;
    for (key, path) in files {
        let sender = sender.clone();
        std::thread::spawn(move || {
            let _ = sender.send(inspect(&path, key, with_checkpoints));
        });
    }
    drop(sender);
    let mut rows = Vec::new();
    while rows.len() < total {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match receiver.recv_timeout(remaining) {
            Ok(report) => rows.push(Row {
                key: report.key,
                status: report.status,
                entry: report.entry,
                checkpoints: report.checkpoints,
            }),
            Err(_) => break,
        }
    }
    for key in keys {
        if !rows.iter().any(|row| row.key == key) {
            rows.push(Row {
                key,
                status: None,
                entry: None,
                checkpoints: Vec::new(),
            });
        }
    }
    rows
}

fn newest(row: &Row) -> Option<DateTime<FixedOffset>> {
    row.checkpoints.first().and_then(checkpoint_time)
}

fn last_seen(row: &Row) -> Option<DateTime<FixedOffset>> {
    row.entry
        .as_ref()
        .and_then(|entry| DateTime::parse_from_str(&entry.last_seen_at, TIMESTAMP_FORMAT).ok())
}

fn list(args: impl Iterator<Item = String>) -> i32 {
    let Some(options) = parse_list_args(args) else {
        usage();
        return 1;
    };
    let dir = match store_dir() {
        Ok(dir) => dir,
        Err(reason) => {
            eprintln!("loam loams: {reason}");
            return 1;
        }
    };
    let files = match entry_files(&dir) {
        Ok(files) => files,
        Err(error) if error.kind() == ErrorKind::NotFound => Vec::new(),
        Err(error) => {
            eprintln!("loam loams: cannot read {}: {error}", dir.display());
            return 1;
        }
    };
    let mut rows = collect_rows(files, options.checkpoints);
    for row in &mut rows {
        sort_checkpoints(&mut row.checkpoints);
        if options.limit > 0 {
            row.checkpoints.truncate(options.limit);
        }
    }
    rows.sort_by(|left, right| {
        newest(right)
            .cmp(&newest(left))
            .then_with(|| last_seen(right).cmp(&last_seen(left)))
            .then_with(|| left.key.cmp(&right.key))
    });
    if options.json {
        println!("{}", render_json(&rows, options.checkpoints));
    } else {
        print!("{}", render_table(&rows, options.checkpoints));
    }
    0
}

fn text(value: &str) -> Value {
    Value::String(value.to_owned())
}

fn optional(value: Option<&str>) -> Value {
    value.map_or(Value::Null, text)
}

fn render_json(rows: &[Row], with_checkpoints: bool) -> String {
    let loams = rows
        .iter()
        .map(|row| {
            let mut fields = vec![
                ("key".to_owned(), text(&row.key)),
                ("status".to_owned(), text(&status_text(row.status.as_ref()))),
            ];
            if let Some(entry) = &row.entry {
                fields.push(("workspace".to_owned(), text(&entry.workspace)));
                fields.push(("wiki_root".to_owned(), text(&entry.wiki_root)));
                fields.push(("registered_at".to_owned(), text(&entry.registered_at)));
                fields.push(("last_seen_at".to_owned(), text(&entry.last_seen_at)));
                if with_checkpoints {
                    let items = row
                        .checkpoints
                        .iter()
                        .map(|checkpoint| {
                            Value::Object(vec![
                                ("path".to_owned(), text(&checkpoint.path)),
                                ("title".to_owned(), optional(checkpoint.title.as_deref())),
                                (
                                    "captured_at".to_owned(),
                                    optional(checkpoint.captured_at.as_deref()),
                                ),
                                ("scope".to_owned(), optional(checkpoint.scope.as_deref())),
                            ])
                        })
                        .collect();
                    fields.push(("checkpoints".to_owned(), Value::Array(items)));
                }
            }
            Value::Object(fields)
        })
        .collect();
    Value::Object(vec![
        ("schema".to_owned(), Value::Number(SCHEMA.to_owned())),
        ("loams".to_owned(), Value::Array(loams)),
    ])
    .to_json()
}

fn render_table(rows: &[Row], with_checkpoints: bool) -> String {
    let mut out = String::new();
    for row in rows {
        let status = status_text(row.status.as_ref());
        match &row.entry {
            Some(entry) => out.push_str(&format!(
                "{status:<12} {}  (last seen {})\n",
                entry.workspace, entry.last_seen_at
            )),
            None => out.push_str(&format!("{status:<12} {}\n", row.key)),
        }
        if with_checkpoints {
            for checkpoint in &row.checkpoints {
                out.push_str(&format!(
                    "    {}  {}\n",
                    checkpoint.captured_at.as_deref().unwrap_or("-"),
                    checkpoint.title.as_deref().unwrap_or("(untitled)")
                ));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    const NO_TEMP: &str = "/nonexistent-temp";

    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Scratch {
            static N: AtomicU64 = AtomicU64::new(0);
            let nanos = chrono::Local::now().timestamp_nanos_opt().unwrap_or(0);
            let dir = fs::canonicalize(std::env::temp_dir())
                .unwrap()
                .join(format!(
                    "loam-loams-test-{nanos}-{}",
                    N.fetch_add(1, Ordering::SeqCst)
                ));
            fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }
        fn workspace(&self, name: &str) -> PathBuf {
            let path = self.0.join(name);
            fs::create_dir_all(&path).unwrap();
            fs::write(path.join("index.md"), "# index\n").unwrap();
            path
        }
        fn config(&self) -> PathBuf {
            self.0.join("config")
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// Sets env vars for one test and restores them on drop.
    struct Env(Vec<(&'static str, Option<String>)>);
    impl Env {
        fn set(vars: &[(&'static str, Option<&str>)]) -> Env {
            let saved = vars
                .iter()
                .map(|(name, _)| (*name, std::env::var(name).ok()))
                .collect();
            for (name, value) in vars {
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
            Env(saved)
        }
    }
    impl Drop for Env {
        fn drop(&mut self) {
            for (name, value) in &self.0 {
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }
    }

    fn at(text: &str) -> DateTime<FixedOffset> {
        DateTime::parse_from_str(text, TIMESTAMP_FORMAT).unwrap()
    }

    fn registered(scratch: &Scratch) -> Env {
        Env::set(&[
            ("LOAM_CONFIG_DIR", Some(scratch.config().to_str().unwrap())),
            ("LOAM_REGISTRY", None),
        ])
    }

    fn entry_files(scratch: &Scratch) -> Vec<PathBuf> {
        let mut files: Vec<PathBuf> = fs::read_dir(scratch.config().join("loams"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        files.sort();
        files
    }

    #[test]
    fn create_then_unchanged_then_rewrite_on_a_later_day_preserving_registered_at() {
        let _lock = crate::env_lock();
        let scratch = Scratch::new();
        let _env = registered(&scratch);
        let workspace = scratch.workspace("one");
        let first = at("2026-10-30 09:00:00+02:00");

        assert_eq!(
            register(&workspace, first, Path::new(NO_TEMP)),
            Ok(Outcome::Created)
        );
        let files = entry_files(&scratch);
        assert_eq!(files.len(), 1);
        let mtime = fs::metadata(&files[0]).unwrap().modified().unwrap();

        let later_same_day = at("2026-10-30 18:30:00+02:00");
        assert_eq!(
            register(&workspace, later_same_day, Path::new(NO_TEMP)),
            Ok(Outcome::Unchanged)
        );
        assert_eq!(fs::metadata(&files[0]).unwrap().modified().unwrap(), mtime);

        let next_day = at("2026-10-31 08:00:00+02:00");
        assert_eq!(
            register(&workspace, next_day, Path::new(NO_TEMP)),
            Ok(Outcome::Rewritten)
        );
        let entry = Entry::parse(&fs::read_to_string(&files[0]).unwrap()).unwrap();
        assert_eq!(entry.registered_at, "2026-10-30 09:00:00+02:00");
        assert_eq!(entry.last_seen_at, "2026-10-31 08:00:00+02:00");
        assert_eq!(entry_files(&scratch).len(), 1);
    }

    #[test]
    fn a_changed_wiki_root_rewrites_and_keeps_registered_at() {
        let _lock = crate::env_lock();
        let scratch = Scratch::new();
        let _env = registered(&scratch);
        let workspace = scratch.workspace("moves");
        let now = at("2026-10-30 09:00:00+02:00");
        assert_eq!(
            register(&workspace, now, Path::new(NO_TEMP)),
            Ok(Outcome::Created)
        );

        fs::create_dir_all(workspace.join("wiki")).unwrap();
        fs::write(workspace.join("wiki").join("index.md"), "# wiki\n").unwrap();
        let same_day = at("2026-10-30 10:00:00+02:00");
        assert_eq!(
            register(&workspace, same_day, Path::new(NO_TEMP)),
            Ok(Outcome::Rewritten)
        );
        let entry = Entry::parse(&fs::read_to_string(&entry_files(&scratch)[0]).unwrap()).unwrap();
        assert!(entry.wiki_root.ends_with("wiki"), "{}", entry.wiki_root);
        assert_eq!(entry.registered_at, "2026-10-30 09:00:00+02:00");
    }

    #[test]
    fn an_unparseable_existing_entry_is_rewritten_with_fresh_registered_at() {
        let _lock = crate::env_lock();
        let scratch = Scratch::new();
        let _env = registered(&scratch);
        let workspace = scratch.workspace("broken");
        let now = at("2026-10-30 09:00:00+02:00");
        register(&workspace, now, Path::new(NO_TEMP)).unwrap();
        let file = entry_files(&scratch).remove(0);
        fs::write(&file, "{not json").unwrap();

        assert_eq!(
            register(&workspace, now, Path::new(NO_TEMP)),
            Ok(Outcome::Rewritten)
        );
        assert!(Entry::parse(&fs::read_to_string(&file).unwrap()).is_ok());
    }

    #[test]
    fn non_loams_are_skipped_and_nothing_is_written() {
        let _lock = crate::env_lock();
        let scratch = Scratch::new();
        let _env = registered(&scratch);
        let now = at("2026-10-30 09:00:00+02:00");

        let empty = scratch.0.join("empty");
        fs::create_dir_all(&empty).unwrap();
        let goals_only = scratch.0.join("goals-only");
        fs::create_dir_all(goals_only.join("goals")).unwrap();
        fs::write(goals_only.join("goals").join("g.md"), "# g\n").unwrap();
        let checkpoints_only = scratch.0.join("checkpoints-only");
        fs::create_dir_all(checkpoints_only.join("wiki").join("checkpoints")).unwrap();
        fs::write(
            checkpoints_only
                .join("wiki")
                .join("checkpoints")
                .join("c.md"),
            "# c\n",
        )
        .unwrap();

        for workspace in [&empty, &goals_only, &checkpoints_only] {
            assert_eq!(
                register(workspace, now, Path::new(NO_TEMP)),
                Ok(Outcome::Skipped(Skip::NotLoam)),
                "{}",
                workspace.display()
            );
        }
        assert!(!scratch.config().join("loams").exists());
    }

    #[test]
    fn a_workspace_under_the_temp_root_is_skipped() {
        let _lock = crate::env_lock();
        let scratch = Scratch::new();
        let _env = registered(&scratch);
        let workspace = scratch.workspace("tmp-loam");
        let now = at("2026-10-30 09:00:00+02:00");

        assert_eq!(
            register(&workspace, now, &scratch.0),
            Ok(Outcome::Skipped(Skip::Temp))
        );
        // A sibling path sharing only a string prefix is not under the root.
        let sibling = scratch.workspace("tmp-loam");
        let narrow_root = scratch.0.join("tmp");
        fs::create_dir_all(&narrow_root).unwrap();
        assert!(gate(&sibling, &narrow_root).is_ok());
        assert!(!scratch.config().join("loams").exists());
    }

    #[test]
    fn registry_off_relative_and_unresolved_config_roots_skip_silently() {
        let _lock = crate::env_lock();
        let scratch = Scratch::new();
        let workspace = scratch.workspace("gated");
        let now = at("2026-10-30 09:00:00+02:00");

        {
            let _env = Env::set(&[
                ("LOAM_CONFIG_DIR", Some(scratch.config().to_str().unwrap())),
                ("LOAM_REGISTRY", Some("off")),
            ]);
            assert_eq!(
                register(&workspace, now, Path::new(NO_TEMP)),
                Ok(Outcome::Skipped(Skip::Disabled))
            );
        }
        {
            let _env = Env::set(&[
                ("LOAM_CONFIG_DIR", Some("relative/config")),
                ("LOAM_REGISTRY", None),
            ]);
            assert_eq!(store_dir(), Err("config_root_relative"));
            assert_eq!(
                register(&workspace, now, Path::new(NO_TEMP)),
                Ok(Outcome::Skipped(Skip::ConfigRootRelative))
            );
        }
        {
            let _env = Env::set(&[
                ("LOAM_CONFIG_DIR", None),
                ("XDG_CONFIG_HOME", None),
                ("APPDATA", None),
                ("HOME", None),
                ("LOAM_REGISTRY", None),
            ]);
            assert_eq!(store_dir(), Err("config_root_unresolved"));
            assert_eq!(
                register(&workspace, now, Path::new(NO_TEMP)),
                Ok(Outcome::Skipped(Skip::ConfigRootUnresolved))
            );
        }
        assert!(!scratch.config().exists());
        assert!(!workspace.join("loams").exists());
    }

    #[test]
    fn store_dir_is_the_config_root_plus_loams() {
        let _lock = crate::env_lock();
        let scratch = Scratch::new();
        let _env = registered(&scratch);
        assert_eq!(store_dir(), Ok(scratch.config().join("loams")));
    }

    #[test]
    fn two_threads_registering_one_key_leave_one_valid_entry() {
        let _lock = crate::env_lock();
        let scratch = Scratch::new();
        let _env = registered(&scratch);
        let workspace = scratch.workspace("race");
        let now = at("2026-10-30 09:00:00+02:00");

        let handles: Vec<_> = (0..4)
            .map(|_| {
                let workspace = workspace.clone();
                std::thread::spawn(move || register(&workspace, now, Path::new(NO_TEMP)))
            })
            .collect();
        for handle in handles {
            assert!(handle.join().unwrap().is_ok());
        }
        let files = entry_files(&scratch);
        assert_eq!(files.len(), 1, "{files:?}");
        assert!(files[0].extension().is_some_and(|ext| ext == "json"));
        assert!(Entry::parse(&fs::read_to_string(&files[0]).unwrap()).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn the_store_is_private_to_the_user() {
        use std::os::unix::fs::PermissionsExt;
        let _lock = crate::env_lock();
        let scratch = Scratch::new();
        let _env = registered(&scratch);
        let workspace = scratch.workspace("private");
        register(
            &workspace,
            at("2026-10-30 09:00:00+02:00"),
            Path::new(NO_TEMP),
        )
        .unwrap();
        let dir_mode = fs::metadata(scratch.config().join("loams"))
            .unwrap()
            .permissions()
            .mode();
        let file_mode = fs::metadata(&entry_files(&scratch)[0])
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(dir_mode & 0o777, 0o700);
        assert_eq!(file_mode & 0o777, 0o600);
    }

    #[test]
    fn registering_never_writes_under_the_workspace() {
        let _lock = crate::env_lock();
        let scratch = Scratch::new();
        let _env = registered(&scratch);
        let workspace = scratch.workspace("readonly");
        let listing = |path: &Path| {
            let mut names: Vec<_> = fs::read_dir(path)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect();
            names.sort();
            names
        };
        let before = listing(&workspace);
        register(
            &workspace,
            at("2026-10-30 09:00:00+02:00"),
            Path::new(NO_TEMP),
        )
        .unwrap();
        assert_eq!(listing(&workspace), before);
    }

    #[test]
    fn the_key_is_stable_lowercase_sha256_hex_per_path() {
        let a = key(Path::new("/work/alpha"));
        let b = key(Path::new("/work/beta"));
        assert_eq!(a, key(Path::new("/work/alpha")));
        assert_ne!(a, b);
        assert_eq!(a.len(), 64);
        assert!(a
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)));
    }

    #[cfg(unix)]
    #[test]
    fn the_volume_is_the_workspace_device_id() {
        use std::os::unix::fs::MetadataExt;
        let scratch = Scratch::new();
        let expected = format!("dev:{}", fs::metadata(&scratch.0).unwrap().dev());
        assert_eq!(volume(&scratch.0).unwrap(), expected);
    }

    fn sample_entry() -> Entry {
        Entry {
            workspace: "/work/\"quoted\"\\path".to_owned(),
            wiki_root: "/work/wiki".to_owned(),
            volume: "dev:42".to_owned(),
            registered_at: "2026-10-30 09:00:00+02:00".to_owned(),
            last_seen_at: "2026-10-30 10:00:00+02:00".to_owned(),
        }
    }

    #[test]
    fn an_entry_round_trips_through_the_real_parser_and_declares_schema_1() {
        let entry = sample_entry();
        let text = entry.to_json();
        let value = crate::json::parse(&text).unwrap();
        assert_eq!(
            value.get("schema").map(|v| v.render()),
            Some("1".to_owned())
        );
        assert_eq!(Entry::parse(&text), Ok(entry));
    }

    #[test]
    fn parse_ignores_unknown_fields_and_rejects_missing_required_ones() {
        let mut text = sample_entry().to_json();
        text.insert_str(text.len() - 2, ",\"future\":{\"x\":[1,2]}");
        assert_eq!(Entry::parse(&text), Ok(sample_entry()));

        for field in [
            "workspace",
            "wiki_root",
            "volume",
            "registered_at",
            "last_seen_at",
        ] {
            let broken = sample_entry()
                .to_json()
                .replace(&format!("\"{field}\""), "\"renamed\"");
            assert!(Entry::parse(&broken).is_err(), "{field}");
        }
        assert!(Entry::parse("[]").is_err());
        assert!(Entry::parse("").is_err());
    }

    fn write_entry(scratch: &Scratch, entry: &Entry) -> PathBuf {
        let path = scratch.0.join("entry.json");
        fs::write(&path, entry.to_json()).unwrap();
        path
    }

    fn entry_for(workspace: &Path) -> Entry {
        Entry {
            workspace: workspace.display().to_string(),
            wiki_root: workspace.display().to_string(),
            volume: volume(workspace)
                .or_else(|_| volume(workspace.parent().unwrap()))
                .unwrap(),
            registered_at: "2026-10-30 09:00:00+02:00".to_owned(),
            last_seen_at: "2026-10-30 09:00:00+02:00".to_owned(),
        }
    }

    #[test]
    fn classify_live_when_the_workspace_and_wiki_marker_exist() {
        let scratch = Scratch::new();
        let workspace = scratch.workspace("live");
        let path = write_entry(&scratch, &entry_for(&workspace));
        assert_eq!(classify(&path, &Stat::default()), Status::Live);
    }

    #[test]
    fn classify_missing_when_the_wiki_marker_is_gone() {
        let scratch = Scratch::new();
        let workspace = scratch.workspace("unmarked");
        let path = write_entry(&scratch, &entry_for(&workspace));
        fs::remove_file(workspace.join("index.md")).unwrap();
        assert_eq!(classify(&path, &Stat::default()), Status::Missing);
    }

    #[test]
    fn classify_missing_when_the_workspace_is_gone_but_its_volume_is_present() {
        let scratch = Scratch::new();
        let workspace = scratch.workspace("deleted");
        let path = write_entry(&scratch, &entry_for(&workspace));
        fs::remove_dir_all(&workspace).unwrap();
        assert_eq!(classify(&path, &Stat::default()), Status::Missing);
    }

    #[test]
    fn classify_unreachable_when_the_nearest_ancestor_is_on_another_volume() {
        let scratch = Scratch::new();
        let workspace = scratch.workspace("unmounted");
        let path = write_entry(&scratch, &entry_for(&workspace));
        fs::remove_dir_all(&workspace).unwrap();
        let other = Stat {
            ancestor_volume: Some("dev:no-such-device".to_owned()),
        };
        assert_eq!(classify(&path, &other), Status::Unreachable);

        // The recorded volume itself differing from the ancestor's is the same
        // signal without any injection.
        let mut entry = entry_for(&workspace);
        entry.volume = "dev:no-such-device".to_owned();
        let path = write_entry(&scratch, &entry);
        assert_eq!(classify(&path, &Stat::default()), Status::Unreachable);
    }

    #[test]
    fn classify_error_for_io_errors_other_than_not_found() {
        let scratch = Scratch::new();
        let mut entry = entry_for(&scratch.workspace("x"));
        entry.workspace = "/bad\0path".to_owned();
        let path = write_entry(&scratch, &entry);
        assert!(matches!(
            classify(&path, &Stat::default()),
            Status::Error(_)
        ));
    }

    #[test]
    fn classify_invalid_when_the_entry_does_not_parse() {
        let scratch = Scratch::new();
        let path = scratch.0.join("garbage.json");
        fs::write(&path, "{\"schema\":1").unwrap();
        assert_eq!(classify(&path, &Stat::default()), Status::Invalid);
        fs::write(&path, "{\"schema\":1,\"workspace\":\"/x\"}").unwrap();
        assert_eq!(classify(&path, &Stat::default()), Status::Invalid);
    }
}
