//! `loam checkpoint verify` and `loam checkpoint state`.
//!
//! Both replace Bash scripts that only ran on Linux in practice: between them
//! they required GNU `awk` three-argument `match`, `grep -E`/`grep -oP`, `sed`,
//! `jq`, GNU `date -d`, GNU `date -r`, and `find -mmin`.
//!
//! `verify` reproduces `checkpoint-verify-legacy` byte for byte; that script is
//! retained as the parity oracle and `cli/tests/checkpoint_parity.rs` compares
//! against it on Linux. The output is human-oriented text rather than the lint
//! NDJSON envelope, deliberately: it is read at save time as orientation, and
//! it must never block a save, so it exits 0 for every note-content finding.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

use crate::json;

const REASONS: [&str; 4] = ["shutdown", "pause", "handoff", "context-switch"];
const STATUSES: [&str; 5] = ["active", "blocked", "waiting", "ready-to-resume", "done"];
const VOLATILE: &str = "VOLATILE \u{2014} may not survive into resumed session";
const DEFAULT_WINDOW: u64 = 180;
const RECENT_LIMIT: usize = 15;
const TASK_LIMIT: usize = 10;
const DESCRIPTION_LIMIT: usize = 60;

pub fn run(mut args: impl Iterator<Item = String>) -> i32 {
    match args.next().as_deref() {
        Some("verify") => verify(args),
        Some("state") => state(args),
        _ => {
            usage();
            1
        }
    }
}

fn usage() {
    eprintln!(
        "Usage:\n  loam checkpoint verify <note.md>\n  loam checkpoint state [--window <minutes>] [<workspace-root>]"
    );
}

// ---------------------------------------------------------------------------
// verify
// ---------------------------------------------------------------------------

fn verify(mut args: impl Iterator<Item = String>) -> i32 {
    let note = match args.next() {
        Some(value) if value != "--help" && value != "-h" => value,
        // The oracle treats a missing argument and an explicit help request
        // identically, and exits 0 either way so a wrapper cannot fail a save.
        _ => {
            println!("Usage: loam checkpoint verify <note.md>");
            return 0;
        }
    };
    let path = Path::new(&note);
    let Ok(raw) = fs::read_to_string(path) else {
        println!("NOTE: not found: {note}");
        return 0;
    };
    // `$(<file)` strips trailing newlines; the line scan below must see the
    // same input the oracle saw.
    let raw = raw.trim_end_matches(['\n', '\r']);

    report_front_matter(raw);
    report_none_literals(raw);
    let pointers = report_workstreams(raw);
    report_pointers(path, &pointers);
    0
}

fn report_front_matter(raw: &str) {
    let format = front_matter(raw, "Format");
    if format.is_empty() || format != "v1" {
        println!("FORMAT: unknown \u{2014} got \"{format}\" (expected v1)");
    } else {
        println!("FRONTMATTER Format: PASS");
    }

    if front_matter(raw, "Captured").is_empty() {
        println!("FRONTMATTER Captured: FAIL: missing");
    } else {
        println!("FRONTMATTER Captured: PASS");
    }

    let reason = front_matter(raw, "Reason");
    if reason.is_empty() {
        println!("FRONTMATTER Reason: FAIL: missing");
    } else if REASONS.contains(&reason.as_str()) {
        println!("FRONTMATTER Reason: PASS");
    } else {
        println!("FRONTMATTER Reason: FAIL: not in {{shutdown,pause,handoff,context-switch}}");
        println!("  WARN: {reason}");
    }

    let scope = front_matter(raw, "Scope");
    if scope.is_empty() {
        println!("FRONTMATTER Scope: FAIL: missing");
    } else if scope.trim_matches(is_space).is_empty() {
        println!("FRONTMATTER Scope: WARN: empty");
    } else {
        println!("FRONTMATTER Scope: PASS");
    }

    if front_matter(raw, "Intended return").is_empty() {
        println!("FRONTMATTER Intended return: none recorded");
    } else {
        println!("FRONTMATTER Intended return: PRESENT");
    }
}

/// Bullet-list front matter: `- Key: value`, scanned from the first `# `
/// heading to the second. Repeated keys concatenate with a newline, exactly as
/// the oracle's command substitution did.
fn front_matter(raw: &str, field: &str) -> String {
    let mut seen = false;
    let mut values = Vec::new();
    for line in raw.lines() {
        if line.starts_with("# ") {
            if seen {
                break;
            }
            seen = true;
        }
        if !seen {
            continue;
        }
        if let Some(value) = bullet_value(line, field) {
            values.push(value);
        }
    }
    values.join("\n")
}

/// `^[[:space:]]*-[[:space:]]*<field>[[:space:]]*:(.*)$`, trimmed.
fn bullet_value(line: &str, field: &str) -> Option<String> {
    let rest = line.trim_start_matches(is_space);
    let rest = rest.strip_prefix('-')?;
    let rest = rest.trim_start_matches(is_space);
    let rest = rest.strip_prefix(field)?;
    let rest = rest.trim_start_matches(is_space);
    let rest = rest.strip_prefix(':')?;
    Some(rest.trim_matches(is_space).to_owned())
}

fn report_none_literals(raw: &str) {
    for field in ["Previous", "Supersedes"] {
        if has_none_literal(raw, field) {
            println!("WARN: \"{field}: none\" literal \u{2014} should be omitted, not written");
        }
    }
}

/// `grep -qi '^-\s*<field>:\s*none'`: anchored at the line start, no leading
/// indent allowed, case-insensitive, and a prefix match on `none`.
fn has_none_literal(raw: &str, field: &str) -> bool {
    raw.lines().any(|line| {
        let Some(rest) = line.strip_prefix('-') else {
            return false;
        };
        let rest = rest.trim_start_matches(is_space);
        let Some(rest) = strip_prefix_ignoring_case(rest, field) else {
            return false;
        };
        let Some(rest) = rest.strip_prefix(':') else {
            return false;
        };
        let rest = rest.trim_start_matches(is_space);
        strip_prefix_ignoring_case(rest, "none").is_some()
    })
}

fn report_workstreams(raw: &str) -> Vec<String> {
    let mut pointers = Vec::new();
    let mut current = String::new();
    let mut have_status = false;
    let mut inside = false;

    for line in raw.lines() {
        if line.starts_with("## Workstreams") {
            inside = true;
            continue;
        }
        if inside && line.starts_with("## ") {
            inside = false;
        }
        if !inside {
            continue;
        }

        if let Some(title) = line.strip_prefix("### ") {
            if !current.is_empty() && !have_status {
                println!("  WORKSTREAM {current} (no status field found)");
            }
            current = title.trim_matches(is_space).to_owned();
            have_status = false;
            println!("WORKSTREAM {current}");
            continue;
        }
        if line.starts_with("- Status:") {
            have_status = true;
            let value = after_first_colon(line);
            if value.is_empty() {
                println!("  WORKSTREAM {current} Status: FAIL: missing");
            } else if STATUSES.contains(&value.as_str()) {
                println!("  WORKSTREAM {current} Status: PASS");
            } else {
                println!("  WORKSTREAM {current} Status: FAIL: {value} not in enum");
            }
            continue;
        }
        if line.starts_with("- Next:") {
            if after_first_colon(line).is_empty() {
                println!("  WORKSTREAM {current} Next: FAIL: empty or missing");
            } else {
                println!("  WORKSTREAM {current} Next: PASS");
            }
            continue;
        }
        if line.starts_with("- Pointers:") {
            collect_pointers(&after_first_colon(line), &mut pointers);
            continue;
        }
        if line.starts_with("  - ") {
            let value: String = line.chars().skip(4).collect();
            collect_pointers(value.trim_matches(is_space), &mut pointers);
        }
    }

    if !current.is_empty() && !have_status {
        println!("  WORKSTREAM {current} (no status field found)");
    }
    pointers
}

/// `substr($0, index($0, ":") + 2)`: everything after the first colon minus one
/// character, counted in characters rather than bytes.
fn after_first_colon(line: &str) -> String {
    let mut characters = line.chars();
    for character in characters.by_ref() {
        if character == ':' {
            break;
        }
    }
    characters.next();
    characters.as_str().trim_matches(is_space).to_owned()
}

fn collect_pointers(value: &str, pointers: &mut Vec<String>) {
    if value.is_empty() {
        return;
    }
    for part in value.split(',') {
        let trimmed = part.trim_matches(is_space);
        if !trimmed.is_empty() {
            pointers.push(trimmed.to_owned());
        }
    }
}

fn report_pointers(note: &Path, pointers: &[String]) {
    println!();
    println!("=== Pointer checks ===");
    for pointer in pointers {
        report_pointer(note, &strip_trailing_context(pointer));
    }
    if pointers.is_empty() {
        println!("  (no pointers found)");
    }
}

fn report_pointer(note: &Path, pointer: &str) {
    if let Some(resolved) = filesystem_prefix(note, pointer) {
        if resolved.exists() {
            println!("  POINTER {pointer}: OK");
        } else {
            println!("  POINTER {pointer}: MISSING: {pointer}");
        }
        if is_volatile(&resolved.to_string_lossy()) {
            println!("  POINTER {pointer}: {VOLATILE}");
        }
        if pointer.contains("$TMPDIR") {
            println!("  POINTER {pointer}: {VOLATILE}");
        }
        return;
    }
    if let Some(thread) = hcom_thread(pointer) {
        match hcom_recent(&thread) {
            None => println!("  POINTER hcom thread {thread}: HCOM: not available"),
            Some(true) => println!("  POINTER hcom thread {thread}: OK"),
            Some(false) => println!("  POINTER hcom thread {thread}: STALE: {thread}"),
        }
        return;
    }
    if let Some(uuid) = task_uuid(pointer) {
        match task_exists(&uuid) {
            None => println!("  POINTER task {uuid}: TASK: not available"),
            Some(true) => println!("  POINTER task {uuid}: OK"),
            Some(false) => println!("  POINTER task {uuid}: NOT FOUND"),
        }
        return;
    }
    if looks_like_relative_path(pointer) {
        let relative = format!("{}/{}", note_directory(note), pointer);
        if Path::new(&relative).exists() {
            println!("  POINTER {pointer}: OK (resolved to {relative})");
            if is_volatile(&relative) {
                println!("  POINTER {pointer}: {VOLATILE}");
            }
            return;
        }
        let workspace = std::env::var("WORKSPACE").ok().unwrap_or_else(|| {
            std::env::current_dir()
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_default()
        });
        let from_workspace = format!("{workspace}/{pointer}");
        if Path::new(&from_workspace).exists() {
            println!("  POINTER {pointer}: OK (resolved to {from_workspace})");
        } else {
            println!("  POINTER {pointer}: MISSING (tried {relative} and {from_workspace})");
        }
        return;
    }
    if Path::new(pointer).exists() {
        println!("  POINTER {pointer}: OK");
        if is_volatile(pointer) {
            println!("  POINTER {pointer}: {VOLATILE}");
        }
        return;
    }
    println!("  POINTER {pointer}: (unrecognized format, not checked by verify)");
}

/// Strips a trailing `(events ...)` or `(pending: ...)` annotation, then any
/// trailing whitespace, in that order.
fn strip_trailing_context(pointer: &str) -> String {
    let mut value = pointer;
    for prefix in ["(events ", "(pending:"] {
        if let Some(stripped) = strip_trailing_group(value, prefix) {
            value = stripped;
        }
    }
    value.trim_end_matches(is_space).to_owned()
}

fn strip_trailing_group<'a>(value: &'a str, prefix: &str) -> Option<&'a str> {
    let trimmed = value.trim_end_matches(is_space);
    let rest = trimmed.strip_suffix(')')?;
    let open = rest.rfind('(')?;
    if !rest[open..].starts_with(prefix) || rest[open..].contains(')') {
        return None;
    }
    Some(value[..open].trim_end_matches(is_space))
}

/// Pattern A: an absolute, note-relative, or home-relative filesystem path.
/// The `~` and `./` forms are resolved; everything else is taken verbatim.
fn filesystem_prefix(note: &Path, pointer: &str) -> Option<PathBuf> {
    if pointer.starts_with("./") || pointer.starts_with("../") {
        return Some(PathBuf::from(format!(
            "{}/{}",
            note_directory(note),
            pointer
        )));
    }
    if let Some(rest) = pointer.strip_prefix('~') {
        let home = home_directory();
        return Some(PathBuf::from(format!("{home}{rest}")));
    }
    if pointer.starts_with('/') || is_native_absolute(pointer) {
        return Some(PathBuf::from(pointer));
    }
    None
}

/// Windows drive-absolute and UNC paths never match the oracle's POSIX-only
/// prefixes, so they are recognised here rather than falling through to the
/// unrecognised-format branch. On Unix this is a no-op.
#[cfg(windows)]
fn is_native_absolute(pointer: &str) -> bool {
    let bytes = pointer.as_bytes();
    if pointer.starts_with("\\\\") {
        return true;
    }
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
}

#[cfg(not(windows))]
fn is_native_absolute(_pointer: &str) -> bool {
    false
}

fn note_directory(note: &Path) -> String {
    match note.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_string_lossy().into_owned(),
        _ => ".".to_owned(),
    }
}

fn home_directory() -> String {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_default()
}

/// `/tmp` and `/run` are POSIX conventions; on Windows nothing is flagged,
/// which is correct rather than merely convenient.
fn is_volatile(path: &str) -> bool {
    path.starts_with("/tmp") || path.starts_with("/run")
}

/// Pattern B: the last `hcom thread <name>` mention, backticks optional.
fn hcom_thread(pointer: &str) -> Option<String> {
    const MARKER: &str = "hcom thread";
    let mut found = None;
    let mut search = 0;
    while let Some(offset) = pointer[search..].find(MARKER) {
        let start = search + offset;
        search = start + MARKER.len();
        let rest = &pointer[search..];
        let trimmed = rest.trim_start_matches(is_space);
        if trimmed.len() == rest.len() {
            continue;
        }
        let trimmed = trimmed.strip_prefix('`').unwrap_or(trimmed);
        let name: String = trimmed
            .chars()
            .take_while(|value| value.is_ascii_alphanumeric() || *value == '_' || *value == '-')
            .collect();
        if !name.is_empty() {
            found = Some(name);
        }
    }
    found
}

/// `Some(true)` recent, `Some(false)` stale, `None` when `hcom` is absent.
fn hcom_recent(thread: &str) -> Option<bool> {
    let output = Command::new("hcom")
        .args(["events", "--last", "500", "--type", "message", "--thread"])
        .arg(thread)
        .output()
        .ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let first = stdout.lines().next().unwrap_or("");
    Some(!first.is_empty() && first != "[]")
}

/// Pattern C: the first RFC-4122-shaped identifier on a word boundary.
fn task_uuid(pointer: &str) -> Option<String> {
    const GROUPS: [usize; 5] = [8, 4, 4, 4, 12];
    let characters: Vec<char> = pointer.chars().collect();
    'start: for start in 0..characters.len() {
        if start > 0 && is_word(characters[start - 1]) {
            continue;
        }
        let mut position = start;
        for (index, length) in GROUPS.iter().enumerate() {
            if index > 0 {
                if characters.get(position) != Some(&'-') {
                    continue 'start;
                }
                position += 1;
            }
            for _ in 0..*length {
                match characters.get(position) {
                    Some(value) if value.is_ascii_digit() || ('a'..='f').contains(value) => {
                        position += 1
                    }
                    _ => continue 'start,
                }
            }
        }
        if characters.get(position).copied().is_some_and(is_word) {
            continue;
        }
        return Some(characters[start..position].iter().collect());
    }
    None
}

fn is_word(value: char) -> bool {
    value.is_ascii_alphanumeric() || value == '_'
}

/// `Some(true)` known, `Some(false)` unknown, `None` when `task` is absent.
fn task_exists(uuid: &str) -> Option<bool> {
    let output = Command::new("task").args([uuid, "info"]).output().ok()?;
    Some(output.status.success())
}

/// Pattern D: contains a `/`-delimited final segment and is not an `hcom` or
/// `task` command fragment.
fn looks_like_relative_path(pointer: &str) -> bool {
    if pointer.starts_with("hcom") {
        return false;
    }
    if let Some(rest) = pointer.strip_prefix("task") {
        if !rest.starts_with(is_word) {
            return false;
        }
    }
    let Some(slash) = pointer.rfind('/') else {
        return false;
    };
    let tail = &pointer[slash + 1..];
    !tail.is_empty()
        && tail
            .chars()
            .all(|value| value.is_ascii_alphanumeric() || "._-".contains(value))
}

// ---------------------------------------------------------------------------
// state
// ---------------------------------------------------------------------------

fn state(mut args: impl Iterator<Item = String>) -> i32 {
    let mut window = DEFAULT_WINDOW;
    let mut workspace = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--window" => match args.next().and_then(|value| value.parse::<u64>().ok()) {
                Some(value) => window = value,
                None => {
                    eprintln!("loam checkpoint state: --window takes a whole number of minutes");
                    return 1;
                }
            },
            value if value.starts_with('-') => {
                usage();
                return 1;
            }
            value if workspace.is_none() => workspace = Some(value.to_owned()),
            _ => {
                usage();
                return 1;
            }
        }
    }

    let workspace = workspace
        .or_else(|| std::env::var("WORKSPACE").ok())
        .unwrap_or_else(|| {
            std::env::current_dir()
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_else(|_| ".".to_owned())
        });
    let workspace = PathBuf::from(workspace);
    if !workspace.is_dir() {
        eprintln!(
            "loam checkpoint state: workspace not found: {}",
            workspace.display()
        );
        return 1;
    }

    println!("=== hcom threads ===");
    report_threads(window);
    println!();
    println!("=== taskwarrior active ===");
    report_tasks();
    println!();
    println!("=== files touched recently ===");
    report_recent_files(&workspace, window);
    0
}

fn report_threads(window: u64) {
    let cutoff = chrono::Local::now() - chrono::Duration::minutes(window as i64);
    let cutoff = cutoff.format("%Y-%m-%dT%H:%M:%S").to_string();

    let Ok(output) = Command::new("hcom")
        .args(["events", "--last", "500"])
        .output()
    else {
        println!("hcom: not available");
        return;
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let Ok(events) = json::parse_stream(&stdout) else {
        return;
    };

    // One row per thread, keeping the latest message; ties resolve to the last
    // in input order, matching `max_by`.
    let mut latest: Vec<(String, &json::Value)> = Vec::new();
    for event in &events {
        if event.get("type").and_then(json::Value::as_str) != Some("message") {
            continue;
        }
        let Some(thread) = event
            .get("data")
            .and_then(|data| data.get("thread"))
            .and_then(json::Value::as_str)
        else {
            continue;
        };
        let Some(timestamp) = event.get("ts").and_then(json::Value::as_str) else {
            continue;
        };
        if timestamp < cutoff.as_str() {
            continue;
        }
        match latest.iter_mut().find(|(name, _)| name == thread) {
            Some((_, held)) => {
                let previous = held.get("ts").and_then(json::Value::as_str).unwrap_or("");
                if timestamp >= previous {
                    *held = event;
                }
            }
            None => latest.push((thread.to_owned(), event)),
        }
    }
    latest.sort_by(|left, right| left.0.cmp(&right.0));

    for (thread, event) in latest {
        let identifier = field(event, &["id"]);
        let intent = field(event, &["data", "intent"]);
        let sender = field(event, &["data", "from"]);
        println!("{thread} | msg #{identifier} | intent: {intent} | from: {sender}");
    }
}

/// An absent key and an explicit `null` both render as `null`, as they did
/// through `jq`'s string interpolation.
fn field(value: &json::Value, path: &[&str]) -> String {
    let mut current = value;
    for key in path {
        match current.get(key) {
            Some(next) => current = next,
            None => return "null".to_owned(),
        }
    }
    current.render()
}

fn report_tasks() {
    let Ok(output) = Command::new("task")
        .args(["status:pending", "export"])
        .output()
    else {
        println!("task: not available");
        return;
    };
    if !output.status.success() {
        println!("  (no pending tasks or export failed)");
        return;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let Ok(parsed) = json::parse(stdout.trim()) else {
        println!("  (no pending tasks or export failed)");
        return;
    };
    let Some(items) = parsed.as_array() else {
        println!("  (no pending tasks or export failed)");
        return;
    };
    for item in items.iter().take(TASK_LIMIT) {
        let uuid = item
            .get("uuid")
            .map(json::Value::render)
            .unwrap_or_default();
        let project = match item.get("project") {
            Some(value) if !value.is_null() => value.render(),
            _ => "none".to_owned(),
        };
        let description: String = item
            .get("description")
            .map(json::Value::render)
            .unwrap_or_default()
            .chars()
            .take(DESCRIPTION_LIMIT)
            .collect();
        println!(
            "{}\t{}\t{}",
            tab_separated(&uuid),
            tab_separated(&project),
            tab_separated(&description)
        );
    }
}

/// `@tsv` escaping, so an embedded tab or newline cannot forge a column.
fn tab_separated(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\t' => output.push_str("\\t"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\\' => output.push_str("\\\\"),
            other => output.push(other),
        }
    }
    output
}

fn report_recent_files(workspace: &Path, window: u64) {
    let cutoff = SystemTime::now()
        .checked_sub(std::time::Duration::from_secs(window * 60))
        .unwrap_or(SystemTime::UNIX_EPOCH);
    let mut recent = Vec::new();
    collect_recent(workspace, cutoff, &mut recent, 0);
    // The oracle printed `find` order, which is unspecified. Sorting newest
    // first makes the digest reproducible and puts the useful rows on top.
    recent.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| left.1.to_string_lossy().cmp(&right.1.to_string_lossy()))
    });

    if recent.is_empty() {
        println!("none");
        return;
    }
    for (modified, path) in recent.iter().take(RECENT_LIMIT) {
        let stamp = chrono::DateTime::<chrono::Local>::from(*modified).format("%Y-%m-%d %H:%M");
        println!("{stamp} {}", path.display());
    }
}

/// Depth-bounded so a pathological tree cannot stall a save-time digest.
fn collect_recent(
    directory: &Path,
    cutoff: SystemTime,
    recent: &mut Vec<(SystemTime, PathBuf)>,
    depth: usize,
) {
    const MAX_DEPTH: usize = 32;
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        // `symlink_metadata` keeps a symlinked directory from creating a cycle.
        let Ok(metadata) = entry.path().symlink_metadata() else {
            continue;
        };
        if metadata.is_dir() {
            collect_recent(&entry.path(), cutoff, recent, depth + 1);
            continue;
        }
        if !metadata.is_file() {
            continue;
        }
        if let Ok(modified) = metadata.modified() {
            if modified >= cutoff {
                recent.push((modified, entry.path()));
            }
        }
    }
}

fn is_space(value: char) -> bool {
    matches!(value, ' ' | '\t' | '\u{b}' | '\u{c}' | '\r')
}

fn strip_prefix_ignoring_case<'a>(value: &'a str, prefix: &str) -> Option<&'a str> {
    if value.len() < prefix.len() {
        return None;
    }
    let (head, tail) = value.split_at(prefix.len());
    head.eq_ignore_ascii_case(prefix).then_some(tail)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn front_matter_stops_at_the_second_top_level_heading() {
        let raw = "# One\n- Scope: first\n# Two\n- Scope: second\n";

        assert_eq!(front_matter(raw, "Scope"), "first");
    }

    #[test]
    fn front_matter_joins_repeated_keys() {
        let raw = "# One\n- Scope: a\n- Scope: b\n";

        assert_eq!(front_matter(raw, "Scope"), "a\nb");
    }

    #[test]
    fn bullet_values_tolerate_surrounding_space() {
        assert_eq!(
            bullet_value("  -   Format  :  v1  ", "Format").as_deref(),
            Some("v1")
        );
        assert_eq!(bullet_value("- Formatting: v1", "Format"), None);
    }

    #[test]
    fn none_literals_are_matched_case_insensitively_at_the_line_start() {
        assert!(has_none_literal("- Previous: none", "Previous"));
        assert!(has_none_literal("- previous:NONE recorded", "Previous"));
        assert!(!has_none_literal("  - Previous: none", "Previous"));
        assert!(!has_none_literal("- Previous: wiki/a.md", "Previous"));
    }

    #[test]
    fn a_value_after_the_first_colon_drops_exactly_one_character() {
        assert_eq!(after_first_colon("- Status: active"), "active");
        assert_eq!(after_first_colon("- Status:active"), "ctive");
        assert_eq!(after_first_colon("- Status:"), "");
    }

    #[test]
    fn a_multibyte_value_is_sliced_by_character_not_byte() {
        assert_eq!(after_first_colon("- Next: caf\u{e9}"), "caf\u{e9}");
        assert_eq!(after_first_colon("- Next:\u{2014}done"), "done");
    }

    #[test]
    fn trailing_annotations_are_stripped_in_order() {
        assert_eq!(strip_trailing_context("a.md (events 1-9)"), "a.md");
        assert_eq!(strip_trailing_context("a.md (pending: #4)"), "a.md");
        assert_eq!(strip_trailing_context("a.md  "), "a.md");
        assert_eq!(strip_trailing_context("a (b) c"), "a (b) c");
    }

    #[test]
    fn the_last_hcom_thread_mention_wins() {
        assert_eq!(
            hcom_thread("hcom thread `alpha-1`").as_deref(),
            Some("alpha-1")
        );
        assert_eq!(hcom_thread("hcom thread beta_2").as_deref(), Some("beta_2"));
        assert_eq!(
            hcom_thread("hcom thread one then hcom thread two").as_deref(),
            Some("two")
        );
        assert_eq!(hcom_thread("hcom threadless"), None);
        assert_eq!(hcom_thread("a plain note"), None);
    }

    #[test]
    fn task_uuids_are_matched_on_word_boundaries() {
        assert_eq!(
            task_uuid("550e8400-e29b-41d4-a716-446655440000").as_deref(),
            Some("550e8400-e29b-41d4-a716-446655440000")
        );
        assert_eq!(
            task_uuid("see 550e8400-e29b-41d4-a716-446655440000 now").as_deref(),
            Some("550e8400-e29b-41d4-a716-446655440000")
        );
        assert_eq!(task_uuid("550e8400-e29b-41d4-a716-44665544000"), None);
        assert_eq!(task_uuid("x550e8400-e29b-41d4-a716-446655440000"), None);
    }

    #[test]
    fn relative_path_detection_excludes_command_fragments() {
        assert!(looks_like_relative_path("wiki/checkpoints/a.md"));
        assert!(!looks_like_relative_path("hcom thread alpha/beta"));
        assert!(!looks_like_relative_path("plain-note"));
    }

    #[test]
    fn tsv_escaping_cannot_forge_a_column() {
        assert_eq!(tab_separated("a\tb\nc\\d"), "a\\tb\\nc\\\\d");
    }
}
