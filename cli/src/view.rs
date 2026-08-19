//! `loam state --view` snapshot producer (Loam View, T3 + T4).
//!
//! Emits the `workspace`, `capabilities`, `artifacts` (T3), and
//! `relationships` (T4: wikilink scanner + derivation) sections of the
//! snapshot v1 contract (`view/schema/snapshot-v1.schema.json`). The
//! remaining arrays (`events`, `metrics`, `signals`, `hints`, `probes`) are
//! always empty here; other tasks own them.
//! See `specs/loam-view.md` "Snapshot v1 shape", "Artifact inventory and
//! wikilink rules", and "V1 relationship rules are limited to ...".

use std::fs;
use std::path::Path;
use std::process::Command;

use crate::{sha256, state};

const SKIP_DIRS: [&str; 5] = [".git", "node_modules", "target", ".archive", "log-archive"];

pub(crate) fn snapshot(workspace: &Path) -> String {
    let canonical_root = fs::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf());
    let name = canonical_root
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("workspace")
        .to_owned();
    let platform = std::env::consts::OS;
    let git = git_info(workspace);

    let wiki_root = workspace.join("wiki");
    let has_wiki = wiki_root.is_dir()
        && ["SCHEMA.md", "index.md", "log.md"]
            .into_iter()
            .any(|marker| wiki_root.join(marker).is_file());

    let mut artifacts = Vec::new();
    if has_wiki {
        walk_wiki(
            workspace,
            &wiki_root,
            &wiki_root,
            &canonical_root,
            &mut artifacts,
        );
    }
    collect_top_level_kind(workspace, &canonical_root, "goals", "goal", &mut artifacts);
    collect_top_level_kind(workspace, &canonical_root, "specs", "spec", &mut artifacts);
    collect_top_level_kind(workspace, &canonical_root, "plans", "plan", &mut artifacts);
    collect_agents(workspace, workspace, &canonical_root, &mut artifacts);
    artifacts.sort_by(|left, right| left.path.cmp(&right.path));

    let status = if has_wiki { "ready" } else { "not-configured" };
    let capabilities = build_capabilities(&wiki_root, has_wiki, &artifacts, &git);
    let generated_at = chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false);
    let relationships = derive_relationships(&artifacts, workspace, &generated_at);

    format!(
        "{{\"profile\":\"loam-view\",\"schema_version\":1,\"generated_at\":\"{}\",\"status\":\"{}\",\"workspace\":{},\"capabilities\":{},\"artifacts\":[{}],\"relationships\":[{}],\"events\":[],\"metrics\":{{}},\"signals\":[],\"hints\":[],\"probes\":[]}}",
        state::json_escape(&generated_at),
        status,
        workspace_json(&canonical_root, &name, platform, &git),
        capabilities.to_json(),
        artifacts
            .iter()
            .map(Artifact::to_json)
            .collect::<Vec<_>>()
            .join(","),
        relationships
            .iter()
            .map(Relationship::to_json)
            .collect::<Vec<_>>()
            .join(","),
    )
}

// --- workspace / git -------------------------------------------------

struct GitInfo {
    state: &'static str,
    branch: Option<String>,
    dirty: Option<bool>,
    changed_count: Option<usize>,
    capability_state: &'static str,
    capability_reason: Option<String>,
}

fn git_info(workspace: &Path) -> GitInfo {
    match state::git_status(workspace) {
        Some(porcelain) => {
            let changed_count = porcelain.lines().filter(|line| !line.is_empty()).count();
            let dirty = changed_count > 0;
            GitInfo {
                state: if dirty { "dirty" } else { "clean" },
                branch: git_branch(workspace),
                dirty: Some(dirty),
                changed_count: Some(changed_count),
                capability_state: "ready",
                capability_reason: None,
            }
        }
        None => GitInfo {
            state: "unavailable",
            branch: None,
            dirty: None,
            changed_count: None,
            capability_state: "unavailable",
            capability_reason: Some(git_unavailable_reason(workspace)),
        },
    }
}

fn git_branch(workspace: &Path) -> Option<String> {
    let output = Command::new("git")
        .args([
            "-C",
            &workspace.to_string_lossy(),
            "rev-parse",
            "--abbrev-ref",
            "HEAD",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let branch = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!branch.is_empty()).then_some(branch)
}

/// Distinguishes "git is not installed" from "this is not a repository" so
/// the capability reason is honest rather than a generic catch-all.
fn git_unavailable_reason(workspace: &Path) -> String {
    match Command::new("git")
        .args(["-C", &workspace.to_string_lossy(), "rev-parse"])
        .output()
    {
        Ok(output) if !output.status.success() => "not a git repository".to_owned(),
        Ok(_) => "git status unavailable".to_owned(),
        Err(_) => "git binary not found".to_owned(),
    }
}

fn workspace_json(root: &Path, name: &str, platform: &str, git: &GitInfo) -> String {
    format!(
        "{{\"root\":\"{}\",\"name\":\"{}\",\"platform\":\"{}\",\"git\":{{\"state\":\"{}\",\"branch\":{},\"dirty\":{},\"changed_count\":{}}}}}",
        state::json_escape(&root.display().to_string()),
        state::json_escape(name),
        state::json_escape(platform),
        git.state,
        state::optional_json(git.branch.as_deref()),
        git.dirty.map_or_else(|| "null".to_owned(), |value| value.to_string()),
        git.changed_count
            .map_or_else(|| "null".to_owned(), |value| value.to_string()),
    )
}

// --- capabilities ------------------------------------------------------

struct Capability {
    state: &'static str,
    required: bool,
    reason: Option<String>,
    evidence: Option<String>,
}

impl Capability {
    fn to_json(&self) -> String {
        format!(
            "{{\"state\":\"{}\",\"required\":{},\"reason\":{},\"evidence\":{}}}",
            self.state,
            self.required,
            state::optional_json(self.reason.as_deref()),
            self.evidence.as_deref().unwrap_or("null"),
        )
    }
}

fn cap(
    state: &'static str,
    required: bool,
    reason: Option<String>,
    evidence: Option<String>,
) -> Capability {
    Capability {
        state,
        required,
        reason,
        evidence,
    }
}

struct Capabilities {
    wiki: Capability,
    code_graph: Capability,
    goals: Capability,
    work: Capability,
    checkpoints: Capability,
    git: Capability,
    qmd: Capability,
    search_corpus: Capability,
}

impl Capabilities {
    fn to_json(&self) -> String {
        format!(
            "{{\"wiki\":{},\"code_graph\":{},\"goals\":{},\"work\":{},\"checkpoints\":{},\"git\":{},\"qmd\":{},\"search_corpus\":{}}}",
            self.wiki.to_json(),
            self.code_graph.to_json(),
            self.goals.to_json(),
            self.work.to_json(),
            self.checkpoints.to_json(),
            self.git.to_json(),
            self.qmd.to_json(),
            self.search_corpus.to_json(),
        )
    }
}

fn build_capabilities(
    wiki_root: &Path,
    has_wiki: bool,
    artifacts: &[Artifact],
    git: &GitInfo,
) -> Capabilities {
    let no_wiki_reason = || Some("no wiki/ directory found".to_owned());
    let wiki = if has_wiki {
        cap("ready", true, None, None)
    } else {
        cap("absent", true, no_wiki_reason(), None)
    };
    let search_corpus = if has_wiki {
        cap("ready", true, None, None)
    } else {
        cap("absent", true, no_wiki_reason(), None)
    };

    let has_kind = |kind: &str| artifacts.iter().any(|artifact| artifact.kind == kind);
    let code_graph = if has_kind("code") {
        cap(
            "ready",
            false,
            None,
            Some("{\"path\":\"wiki/code\"}".to_owned()),
        )
    } else {
        cap("absent", false, None, None)
    };
    let goals = if has_kind("goal") {
        cap("ready", false, None, None)
    } else {
        cap("absent", false, None, None)
    };
    let work = if has_kind("spec") || has_kind("plan") {
        cap("ready", false, None, None)
    } else {
        cap("absent", false, None, None)
    };
    let checkpoints = if has_kind("checkpoint") {
        cap("ready", false, None, None)
    } else {
        cap("absent", false, None, None)
    };

    let git_cap = cap(
        git.capability_state,
        false,
        git.capability_reason.clone(),
        None,
    );

    let (qmd_ready, qmd_reason) = qmd_capability(wiki_root, has_wiki);
    let qmd = if qmd_ready {
        cap("ready", false, None, None)
    } else {
        cap("absent", false, qmd_reason, None)
    };

    Capabilities {
        wiki,
        code_graph,
        goals,
        work,
        checkpoints,
        git: git_cap,
        qmd,
        search_corpus,
    }
}

fn qmd_capability(wiki_root: &Path, has_wiki: bool) -> (bool, Option<String>) {
    if !has_wiki {
        return (false, None);
    }
    let metadata = state::read_metadata(wiki_root);
    let (ready, _collection) = state::qmd_readiness(wiki_root, &metadata.collection);
    if ready {
        (true, None)
    } else {
        (false, Some("no qmd config found".to_owned()))
    }
}

// --- artifact inventory --------------------------------------------------

struct Artifact {
    path: String,
    kind: &'static str,
    title: Option<String>,
    lifecycle_status: Option<String>,
    created_at: Option<String>,
    updated_at: Option<String>,
    captured_at: Option<String>,
    content_hash: String,
    bytes: u64,
    attributes: String,
    parse_errors: Vec<String>,
    /// Structured fields relationship derivation needs, captured once here
    /// (front matter/body are already parsed in `build_artifact`) rather than
    /// re-parsed from `attributes`'s JSON string. Not part of the wire shape.
    link_facts: LinkFacts,
}

#[derive(Default)]
struct LinkFacts {
    goal_linked_specs: Vec<String>,
    goal_linked_plans: Vec<String>,
    spec_goal: Option<String>,
    plan_spec: Option<String>,
    plan_goal: Option<String>,
    plan_touched_files: Vec<String>,
    checkpoint_previous: Option<String>,
    checkpoint_supersedes: Option<String>,
    code_source_path: Option<String>,
}

impl Artifact {
    fn to_json(&self) -> String {
        format!(
            "{{\"id\":\"{}\",\"path\":\"{}\",\"kind\":\"{}\",\"title\":{},\"lifecycle_status\":{},\"created_at\":{},\"updated_at\":{},\"captured_at\":{},\"content_hash\":\"{}\",\"bytes\":{},\"attributes\":{},\"parse_errors\":[{}]}}",
            state::json_escape(&self.path),
            state::json_escape(&self.path),
            self.kind,
            state::optional_json(self.title.as_deref()),
            state::optional_json(self.lifecycle_status.as_deref()),
            state::optional_json(self.created_at.as_deref()),
            state::optional_json(self.updated_at.as_deref()),
            state::optional_json(self.captured_at.as_deref()),
            self.content_hash,
            self.bytes,
            self.attributes,
            self.parse_errors
                .iter()
                .map(|value| format!("\"{}\"", state::json_escape(value)))
                .collect::<Vec<_>>()
                .join(","),
        )
    }
}

fn walk_wiki(
    workspace: &Path,
    wiki_root: &Path,
    dir: &Path,
    canonical_root: &Path,
    artifacts: &mut Vec<Artifact>,
) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.filter_map(Result::ok).collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();

        if is_symlink(&path) && !resolves_within_root(&path, canonical_root) {
            continue;
        }

        if path.is_dir() {
            if SKIP_DIRS.contains(&name.as_ref()) {
                continue;
            }
            walk_wiki(workspace, wiki_root, &path, canonical_root, artifacts);
            continue;
        }

        if path.extension().and_then(|value| value.to_str()) != Some("md") {
            continue;
        }

        let relative_to_wiki = path.strip_prefix(wiki_root).unwrap_or(path.as_path());
        if relative_to_wiki == Path::new("log.md") {
            continue;
        }

        let kind = classify_wiki_kind(relative_to_wiki);
        artifacts.push(build_artifact(workspace, &path, kind));
    }
}

fn classify_wiki_kind(relative_to_wiki: &Path) -> &'static str {
    if relative_to_wiki == Path::new("index.md") {
        return "wiki-index";
    }
    if relative_to_wiki == Path::new("SCHEMA.md") {
        return "wiki-schema";
    }
    match relative_to_wiki
        .components()
        .next()
        .and_then(|component| component.as_os_str().to_str())
    {
        Some("topics") => "topic",
        Some("entities") => "entity",
        Some("concepts") => "concept",
        Some("analyses") => "analysis",
        Some("code") => "code",
        Some("checkpoints") => "checkpoint",
        _ => "wiki-other",
    }
}

fn collect_top_level_kind(
    workspace: &Path,
    canonical_root: &Path,
    dirname: &str,
    kind: &'static str,
    artifacts: &mut Vec<Artifact>,
) {
    let dir = workspace.join(dirname);
    let Ok(entries) = fs::read_dir(&dir) else {
        return;
    };
    let mut files: Vec<_> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_file())
        .collect();
    files.sort_by_key(|entry| entry.file_name());
    for entry in files {
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("md") {
            continue;
        }
        if path.file_name().and_then(|value| value.to_str()) == Some("INDEX.md") {
            continue;
        }
        if is_symlink(&path) && !resolves_within_root(&path, canonical_root) {
            continue;
        }
        artifacts.push(build_artifact(workspace, &path, kind));
    }
}

fn collect_agents(
    workspace: &Path,
    dir: &Path,
    canonical_root: &Path,
    artifacts: &mut Vec<Artifact>,
) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.filter_map(Result::ok).collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();

        if is_symlink(&path) && !resolves_within_root(&path, canonical_root) {
            continue;
        }

        if path.is_dir() {
            if SKIP_DIRS.contains(&name.as_ref()) {
                continue;
            }
            collect_agents(workspace, &path, canonical_root, artifacts);
            continue;
        }

        if name == "AGENTS.md" {
            artifacts.push(build_artifact(workspace, &path, "guidance"));
        }
    }
}

fn is_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|metadata| metadata.file_type().is_symlink())
        .unwrap_or(false)
}

/// Symlinks resolving outside the workspace are omitted, per
/// `specs/loam-view.md` "Every snapshot path except workspace.root ... ;
/// symlinks that resolve outside the selected root are omitted".
fn resolves_within_root(path: &Path, canonical_root: &Path) -> bool {
    fs::canonicalize(path)
        .map(|resolved| resolved.starts_with(canonical_root))
        .unwrap_or(false)
}

fn relative_path(workspace: &Path, file: &Path) -> String {
    file.strip_prefix(workspace)
        .unwrap_or(file)
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .collect::<Vec<_>>()
        .join("/")
}

fn build_artifact(workspace: &Path, file: &Path, kind: &'static str) -> Artifact {
    let path = relative_path(workspace, file);
    let content_hash = sha256::file_hex(file);
    let bytes = fs::metadata(file)
        .map(|metadata| metadata.len())
        .unwrap_or(0);

    let Ok(content) = fs::read_to_string(file) else {
        return Artifact {
            path,
            kind,
            title: None,
            lifecycle_status: None,
            created_at: None,
            updated_at: None,
            captured_at: None,
            content_hash,
            bytes,
            attributes: default_attributes(kind),
            parse_errors: vec!["content is not valid UTF-8".to_owned()],
            link_facts: LinkFacts::default(),
        };
    };

    let (front_matter, body) = parse_front_matter(&content);
    let mut parse_errors = front_matter.parse_errors.clone();

    let title = extract_title(&front_matter, &body);
    let lifecycle_status = front_matter.get("status").map(str::to_owned);

    let created_at = extract_timestamp(&front_matter, "created_at", &mut parse_errors);
    let updated_at = extract_timestamp(&front_matter, "updated_at", &mut parse_errors);

    let captured_at = if kind == "checkpoint" {
        body.lines()
            .find_map(|line| state::checkpoint_field(line, "Captured"))
            .and_then(|raw| match parse_loam_timestamp(&raw) {
                Some(value) => Some(value),
                None => {
                    parse_errors.push(format!("invalid captured_at: {raw}"));
                    None
                }
            })
    } else {
        None
    };

    let attributes = match kind {
        "code" => code_attributes(workspace, &front_matter),
        "goal" => goal_attributes(&body),
        "spec" => spec_attributes(&front_matter),
        "plan" => plan_attributes(&front_matter, &body),
        "checkpoint" => checkpoint_attributes(&body),
        _ => "{}".to_owned(),
    };

    let link_facts = match kind {
        "code" => LinkFacts {
            code_source_path: front_matter.get("source_path").map(str::to_owned),
            ..LinkFacts::default()
        },
        "goal" => LinkFacts {
            goal_linked_specs: linked_paths(&body, "specs/"),
            goal_linked_plans: linked_paths(&body, "plans/"),
            ..LinkFacts::default()
        },
        "spec" => LinkFacts {
            spec_goal: front_matter.get("goal").map(str::to_owned),
            ..LinkFacts::default()
        },
        "plan" => LinkFacts {
            plan_spec: front_matter.get("spec").map(str::to_owned),
            plan_goal: front_matter.get("goal").map(str::to_owned),
            plan_touched_files: plan_touched_files(&body),
            ..LinkFacts::default()
        },
        "checkpoint" => LinkFacts {
            checkpoint_previous: body
                .lines()
                .find_map(|line| state::checkpoint_field(line, "Previous")),
            checkpoint_supersedes: body
                .lines()
                .find_map(|line| state::checkpoint_field(line, "Supersedes")),
            ..LinkFacts::default()
        },
        _ => LinkFacts::default(),
    };

    Artifact {
        path,
        kind,
        title,
        lifecycle_status,
        created_at,
        updated_at,
        captured_at,
        content_hash,
        bytes,
        attributes,
        parse_errors,
        link_facts,
    }
}

/// Same "Files:" body lines `plan_attributes` reads, factored out so
/// relationship derivation (rule 7) doesn't reparse `attributes`' JSON string.
fn plan_touched_files(body: &str) -> Vec<String> {
    let mut touched = Vec::new();
    for line in body.lines() {
        if let Some(value) = state::checkpoint_field(line, "Files") {
            touched.extend(
                value
                    .split(',')
                    .map(|item| item.trim().to_owned())
                    .filter(|item| !item.is_empty()),
            );
        }
    }
    touched.sort();
    touched.dedup();
    touched
}

fn extract_timestamp(
    front_matter: &FrontMatter,
    key: &str,
    parse_errors: &mut Vec<String>,
) -> Option<String> {
    let raw = front_matter.get(key)?;
    match parse_loam_timestamp(raw) {
        Some(value) => Some(value),
        None => {
            parse_errors.push(format!("invalid {key}: {raw}"));
            None
        }
    }
}

fn default_attributes(kind: &str) -> String {
    match kind {
        "code" => {
            "{\"source_path\":null,\"ingested_at\":null,\"source_size\":null,\"source_hash\":null,\"source_exists\":null}"
                .to_owned()
        }
        "goal" => "{\"linked_specs\":[],\"linked_plans\":[]}".to_owned(),
        "spec" => "{\"goal\":null,\"research\":[]}".to_owned(),
        "plan" => {
            "{\"spec\":null,\"goal\":null,\"task_count_declared\":null,\"task_count_observed\":0,\"task_statuses\":[],\"touched_files\":[],\"acceptance_criteria\":{\"total\":0,\"done\":0}}"
                .to_owned()
        }
        "checkpoint" => {
            "{\"reason\":null,\"scope\":null,\"intended_return\":null,\"previous\":null,\"supersedes\":null,\"workstreams\":[]}"
                .to_owned()
        }
        _ => "{}".to_owned(),
    }
}

// --- kind-specific attributes --------------------------------------------

fn code_attributes(workspace: &Path, front_matter: &FrontMatter) -> String {
    let source_path = front_matter.get("source_path");
    let ingested_at = front_matter.get("ingested_at");
    let source_size = front_matter
        .get("source_size")
        .and_then(|value| value.parse::<u64>().ok());
    // ponytail: attribute name diverges from the front-matter key (`content_hash`)
    // by design -- see specs/loam-view.md's artifact attribute table.
    let source_hash = front_matter.get("content_hash");
    let source_exists = source_path.map(|value| workspace.join(value).is_file());

    format!(
        "{{\"source_path\":{},\"ingested_at\":{},\"source_size\":{},\"source_hash\":{},\"source_exists\":{}}}",
        state::optional_json(source_path),
        state::optional_json(ingested_at),
        source_size.map_or_else(|| "null".to_owned(), |value| value.to_string()),
        state::optional_json(source_hash),
        source_exists.map_or_else(|| "null".to_owned(), |value| value.to_string()),
    )
}

fn goal_attributes(body: &str) -> String {
    let specs = linked_paths(body, "specs/");
    let plans = linked_paths(body, "plans/");
    format!(
        "{{\"linked_specs\":[{}],\"linked_plans\":[{}]}}",
        json_string_array(&specs),
        json_string_array(&plans),
    )
}

fn spec_attributes(front_matter: &FrontMatter) -> String {
    let goal = front_matter.get("goal");
    let empty = Vec::new();
    let research = front_matter.get_list("research").unwrap_or(&empty);
    format!(
        "{{\"goal\":{},\"research\":[{}]}}",
        state::optional_json(goal),
        json_string_array(research),
    )
}

fn plan_attributes(front_matter: &FrontMatter, body: &str) -> String {
    let spec = front_matter.get("spec");
    let goal = front_matter.get("goal");
    let declared = front_matter
        .get("task_count")
        .and_then(|value| value.parse::<u64>().ok());

    let mut statuses = Vec::new();
    let mut touched = Vec::new();
    for line in body.lines() {
        if let Some(value) = state::checkpoint_field(line, "Status") {
            statuses.push(value);
        } else if let Some(value) = state::checkpoint_field(line, "Files") {
            touched.extend(
                value
                    .split(',')
                    .map(|item| item.trim().to_owned())
                    .filter(|item| !item.is_empty()),
            );
        }
    }
    touched.sort();
    touched.dedup();

    let (mut total, mut done) = (0u64, 0u64);
    let mut in_criteria = false;
    for line in body.lines() {
        if let Some(heading) = line.strip_prefix("## ") {
            in_criteria = heading.trim().eq_ignore_ascii_case("acceptance criteria");
            continue;
        }
        if in_criteria {
            if let Some(rest) = line.trim_start().strip_prefix("- [") {
                total += 1;
                if matches!(rest.as_bytes().first(), Some(b'x') | Some(b'-')) {
                    done += 1;
                }
            }
        }
    }

    format!(
        "{{\"spec\":{},\"goal\":{},\"task_count_declared\":{},\"task_count_observed\":{},\"task_statuses\":[{}],\"touched_files\":[{}],\"acceptance_criteria\":{{\"total\":{total},\"done\":{done}}}}}",
        state::optional_json(spec),
        state::optional_json(goal),
        declared.map_or_else(|| "null".to_owned(), |value| value.to_string()),
        statuses.len(),
        json_string_array(&statuses),
        json_string_array(&touched),
    )
}

fn checkpoint_attributes(body: &str) -> String {
    let reason = body
        .lines()
        .find_map(|line| state::checkpoint_field(line, "Reason"));
    let scope = body
        .lines()
        .find_map(|line| state::checkpoint_field(line, "Scope"));
    let intended_return = body
        .lines()
        .find_map(|line| state::checkpoint_field(line, "Intended return"));
    let previous = body
        .lines()
        .find_map(|line| state::checkpoint_field(line, "Previous"));
    let supersedes = body
        .lines()
        .find_map(|line| state::checkpoint_field(line, "Supersedes"));
    let workstreams = parse_workstreams(body);

    format!(
        "{{\"reason\":{},\"scope\":{},\"intended_return\":{},\"previous\":{},\"supersedes\":{},\"workstreams\":[{}]}}",
        state::optional_json(reason.as_deref()),
        state::optional_json(scope.as_deref()),
        state::optional_json(intended_return.as_deref()),
        state::optional_json(previous.as_deref()),
        state::optional_json(supersedes.as_deref()),
        workstreams
            .iter()
            .map(Workstream::to_json)
            .collect::<Vec<_>>()
            .join(","),
    )
}

struct Workstream {
    name: String,
    status: Option<String>,
    next: Option<String>,
    pointers: Vec<String>,
}

impl Workstream {
    fn to_json(&self) -> String {
        format!(
            "{{\"name\":\"{}\",\"status\":{},\"next\":{},\"pointers\":[{}]}}",
            state::json_escape(&self.name),
            state::optional_json(self.status.as_deref()),
            state::optional_json(self.next.as_deref()),
            json_string_array(&self.pointers),
        )
    }
}

fn parse_workstreams(body: &str) -> Vec<Workstream> {
    let mut result = Vec::new();
    let mut in_section = false;
    let mut current: Option<Workstream> = None;
    for line in body.lines() {
        if let Some(heading) = line.strip_prefix("## ") {
            if let Some(workstream) = current.take() {
                result.push(workstream);
            }
            in_section = heading.trim().eq_ignore_ascii_case("workstreams");
            continue;
        }
        if !in_section {
            continue;
        }
        if let Some(name) = line.strip_prefix("### ") {
            if let Some(workstream) = current.take() {
                result.push(workstream);
            }
            current = Some(Workstream {
                name: name.trim().to_owned(),
                status: None,
                next: None,
                pointers: Vec::new(),
            });
            continue;
        }
        let Some(workstream) = current.as_mut() else {
            continue;
        };
        if let Some(value) = state::checkpoint_field(line, "Status") {
            workstream.status = Some(value);
        } else if let Some(value) = state::checkpoint_field(line, "Next") {
            workstream.next = Some(value);
        } else if let Some(value) = state::checkpoint_field(line, "Pointers") {
            workstream.pointers = value
                .split(',')
                .map(|item| item.trim().to_owned())
                .filter(|item| !item.is_empty())
                .collect();
        }
    }
    if let Some(workstream) = current.take() {
        result.push(workstream);
    }
    result
}

fn json_string_array(values: &[String]) -> String {
    values
        .iter()
        .map(|value| format!("\"{}\"", state::json_escape(value)))
        .collect::<Vec<_>>()
        .join(",")
}

/// Token scan for `specs/*.md` / `plans/*.md` mentions in a goal's body, not
/// a Markdown link parser. Good enough for the "## Linked work" convention;
/// upgrade to real link parsing if goals start linking through opaque
/// labels instead of literal paths.
fn linked_paths(body: &str, prefix: &str) -> Vec<String> {
    let mut found = Vec::new();
    for raw in
        body.split(|character: char| character.is_whitespace() || "()[]{}\"'".contains(character))
    {
        let trimmed = raw.trim_matches(|character: char| ",;:".contains(character));
        let normalized = normalize_relative(trimmed);
        if normalized.starts_with(prefix) && normalized.ends_with(".md") {
            let value = normalized.to_owned();
            if !found.contains(&value) {
                found.push(value);
            }
        }
    }
    found.sort();
    found
}

fn normalize_relative(token: &str) -> &str {
    let mut value = token;
    loop {
        if let Some(rest) = value.strip_prefix("../") {
            value = rest;
            continue;
        }
        if let Some(rest) = value.strip_prefix("./") {
            value = rest;
            continue;
        }
        break;
    }
    value
}

// --- front matter ----------------------------------------------------

struct FrontMatter {
    fields: Vec<(String, String)>,
    list_fields: Vec<(String, Vec<String>)>,
    parse_errors: Vec<String>,
}

impl FrontMatter {
    fn get(&self, key: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    }

    fn get_list(&self, key: &str) -> Option<&[String]> {
        self.list_fields
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_slice())
    }
}

/// Parses front matter line-wise (not as full YAML): one `key: value` pair
/// per line, with just enough handling of quoted and bracketed values to
/// detect the malformed cases `specs/loam-view.md` calls out. Malformed
/// fields are recorded in `parse_errors` and left out of `fields`/
/// `list_fields` rather than dropping the artifact.
fn parse_front_matter(content: &str) -> (FrontMatter, String) {
    let mut fields = Vec::new();
    let mut list_fields = Vec::new();
    let mut parse_errors = Vec::new();

    let mut lines = content.lines();
    let Some(first) = lines.next() else {
        return (
            FrontMatter {
                fields,
                list_fields,
                parse_errors,
            },
            String::new(),
        );
    };
    if first.trim_end() != "---" {
        return (
            FrontMatter {
                fields,
                list_fields,
                parse_errors,
            },
            content.to_owned(),
        );
    }

    let mut closed = false;
    for line in lines.by_ref() {
        if line.trim_end() == "---" {
            closed = true;
            break;
        }
        let Some(colon) = line.find(':') else {
            continue;
        };
        let key = line[..colon].trim().to_owned();
        let raw = line[colon + 1..].trim();
        if key.is_empty() {
            continue;
        }
        if let Some(rest) = raw.strip_prefix('"') {
            if let Some(stripped) = rest.strip_suffix('"') {
                fields.push((key, stripped.to_owned()));
            } else {
                parse_errors.push(format!(
                    "malformed front matter field '{key}': unterminated quoted value"
                ));
            }
        } else if let Some(rest) = raw.strip_prefix('[') {
            if let Some(inside) = rest.strip_suffix(']') {
                let items = inside
                    .split(',')
                    .map(|item| item.trim().trim_matches('"').to_owned())
                    .filter(|item| !item.is_empty())
                    .collect::<Vec<_>>();
                list_fields.push((key, items));
            } else {
                parse_errors.push(format!(
                    "malformed front matter field '{key}': unterminated list value"
                ));
            }
        } else {
            fields.push((key, raw.to_owned()));
        }
    }
    if !closed {
        parse_errors.push("front matter missing closing delimiter".to_owned());
    }

    let body = lines.collect::<Vec<_>>().join("\n");
    (
        FrontMatter {
            fields,
            list_fields,
            parse_errors,
        },
        body,
    )
}

fn extract_title(front_matter: &FrontMatter, body: &str) -> Option<String> {
    if let Some(value) = front_matter.get("title") {
        if !value.is_empty() {
            return Some(value.to_owned());
        }
    }
    body.lines()
        .find_map(|line| line.strip_prefix("# ").map(|rest| rest.trim().to_owned()))
        .filter(|value| !value.is_empty())
}

/// Loam's flat timestamp convention (`YYYY-MM-DD HH:MM ±HH:MM`, matching
/// `state.rs`'s `epoch_of`) reformatted to the schema's RFC 3339 shape
/// (`YYYY-MM-DDTHH:MM:SS±HH:MM`). Returns `None` for anything that isn't
/// that exact shape or fails calendar validation, rather than guessing.
fn parse_loam_timestamp(raw: &str) -> Option<String> {
    let value = raw.trim();
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
    state::days_since_unix_epoch(&value[..10])?;
    let hour: u32 = value[11..13].parse().ok()?;
    let minute: u32 = value[14..16].parse().ok()?;
    let offset_hour: u32 = value[18..20].parse().ok()?;
    let offset_minute: u32 = value[21..23].parse().ok()?;
    if hour > 23 || minute > 59 || offset_hour > 23 || offset_minute > 59 {
        return None;
    }
    Some(format!(
        "{}T{}:00{}",
        &value[..10],
        &value[11..16],
        &value[17..23]
    ))
}

// --- relationships: wikilink scanner + derivation (T4) --------------------
//
// See specs/loam-view.md "Artifact inventory and wikilink rules" (scanner
// state machine) and the "V1 relationship rules are limited to ..."
// paragraph. Two families of edges are derived:
//
//   1. every resolved `[[wikilink]]` anywhere in the corpus (rule 1), plus a
//      typed overlay for wikilinks found under a code page's "Dependencies"
//      or "Callers" heading (rule 6);
//   2. structural edges read from already-parsed front matter/body fields
//      captured on `Artifact::link_facts` at build time (rules 2-5, 7).
//
// Unresolved wikilinks (broken/ambiguous) and structural targets absent from
// the inventory never become edges -- `derive_relationships` is the only
// place that turns a candidate into a `Relationship`.

struct EvidenceLocation {
    path: Option<String>,
    line: Option<u64>,
    section: Option<String>,
    field: Option<String>,
    content_hash: Option<String>,
}

impl Clone for EvidenceLocation {
    fn clone(&self) -> Self {
        EvidenceLocation {
            path: self.path.clone(),
            line: self.line,
            section: self.section.clone(),
            field: self.field.clone(),
            content_hash: self.content_hash.clone(),
        }
    }
}

impl EvidenceLocation {
    fn to_json(&self) -> String {
        format!(
            "{{\"path\":{},\"line\":{},\"section\":{},\"field\":{},\"content_hash\":{}}}",
            state::optional_json(self.path.as_deref()),
            self.line
                .map_or_else(|| "null".to_owned(), |line| line.to_string()),
            state::optional_json(self.section.as_deref()),
            state::optional_json(self.field.as_deref()),
            state::optional_json(self.content_hash.as_deref()),
        )
    }
}

struct Rule {
    id: String,
    version: String,
    generated_at: String,
    confidence: f64,
}

impl Rule {
    fn new(id: &str, generated_at: &str) -> Self {
        Rule {
            id: id.to_owned(),
            version: "1".to_owned(),
            generated_at: generated_at.to_owned(),
            confidence: 1.0,
        }
    }

    fn to_json(&self) -> String {
        format!(
            "{{\"id\":\"{}\",\"version\":\"{}\",\"generated_at\":\"{}\",\"confidence\":{}}}",
            state::json_escape(&self.id),
            state::json_escape(&self.version),
            state::json_escape(&self.generated_at),
            self.confidence,
        )
    }
}

struct Relationship {
    id: String,
    from: String,
    to: String,
    kind: String,
    origin: &'static str,
    evidence: EvidenceLocation,
    rule: Option<Rule>,
}

impl Relationship {
    fn new(
        from: String,
        to: String,
        kind: String,
        origin: &'static str,
        evidence: EvidenceLocation,
        rule: Option<Rule>,
    ) -> Self {
        let id = relationship_id(origin, &kind, &from, &to, &evidence);
        Relationship {
            id,
            from,
            to,
            kind,
            origin,
            evidence,
            rule,
        }
    }

    fn to_json(&self) -> String {
        format!(
            "{{\"id\":\"{}\",\"from\":\"{}\",\"to\":\"{}\",\"kind\":\"{}\",\"origin\":\"{}\",\"evidence\":{},\"rule\":{}}}",
            self.id,
            state::json_escape(&self.from),
            state::json_escape(&self.to),
            state::json_escape(&self.kind),
            self.origin,
            self.evidence.to_json(),
            self.rule
                .as_ref()
                .map_or_else(|| "null".to_owned(), Rule::to_json),
        )
    }
}

/// SHA-256 over origin+kind+endpoints+evidence location, unit-separated so
/// adjacent fields can never collide (`"ab"+"c"` vs. `"a"+"bc"`). Every input
/// is deterministic given the same workspace, so ids are stable across runs.
fn relationship_id(
    origin: &str,
    kind: &str,
    from: &str,
    to: &str,
    evidence: &EvidenceLocation,
) -> String {
    let line = evidence.line.map(|value| value.to_string());
    let parts: [&str; 9] = [
        origin,
        kind,
        from,
        to,
        evidence.path.as_deref().unwrap_or(""),
        line.as_deref().unwrap_or(""),
        evidence.section.as_deref().unwrap_or(""),
        evidence.field.as_deref().unwrap_or(""),
        evidence.content_hash.as_deref().unwrap_or(""),
    ];
    let mut hasher = sha256::Sha256::default();
    for part in parts {
        hasher.update(part.as_bytes());
        hasher.update(&[0x1f]);
    }
    hasher.finish()
}

fn derive_relationships(
    artifacts: &[Artifact],
    workspace: &Path,
    generated_at: &str,
) -> Vec<Relationship> {
    let refs = wiki_refs(artifacts);
    let known_paths: std::collections::HashSet<&str> = artifacts
        .iter()
        .map(|artifact| artifact.path.as_str())
        .collect();
    let ctx = StructuralEdgeContext {
        generated_at,
        known_paths: &known_paths,
    };
    let mut relationships = Vec::new();

    for artifact in artifacts {
        let Ok(content) = fs::read_to_string(workspace.join(&artifact.path)) else {
            continue;
        };
        for occurrence in scan_wikilink_occurrences(&content) {
            let Resolution::Resolved { target_path, .. } =
                resolve_wikilink_target(&occurrence.raw_target, &refs)
            else {
                continue;
            };
            let evidence = EvidenceLocation {
                path: Some(artifact.path.clone()),
                line: Some(occurrence.line),
                section: occurrence.section.clone(),
                field: None,
                content_hash: Some(artifact.content_hash.clone()),
            };
            relationships.push(Relationship::new(
                artifact.path.clone(),
                target_path.clone(),
                "wikilink".to_owned(),
                "explicit",
                evidence.clone(),
                None,
            ));

            if artifact.kind == "code" {
                let derived_kind = match occurrence.section.as_deref() {
                    Some(section) if section.eq_ignore_ascii_case("dependencies") => {
                        Some("code-dependency")
                    }
                    Some(section) if section.eq_ignore_ascii_case("callers") => Some("code-caller"),
                    _ => None,
                };
                if let Some(kind) = derived_kind {
                    relationships.push(Relationship::new(
                        artifact.path.clone(),
                        target_path,
                        kind.to_owned(),
                        "derived",
                        evidence,
                        Some(Rule::new(kind, generated_at)),
                    ));
                }
            }
        }
    }

    for artifact in artifacts {
        match artifact.kind {
            "goal" => {
                for spec in &artifact.link_facts.goal_linked_specs {
                    push_structural_edge(
                        &mut relationships,
                        artifact,
                        spec,
                        "goal-linked-spec",
                        Some("Linked work"),
                        None,
                        &ctx,
                    );
                }
                for plan in &artifact.link_facts.goal_linked_plans {
                    push_structural_edge(
                        &mut relationships,
                        artifact,
                        plan,
                        "goal-linked-plan",
                        Some("Linked work"),
                        None,
                        &ctx,
                    );
                }
            }
            "spec" => {
                if let Some(goal) = &artifact.link_facts.spec_goal {
                    push_structural_edge(
                        &mut relationships,
                        artifact,
                        goal,
                        "spec-goal",
                        None,
                        Some("goal"),
                        &ctx,
                    );
                }
            }
            "plan" => {
                if let Some(spec) = &artifact.link_facts.plan_spec {
                    push_structural_edge(
                        &mut relationships,
                        artifact,
                        spec,
                        "plan-spec",
                        None,
                        Some("spec"),
                        &ctx,
                    );
                }
                if let Some(goal) = &artifact.link_facts.plan_goal {
                    push_structural_edge(
                        &mut relationships,
                        artifact,
                        goal,
                        "plan-goal",
                        None,
                        Some("goal"),
                        &ctx,
                    );
                }
                for touched in &artifact.link_facts.plan_touched_files {
                    let mapped: Vec<&Artifact> = artifacts
                        .iter()
                        .filter(|candidate| candidate.kind == "code")
                        .filter(|candidate| {
                            candidate.link_facts.code_source_path.as_deref()
                                == Some(touched.as_str())
                        })
                        .collect();
                    if let [only] = mapped.as_slice() {
                        push_structural_edge(
                            &mut relationships,
                            artifact,
                            only.path.as_str(),
                            "plan-touched-file",
                            Some("Touched files"),
                            None,
                            &ctx,
                        );
                    }
                }
            }
            "checkpoint" => {
                if let Some(previous) = &artifact.link_facts.checkpoint_previous {
                    push_structural_edge(
                        &mut relationships,
                        artifact,
                        previous,
                        "checkpoint-previous",
                        None,
                        Some("previous"),
                        &ctx,
                    );
                }
                if let Some(supersedes) = &artifact.link_facts.checkpoint_supersedes {
                    push_structural_edge(
                        &mut relationships,
                        artifact,
                        supersedes,
                        "checkpoint-supersedes",
                        None,
                        Some("supersedes"),
                        &ctx,
                    );
                }
            }
            _ => {}
        }
    }

    relationships.sort_by(|left, right| {
        (left.from.as_str(), left.to.as_str(), left.kind.as_str()).cmp(&(
            right.from.as_str(),
            right.to.as_str(),
            right.kind.as_str(),
        ))
    });
    relationships
}

struct StructuralEdgeContext<'a> {
    generated_at: &'a str,
    known_paths: &'a std::collections::HashSet<&'a str>,
}

/// Rules 2-5 and 7: a structural target that is not an inventoried artifact
/// path is never turned into an edge, matching the wikilink scanner's
/// "unresolved links never become edges" rule.
fn push_structural_edge(
    relationships: &mut Vec<Relationship>,
    from_artifact: &Artifact,
    to_path: &str,
    kind: &str,
    section: Option<&str>,
    field: Option<&str>,
    ctx: &StructuralEdgeContext,
) {
    if !ctx.known_paths.contains(to_path) {
        return;
    }
    let evidence = EvidenceLocation {
        path: Some(from_artifact.path.clone()),
        line: None,
        section: section.map(str::to_owned),
        field: field.map(str::to_owned),
        content_hash: Some(from_artifact.content_hash.clone()),
    };
    relationships.push(Relationship::new(
        from_artifact.path.clone(),
        to_path.to_owned(),
        kind.to_owned(),
        "derived",
        evidence,
        Some(Rule::new(kind, ctx.generated_at)),
    ));
}

// --- wikilink resolution ---------------------------------------------------

struct WikiArtifactRef {
    path: String,
    rel_no_ext: String,
    stem: String,
}

/// The resolution pool: every inventoried Markdown artifact under `wiki/`
/// (goals/specs/plans/AGENTS.md are never wikilink targets -- they use plain
/// Markdown links, per `specs/loam-view.md`'s path-normalization rule).
fn wiki_refs(artifacts: &[Artifact]) -> Vec<WikiArtifactRef> {
    artifacts
        .iter()
        .filter(|artifact| artifact.path.starts_with("wiki/"))
        .map(|artifact| {
            let rel = artifact
                .path
                .strip_prefix("wiki/")
                .unwrap_or(artifact.path.as_str());
            let rel_no_ext = rel.strip_suffix(".md").unwrap_or(rel).to_owned();
            let stem = rel_no_ext
                .rsplit('/')
                .next()
                .unwrap_or(rel_no_ext.as_str())
                .to_owned();
            WikiArtifactRef {
                path: artifact.path.clone(),
                rel_no_ext,
                stem,
            }
        })
        .collect()
}

#[derive(Debug, PartialEq)]
enum Diagnostic {
    NoncanonicalCase,
    Ambiguous,
    Broken,
}

#[derive(Debug, PartialEq)]
enum Resolution {
    Resolved {
        target_path: String,
        diagnostic: Option<Diagnostic>,
    },
    Unresolved(Diagnostic),
}

/// `specs/loam-view.md` "Artifact inventory and wikilink rules", steps 3-4:
/// a `/`-containing target resolves by normalized wiki-relative path; a bare
/// target resolves by unique case-sensitive basename stem. Either form falls
/// back to a unique case-insensitive match (`noncanonical-link-case`); more
/// than one match at either case sensitivity is `ambiguous-wikilink`; no
/// match at all is `broken-wikilink`.
fn resolve_wikilink_target(raw_target: &str, refs: &[WikiArtifactRef]) -> Resolution {
    let normalized = normalize_wikilink_target(raw_target);
    if normalized.contains('/') {
        resolve_by_key(&normalized, refs, false)
    } else {
        resolve_by_key(&normalized, refs, true)
    }
}

fn normalize_wikilink_target(raw_target: &str) -> String {
    let value = raw_target.trim().replace('\\', "/");
    value.strip_suffix(".md").unwrap_or(&value).to_owned()
}

fn resolve_by_key(target: &str, refs: &[WikiArtifactRef], by_stem: bool) -> Resolution {
    fn key_of(reference: &WikiArtifactRef, by_stem: bool) -> &str {
        if by_stem {
            reference.stem.as_str()
        } else {
            reference.rel_no_ext.as_str()
        }
    }
    let exact: Vec<&WikiArtifactRef> = refs
        .iter()
        .filter(|r| key_of(r, by_stem) == target)
        .collect();
    if exact.len() == 1 {
        return Resolution::Resolved {
            target_path: exact[0].path.clone(),
            diagnostic: None,
        };
    }
    if exact.len() > 1 {
        return Resolution::Unresolved(Diagnostic::Ambiguous);
    }
    let case_insensitive: Vec<&WikiArtifactRef> = refs
        .iter()
        .filter(|r| key_of(r, by_stem).eq_ignore_ascii_case(target))
        .collect();
    match case_insensitive.len() {
        1 => Resolution::Resolved {
            target_path: case_insensitive[0].path.clone(),
            diagnostic: Some(Diagnostic::NoncanonicalCase),
        },
        0 => Resolution::Unresolved(Diagnostic::Broken),
        _ => Resolution::Unresolved(Diagnostic::Ambiguous),
    }
}

// --- wikilink scanner state machine ----------------------------------------

struct WikilinkOccurrence {
    line: u64,
    section: Option<String>,
    raw_target: String,
}

/// `specs/loam-view.md` "Artifact inventory and wikilink rules", steps 1-2:
/// strip front matter for scanning only, ignore fenced code blocks and
/// inline-code spans, and recognize `[[target]]`, `[[target|alias]]`,
/// `[[target#heading]]`, and their `![[...]]` embed form (alias/heading
/// fragments never change resolution).
fn scan_wikilink_occurrences(content: &str) -> Vec<WikilinkOccurrence> {
    let mut occurrences = Vec::new();
    let mut in_front_matter = false;
    let mut in_fence = false;
    let mut section: Option<String> = None;

    for (index, raw_line) in content.lines().enumerate() {
        let line_number = (index + 1) as u64;

        if index == 0 && raw_line.trim_end() == "---" {
            in_front_matter = true;
            continue;
        }
        if in_front_matter {
            if raw_line.trim_end() == "---" {
                in_front_matter = false;
            }
            continue;
        }

        if raw_line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }

        if let Some(heading) = heading_text(raw_line) {
            section = Some(heading);
            continue;
        }

        let scanned = strip_inline_code(raw_line);
        for raw_target in extract_wikilink_targets(&scanned) {
            occurrences.push(WikilinkOccurrence {
                line: line_number,
                section: section.clone(),
                raw_target,
            });
        }
    }
    occurrences
}

fn heading_text(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    let hashes = trimmed
        .chars()
        .take_while(|&character| character == '#')
        .count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let rest = &trimmed[hashes..];
    if !rest.is_empty() && !rest.starts_with(' ') {
        return None;
    }
    Some(rest.trim().to_owned())
}

/// Blanks out `` `...` `` spans (backticks included) so a link-like sequence
/// inside inline code, such as `` `[[not-a-link]]` ``, is never scanned.
/// Ceiling: single-backtick spans only, matching every fixture's usage; a
/// double-backtick span containing a literal backtick would misparse.
fn strip_inline_code(line: &str) -> String {
    let mut result = String::with_capacity(line.len());
    let mut in_code = false;
    for character in line.chars() {
        if character == '`' {
            in_code = !in_code;
            result.push(' ');
            continue;
        }
        result.push(if in_code { ' ' } else { character });
    }
    result
}

fn extract_wikilink_targets(line: &str) -> Vec<String> {
    let mut targets = Vec::new();
    let bytes = line.as_bytes();
    let mut index = 0;
    while index + 1 < bytes.len() {
        if bytes[index] == b'[' && bytes[index + 1] == b'[' {
            if let Some(end) = line[index + 2..].find("]]") {
                let inner = &line[index + 2..index + 2 + end];
                let target = inner
                    .split(['|', '#'])
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_owned();
                if !target.is_empty() {
                    targets.push(target);
                }
                index += 2 + end + 2;
                continue;
            }
        }
        index += 1;
    }
    targets
}

#[cfg(test)]
mod tests {
    use super::{
        extract_wikilink_targets, linked_paths, parse_front_matter, parse_loam_timestamp,
        resolve_wikilink_target, scan_wikilink_occurrences, Diagnostic, Resolution,
        WikiArtifactRef,
    };

    fn wiki_ref(path: &str) -> WikiArtifactRef {
        let rel = path.strip_prefix("wiki/").unwrap_or(path);
        let rel_no_ext = rel.strip_suffix(".md").unwrap_or(rel).to_owned();
        let stem = rel_no_ext
            .rsplit('/')
            .next()
            .unwrap_or(rel_no_ext.as_str())
            .to_owned();
        WikiArtifactRef {
            path: path.to_owned(),
            rel_no_ext,
            stem,
        }
    }

    /// Mirrors `cli/tests/fixtures/view/broken-links/` exactly: one basename
    /// resolves cleanly, one is broken, one is ambiguous (shared stem across
    /// two directories), and one resolves only case-insensitively.
    fn broken_links_refs() -> Vec<WikiArtifactRef> {
        vec![
            wiki_ref("wiki/topics/broken-links-demo.md"),
            wiki_ref("wiki/topics/overview.md"),
            wiki_ref("wiki/entities/overview.md"),
            wiki_ref("wiki/topics/Setup.md"),
        ]
    }

    #[test]
    fn view_links_scanner_classifies_broken_ambiguous_and_noncanonical_targets() {
        let refs = broken_links_refs();

        assert_eq!(
            resolve_wikilink_target("does-not-exist", &refs),
            Resolution::Unresolved(Diagnostic::Broken)
        );
        assert_eq!(
            resolve_wikilink_target("overview", &refs),
            Resolution::Unresolved(Diagnostic::Ambiguous)
        );
        assert_eq!(
            resolve_wikilink_target("setup", &refs),
            Resolution::Resolved {
                target_path: "wiki/topics/Setup.md".to_owned(),
                diagnostic: Some(Diagnostic::NoncanonicalCase),
            }
        );
        assert_eq!(
            resolve_wikilink_target("topics/broken-links-demo", &refs),
            Resolution::Resolved {
                target_path: "wiki/topics/broken-links-demo.md".to_owned(),
                diagnostic: None,
            }
        );
    }

    #[test]
    fn view_links_scanner_ignores_fenced_and_inline_code() {
        let content = "# Demo\n\n\
             A real link: [[real-target]].\n\n\
             ```\n[[fenced-not-a-link]]\n```\n\n\
             An inline code span is inert too: `[[inline-not-a-link]]`.\n";
        let occurrences = scan_wikilink_occurrences(content);
        assert_eq!(
            occurrences.len(),
            1,
            "{:?}",
            occurrences
                .iter()
                .map(|o| &o.raw_target)
                .collect::<Vec<_>>()
        );
        assert_eq!(occurrences[0].raw_target, "real-target");
        assert_eq!(occurrences[0].line, 3);
    }

    #[test]
    fn view_links_scanner_strips_front_matter_and_tracks_sections() {
        let content = "---\ntitle: Demo\n---\n\n## Callers\n\n- [[greeting]]\n";
        let occurrences = scan_wikilink_occurrences(content);
        assert_eq!(occurrences.len(), 1);
        assert_eq!(occurrences[0].line, 7);
        assert_eq!(occurrences[0].section.as_deref(), Some("Callers"));
    }

    #[test]
    fn view_links_extracts_alias_and_heading_forms_without_changing_the_target() {
        let line =
            "[[topics/greeting|Greeting]] and [[code/greeter#Summary]] and ![[embed-target]]";
        assert_eq!(
            extract_wikilink_targets(line),
            vec![
                "topics/greeting".to_owned(),
                "code/greeter".to_owned(),
                "embed-target".to_owned(),
            ]
        );
    }

    #[test]
    fn loam_timestamps_convert_to_rfc3339_and_reject_invalid_calendar_values() {
        assert_eq!(
            parse_loam_timestamp("2026-08-10 09:00 +02:00"),
            Some("2026-08-10T09:00:00+02:00".to_owned())
        );
        assert_eq!(parse_loam_timestamp("not-a-real-date"), None);
        assert_eq!(parse_loam_timestamp("2026-13-45 99:99 +99:99"), None);
    }

    #[test]
    fn front_matter_flags_unterminated_quotes_and_lists_without_dropping_the_body() {
        let content =
            "---\ntitle: \"Bad Frontmatter\ntags: [unterminated, list\n---\n\n# Bad frontmatter\n";
        let (front_matter, body) = parse_front_matter(content);
        assert_eq!(
            front_matter.parse_errors.len(),
            2,
            "{:?}",
            front_matter.parse_errors
        );
        assert_eq!(front_matter.get("title"), None);
        assert!(body.contains("# Bad frontmatter"));
    }

    #[test]
    fn linked_paths_extracts_markdown_link_targets_by_prefix() {
        let body = "### Specs\n\n- [specs/greeting-spec.md](../specs/greeting-spec.md)\n";
        assert_eq!(
            linked_paths(body, "specs/"),
            vec!["specs/greeting-spec.md".to_owned()]
        );
    }
}
