use chrono::{SecondsFormat, TimeZone, Utc};
use rusqlite::{params, Connection, OpenFlags, TransactionBehavior};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

const DATABASE_NAME: &str = "loam.sqlite3";
const SCHEMA_VERSION: i64 = 1;
const RETENTION: i64 = 10_000;
const SUPPORTED_TARGETS: [&str; 5] = [
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
    "x86_64-pc-windows-msvc",
    "x86_64-unknown-linux-musl",
    "aarch64-unknown-linux-musl",
];

pub fn run(args: impl Iterator<Item = String>) -> i32 {
    match execute(args.collect()) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("loam hooks: {error}");
            1
        }
    }
}

fn execute(mut args: Vec<String>) -> Result<(), String> {
    if args.is_empty() {
        return Err(usage());
    }
    match args.remove(0).as_str() {
        "begin" => begin(parse_begin(args)?),
        "finish" => finish(parse_finish(args)?),
        "list" => list(parse_list(args)?),
        _ => Err(usage()),
    }
}

struct BeginArgs {
    root: PathBuf,
    harness: String,
    hook: String,
    workspace: String,
    plugin_version: String,
    session_id: Option<String>,
}

fn parse_begin(args: Vec<String>) -> Result<BeginArgs, String> {
    let mut args = args.into_iter();
    let root = absolute_path(args.next(), "global root")?;
    let mut harness = None;
    let mut hook = None;
    let mut workspace = None;
    let mut plugin_version = None;
    let mut session_id = None;
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--harness" => take_value(&mut harness, &flag, &mut args)?,
            "--hook" => take_value(&mut hook, &flag, &mut args)?,
            "--workspace" => take_value(&mut workspace, &flag, &mut args)?,
            "--plugin-version" => take_value(&mut plugin_version, &flag, &mut args)?,
            "--session-id" => take_value(&mut session_id, &flag, &mut args)?,
            _ => return Err(format!("unknown option {flag}")),
        }
    }
    let harness = valid_identifier(harness, "harness")?;
    let hook = valid_identifier(hook, "hook")?;
    let workspace = absolute_path(workspace, "workspace")?
        .into_os_string()
        .into_string()
        .map_err(|_| "workspace must be valid UTF-8".to_owned())?;
    let plugin_version = plugin_version.ok_or_else(|| "missing --plugin-version".to_owned())?;
    if !valid_semver(&plugin_version) {
        return Err("plugin version must be MAJOR.MINOR.PATCH".to_owned());
    }
    let session_id = optional_session_id(session_id)?;
    Ok(BeginArgs {
        root,
        harness,
        hook,
        workspace,
        plugin_version,
        session_id,
    })
}

struct FinishArgs {
    root: PathBuf,
    id: i64,
    status: String,
    detail: Option<String>,
}

fn parse_finish(args: Vec<String>) -> Result<FinishArgs, String> {
    let mut args = args.into_iter();
    let root = absolute_path(args.next(), "global root")?;
    let mut id = None;
    let mut status = None;
    let mut detail = None;
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--id" => take_value(&mut id, &flag, &mut args)?,
            "--status" => take_value(&mut status, &flag, &mut args)?,
            "--detail" => take_value(&mut detail, &flag, &mut args)?,
            _ => return Err(format!("unknown option {flag}")),
        }
    }
    let id = id
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| "id must be a positive integer".to_owned())?;
    let status = status.ok_or_else(|| "missing --status".to_owned())?;
    match status.as_str() {
        "failed" if detail.as_ref().is_some_and(|value| valid_detail(value)) => {}
        "succeeded" if detail.is_none() => {}
        "failed" => return Err("failed status requires detail of 1..1024 characters".to_owned()),
        "succeeded" => return Err("succeeded status does not accept detail".to_owned()),
        _ => return Err("status must be succeeded or failed".to_owned()),
    }
    Ok(FinishArgs {
        root,
        id,
        status,
        detail,
    })
}

struct ListArgs {
    root: PathBuf,
    harness: Option<String>,
    hook: Option<String>,
    status: Option<String>,
    session_id: Option<String>,
    limit: usize,
}

fn parse_list(mut args: Vec<String>) -> Result<ListArgs, String> {
    let root = if args.first().is_some_and(|value| !value.starts_with("--")) {
        absolute_path(Some(args.remove(0)), "global root")?
    } else {
        installed_global_root()?
    };
    let mut args = args.into_iter();
    let mut harness = None;
    let mut hook = None;
    let mut status = None;
    let mut session_id = None;
    let mut limit = None;
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--harness" => take_value(&mut harness, &flag, &mut args)?,
            "--hook" => take_value(&mut hook, &flag, &mut args)?,
            "--status" => take_value(&mut status, &flag, &mut args)?,
            "--session-id" => take_value(&mut session_id, &flag, &mut args)?,
            "--limit" => take_value(&mut limit, &flag, &mut args)?,
            _ => return Err(format!("unknown option {flag}")),
        }
    }
    let harness = optional_identifier(harness, "harness")?;
    let hook = optional_identifier(hook, "hook")?;
    let status = status
        .map(|value| match value.as_str() {
            "started" | "succeeded" | "failed" => Ok(value),
            _ => Err("status must be started, succeeded, or failed".to_owned()),
        })
        .transpose()?;
    let session_id = optional_session_id(session_id)?;
    let limit = match limit {
        Some(value) => value
            .parse::<usize>()
            .ok()
            .filter(|value| (1..=1000).contains(value))
            .ok_or_else(|| "limit must be between 1 and 1000".to_owned())?,
        None => 100,
    };
    Ok(ListArgs {
        root,
        harness,
        hook,
        status,
        session_id,
        limit,
    })
}

fn take_value(
    slot: &mut Option<String>,
    flag: &str,
    args: &mut impl Iterator<Item = String>,
) -> Result<(), String> {
    if slot.is_some() {
        return Err(format!("repeated option {flag}"));
    }
    *slot = Some(
        args.next()
            .ok_or_else(|| format!("missing value for {flag}"))?,
    );
    Ok(())
}

fn absolute_path(value: Option<String>, name: &str) -> Result<PathBuf, String> {
    let value = value.ok_or_else(|| format!("missing {name}"))?;
    let path = PathBuf::from(value);
    if !path.is_absolute() {
        return Err(format!("{name} must be absolute"));
    }
    Ok(path)
}

fn installed_global_root() -> Result<PathBuf, String> {
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let expected_name = if cfg!(windows) { "loam.exe" } else { "loam" };
    let target = executable.parent();
    let version = target.and_then(Path::parent);
    let bin = version.and_then(Path::parent);
    let root = bin.and_then(Path::parent);
    let valid = executable
        .file_name()
        .is_some_and(|name| name == expected_name)
        && target
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .is_some_and(|name| SUPPORTED_TARGETS.contains(&name))
        && version
            .and_then(Path::file_name)
            .is_some_and(|name| name == env!("CARGO_PKG_VERSION"))
        && bin
            .and_then(Path::file_name)
            .is_some_and(|name| name == "bin");
    if valid {
        return root.map(Path::to_path_buf).ok_or_else(inferred_root_error);
    }
    Err(inferred_root_error())
}

fn inferred_root_error() -> String {
    "cannot infer global root outside the installed runtime layout; pass it explicitly".to_owned()
}

fn optional_identifier(value: Option<String>, name: &str) -> Result<Option<String>, String> {
    value
        .map(|value| valid_identifier(Some(value), name))
        .transpose()
}

fn valid_identifier(value: Option<String>, name: &str) -> Result<String, String> {
    let value = value.ok_or_else(|| format!("missing --{name}"))?;
    let mut bytes = value.bytes();
    let first = bytes.next();
    if value.len() > 32
        || !matches!(first, Some(b'a'..=b'z'))
        || !bytes.all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
        })
    {
        return Err(format!("invalid {name} identifier"));
    }
    Ok(value)
}

fn optional_session_id(value: Option<String>) -> Result<Option<String>, String> {
    value
        .map(|value| {
            if value.is_empty()
                || value.chars().count() > 256
                || value.chars().any(char::is_control)
            {
                return Err("session id must be 1..256 characters without controls".to_owned());
            }
            Ok(value)
        })
        .transpose()
}

fn valid_detail(value: &str) -> bool {
    !value.is_empty() && value.chars().count() <= 1024
}

fn valid_semver(value: &str) -> bool {
    let parts: Vec<_> = value.split('.').collect();
    parts.len() == 3
        && parts.iter().all(|part| {
            !part.is_empty()
                && part.bytes().all(|byte| byte.is_ascii_digit())
                && (part == &"0" || !part.starts_with('0'))
        })
}

fn begin(args: BeginArgs) -> Result<(), String> {
    let root_is_new = !args.root.exists();
    fs::create_dir_all(&args.root).map_err(|error| error.to_string())?;
    if root_is_new {
        private_permissions(&args.root, 0o700)?;
    }
    let database = args.root.join(DATABASE_NAME);
    let database_is_new = !database.exists();
    let mut connection = Connection::open(&database).map_err(|error| error.to_string())?;
    if database_is_new {
        private_permissions(&database, 0o600)?;
    }
    connection
        .busy_timeout(Duration::from_millis(250))
        .map_err(|error| error.to_string())?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| error.to_string())?;
    match schema_version(&transaction)? {
        0 => transaction
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
                PRAGMA user_version = 1;",
            )
            .map_err(|error| error.to_string())?,
        SCHEMA_VERSION => {}
        version => return Err(format!("unsupported database schema version {version}")),
    }
    transaction
        .execute(
            "INSERT INTO hook_run (started_at_ms, finished_at_ms, harness, hook, status, detail, session_id, workspace, plugin_version, runtime_version)
             VALUES (?1, NULL, ?2, ?3, 'started', NULL, ?4, ?5, ?6, ?7)",
            params![
                Utc::now().timestamp_millis(),
                args.harness,
                args.hook,
                args.session_id,
                args.workspace,
                args.plugin_version,
                env!("CARGO_PKG_VERSION")
            ],
        )
        .map_err(|error| error.to_string())?;
    let id = transaction.last_insert_rowid();
    transaction
        .execute(
            "DELETE FROM hook_run WHERE id IN (
                SELECT id FROM hook_run ORDER BY id DESC LIMIT -1 OFFSET ?1
            )",
            params![RETENTION],
        )
        .map_err(|error| error.to_string())?;
    transaction.commit().map_err(|error| error.to_string())?;
    println!("{id}");
    Ok(())
}

fn finish(args: FinishArgs) -> Result<(), String> {
    let database = args.root.join(DATABASE_NAME);
    if !database.is_file() {
        return Err("hook-run store does not exist".to_owned());
    }
    let mut connection = Connection::open_with_flags(&database, OpenFlags::SQLITE_OPEN_READ_WRITE)
        .map_err(|error| error.to_string())?;
    connection
        .busy_timeout(Duration::from_millis(250))
        .map_err(|error| error.to_string())?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| error.to_string())?;
    let version = schema_version(&transaction)?;
    if version != SCHEMA_VERSION {
        return Err(format!("unsupported database schema version {version}"));
    }
    let changed = transaction
        .execute(
            "UPDATE hook_run SET finished_at_ms = ?1, status = ?2, detail = ?3
             WHERE id = ?4 AND status = 'started'",
            params![
                Utc::now().timestamp_millis(),
                args.status,
                args.detail,
                args.id
            ],
        )
        .map_err(|error| error.to_string())?;
    if changed != 1 {
        return Err("hook run is missing or already finished".to_owned());
    }
    transaction.commit().map_err(|error| error.to_string())
}

fn list(args: ListArgs) -> Result<(), String> {
    let database = args.root.join(DATABASE_NAME);
    if !database.is_file() {
        return Ok(());
    }
    let connection = Connection::open_with_flags(&database, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| error.to_string())?;
    connection
        .busy_timeout(Duration::from_millis(250))
        .map_err(|error| error.to_string())?;
    let version = schema_version(&connection)?;
    if version != SCHEMA_VERSION {
        return Err(format!("unsupported database schema version {version}"));
    }
    let mut statement = connection
        .prepare(
            "SELECT id, started_at_ms, finished_at_ms, harness, hook, status, detail,
                    session_id, workspace, plugin_version, runtime_version
             FROM hook_run ORDER BY id DESC LIMIT 10000",
        )
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map([], |row| {
            Ok(HookRun {
                id: row.get(0)?,
                started_at_ms: row.get(1)?,
                finished_at_ms: row.get(2)?,
                harness: row.get(3)?,
                hook: row.get(4)?,
                status: row.get(5)?,
                detail: row.get(6)?,
                session_id: row.get(7)?,
                workspace: row.get(8)?,
                plugin_version: row.get(9)?,
                runtime_version: row.get(10)?,
            })
        })
        .map_err(|error| error.to_string())?;
    let mut emitted = 0;
    for row in rows {
        let run = row.map_err(|error| error.to_string())?;
        if args
            .harness
            .as_ref()
            .is_some_and(|value| value != &run.harness)
            || args.hook.as_ref().is_some_and(|value| value != &run.hook)
            || args
                .status
                .as_ref()
                .is_some_and(|value| value != &run.status)
            || args
                .session_id
                .as_ref()
                .is_some_and(|value| run.session_id.as_ref() != Some(value))
        {
            continue;
        }
        println!("{}", run.json()?);
        emitted += 1;
        if emitted == args.limit {
            break;
        }
    }
    Ok(())
}

struct HookRun {
    id: i64,
    started_at_ms: i64,
    finished_at_ms: Option<i64>,
    harness: String,
    hook: String,
    status: String,
    detail: Option<String>,
    session_id: Option<String>,
    workspace: String,
    plugin_version: String,
    runtime_version: String,
}

impl HookRun {
    fn json(&self) -> Result<String, String> {
        Ok(format!(
            "{{\"schema\":1,\"id\":{},\"started_at\":\"{}\",\"finished_at\":{},\"harness\":\"{}\",\"hook\":\"{}\",\"status\":\"{}\",\"detail\":{},\"session_id\":{},\"workspace\":\"{}\",\"plugin_version\":\"{}\",\"runtime_version\":\"{}\"}}",
            self.id,
            timestamp(self.started_at_ms)?,
            optional_timestamp(self.finished_at_ms)?,
            json_escape(&self.harness),
            json_escape(&self.hook),
            json_escape(&self.status),
            optional_json_string(self.detail.as_deref()),
            optional_json_string(self.session_id.as_deref()),
            json_escape(&self.workspace),
            json_escape(&self.plugin_version),
            json_escape(&self.runtime_version),
        ))
    }
}

fn timestamp(value: i64) -> Result<String, String> {
    Utc.timestamp_millis_opt(value)
        .single()
        .map(|value| value.to_rfc3339_opts(SecondsFormat::Millis, true))
        .ok_or_else(|| format!("invalid hook-run timestamp {value}"))
}

fn optional_timestamp(value: Option<i64>) -> Result<String, String> {
    value
        .map(|value| timestamp(value).map(|value| format!("\"{value}\"")))
        .unwrap_or_else(|| Ok("null".to_owned()))
}

fn optional_json_string(value: Option<&str>) -> String {
    value
        .map(|value| format!("\"{}\"", json_escape(value)))
        .unwrap_or_else(|| "null".to_owned())
}

fn schema_version(connection: &Connection) -> Result<i64, String> {
    connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|error| error.to_string())
}

fn json_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            character if character.is_control() => {
                escaped.push_str(&format!("\\u{:04x}", character as u32));
            }
            character => escaped.push(character),
        }
    }
    escaped
}

#[cfg(unix)]
fn private_permissions(path: &Path, mode: u32) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(|error| error.to_string())
}

#[cfg(not(unix))]
fn private_permissions(_path: &Path, _mode: u32) -> Result<(), String> {
    Ok(())
}

fn usage() -> String {
    "usage: loam hooks begin <global-root> --harness <id> --hook <id> --workspace <absolute-path> --plugin-version <semver> [--session-id <id>]\n       loam hooks finish <global-root> --id <positive-integer> --status <succeeded|failed> [--detail <diagnostic>]\n       loam hooks list [<global-root>] [--harness <id>] [--hook <id>] [--status <started|succeeded|failed>] [--session-id <id>] [--limit <1..1000>]".to_owned()
}
