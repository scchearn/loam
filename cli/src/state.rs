use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::{codegraph, datecheck};

pub fn run(mut args: impl Iterator<Item = String>) -> i32 {
    let mut fast = false;
    let mut view = false;
    let mut workspace = None;
    for arg in args.by_ref() {
        match arg.as_str() {
            "--fast" => fast = true,
            "--view" => view = true,
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

    if view {
        println!("{}", crate::view::snapshot(workspace_path));
    } else {
        println!("{}", aggregate(workspace_path, fast));
    }
    0
}

fn usage() {
    eprintln!("Usage: loam state [--fast|--view] <workspace-root>");
}

pub(crate) fn aggregate(workspace: &Path, fast: bool) -> String {
    // hcom is workspace-independent, so its readiness is resolved before the
    // wiki gate and reported by both the full and the minimal aggregate.
    let hcom_ready = hcom_readiness();
    let Some(wiki_root) = resolve_wiki_root(workspace) else {
        return minimal_state(hcom_ready);
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
        "{{\"version\":\"{}\",\"wiki_root\":\"{}\",\"exists\":true,\"has_schema\":{},\"has_index\":{},\"has_log\":{},\"has_overview\":{},\"qmd_ready\":{},\"hcom_ready\":{},\"collection\":\"{}\",\"metadata_status\":\"{}\",\"metadata_path\":\"{}\",\"latest_checkpoint\":{},\"recent_checkpoints\":{},\"checkpoint_count\":{},\"git_status\":{},\"drift_count\":{},\"hints\":{}}}",
        runtime_version(),
        json_escape(&wiki_root.display().to_string()),
        has_schema,
        has_index,
        has_log,
        has_overview,
        qmd_ready,
        hcom_ready,
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

/// The exact hint pipeline `aggregate` uses, reused verbatim by the View
/// snapshot's `hints` field (`specs/loam-view.md`: "pass through existing
/// hint objects unchanged"). Each returned string is already a serialized
/// hint object.
pub(crate) fn hints_for_view(workspace: &Path, wiki_root: &Path) -> Vec<String> {
    let metadata = read_metadata(wiki_root);
    let checkpoints = read_checkpoints(wiki_root);
    let git_status = git_status(workspace);
    let drift_count = Some(datecheck::drift_count(wiki_root));
    let mut hints = Vec::new();
    add_hints(
        HintContext {
            workspace,
            wiki_root,
            metadata: &metadata,
            checkpoints: &checkpoints,
            git_status: git_status.as_deref(),
            drift_count,
            fast: false,
        },
        &mut hints,
    );
    hints
}

/// `wiki/log.md`'s newest `## [YYYY-MM-DD] lint-check` marker and its age in
/// whole UTC days, for the View `wiki.last_lint_at`/`wiki.lint_age_days`
/// metrics and the `memory-lint` signal. Reuses `lint_age` rather than
/// re-scanning `log.md`.
pub(crate) fn last_lint(wiki_root: &Path) -> Option<(String, i64)> {
    let content = fs::read_to_string(wiki_root.join("log.md")).ok()?;
    lint_age(&content)
}

/// The runtime's own compiled version, from the crate's `CARGO_PKG_VERSION`.
/// This is the self-report the config-dir ledger compares against at readiness;
/// it can never be a stale skills-tree `CLI_VERSION`. See
/// `plans/runtime-channel-ledger.md`.
pub(crate) fn runtime_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

fn minimal_state(hcom_ready: bool) -> String {
    format!(
        "{{\"version\":\"{}\",\"wiki_root\":\"\",\"exists\":false,\"qmd_ready\":false,\"hcom_ready\":{},\"latest_checkpoint\":null,\"recent_checkpoints\":[],\"checkpoint_count\":0,\"git_status\":null,\"drift_count\":null,\"hints\":[{{\"kind\":\"memory_missing\",\"group\":\"maintenance\",\"severity\":\"info\",\"message\":\"No memory substrate found; scaffold a wiki to begin.\",\"command\":\"/loam::scaffolding-wiki <goal>\",\"evidence\":{{}}}}]}}",
        runtime_version(),
        hcom_ready
    )
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

pub(crate) struct Metadata {
    path: Option<String>,
    pub(crate) status: String,
    pub(crate) collection: String,
}

pub(crate) fn read_metadata(wiki_root: &Path) -> Metadata {
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

pub(crate) fn qmd_readiness(wiki_root: &Path, metadata_collection: &str) -> (bool, String) {
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

/// Detection-only readiness for the optional hcom integration (spec:
/// loam-optional-integrations). loam never installs hcom, so this only answers
/// "can this session reach it", cheapest rung first, because `state --fast` runs
/// on every session start under a hard hook budget (`cli/tests/state_budget.rs`)
/// and this probe sits outside the wiki gate that short-circuits the qmd one:
///   1. `HCOM_TOOL` — hcom's launcher sets it for every session it starts, so an
///      hcom-managed session skips straight past the health check. The marker is
///      an identity marker, not a liveness one: it outlives an hcom that was
///      removed or broken, and a user who exports it from a shell rc would
///      otherwise be told `ready` on a machine with no hcom at all. So the rung
///      still confirms by stat — no spawn, which is the property it exists for.
///   2. Binary resolution by stat alone: PATH, then `HCOM_INSTALL_DIR` (and its
///      `bin/`), then `~/.local/bin`. brew, uv/pip and both official installers
///      land in one of those, on every OS.
///   3. `hcom --version` — the only subprocess in the ladder, reached only once a
///      binary actually exists. (`hcom version` is not a command.)
///
/// On Windows this ladder matches `hcom.exe` only, while the Node-side ladder in
/// `setup/integrations/tools.mjs` also accepts `.cmd` and `.bat`. A hand-written
/// `hcom.cmd` shim is therefore visible to `loam doctor` and to
/// `--integration hcom` but not to this line. That is the adjudicated shape —
/// every official installer produces the real executable — and it is recorded
/// here so the next reader does not read the narrower rung as a bug.
fn hcom_readiness() -> bool {
    let Some(binary) = resolve_hcom_binary() else {
        return false;
    };
    if std::env::var_os("HCOM_TOOL").is_some_and(|value| !value.is_empty()) {
        return true;
    }
    hcom_answers_its_version(&binary)
}

/// How long the health check waits for `hcom --version` before giving up. The
/// hook budget is five seconds and this is the one spawn on the path with no
/// gate in front of it, so a binary on a stalled network mount, blocked on a
/// build lock, or behind a wrapper shim that waits on something must not be able
/// to hold the whole session start hostage. A real answer is milliseconds.
const HCOM_HEALTH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);
const HCOM_HEALTH_POLL: std::time::Duration = std::time::Duration::from_millis(10);

/// `hcom --version`, bounded. Expiry counts as not-ready: a binary that cannot
/// say its own version in a second is not one a skill should route work to, and
/// the briefing promising otherwise is the failure this probe exists to prevent.
/// `std` has no `wait_timeout`, so this is a `try_wait` poll loop — the same
/// shape as `run_bounded` in `cli/src/service.rs`, without its scratch-file
/// capture, because only the exit status is read here. On expiry the child is
/// killed and reaped so no zombie is left behind.
fn hcom_answers_its_version(binary: &Path) -> bool {
    use std::process::Stdio;
    let Ok(mut child) = Command::new(binary)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let deadline = std::time::Instant::now() + HCOM_HEALTH_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return false;
                }
                std::thread::sleep(HCOM_HEALTH_POLL);
            }
            Err(_) => return false,
        }
    }
}

/// The hcom binary as an absolute path, or `None` when no install site holds an
/// executable of that name. Stat-only: never spawns, never trusts a bare name.
///
/// Only absolute directories are searched, and that is load-bearing rather than
/// tidy. `split_paths` yields an EMPTY component for an empty `PATH` and for the
/// empty element in `PATH=/usr/bin:` — a trailing colon is a common shell-rc
/// accident — and joining a name onto an empty path gives the bare relative
/// `hcom`, which `fs::metadata` resolves against the process CWD. The hook runs
/// with CWD set to the workspace, so without this filter a checked-in `hcom` in
/// a cloned repository would be stat'd and then run at session start. `HOME=""`
/// and a relative `HCOM_INSTALL_DIR` reach the same place by the same route.
fn resolve_hcom_binary() -> Option<PathBuf> {
    let name = if cfg!(windows) { "hcom.exe" } else { "hcom" };
    let mut directories = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .unwrap_or_default();
    if let Some(install_dir) = std::env::var_os("HCOM_INSTALL_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
    {
        directories.push(install_dir.join("bin"));
        directories.push(install_dir);
    }
    if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
        directories.push(PathBuf::from(home).join(".local").join("bin"));
    }
    directories
        .into_iter()
        .filter(|directory| directory.is_absolute())
        .map(|directory| directory.join(name))
        .find(|candidate| is_executable_file(candidate))
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
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

pub(crate) fn checkpoint_field(line: &str, field: &str) -> Option<String> {
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

pub(crate) fn git_status(workspace: &Path) -> Option<String> {
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

    // The memory map in AGENTS.md is how an agent learns the wiki exists at
    // session start, so its absence or drift is surfaced here as well as by
    // `lint --only guidance`. One file read; cheap enough for --fast.
    let mut guidance_findings = Vec::new();
    crate::guidance::findings(workspace, &mut guidance_findings);
    for finding in &guidance_findings {
        let (kind, message) = match finding.rule {
            "GDN001" => (
                "guidance_map_missing",
                "AGENTS.md has no loam memory map; regenerate it so agents learn the wiki exists.",
            ),
            "GDN002" => (
                "guidance_map_stale",
                "AGENTS.md memory map no longer matches the wiki; regenerate it.",
            ),
            _ => continue,
        };
        add_hint(
            hints,
            kind,
            "maintenance",
            "info",
            message,
            Some("/loam::linting-memory"),
            format!("{{\"finding\":\"{}\"}}", json_escape(&finding.description)),
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
    let mut specs_ready: Vec<String> = Vec::new();
    for (name, path) in specs {
        let status = frontmatter_value(&path, "status").unwrap_or_default();
        let approved_at = frontmatter_value(&path, "approved_at").unwrap_or_default();
        if (status == "approved" || (!approved_at.is_empty() && approved_at != "null"))
            && !workspace.join("plans").join(&name).is_file()
        {
            specs_ready.push(format!("specs/{name}"));
        }
    }
    workflow_hint(
        hints,
        "spec_ready_for_plan",
        "Approved spec has no plan yet.",
        "/loam::planning",
        &specs_ready,
    );

    let (mut ready, mut in_progress, mut reconcilable) = (Vec::new(), Vec::new(), Vec::new());
    for (name, path) in direct_markdown_files(&workspace.join("plans")) {
        match frontmatter_value(&path, "status").as_deref() {
            Some("pending") => ready.push(format!("plans/{name}")),
            Some("in-progress") => in_progress.push(format!("plans/{name}")),
            _ => {}
        }
        if plan_reconciliation(&path) {
            reconcilable.push(format!("plans/{name}"));
        }
    }
    workflow_hint(
        hints,
        "plan_ready_to_start",
        "A plan is ready to start.",
        "/loam::starting",
        &ready,
    );
    workflow_hint(
        hints,
        "plan_in_progress",
        "A plan is in progress.",
        "/loam::starting",
        &in_progress,
    );
    workflow_hint(
        hints,
        "plan_reconcilable",
        "Every task is complete but acceptance criteria are unresolved.",
        "/loam::amending-plan",
        &reconcilable,
    );
}

/// True when every task in a plan is `[x]` but some acceptance criterion is
/// still open. Resolved is `[x]` or `[-]`; `[ ]`, `[>]`, and any unrecognized
/// marker are open. See `specs/acceptance-criteria-lifecycle.md`.
// ponytail: line-prefix scan, not a Markdown parse. Criteria and task status
// lines are template-generated and column-anchored; switch to markdown.rs if
// they ever stop being.
fn plan_reconciliation(path: &Path) -> bool {
    let Ok(content) = fs::read_to_string(path) else {
        return false;
    };
    let (mut tasks, mut tasks_done, mut open) = (0usize, 0usize, 0usize);
    let mut in_criteria = false;
    for line in content.lines() {
        if let Some(heading) = line.strip_prefix("## ") {
            in_criteria = heading.trim().eq_ignore_ascii_case("acceptance criteria");
            continue;
        }
        if let Some(marker) = line.trim_start().strip_prefix("- **Status:** [") {
            tasks += 1;
            tasks_done += usize::from(marker.starts_with('x'));
            continue;
        }
        if in_criteria && line.starts_with("- [") {
            open += usize::from(!matches!(line.as_bytes().get(3), Some(b'x') | Some(b'-')));
        }
    }
    tasks > 0 && tasks_done == tasks && open > 0
}

/// One aggregate hint per kind, listing every affected path. Kind stays the
/// suppression key a skill matches on; the agent infers the per-file command
/// from the list, so the hint does not repeat itself once per file.
fn workflow_hint(
    hints: &mut Vec<String>,
    kind: &str,
    message: &str,
    command: &str,
    paths: &[String],
) {
    if paths.is_empty() {
        return;
    }
    let list = paths
        .iter()
        .map(|path| format!("\"{}\"", json_escape(path)))
        .collect::<Vec<_>>()
        .join(",");
    let key = if kind == "spec_ready_for_plan" {
        "specs"
    } else {
        "plans"
    };
    add_hint(
        hints,
        kind,
        "workflow",
        "info",
        message,
        Some(command),
        format!("{{\"{key}\":[{list}]}}"),
    );
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
    let day = days_since_unix_epoch(&date)?;
    Some((date, now_epoch() / 86400 - day))
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

pub(crate) fn optional_json(value: Option<&str>) -> String {
    value
        .map(|value| format!("\"{}\"", json_escape(value)))
        .unwrap_or_else(|| "null".to_owned())
}

pub(crate) fn days_since_unix_epoch(value: &str) -> Option<i64> {
    if !is_iso_date(value) {
        return None;
    }
    let year: i64 = value[0..4].parse().ok()?;
    let month: i64 = value[5..7].parse().ok()?;
    let day: i64 = value[8..10].parse().ok()?;
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let month_days = [
        31,
        28 + i64::from(leap),
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    if year == 0
        || !(1..=12).contains(&month)
        || !(1..=month_days[(month - 1) as usize]).contains(&day)
    {
        return None;
    }

    // Howard Hinnant's civil-calendar conversion, offset to 1970-01-01.
    let adjusted_year = year - i64::from(month <= 2);
    let era = adjusted_year.div_euclid(400);
    let year_of_era = adjusted_year - era * 400;
    let month_prime = month + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * month_prime + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    Some(era * 146_097 + day_of_era - 719_468)
}

fn epoch_of(value: &str) -> Option<i64> {
    let bytes = value.as_bytes();
    if bytes.len() != 23
        || bytes[10] != b' '
        || bytes[13] != b':'
        || bytes[16] != b' '
        || !matches!(bytes[17], b'+' | b'-')
        || bytes[20] != b':'
    {
        return None;
    }
    let day = days_since_unix_epoch(&value[..10])?;
    let hour: i64 = value[11..13].parse().ok()?;
    let minute: i64 = value[14..16].parse().ok()?;
    let offset_hour: i64 = value[18..20].parse().ok()?;
    let offset_minute: i64 = value[21..23].parse().ok()?;
    if hour > 23 || minute > 59 || offset_hour > 23 || offset_minute > 59 {
        return None;
    }
    let offset = (offset_hour * 60 + offset_minute) * 60;
    let offset = if bytes[17] == b'+' { offset } else { -offset };
    Some(day * 86_400 + hour * 3_600 + minute * 60 - offset)
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

pub(crate) fn json_escape(value: &str) -> String {
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
    use super::{
        aggregate, days_since_unix_epoch, epoch_of, lint_age, minimal_state, runtime_version,
    };

    #[test]
    fn version_is_the_compiled_crate_version() {
        assert_eq!(runtime_version(), env!("CARGO_PKG_VERSION"));
        let needle = format!("\"version\":\"{}\"", env!("CARGO_PKG_VERSION"));
        // Present in both the wiki-less minimal state and a full aggregate.
        assert!(minimal_state(false).contains(&needle));
        let tmp = std::env::temp_dir();
        assert!(aggregate(&tmp, true).contains("\"version\":\""));
    }

    #[test]
    fn civil_dates_convert_without_platform_tools() {
        assert_eq!(days_since_unix_epoch("1970-01-01"), Some(0));
        assert_eq!(days_since_unix_epoch("2000-02-29"), Some(11016));
        assert_eq!(days_since_unix_epoch("2026-07-20"), Some(20654));
        assert_eq!(days_since_unix_epoch("2026-02-29"), None);
        assert_eq!(epoch_of("1970-01-01 02:30 +02:30"), Some(0));
    }

    #[test]
    fn lint_check_parser_accepts_trailing_annotation() {
        let (date, _) = lint_age("## [2026-07-11] lint-check | wiki marksman links\n")
            .expect("annotated lint-check should be recognized");
        assert_eq!(date, "2026-07-11");
    }
}
