use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::{codegraph, datecheck};

pub fn run(mut args: impl Iterator<Item = String>) -> i32 {
    let mut fast = false;
    let mut workspace = None;
    for arg in args.by_ref() {
        match arg.as_str() {
            "--fast" => fast = true,
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

    let Some(workspace) = workspace else {
        usage();
        return 1;
    };
    let workspace_path = Path::new(&workspace);
    if !workspace_path.is_dir() {
        println!(
            "{{\"error\":\"workspace not found: {}\"}}",
            json_escape(&workspace)
        );
        return 0;
    }

    println!("{}", aggregate(workspace_path, fast));
    0
}

fn usage() {
    eprintln!("Usage: loam state [--fast] <workspace-root>");
}

fn aggregate(workspace: &Path, fast: bool) -> String {
    let Some(wiki_root) = resolve_wiki_root(workspace) else {
        return minimal_state();
    };

    let has_schema = wiki_root.join("SCHEMA.md").is_file();
    let has_index = wiki_root.join("index.md").is_file();
    let has_log = wiki_root.join("log.md").is_file();
    let has_overview = wiki_root.join("overview.md").is_file();
    let metadata = read_metadata(&wiki_root);
    let (qmd_ready, collection) = qmd_readiness(&wiki_root, &metadata.collection);
    let checkpoints = read_checkpoints(&wiki_root);
    let git_status = git_status(workspace);
    let drift_count = (!fast).then(|| datecheck::drift_count(&wiki_root));
    let mut hints = Vec::new();

    add_hints(
        HintContext {
            workspace,
            wiki_root: &wiki_root,
            metadata: &metadata,
            checkpoints: &checkpoints,
            git_status: git_status.as_deref(),
            drift_count,
            fast,
        },
        &mut hints,
    );

    let latest_checkpoint = checkpoints
        .first()
        .map(|checkpoint| checkpoint_json(checkpoint, true));
    let recent_checkpoints = checkpoints
        .iter()
        .take(5)
        .map(|checkpoint| checkpoint_json(checkpoint, false))
        .collect::<Vec<_>>();
    let hints_json = format!("[{}]", hints.join(","));
    let latest_json = latest_checkpoint.unwrap_or_else(|| "null".to_owned());
    let recent_json = format!("[{}]", recent_checkpoints.join(","));
    let git_json = git_status
        .as_deref()
        .map(|value| format!("\"{}\"", json_escape(value)))
        .unwrap_or_else(|| "null".to_owned());
    let drift_json = drift_count
        .map(|value| value.to_string())
        .unwrap_or_else(|| "null".to_owned());
    let metadata_path = metadata
        .path
        .as_deref()
        .map(json_escape)
        .unwrap_or_default();

    format!(
        "{{\"wiki_root\":\"{}\",\"exists\":true,\"has_schema\":{},\"has_index\":{},\"has_log\":{},\"has_overview\":{},\"qmd_ready\":{},\"collection\":\"{}\",\"metadata_status\":\"{}\",\"metadata_path\":\"{}\",\"latest_checkpoint\":{},\"recent_checkpoints\":{},\"checkpoint_count\":{},\"git_status\":{},\"drift_count\":{},\"hints\":{}}}",
        json_escape(&wiki_root.display().to_string()),
        has_schema,
        has_index,
        has_log,
        has_overview,
        qmd_ready,
        json_escape(&collection),
        json_escape(&metadata.status),
        metadata_path,
        latest_json,
        recent_json,
        checkpoints.len(),
        git_json,
        drift_json,
        hints_json,
    )
}

fn minimal_state() -> String {
    "{\"wiki_root\":\"\",\"exists\":false,\"qmd_ready\":false,\"latest_checkpoint\":null,\"recent_checkpoints\":[],\"checkpoint_count\":0,\"git_status\":null,\"drift_count\":null,\"hints\":[{\"kind\":\"memory_missing\",\"group\":\"maintenance\",\"severity\":\"info\",\"message\":\"No memory substrate found; scaffold a wiki to begin.\",\"command\":\"/loam::scaffolding-wiki <goal>\",\"evidence\":{}}]}".to_owned()
}

pub fn resolve_wiki_root(workspace: &Path) -> Option<PathBuf> {
    [workspace.join("wiki"), workspace.to_path_buf()]
        .into_iter()
        .find(|candidate| {
            ["SCHEMA.md", "index.md", "log.md"]
                .into_iter()
                .any(|name| candidate.join(name).is_file())
        })
        .and_then(|path| fs::canonicalize(path).ok())
}

struct Metadata {
    path: Option<String>,
    status: String,
    collection: String,
}

fn read_metadata(wiki_root: &Path) -> Metadata {
    let path = wiki_root.join(".wiki-metadata.json");
    let Ok(content) = fs::read_to_string(&path) else {
        return Metadata {
            path: None,
            status: String::new(),
            collection: String::new(),
        };
    };
    Metadata {
        path: Some(path.display().to_string()),
        status: json_string_value(&content, "status").unwrap_or_default(),
        collection: json_string_value(&content, "collection_name").unwrap_or_default(),
    }
}

fn qmd_readiness(wiki_root: &Path, metadata_collection: &str) -> (bool, String) {
    let metadata_path = wiki_root.join(".wiki-metadata.json");
    if let Ok(content) = fs::read_to_string(metadata_path) {
        if json_string_value(&content, "status").as_deref() == Some("ready") {
            return (true, metadata_collection.to_owned());
        }
    }

    let output = Command::new("qmd").args(["collection", "list"]).output();
    let Ok(output) = output else {
        return (false, metadata_collection.to_owned());
    };
    if !output.status.success() {
        return (false, metadata_collection.to_owned());
    }
    let root = wiki_root.display().to_string();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        if !line.contains(&root) {
            continue;
        }
        let collection = json_string_value(line, "collection_name")
            .filter(|value| !value.is_empty())
            .or_else(|| {
                line.split_whitespace()
                    .next()
                    .map(|value| value.replace(':', ""))
            })
            .unwrap_or_default();
        return (true, collection);
    }
    (false, metadata_collection.to_owned())
}

fn json_string_value(content: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let start = content.find(&needle)? + needle.len();
    let rest = content[start..].trim_start();
    let rest = rest.strip_prefix(':')?.trim_start();
    let rest = rest.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].to_owned())
}

struct Checkpoint {
    path: String,
    title: Option<String>,
    captured_at: Option<String>,
    scope: Option<String>,
}

fn read_checkpoints(wiki_root: &Path) -> Vec<Checkpoint> {
    let directory = wiki_root.join("checkpoints");
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut paths = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("checkpoint-") && name.ends_with(".md"))
        })
        .collect::<Vec<_>>();
    paths.sort_by(|left, right| right.cmp(left));
    paths.into_iter().map(parse_checkpoint).collect()
}

fn parse_checkpoint(path: PathBuf) -> Checkpoint {
    let content = fs::read_to_string(&path).unwrap_or_default();
    let mut h1_title = None;
    let mut h3_title = None;
    let mut first_header_seen = false;
    let mut captured_at = None;
    let mut scope = None;
    for line in content.lines() {
        if h3_title.is_none() && line.starts_with("### ") {
            h3_title = Some(line[4..].trim().to_owned());
        } else if h1_title.is_none() && line.starts_with("# ") {
            h1_title = Some(line[2..].trim().to_owned());
        }
        if line.starts_with("# ") {
            if first_header_seen {
                break;
            }
            first_header_seen = true;
            continue;
        }
        if !first_header_seen {
            continue;
        }
        if let Some(value) = checkpoint_field(line, "Captured") {
            captured_at = Some(value);
        }
        if let Some(value) = checkpoint_field(line, "Scope") {
            scope = Some(value);
        }
    }
    Checkpoint {
        path: path.display().to_string(),
        title: h3_title.or(h1_title),
        captured_at,
        scope,
    }
}

fn checkpoint_field(line: &str, field: &str) -> Option<String> {
    let rest = line.trim_start().strip_prefix('-')?.trim_start();
    let rest = rest.strip_prefix(field)?.strip_prefix(':')?.trim();
    Some(rest.to_owned())
}

fn checkpoint_json(checkpoint: &Checkpoint, include_scope: bool) -> String {
    let mut output = format!(
        "{{\"path\":\"{}\",\"title\":{},\"captured_at\":{}",
        json_escape(&checkpoint.path),
        optional_json(checkpoint.title.as_deref()),
        optional_json(checkpoint.captured_at.as_deref()),
    );
    if include_scope {
        output.push_str(&format!(
            ",\"scope\":{}",
            optional_json(checkpoint.scope.as_deref())
        ));
    }
    output.push('}');
    output
}

fn git_status(workspace: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["-C", &workspace.to_string_lossy(), "status", "--porcelain"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(
        String::from_utf8_lossy(&output.stdout)
            .trim_end_matches('\n')
            .to_owned(),
    )
}

struct HintContext<'a> {
    workspace: &'a Path,
    wiki_root: &'a Path,
    metadata: &'a Metadata,
    checkpoints: &'a [Checkpoint],
    git_status: Option<&'a str>,
    drift_count: Option<usize>,
    fast: bool,
}

fn add_hints(context: HintContext<'_>, hints: &mut Vec<String>) {
    let HintContext {
        workspace,
        wiki_root,
        metadata,
        checkpoints,
        git_status,
        drift_count,
        fast,
    } = context;
    let checkpoint_age = checkpoints
        .first()
        .and_then(|checkpoint| checkpoint.captured_at.as_deref())
        .and_then(epoch_of)
        .map(|captured| (now_epoch() - captured) / 60);
    let git_dirty = git_status.is_some_and(|status| !status.is_empty());

    if git_dirty && (checkpoints.is_empty() || checkpoint_age.is_some_and(|age| age >= 30)) {
        add_hint(
            hints,
            "checkpoint_stale",
            "maintenance",
            "info",
            "Working tree changed; the last checkpoint is missing or 30+ min old.",
            Some("/loam::checkpointing"),
            format!(
                "{{\"git_dirty\":true,\"age_minutes\":{},\"checkpoint_count\":{}}}",
                checkpoint_age.map_or_else(|| "null".to_owned(), |value| value.to_string()),
                checkpoints.len()
            ),
        );
    }

    if !checkpoints.is_empty() {
        if checkpoint_age.is_some_and(|age| age >= 1440) {
            add_hint(
                hints,
                "resume_stale",
                "workflow",
                "info",
                "Latest checkpoint is over 24h old; resume context may be outdated.",
                Some("/loam::resuming"),
                format!(
                    "{{\"age_minutes\":{}}}",
                    checkpoint_age.map_or_else(|| "null".to_owned(), |age| age.to_string())
                ),
            );
        } else {
            add_hint(
                hints,
                "resume_available",
                "workflow",
                "info",
                "A checkpoint exists; you can resume prior work.",
                Some("/loam::resuming"),
                format!(
                    "{{\"age_minutes\":{}}}",
                    checkpoint_age.map_or_else(|| "null".to_owned(), |age| age.to_string())
                ),
            );
        }
    }

    if !fast {
        if let Some(count) = drift_count.filter(|count| *count > 0) {
            add_hint(
                hints,
                "date_drift_pending",
                "maintenance",
                "info",
                "Date/timezone drift found in memory pages.",
                Some("/loam::linting-memory"),
                format!("{{\"drift_count\":{count}}}"),
            );
        }
    }

    if wiki_root.join("log.md").is_file() {
        let content = fs::read_to_string(wiki_root.join("log.md")).unwrap_or_default();
        let line_count = content.bytes().filter(|byte| *byte == b'\n').count();
        if line_count > 500 {
            add_hint(
                hints,
                "log_rotation_due",
                "maintenance",
                "info",
                "log.md exceeds 500 lines; consider rotating it.",
                Some("/loam::linting-memory"),
                format!("{{\"log_lines\":{line_count}}}"),
            );
        }
    }

    if wiki_root.join("overview.md").is_file() {
        add_hint(
            hints,
            "legacy_structure_pending",
            "maintenance",
            "info",
            "Legacy overview.md present; consolidate into index.md.",
            Some("/loam::linting-memory"),
            "{\"has_overview\":true}".to_owned(),
        );
    }

    if !metadata.status.is_empty() && metadata.status != "ready" {
        add_hint(
            hints,
            "retrieval_not_ready",
            "maintenance",
            "info",
            "qmd retrieval metadata is present but not ready.",
            None,
            format!(
                "{{\"metadata_status\":\"{}\"}}",
                json_escape(&metadata.status)
            ),
        );
    }

    if wiki_root.join("log.md").is_file() {
        let content = fs::read_to_string(wiki_root.join("log.md")).unwrap_or_default();
        if let Some((last_lint, age_days)) = lint_age(&content) {
            if age_days >= 7 {
                add_hint(
                    hints,
                    "memory_lint_stale",
                    "maintenance",
                    "info",
                    "Memory lint is stale or was never recorded.",
                    Some("/loam::linting-memory"),
                    format!(
                        "{{\"last_lint\":{},\"age_days\":{age_days}}}",
                        optional_json(Some(&last_lint))
                    ),
                );
            }
        } else {
            add_hint(
                hints,
                "memory_lint_stale",
                "maintenance",
                "info",
                "Memory lint is stale or was never recorded.",
                Some("/loam::linting-memory"),
                "{\"last_lint\":null,\"age_days\":null}".to_owned(),
            );
        }
    }

    if !fast {
        if let Some(count) = codegraph_pending(workspace, wiki_root) {
            if count > 0 {
                add_hint(
                    hints,
                    "code_ingest_pending",
                    "maintenance",
                    "info",
                    &format!("{count} source file(s) new or changed since last ingest."),
                    Some("/loam::ingesting-codebase <workspace-root>"),
                    format!("{{\"pending_count\":{count}}}"),
                );
            }
        }
    }

    workflow_hints(workspace, hints);
}

fn workflow_hints(workspace: &Path, hints: &mut Vec<String>) {
    let specs = direct_markdown_files(&workspace.join("specs"));
    for (name, path) in specs {
        let status = frontmatter_value(&path, "status").unwrap_or_default();
        let approved_at = frontmatter_value(&path, "approved_at").unwrap_or_default();
        let slug = name.trim_end_matches(".md");
        if (status == "approved" || (!approved_at.is_empty() && approved_at != "null"))
            && !workspace.join("plans").join(&name).is_file()
        {
            add_hint(
                hints,
                "spec_ready_for_plan",
                "workflow",
                "info",
                "Approved spec has no plan yet.",
                Some(&format!("/loam::planning specs/{name}")),
                format!("{{\"spec\":\"specs/{name}\"}}"),
            );
        }
        let _ = slug;
    }

    for (name, path) in direct_markdown_files(&workspace.join("plans")) {
        match frontmatter_value(&path, "status").as_deref() {
            Some("pending") => add_hint(
                hints,
                "plan_ready_to_start",
                "workflow",
                "info",
                "A plan is ready to start.",
                Some(&format!("/loam::starting plans/{name}")),
                format!("{{\"plan\":\"plans/{name}\"}}"),
            ),
            Some("in-progress") => add_hint(
                hints,
                "plan_in_progress",
                "workflow",
                "info",
                "A plan is in progress.",
                Some(&format!("/loam::starting plans/{name}")),
                format!("{{\"plan\":\"plans/{name}\"}}"),
            ),
            _ => {}
        }
    }
}

fn direct_markdown_files(directory: &Path) -> Vec<(String, PathBuf)> {
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut files = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file() && path.extension().and_then(|value| value.to_str()) == Some("md")
        })
        .filter_map(|path| {
            let name = path.file_name()?.to_str()?.to_owned();
            Some((name, path))
        })
        .collect::<Vec<_>>();
    files.sort_by(|left, right| left.0.cmp(&right.0));
    files
}

fn frontmatter_value(path: &Path, key: &str) -> Option<String> {
    let content = fs::read_to_string(path).ok()?;
    content.lines().find_map(|line| {
        let rest = line.strip_prefix(key)?.strip_prefix(':')?;
        Some(rest.trim().trim_matches('"').to_owned())
    })
}

fn lint_age(content: &str) -> Option<(String, i64)> {
    let date = content
        .lines()
        .filter_map(|line| {
            let rest = line.strip_prefix("## [")?;
            if rest.len() < 22 || &rest[10..12] != "] " || !rest[12..].starts_with("lint-check") {
                return None;
            }
            let date = &rest[..10];
            is_iso_date(date).then(|| date.to_owned())
        })
        .max()?;
    let epoch = epoch_of(&format!("{date} 00:00 +00:00"))?;
    Some((date, (now_epoch() - epoch) / 86400))
}

fn codegraph_pending(workspace: &Path, wiki_root: &Path) -> Option<usize> {
    if !wiki_root.join("code").is_dir() {
        return None;
    }
    codegraph::pending_count(workspace, wiki_root)
}

fn add_hint(
    hints: &mut Vec<String>,
    kind: &str,
    group: &str,
    severity: &str,
    message: &str,
    command: Option<&str>,
    evidence: String,
) {
    let command = command
        .map(|value| format!("\"{}\"", json_escape(value)))
        .unwrap_or_else(|| "null".to_owned());
    hints.push(format!(
        "{{\"kind\":\"{kind}\",\"group\":\"{group}\",\"severity\":\"{severity}\",\"message\":\"{}\",\"command\":{command},\"evidence\":{evidence}}}",
        json_escape(message)
    ));
}

fn optional_json(value: Option<&str>) -> String {
    value
        .map(|value| format!("\"{}\"", json_escape(value)))
        .unwrap_or_else(|| "null".to_owned())
}

fn epoch_of(value: &str) -> Option<i64> {
    let output = Command::new("date")
        .args(["-d", value, "+%s"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}

fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs() as i64)
}

fn is_iso_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 10
        && bytes[0..4].iter().all(u8::is_ascii_digit)
        && bytes[4] == b'-'
        && bytes[5..7].iter().all(u8::is_ascii_digit)
        && bytes[7] == b'-'
        && bytes[8..10].iter().all(u8::is_ascii_digit)
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

#[cfg(test)]
mod tests {
    use super::lint_age;

    #[test]
    fn lint_check_parser_accepts_trailing_annotation() {
        let (date, _) = lint_age("## [2026-07-11] lint-check | wiki marksman links\n")
            .expect("annotated lint-check should be recognized");
        assert_eq!(date, "2026-07-11");
    }
}
