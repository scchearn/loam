//! Machine-local loams store: one small JSON entry per loam workspace under
//! `<configRoot>/loams/`. Nothing here ever writes under a workspace, and
//! nothing here reads federation state.
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
    /// The store directory would live under the workspace.
    StoreInsideWorkspace,
}

/// A workspace that passed the gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Gated {
    /// The wiki root's parent when it is named `wiki`, else the wiki root.
    pub workspace: PathBuf,
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
    /// When set, replaces the measured volume of an existing workspace whose
    /// wiki marker is gone (`None` measures it for real).
    pub workspace_volume: Option<String>,
}

/// Entries larger than this are malformed.
const MAX_ENTRY_BYTES: u64 = 64 * 1024;

/// Read an entry file, refusing anything over `MAX_ENTRY_BYTES` or not UTF-8
/// with `InvalidData`.
fn read_entry(path: &Path) -> std::io::Result<String> {
    use std::io::Read;
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(MAX_ENTRY_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_ENTRY_BYTES {
        return Err(ErrorKind::InvalidData.into());
    }
    String::from_utf8(bytes).map_err(|_| ErrorKind::InvalidData.into())
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
    let wiki_root = crate::state::resolve_wiki_root(&canonical)
        .map(crate::enrollment::dos_compatible)
        .ok_or(Skip::NotLoam)?;
    // A folder inside a wiki (e.g. the code-graph hub `wiki/code/index.md`) is
    // part of that wiki, never a loam of its own.
    if wiki_root.file_name().is_none_or(|name| name != "wiki")
        && wiki_root.ancestors().skip(1).any(|ancestor| {
            ancestor.file_name().is_some_and(|name| name == "wiki")
                && crate::state::resolve_wiki_root(ancestor)
                    .map(crate::enrollment::dos_compatible)
                    .is_some_and(|root| root == ancestor)
        })
    {
        return Err(Skip::NotLoam);
    }
    // A loam's identity is its wiki: `/x` and `/x/wiki` are the same loam.
    let workspace = match wiki_root.parent() {
        Some(parent) if wiki_root.file_name().is_some_and(|name| name == "wiki") => {
            parent.to_path_buf()
        }
        _ => wiki_root.clone(),
    };
    Ok(Gated {
        workspace,
        wiki_root,
    })
}

/// Lowercase hex sha256 of the canonical wiki root. Folded to ASCII lowercase on
/// Windows only, where paths compare case-insensitively.
pub(crate) fn key(canonical: &Path) -> String {
    let text = canonical.to_string_lossy();
    #[cfg(windows)]
    let text: std::borrow::Cow<str> = std::borrow::Cow::Owned(text.to_ascii_lowercase());
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
    register_recorded(workspace, now, temp_root).map(|(outcome, _)| outcome)
}

/// `register`, also returning the normalized workspace it recorded (`None`
/// when skipped).
fn register_recorded(
    workspace: &Path,
    now: DateTime<FixedOffset>,
    temp_root: &Path,
) -> Result<(Outcome, Option<PathBuf>), String> {
    let gated = match gate(workspace, temp_root) {
        Ok(gated) => gated,
        Err(skip) => return Ok((Outcome::Skipped(skip), None)),
    };
    let recorded = Some(gated.workspace.clone());
    let skipped = |skip| Ok((Outcome::Skipped(skip), None));
    let dir = match store_dir() {
        Ok(dir) => dir,
        Err("config_root_relative") => return skipped(Skip::ConfigRootRelative),
        Err(_) => return skipped(Skip::ConfigRootUnresolved),
    };
    // The store must never be created under the workspace, even via an
    // absolute config root that points inside it.
    let anchor = dir
        .ancestors()
        .find_map(|candidate| canonical_path(candidate).ok());
    if anchor.is_some_and(|anchor| anchor.starts_with(&gated.workspace)) {
        return skipped(Skip::StoreInsideWorkspace);
    }
    let volume = volume(&gated.workspace).map_err(|error| format!("volume: {}", error.kind()))?;
    let mut entry = Entry {
        workspace: gated.workspace.display().to_string(),
        wiki_root: gated.wiki_root.display().to_string(),
        volume,
        registered_at: now.format(TIMESTAMP_FORMAT).to_string(),
        last_seen_at: now.format(TIMESTAMP_FORMAT).to_string(),
    };
    let path = dir.join(format!("{}.json", key(&gated.wiki_root)));
    let outcome = match read_entry(&path) {
        Ok(text) => match Entry::parse(&text) {
            Ok(existing) => {
                let same = existing.workspace == entry.workspace
                    && existing.wiki_root == entry.wiki_root
                    && existing.volume == entry.volume;
                if same && existing.seen_on(&now) {
                    return Ok((Outcome::Unchanged, recorded));
                }
                entry.registered_at = existing.registered_at;
                Outcome::Rewritten
            }
            Err(_) => Outcome::Rewritten,
        },
        Err(error) if error.kind() == ErrorKind::NotFound => Outcome::Created,
        Err(error) if error.kind() == ErrorKind::InvalidData => Outcome::Rewritten,
        Err(error) => return Err(format!("read: {}", error.kind())),
    };
    write_atomic(&dir, &path, &entry.to_json())
        .map_err(|error| format!("write: {}", error.kind()))?;
    Ok((outcome, recorded))
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
/// briefly (5 attempts, sleeping 25 ms between them, so at most 100 ms of
/// sleep) inside the caller's deadline.
#[cfg(windows)]
fn rename_into_place(from: &Path, to: &Path) -> std::io::Result<()> {
    let mut attempt = 0;
    loop {
        match fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(_) if attempt < 4 => {
                attempt += 1;
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            Err(error) => return Err(error),
        }
    }
}

/// Classify one entry file against the live filesystem.
pub(crate) fn classify(entry_path: &Path, stat: &Stat) -> Status {
    let text = match read_entry(entry_path) {
        Ok(text) => text,
        Err(error) if error.kind() == ErrorKind::InvalidData => return Status::Invalid,
        Err(error) => return Status::Error(error.kind()),
    };
    let Ok(entry) = Entry::parse(&text) else {
        return Status::Invalid;
    };
    let workspace = Path::new(&entry.workspace);
    match fs::metadata(workspace) {
        Ok(_) => {
            if crate::state::resolve_wiki_root(workspace).is_some() {
                return Status::Live;
            }
            // Only a marker that is absent everywhere counts as gone; an
            // unreadable one is an error, not a deletion.
            for dir in [workspace.join("wiki"), workspace.to_path_buf()] {
                for name in ["SCHEMA.md", "index.md", "log.md"] {
                    if let Err(error) = fs::metadata(dir.join(name)) {
                        if error.kind() != ErrorKind::NotFound {
                            return Status::Error(error.kind());
                        }
                    }
                }
            }
            // A mount point left behind by an unmounted volume is empty.
            let measured = match &stat.workspace_volume {
                Some(volume) => Ok(volume.clone()),
                None => volume(workspace),
            };
            match measured {
                Ok(current) if current == entry.volume => Status::Missing,
                Ok(_) => Status::Unreachable,
                Err(error) => Status::Error(error.kind()),
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
        Some("add") => add(args),
        Some("import") => import(args),
        Some("prune") => prune(args),
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

/// Worker messages. `Parsed` goes out as soon as the entry parses so a
/// worker that later hangs still leaves its workspace for the timeout row.
enum Message {
    Parsed(String, Entry),
    Done(Report),
}

/// A row after collection; `status: None` means the worker never answered.
struct Row {
    key: String,
    status: Option<Status>,
    /// Overrides the status text: `error: <detail>` for failures that are not
    /// an `io::ErrorKind` (a thread that could not be spawned, an unreadable
    /// directory item).
    error: Option<String>,
    entry: Option<Entry>,
    checkpoints: Vec<Checkpoint>,
}

impl Row {
    fn bare(key: String, status: Option<Status>) -> Row {
        Row {
            key,
            status,
            error: None,
            entry: None,
            checkpoints: Vec::new(),
        }
    }
}

fn status_label(status: &Status) -> String {
    match status {
        Status::Live => "live".to_owned(),
        Status::Missing => "missing".to_owned(),
        Status::Unreachable => "unreachable".to_owned(),
        Status::Error(kind) => format!("error: {kind}"),
        Status::Invalid => "invalid".to_owned(),
    }
}

fn status_text(row: &Row) -> String {
    if let Some(detail) = &row.error {
        return format!("error: {detail}");
    }
    row.status
        .as_ref()
        .map_or_else(|| "timeout".to_owned(), status_label)
}

/// `captured_at` as written by checkpoints: `YYYY-MM-DD HH:MM ±HH:MM` is the
/// norm. Seconds, RFC 3339 and the compact `±HHMM` offset are tolerated.
/// A stamp with no offset is not guessed at (it would silently be read as the
/// wrong instant); it counts as unparseable.
fn parse_captured(text: &str) -> Option<DateTime<FixedOffset>> {
    let text = text.trim();
    for format in [
        "%Y-%m-%d %H:%M %:z",
        "%Y-%m-%d %H:%M:%S %:z",
        "%Y-%m-%d %H:%M:%S%:z",
        "%Y-%m-%d %H:%M%:z",
        "%Y-%m-%d %H:%M %z",
        "%Y-%m-%d %H:%M:%S %z",
        "%Y-%m-%d %H:%M:%S%z",
        "%Y-%m-%d %H:%M%z",
    ] {
        if let Ok(parsed) = DateTime::parse_from_str(text, format) {
            return Some(parsed);
        }
    }
    DateTime::parse_from_rfc3339(text).ok()
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

/// Read every checkpoint file, reporting the first I/O error. Unlike
/// `state::read_checkpoints` this does not stat each file's type (`is_file`
/// would skip a FIFO, hiding exactly the stalled read the deadline exists
/// for); it only skips directories that happen to be named like checkpoints.
fn read_checkpoints(wiki_root: &Path) -> Result<Vec<Checkpoint>, ErrorKind> {
    let entries = match fs::read_dir(wiki_root.join("checkpoints")) {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.kind()),
    };
    let mut checkpoints = Vec::new();
    for item in entries {
        let item = item.map_err(|error| error.kind())?;
        let path = item.path();
        let named = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("checkpoint-") && name.ends_with(".md"));
        if !named || item.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let content = fs::read_to_string(&path).map_err(|error| error.kind())?;
        checkpoints.push(crate::state::parse_checkpoint_content(&path, &content));
    }
    Ok(checkpoints)
}

fn inspect(
    path: &Path,
    key: String,
    with_checkpoints: bool,
    sender: &mpsc::Sender<Message>,
) -> Report {
    let entry = read_entry(path)
        .ok()
        .and_then(|text| Entry::parse(&text).ok());
    if let Some(entry) = &entry {
        let _ = sender.send(Message::Parsed(key.clone(), entry.clone()));
    }
    let mut status = classify(path, &Stat::default());
    let mut checkpoints = Vec::new();
    if with_checkpoints && status == Status::Live {
        if let Some(entry) = &entry {
            match read_checkpoints(Path::new(&entry.wiki_root)) {
                Ok(found) => checkpoints = found,
                Err(kind) => status = Status::Error(kind),
            }
        }
    }
    Report {
        key,
        status: Some(status),
        entry,
        checkpoints,
    }
}

/// One store file to inspect.
struct Listed {
    key: String,
    path: PathBuf,
    /// The file name is not UTF-8: reported `invalid`, never skipped.
    bad_name: bool,
}

/// The store listing: entry files plus rows for directory items that could
/// not be listed at all. One bad item never fails the whole listing.
struct Listing {
    files: Vec<Listed>,
    failed: Vec<Row>,
}

fn entry_files(dir: &Path) -> std::io::Result<Listing> {
    let mut files = Vec::new();
    let mut failed = Vec::new();
    for item in fs::read_dir(dir)? {
        let item = match item {
            Ok(item) => item,
            Err(error) => {
                let mut row = Row::bare("?".to_owned(), None);
                row.error = Some(error.kind().to_string());
                failed.push(row);
                continue;
            }
        };
        let path = item.path();
        let Some(raw) = path.file_name() else {
            continue;
        };
        let (name, bad_name) = match raw.to_str() {
            Some(name) => (name.to_owned(), false),
            None => (raw.to_string_lossy().into_owned(), true),
        };
        if name.contains(".tmp-") {
            continue;
        }
        if let Some(key) = name.strip_suffix(".json") {
            files.push(Listed {
                key: key.to_owned(),
                path: path.clone(),
                bad_name,
            });
        }
    }
    files.sort_by(|left, right| left.key.cmp(&right.key));
    Ok(Listing { files, failed })
}

/// Spawn one detached worker per entry and collect under a single deadline.
/// Workers are never joined: a hung read must not hang the command.
fn collect_rows(listing: Listing, with_checkpoints: bool) -> Vec<Row> {
    let (sender, receiver) = mpsc::channel();
    let mut rows = listing.failed;
    let mut pending: Vec<String> = Vec::new();
    let deadline = Instant::now() + FANOUT_DEADLINE;
    for listed in listing.files {
        if listed.bad_name {
            rows.push(Row::bare(listed.key, Some(Status::Invalid)));
            continue;
        }
        let worker_sender = sender.clone();
        let Listed { key, path, .. } = listed;
        let worker_key = key.clone();
        let spawned = std::thread::Builder::new().spawn(move || {
            let report = inspect(&path, worker_key, with_checkpoints, &worker_sender);
            let _ = worker_sender.send(Message::Done(report));
        });
        match spawned {
            Ok(_) => pending.push(key),
            Err(_) => {
                let mut row = Row::bare(key, None);
                row.error = Some("spawn".to_owned());
                rows.push(row);
            }
        }
    }
    drop(sender);
    let mut parsed: Vec<(String, Entry)> = Vec::new();
    let mut done = 0;
    while done < pending.len() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match receiver.recv_timeout(remaining) {
            Ok(Message::Parsed(key, entry)) => parsed.push((key, entry)),
            Ok(Message::Done(report)) => {
                done += 1;
                rows.push(Row {
                    key: report.key,
                    status: report.status,
                    error: None,
                    entry: report.entry,
                    checkpoints: report.checkpoints,
                });
            }
            Err(_) => break,
        }
    }
    for key in pending {
        if !rows.iter().any(|row| row.key == key) {
            let mut row = Row::bare(key.clone(), None);
            row.entry = parsed
                .iter()
                .find(|(parsed_key, _)| *parsed_key == key)
                .map(|(_, entry)| entry.clone());
            rows.push(row);
        }
    }
    rows
}

fn skip_text(skip: &Skip) -> &'static str {
    match skip {
        Skip::Disabled => "disabled",
        Skip::Temp => "temp",
        Skip::NotLoam => "not_loam",
        Skip::ConfigRootUnresolved => "config_root_unresolved",
        Skip::ConfigRootRelative => "config_root_relative",
        Skip::StoreInsideWorkspace => "store_inside_workspace",
    }
}

/// `register` with the real clock and temp root.
fn register_now(workspace: &Path) -> Result<(Outcome, Option<PathBuf>), String> {
    register_recorded(
        workspace,
        chrono::Local::now().fixed_offset(),
        &std::env::temp_dir(),
    )
}

fn add(mut args: impl Iterator<Item = String>) -> i32 {
    let (Some(path), None) = (args.next(), args.next()) else {
        usage();
        return 1;
    };
    match register_now(Path::new(&path)) {
        Ok((outcome @ (Outcome::Created | Outcome::Rewritten | Outcome::Unchanged), recorded)) => {
            let label = match outcome {
                Outcome::Created => "created",
                Outcome::Rewritten => "rewritten",
                _ => "unchanged",
            };
            let shown = recorded.map_or(path, |workspace| workspace.display().to_string());
            println!("{label}: {shown}");
        }
        Ok((Outcome::Skipped(skip), _)) => {
            eprintln!("loam loams: skipped: {}", skip_text(&skip));
            return 1;
        }
        Err(reason) => {
            eprintln!("loam loams: {reason}");
            return 1;
        }
    }
    0
}

fn import(mut args: impl Iterator<Item = String>) -> i32 {
    let mut ledger = None;
    while let Some(arg) = args.next() {
        match (arg.as_str(), args.next()) {
            ("--ledger", Some(path)) => ledger = Some(PathBuf::from(path)),
            _ => {
                usage();
                return 1;
            }
        }
    }
    if let Err(reason) = store_dir() {
        eprintln!("loam loams: {reason}");
        return 1;
    }
    let ledger = ledger.unwrap_or_else(|| {
        crate::harness::HookPaths::from_env()
            .global_root
            .join(crate::hooks::DATABASE_NAME)
    });
    let workspaces = match crate::hooks::ledger_workspaces(&ledger) {
        Ok(Some(workspaces)) => workspaces,
        Ok(None) => {
            println!("no hook ledger");
            return 0;
        }
        Err(reason) => {
            eprintln!("loam loams: cannot read {}: {reason}", ledger.display());
            return 1;
        }
    };
    let (mut recorded, mut unchanged, mut skipped, mut failed) = (0, 0, 0, 0);
    for workspace in &workspaces {
        let Some(workspace) = workspace else {
            skipped += 1;
            println!("skipped: non-utf8 workspace");
            continue;
        };
        match register_now(Path::new(workspace)) {
            Ok((Outcome::Created | Outcome::Rewritten, _)) => {
                recorded += 1;
                println!("recorded: {workspace}");
            }
            Ok((Outcome::Unchanged, _)) => {
                unchanged += 1;
                println!("unchanged: {workspace}");
            }
            Ok((Outcome::Skipped(skip), _)) => {
                skipped += 1;
                println!("skipped: {}: {workspace}", skip_text(&skip));
            }
            Err(reason) => {
                failed += 1;
                println!("failed: {reason}: {workspace}");
            }
        }
    }
    println!("{recorded} recorded, {unchanged} unchanged, {skipped} skipped, {failed} failed");
    i32::from(failed > 0)
}

/// A temp file this old is an abandoned write, not one in flight.
const STALE_TEMP: Duration = Duration::from_secs(3600);

/// Delete `target` only if it sits directly inside `store` and is a regular
/// file or a symlink. A symlink is removed itself, never what it points at.
fn remove_inside(store: &Path, target: &Path) -> Result<(), String> {
    let parent = target.parent().map(fs::canonicalize);
    if !matches!(parent, Some(Ok(parent)) if parent == store) {
        return Err("refused: outside the store".to_owned());
    }
    let kind = fs::symlink_metadata(target)
        .map_err(|error| error.to_string())?
        .file_type();
    if !(kind.is_file() || kind.is_symlink()) {
        return Err("refused: not a file".to_owned());
    }
    fs::remove_file(target).map_err(|error| error.to_string())
}

/// An abandoned temp file: a file or symlink (never a directory) older than
/// `STALE_TEMP`. Judged on the link itself, not its target.
fn is_stale_temp(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|meta| {
        (meta.is_file() || meta.file_type().is_symlink())
            && meta
                .modified()
                .ok()
                .and_then(|modified| modified.elapsed().ok())
                .is_some_and(|age| age > STALE_TEMP)
    })
}

/// Re-evaluate a prune candidate right before deleting it; anything whose
/// status changed since it was listed is kept.
fn still_prunable(path: &Path, why: &str) -> bool {
    let name_is_utf8 = path.file_name().is_some_and(|name| name.to_str().is_some());
    let status = || {
        if name_is_utf8 {
            classify(path, &Stat::default())
        } else {
            Status::Invalid
        }
    };
    match why {
        "missing" => status() == Status::Missing,
        "invalid" => status() == Status::Invalid,
        "stale temp" => is_stale_temp(path),
        _ => false,
    }
}

fn prune(args: impl Iterator<Item = String>) -> i32 {
    let mut yes = false;
    for arg in args {
        if arg == "--yes" {
            yes = true;
        } else {
            usage();
            return 1;
        }
    }
    let dir = match store_dir() {
        Ok(dir) => dir,
        Err(reason) => {
            eprintln!("loam loams: {reason}");
            return 1;
        }
    };
    let listing = match entry_files(&dir) {
        Ok(listing) => listing,
        Err(error) if error.kind() == ErrorKind::NotFound => return 0,
        Err(error) => {
            eprintln!("loam loams: cannot read {}: {error}", dir.display());
            return 1;
        }
    };
    let mut remove: Vec<(PathBuf, &'static str)> = Vec::new();
    let mut keep: Vec<(String, String)> = Vec::new();
    // A name that is not UTF-8 is invalid by definition and never read.
    let (bad, good): (Vec<Listed>, Vec<Listed>) = listing
        .files
        .into_iter()
        .partition(|listed| listed.bad_name);
    for listed in bad {
        remove.push((listed.path, "invalid"));
    }
    // Same fan-out and deadline as `list`; a worker that never answers is a
    // timeout row and is kept.
    let rows = collect_rows(
        Listing {
            files: good,
            failed: listing.failed,
        },
        false,
    );
    for row in rows {
        match (&row.status, &row.error) {
            (Some(Status::Missing), None) => {
                remove.push((dir.join(format!("{}.json", row.key)), "missing"));
            }
            (Some(Status::Invalid), None) => {
                remove.push((dir.join(format!("{}.json", row.key)), "invalid"));
            }
            _ => keep.push((row.key.clone(), status_text(&row))),
        }
    }
    if let Ok(items) = fs::read_dir(&dir) {
        for item in items.filter_map(Result::ok) {
            let path = item.path();
            let is_temp = path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().contains(".tmp-"));
            if !is_temp {
                continue;
            }
            if is_stale_temp(&path) {
                remove.push((path, "stale temp"));
            } else if fs::symlink_metadata(&path).is_ok_and(|meta| meta.is_dir()) {
                let name = path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default();
                keep.push((name, "error: not a file".to_owned()));
            }
        }
    }
    remove.sort();
    keep.sort();
    let name_of = |path: &Path| {
        path.file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    if !yes {
        println!("would remove ({}):", remove.len());
        for (path, why) in &remove {
            println!("  {}  [{why}]", name_of(path));
        }
        println!("keep ({}):", keep.len());
        for (key, status) in &keep {
            println!("  {key}.json  [{status}]");
        }
        println!("dry run: pass --yes to delete");
        return 0;
    }
    let store = match fs::canonicalize(&dir) {
        Ok(store) => store,
        Err(error) => {
            eprintln!("loam loams: cannot read {}: {error}", dir.display());
            return 1;
        }
    };
    let mut failed = false;
    for (path, why) in &remove {
        if !still_prunable(path, why) {
            println!("kept: {}  [changed since listing]", name_of(path));
            continue;
        }
        match remove_inside(&store, path) {
            Ok(()) => println!("removed: {}  [{why}]", name_of(path)),
            Err(reason) => {
                failed = true;
                eprintln!("loam loams: could not remove {}: {reason}", name_of(path));
            }
        }
    }
    i32::from(failed)
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
        Err(error) if error.kind() == ErrorKind::NotFound => Listing {
            files: Vec::new(),
            failed: Vec::new(),
        },
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
                ("status".to_owned(), text(&status_text(row))),
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
        let status = status_text(row);
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

    #[test]
    fn a_folder_inside_a_wiki_is_not_a_loam() {
        let root = std::env::temp_dir().join(format!("loams-subwiki-{}", std::process::id()));
        let code = root.join("x").join("wiki").join("code");
        fs::create_dir_all(&code).unwrap();
        fs::write(root.join("x").join("wiki").join("index.md"), "# w\n").unwrap();
        fs::write(code.join("index.md"), "# c\n").unwrap();
        let temp = Path::new("/nonexistent-temp");
        assert!(matches!(gate(&code, temp), Err(Skip::NotLoam)));
        let gated = gate(&root.join("x"), temp).expect("the parent wiki is a loam");
        assert_eq!(
            gated.wiki_root,
            canonical_path(&root.join("x").join("wiki")).unwrap()
        );
        let _ = fs::remove_dir_all(&root);
    }
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
    fn a_new_wiki_root_is_a_new_identity() {
        let _lock = crate::env_lock();
        let scratch = Scratch::new();
        let _env = registered(&scratch);
        let workspace = scratch.workspace("moves");
        let now = at("2026-10-30 09:00:00+02:00");
        assert_eq!(
            register(&workspace, now, Path::new(NO_TEMP)),
            Ok(Outcome::Created)
        );

        // The wiki moves to `<workspace>/wiki`: identity is the wiki, so this
        // is a second loam sharing the workspace path.
        fs::create_dir_all(workspace.join("wiki")).unwrap();
        fs::write(workspace.join("wiki").join("index.md"), "# wiki\n").unwrap();
        assert_eq!(
            register(&workspace, now, Path::new(NO_TEMP)),
            Ok(Outcome::Created)
        );
        assert_eq!(entry_files(&scratch).len(), 2);
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
        // The relative root must not be created relative to the cwd or the
        // workspace either.
        assert!(!std::env::current_dir().unwrap().join("relative").exists());
        assert!(!workspace.join("relative").exists());
    }

    #[test]
    fn a_store_inside_the_workspace_is_skipped_and_nothing_is_created() {
        let _lock = crate::env_lock();
        let scratch = Scratch::new();
        let workspace = scratch.workspace("inside");
        let config = workspace.join(".cfg");
        let _env = Env::set(&[
            ("LOAM_CONFIG_DIR", Some(config.to_str().unwrap())),
            ("LOAM_REGISTRY", None),
        ]);
        let before = fs::read_dir(&workspace).unwrap().count();
        assert_eq!(
            register(
                &workspace,
                at("2026-10-30 09:00:00+02:00"),
                Path::new(NO_TEMP)
            ),
            Ok(Outcome::Skipped(Skip::StoreInsideWorkspace))
        );
        assert!(!config.exists());
        assert_eq!(fs::read_dir(&workspace).unwrap().count(), before);
    }

    #[test]
    fn an_oversized_entry_is_rewritten_by_register() {
        let _lock = crate::env_lock();
        let scratch = Scratch::new();
        let _env = registered(&scratch);
        let workspace = scratch.workspace("big");
        let now = at("2026-10-30 09:00:00+02:00");
        register(&workspace, now, Path::new(NO_TEMP)).unwrap();
        let file = entry_files(&scratch).remove(0);
        fs::write(&file, vec![b' '; 70 * 1024]).unwrap();
        assert_eq!(
            register(&workspace, now, Path::new(NO_TEMP)),
            Ok(Outcome::Rewritten)
        );
        assert!(Entry::parse(&fs::read_to_string(&file).unwrap()).is_ok());
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
            let result = handle.join().unwrap();
            // Windows may refuse a rename onto a file another thread holds.
            let tolerated = cfg!(windows)
                && result
                    .as_ref()
                    .is_err_and(|message| message.starts_with("write:"));
            assert!(result.is_ok() || tolerated, "{result:?}");
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
            ..Stat::default()
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
    fn classify_unreachable_when_the_workspace_exists_on_another_volume_without_a_marker() {
        let scratch = Scratch::new();
        let workspace = scratch.workspace("mount-point");
        let path = write_entry(&scratch, &entry_for(&workspace));
        fs::remove_file(workspace.join("index.md")).unwrap();
        let other = Stat {
            workspace_volume: Some("dev:no-such-device".to_owned()),
            ..Stat::default()
        };
        assert_eq!(classify(&path, &other), Status::Unreachable);
    }

    #[cfg(unix)]
    #[test]
    fn classify_error_when_the_marker_cannot_be_read() {
        use std::os::unix::fs::PermissionsExt;
        let scratch = Scratch::new();
        let workspace = scratch.workspace("locked");
        let path = write_entry(&scratch, &entry_for(&workspace));
        fs::remove_file(workspace.join("index.md")).unwrap();
        let wiki = workspace.join("wiki");
        fs::create_dir_all(&wiki).unwrap();
        fs::write(wiki.join("index.md"), "# wiki\n").unwrap();
        fs::set_permissions(&wiki, fs::Permissions::from_mode(0o000)).unwrap();
        let blocked = fs::metadata(wiki.join("index.md")).is_err();
        let status = classify(&path, &Stat::default());
        fs::set_permissions(&wiki, fs::Permissions::from_mode(0o755)).unwrap();
        if blocked {
            assert_eq!(status, Status::Error(ErrorKind::PermissionDenied));
        }
    }

    #[test]
    fn classify_invalid_when_the_entry_is_over_the_size_cap() {
        let scratch = Scratch::new();
        let path = scratch.0.join("huge.json");
        fs::write(&path, vec![b' '; 70 * 1024]).unwrap();
        assert_eq!(classify(&path, &Stat::default()), Status::Invalid);
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

    fn cp(path: &str, captured: Option<&str>) -> Checkpoint {
        Checkpoint {
            path: path.to_owned(),
            title: None,
            captured_at: captured.map(str::to_owned),
            scope: None,
        }
    }

    #[test]
    fn parse_captured_accepts_offsets_and_rejects_offsetless_and_garbage() {
        let colon = parse_captured("2026-02-01 10:00 +02:00").unwrap();
        let compact = parse_captured("2026-02-01 10:00 +0200").unwrap();
        assert_eq!(colon, compact);
        assert_eq!(colon.offset().local_minus_utc(), 7200);
        assert!(parse_captured("2026-02-01 10:00").is_none());
        assert!(parse_captured("2026-02-01 10:00:00").is_none());
        assert!(parse_captured("2026-02-01").is_none());
        assert!(parse_captured("garbage").is_none());
    }

    #[test]
    fn a_workspace_and_its_wiki_dir_share_one_entry_with_the_parent_as_workspace() {
        let _lock = crate::env_lock();
        let scratch = Scratch::new();
        let _env = registered(&scratch);
        let workspace = scratch.0.join("x");
        fs::create_dir_all(workspace.join("wiki")).unwrap();
        fs::write(workspace.join("wiki").join("index.md"), "# wiki\n").unwrap();
        let now = at("2026-10-30 09:00:00+02:00");
        assert_eq!(
            register(&workspace, now, Path::new(NO_TEMP)),
            Ok(Outcome::Created)
        );
        assert_eq!(
            register(&workspace.join("wiki"), now, Path::new(NO_TEMP)),
            Ok(Outcome::Unchanged)
        );
        let files = entry_files(&scratch);
        assert_eq!(files.len(), 1, "{files:?}");
        let entry = Entry::parse(&fs::read_to_string(&files[0]).unwrap()).unwrap();
        assert_eq!(
            entry.workspace,
            fs::canonicalize(&workspace).unwrap().display().to_string()
        );
        assert!(entry.wiki_root.ends_with("wiki"), "{}", entry.wiki_root);
    }

    #[test]
    fn a_wiki_at_the_root_records_that_root_as_the_workspace() {
        let _lock = crate::env_lock();
        let scratch = Scratch::new();
        let _env = registered(&scratch);
        let workspace = scratch.workspace("y");
        register(
            &workspace,
            at("2026-10-30 09:00:00+02:00"),
            Path::new(NO_TEMP),
        )
        .unwrap();
        let entry = Entry::parse(&fs::read_to_string(&entry_files(&scratch)[0]).unwrap()).unwrap();
        assert_eq!(
            entry.workspace,
            fs::canonicalize(&workspace).unwrap().display().to_string()
        );
    }

    #[test]
    fn a_store_inside_the_wiki_dir_is_skipped_for_the_normalized_workspace() {
        let _lock = crate::env_lock();
        let scratch = Scratch::new();
        let workspace = scratch.0.join("x");
        fs::create_dir_all(workspace.join("wiki")).unwrap();
        fs::write(workspace.join("wiki").join("index.md"), "# wiki\n").unwrap();
        let config = workspace.join(".cfg");
        let _env = Env::set(&[
            ("LOAM_CONFIG_DIR", Some(config.to_str().unwrap())),
            ("LOAM_REGISTRY", None),
        ]);
        assert_eq!(
            register(
                &workspace.join("wiki"),
                at("2026-10-30 09:00:00+02:00"),
                Path::new(NO_TEMP)
            ),
            Ok(Outcome::Skipped(Skip::StoreInsideWorkspace))
        );
        assert!(!config.exists());
    }

    #[test]
    fn prune_rechecks_a_candidate_right_before_deleting_it() {
        let _lock = crate::env_lock();
        let scratch = Scratch::new();
        let _env = registered(&scratch);
        let workspace = scratch.workspace("flip");
        register(
            &workspace,
            at("2026-10-30 09:00:00+02:00"),
            Path::new(NO_TEMP),
        )
        .unwrap();
        let file = entry_files(&scratch).remove(0);
        // Classified missing a moment ago; the marker is back now.
        assert!(!still_prunable(&file, "missing"));
        assert!(!still_prunable(&file, "invalid"));
        fs::write(&file, "{not json").unwrap();
        assert!(still_prunable(&file, "invalid"));
        assert!(!still_prunable(&file, "stale temp"));
    }

    #[test]
    fn sort_checkpoints_puts_unparseable_after_parsed_by_filename_descending() {
        let mut items = vec![
            cp("a-offsetless", Some("2026-03-01 10:00")),
            cp("b-old", Some("2026-01-01 10:00 +02:00")),
            cp("c-garbage", Some("garbage")),
            cp("d-new", Some("2026-02-01 10:00 +0200")),
            cp("e-none", None),
        ];
        sort_checkpoints(&mut items);
        let order: Vec<_> = items.iter().map(|c| c.path.as_str()).collect();
        assert_eq!(
            order,
            ["d-new", "b-old", "e-none", "c-garbage", "a-offsetless"]
        );
    }
}
