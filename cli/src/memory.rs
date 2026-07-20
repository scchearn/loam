use std::fs;
use std::path::{Path, PathBuf};

use crate::lint::{Finding, Severity};
use crate::state;

const DURABLE_EXCLUSIONS: [&str; 4] = ["index.md", "SCHEMA.md", "log.md", "overview.md"];
const EXCLUDED_DIRECTORIES: [&str; 3] = [".archive", ".obsidian", "checkpoints"];
const LOG_ROTATION_LINES: usize = 500;

/// Memory-domain findings (`MEM*`). Silent when the workspace has no wiki.
pub fn wiki_findings(workspace: &Path, findings: &mut Vec<Finding>) {
    if let Some(wiki_root) = state::resolve_wiki_root(workspace) {
        lint_wiki(workspace, &wiki_root, findings);
    }
}

/// Work-artifact findings (`WRK*`). Silent when the workspace has no `goals/`.
pub fn work_findings(workspace: &Path, today: i64, findings: &mut Vec<Finding>) {
    lint_work(workspace, today, findings);
}

fn lint_wiki(workspace: &Path, wiki_root: &Path, findings: &mut Vec<Finding>) {
    let display_root = relative(workspace, wiki_root);

    let index_path = wiki_root.join("index.md");
    let index = fs::read_to_string(&index_path).unwrap_or_default();
    let index_display = join_display(&display_root, "index.md");

    if index_path.is_file() && !has_overview_section(&index) {
        findings.push(Finding::file(
            "MEM001",
            "index-missing-overview",
            Severity::Warning,
            &index_display,
            "Root hub has no `## Overview` section",
        ));
    }

    if wiki_root.join("overview.md").is_file() {
        findings.push(Finding::file(
            "MEM002",
            "legacy-overview-present",
            Severity::Warning,
            &join_display(&display_root, "overview.md"),
            "Legacy root `overview.md` should be folded into `index.md`",
        ));
    }

    let pages = durable_pages(wiki_root);
    let references = index_references(&index);

    for page in &pages {
        let stem = stem(page);
        if is_derived(page) {
            continue;
        }
        let referenced = references
            .iter()
            .any(|reference| references_page(&reference.target, page, &display_root));
        if !referenced {
            findings.push(
                Finding::file(
                    "MEM003",
                    "index-missing-page",
                    Severity::Warning,
                    &join_display(&display_root, page),
                    "Durable page is not reachable from `index.md`",
                )
                .with_target(&stem),
            );
        }
    }

    // Index membership only. General wikilink resolution stays with `loam lint
    // markdown` (LMD001/LMD002) so the two surfaces do not double-report.
    for reference in &references {
        if !pages
            .iter()
            .any(|page| references_page(&reference.target, page, &display_root))
        {
            findings.push(
                Finding::file(
                    "MEM004",
                    "index-dangling-entry",
                    Severity::Warning,
                    &index_display,
                    "`index.md` lists a page that does not exist",
                )
                .with_target(&reference.target),
            );
        }

        if let Some((page, resolution)) = resolve_code_page(reference, &pages, &display_root) {
            findings.push(
                Finding::file(
                    "MEM013",
                    "index-contains-code-page",
                    Severity::Warning,
                    &index_display,
                    "`index.md` lists a code page directly; move the entry to the code hub",
                )
                .with_target(&reference.target)
                .with_evidence("code_page", &page)
                .with_evidence("resolution", resolution),
            );
        }
    }

    lint_code_hub(
        wiki_root,
        &display_root,
        &pages,
        &references,
        &index_display,
        findings,
    );

    if wiki_root.join(".obsidian").is_dir() && !is_workspace_root(workspace, wiki_root) {
        findings.push(Finding::file(
            "MEM005",
            "obsidian-misplaced",
            Severity::Warning,
            &join_display(&display_root, ".obsidian"),
            "Obsidian config belongs at the parent root, not inside the wiki",
        ));
    }

    lint_metadata(wiki_root, &display_root, findings);

    for page in &pages {
        let path = wiki_root.join(page);
        let display = join_display(&display_root, page);
        let front_matter = front_matter(&path);

        if let Some(source_path) = field(&front_matter, "source_path") {
            if page.starts_with("entities/") {
                findings.push(
                    Finding::file(
                        "MEM007",
                        "stranded-code-page",
                        Severity::Warning,
                        &display,
                        "Code-graph page belongs under `code/`, not `entities/`",
                    )
                    .with_target(&source_path),
                );
            }
            let missing: Vec<&str> = ["source_size", "content_hash"]
                .into_iter()
                .filter(|name| field(&front_matter, name).is_none())
                .collect();
            if !missing.is_empty() {
                findings.push(
                    Finding::file(
                        "MEM008",
                        "legacy-hash-fields",
                        Severity::Info,
                        &display,
                        "Code page is missing hash-secondary front matter",
                    )
                    .with_evidence("missing", &missing.join(",")),
                );
            }
        }

        // `_index.md` is the reserved hub name, deliberately not kebab-case.
        if page != CODE_HUB && !is_kebab_case(&stem(page)) {
            findings.push(Finding::file(
                "MEM009",
                "filename-convention",
                Severity::Warning,
                &display,
                "Durable page filename is not canonical kebab-case",
            ));
        }

        if is_empty_page(&path) {
            findings.push(Finding::file(
                "MEM011",
                "empty-page",
                Severity::Warning,
                &display,
                "Page has headings or front matter but no content",
            ));
        }
    }

    lint_checkpoints(wiki_root, &display_root, findings);

    let log = wiki_root.join("log.md");
    if let Ok(content) = fs::read_to_string(&log) {
        let lines = content.lines().count();
        if lines > LOG_ROTATION_LINES {
            findings.push(
                Finding::file(
                    "MEM012",
                    "log-rotation-due",
                    Severity::Warning,
                    &join_display(&display_root, "log.md"),
                    "Log exceeds the rotation threshold",
                )
                .with_evidence("lines", &lines.to_string()),
            );
        }
    }
}

const GOAL_FIELDS: [&str; 7] = [
    "title",
    "slug",
    "status",
    "created_at",
    "updated_at",
    "reviewed_at",
    "next_review_at",
];
const GOAL_SECTIONS: [&str; 5] = [
    "Intent",
    "Validation contract",
    "Linked work",
    "Current state",
    "Reviews",
];
const GOAL_STATUSES: [&str; 5] = ["draft", "active", "paused", "achieved", "abandoned"];
const GOAL_TIMESTAMPS: [&str; 4] = ["created_at", "updated_at", "reviewed_at", "next_review_at"];
const DRAFT_STALE_DAYS: i64 = 30;
const ACTIVE_STALE_DAYS: i64 = 90;

/// Report-only health pass over `goals/`. Runs with or without a wiki, and never
/// writes: corrections belong to `/loam::setting-goals`.
fn lint_work(workspace: &Path, today: i64, findings: &mut Vec<Finding>) {
    let goals = workspace.join("goals");
    let Ok(entries) = fs::read_dir(&goals) else {
        return;
    };
    let mut files: Vec<String> = entries
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".md") && name != "INDEX.md")
        .collect();
    files.sort();

    for name in &files {
        lint_goal(&goals.join(name), &format!("goals/{name}"), today, findings);
    }
    lint_goal_index(&goals, &files, findings);
}

fn lint_goal(path: &Path, display: &str, today: i64, findings: &mut Vec<Finding>) {
    let Ok(content) = fs::read_to_string(path) else {
        return;
    };
    let fields = front_matter(path);

    for name in GOAL_FIELDS {
        if field(&fields, name).is_none() {
            findings.push(
                Finding::file(
                    "WRK001",
                    "missing-front-matter-field",
                    Severity::Warning,
                    display,
                    "Goal is missing a required front-matter field",
                )
                .with_target(name),
            );
        }
    }

    let status = field(&fields, "status").unwrap_or_default();
    if !status.is_empty() && !GOAL_STATUSES.contains(&status.as_str()) {
        findings.push(
            Finding::file(
                "WRK002",
                "invalid-status",
                Severity::Warning,
                display,
                "Goal status is not a known lifecycle value",
            )
            .with_target(&status),
        );
    }

    for name in GOAL_TIMESTAMPS {
        let Some(value) = field(&fields, name) else {
            continue;
        };
        if value != "null" && civil_days(&value).is_none() {
            findings.push(
                Finding::file(
                    "WRK003",
                    "malformed-timestamp",
                    Severity::Warning,
                    display,
                    "Goal timestamp is not 'YYYY-MM-DD HH:MM ±HH:MM' or null",
                )
                .with_target(name),
            );
        }
    }

    let sections = sections(&content);
    for name in GOAL_SECTIONS {
        if !sections.iter().any(|section| section == name) {
            findings.push(
                Finding::file(
                    "WRK004",
                    "missing-required-section",
                    Severity::Warning,
                    display,
                    "Goal is missing a required section",
                )
                .with_target(name),
            );
        }
    }

    let updated = field(&fields, "updated_at").and_then(|value| civil_days(&value));
    let reviewed = field(&fields, "reviewed_at").and_then(|value| civil_days(&value));
    let next_review = field(&fields, "next_review_at").and_then(|value| civil_days(&value));

    if status == "draft" {
        if let Some(updated) = updated {
            if today - updated > DRAFT_STALE_DAYS {
                findings.push(
                    Finding::file(
                        "WRK005",
                        "draft-stale",
                        Severity::Warning,
                        display,
                        "Draft goal has not been updated recently",
                    )
                    .with_evidence("days", &(today - updated).to_string()),
                );
            }
        }
    }

    if status == "active" {
        match next_review {
            Some(next_review) if next_review < today => findings.push(
                Finding::file(
                    "WRK006",
                    "active-overdue",
                    Severity::Warning,
                    display,
                    "Active goal is past its next review date",
                )
                .with_evidence("days", &(today - next_review).to_string()),
            ),
            None => {
                if let Some(last) = reviewed.or(updated) {
                    if today - last > ACTIVE_STALE_DAYS {
                        findings.push(
                            Finding::file(
                                "WRK007",
                                "active-stale",
                                Severity::Warning,
                                display,
                                "Active goal has no scheduled review and has gone stale",
                            )
                            .with_evidence("days", &(today - last).to_string()),
                        );
                    }
                }
            }
            _ => {}
        }
    }

    for target in linked_work(&content) {
        // Goals link either goal-relative (`../specs/x.md`) or repo-relative
        // (`specs/x.md`); either spelling resolving is enough.
        let goal_relative = path.parent().unwrap_or(Path::new(".")).join(&target);
        let repo_relative = workspace_of(path).join(&target);
        if !goal_relative.exists() && !repo_relative.exists() {
            findings.push(
                Finding::file(
                    "WRK008",
                    "linked-work-path-missing",
                    Severity::Warning,
                    display,
                    "Linked work path does not exist",
                )
                .with_target(&target),
            );
        }
    }

    let reviews = review_dates(&content);
    if status == "achieved" && !has_passing_review(&content) {
        findings.push(Finding::file(
            "WRK010",
            "achieved-without-passing-review",
            Severity::Warning,
            display,
            "Achieved goal has no review entry recording a pass",
        ));
    }

    if let (Some(reviewed), Some(newest)) = (reviewed, reviews.last().copied()) {
        if reviewed != newest {
            findings.push(
                Finding::file(
                    "WRK011",
                    "reviewed-at-mismatch",
                    Severity::Warning,
                    display,
                    "`reviewed_at` does not match the newest review entry",
                )
                .with_evidence("newest_review", &civil_date(newest)),
            );
        }
    }
}

fn lint_goal_index(goals: &Path, files: &[String], findings: &mut Vec<Finding>) {
    let index = goals.join("INDEX.md");
    let Ok(content) = fs::read_to_string(&index) else {
        return;
    };
    let mut rows: Vec<String> = content
        .lines()
        .filter(|line| line.trim_start().starts_with('|'))
        .flat_map(|line| {
            line.split('|')
                .map(str::trim)
                .filter(|cell| cell.ends_with(".md"))
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .collect();
    rows.sort();
    rows.dedup();

    for row in &rows {
        let name = row.rsplit('/').next().unwrap_or(row);
        if !files.iter().any(|file| file == name) {
            findings.push(
                Finding::file(
                    "WRK009",
                    "index-row-mismatch",
                    Severity::Warning,
                    "goals/INDEX.md",
                    "Index row has no matching goal file",
                )
                .with_target(row),
            );
        }
    }

    for file in files {
        if !rows.iter().any(|row| row.ends_with(file.as_str())) {
            findings.push(
                Finding::file(
                    "WRK009",
                    "index-row-mismatch",
                    Severity::Warning,
                    &format!("goals/{file}"),
                    "Goal file has no row in `goals/INDEX.md`",
                )
                .with_target(&format!("goals/{file}")),
            );
        }
    }
}

fn workspace_of(goal_path: &Path) -> PathBuf {
    goal_path
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_default()
}

fn sections(content: &str) -> Vec<String> {
    content
        .lines()
        .filter_map(|line| line.strip_prefix("## "))
        .map(|value| value.trim().to_owned())
        .collect()
}

/// Repo-relative `.md` paths listed under `## Linked work`.
fn linked_work(content: &str) -> Vec<String> {
    let mut targets = Vec::new();
    let mut inside = false;
    for line in content.lines() {
        if let Some(heading) = line.strip_prefix("## ") {
            inside = heading.trim() == "Linked work";
            continue;
        }
        if !inside {
            continue;
        }
        let mut token = String::new();
        for character in line.chars() {
            if character.is_ascii_alphanumeric() || "._/-".contains(character) {
                token.push(character);
                continue;
            }
            push_path(&mut targets, &token);
            token.clear();
        }
        push_path(&mut targets, &token);
    }
    targets.sort();
    targets.dedup();
    targets
}

fn push_path(targets: &mut Vec<String>, token: &str) {
    if token.ends_with(".md") && token.contains('/') {
        targets.push(token.to_owned());
    }
}

fn review_dates(content: &str) -> Vec<i64> {
    let mut dates: Vec<i64> = content
        .lines()
        .filter_map(|line| line.strip_prefix("### "))
        .filter_map(|value| civil_days(value.trim()))
        .collect();
    dates.sort();
    dates
}

fn has_passing_review(content: &str) -> bool {
    content.lines().any(|line| {
        line.trim()
            .to_ascii_lowercase()
            .starts_with("- result: pass")
    })
}

/// The full documented shape: `YYYY-MM-DD HH:MM ±HH:MM`, nothing looser.
/// `--now` enforces this so the flag cannot silently accept a partial timestamp.
pub fn timestamp_days(value: &str) -> Option<i64> {
    let parts: Vec<&str> = value.split_whitespace().collect();
    if parts.len() != 3 {
        return None;
    }
    let days = civil_days(parts[0])?;
    if parts[0].len() != 10 {
        return None;
    }
    clock(parts[1])?;
    let (sign, offset) = parts[2].split_at(1);
    if sign != "+" && sign != "-" {
        return None;
    }
    clock(offset)?;
    Some(days)
}

/// `HH:MM` with in-range values.
fn clock(value: &str) -> Option<()> {
    let (hours, minutes) = value.split_once(':')?;
    if hours.len() != 2 || minutes.len() != 2 {
        return None;
    }
    let hours: u32 = hours.parse().ok()?;
    let minutes: u32 = minutes.parse().ok()?;
    (hours < 24 && minutes < 60).then_some(())
}

/// Days since 1970-01-01 for the leading `YYYY-MM-DD` of a loam timestamp.
/// Time and offset are validated in shape but do not move the day boundary,
/// which is the granularity every goal staleness rule uses.
fn civil_days(value: &str) -> Option<i64> {
    let date = value.split_whitespace().next()?;
    let parts: Vec<&str> = date.split('-').collect();
    if parts.len() != 3 || parts[0].len() != 4 || parts[1].len() != 2 || parts[2].len() != 2 {
        return None;
    }
    if !parts
        .iter()
        .all(|part| part.chars().all(|c| c.is_ascii_digit()))
    {
        return None;
    }
    let year: i64 = parts[0].parse().ok()?;
    let month: i64 = parts[1].parse().ok()?;
    let day: i64 = parts[2].parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    Some(days_from_civil(year, month, day))
}

/// Howard Hinnant's civil-from-days algorithm, shifted to the Unix epoch.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn civil_date(days: i64) -> String {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    let year = if month <= 2 { year + 1 } else { year };
    format!("{year:04}-{month:02}-{day:02}")
}

pub fn today_utc() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| (value.as_secs() / 86_400) as i64)
        .unwrap_or_default()
}

fn is_workspace_root(workspace: &Path, wiki_root: &Path) -> bool {
    fs::canonicalize(workspace).ok().as_deref() == Some(wiki_root)
}

fn lint_metadata(wiki_root: &Path, display_root: &str, findings: &mut Vec<Finding>) {
    let path = wiki_root.join(".wiki-metadata.json");
    let Ok(content) = fs::read_to_string(&path) else {
        return;
    };
    // ponytail: single-key string scan, not a JSON parser. Swap for a real parse
    // if the memory rules ever need nested or non-string metadata.
    let Some(recorded) = json_string_field(&content, "collection_path") else {
        return;
    };
    let matches = fs::canonicalize(&recorded)
        .map(|value| value == wiki_root)
        .unwrap_or(false);
    if !matches {
        findings.push(
            Finding::file(
                "MEM006",
                "metadata-path-mismatch",
                Severity::Warning,
                &join_display(display_root, ".wiki-metadata.json"),
                "Recorded collection path is not the resolved wiki root",
            )
            .with_evidence("recorded", &recorded)
            .with_evidence("resolved", &wiki_root.to_string_lossy()),
        );
    }
}

fn lint_checkpoints(wiki_root: &Path, display_root: &str, findings: &mut Vec<Finding>) {
    let directory = wiki_root.join("checkpoints");
    let Ok(entries) = fs::read_dir(&directory) else {
        return;
    };
    let mut names: Vec<String> = entries
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("checkpoint-") && name.ends_with(".md"))
        .collect();
    names.sort();
    for name in names {
        if !is_canonical_checkpoint(&name) {
            findings.push(Finding::file(
                "MEM010",
                "legacy-checkpoint-filename",
                Severity::Warning,
                &join_display(display_root, &format!("checkpoints/{name}")),
                "Checkpoint should be named checkpoint-YYYY-MM-DD-HHMM.md",
            ));
        }
    }
}

/// `checkpoint-YYYY-MM-DD-HHMM.md` exactly; anything longer carries a legacy slug.
fn is_canonical_checkpoint(name: &str) -> bool {
    let stem = name.trim_end_matches(".md");
    let Some(rest) = stem.strip_prefix("checkpoint-") else {
        return false;
    };
    let parts: Vec<&str> = rest.split('-').collect();
    parts.len() == 4
        && [4, 2, 2, 4]
            .iter()
            .zip(&parts)
            .all(|(width, part)| part.len() == *width && part.chars().all(|c| c.is_ascii_digit()))
}

fn is_kebab_case(stem: &str) -> bool {
    !stem.is_empty()
        && stem
            .chars()
            .all(|value| value.is_ascii_lowercase() || value.is_ascii_digit() || value == '-')
}

/// A page whose body is only front matter, headings, and blank lines.
fn is_empty_page(path: &Path) -> bool {
    let Ok(content) = fs::read_to_string(path) else {
        return false;
    };
    body(&content)
        .lines()
        .all(|line| line.trim().is_empty() || line.trim_start().starts_with('#'))
}

fn front_matter(path: &Path) -> Vec<(String, String)> {
    let Ok(content) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut fields = Vec::new();
    let mut inside = false;
    for line in content.lines() {
        if line.trim_end() == "---" {
            if inside {
                break;
            }
            inside = true;
            continue;
        }
        if !inside {
            break;
        }
        if let Some((key, value)) = line.split_once(':') {
            let value = value.trim().trim_matches('"').trim_matches('\'');
            if !value.is_empty() {
                fields.push((key.trim().to_owned(), value.to_owned()));
            }
        }
    }
    fields
}

fn field(fields: &[(String, String)], name: &str) -> Option<String> {
    fields
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.clone())
}

fn body(content: &str) -> &str {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return content;
    }
    match trimmed[3..].find("\n---") {
        Some(end) => &trimmed[3 + end + 4..],
        None => content,
    }
}

fn json_string_field(content: &str, name: &str) -> Option<String> {
    let key = format!("\"{name}\"");
    let start = content.find(&key)? + key.len();
    let rest = content[start..]
        .trim_start()
        .strip_prefix(':')?
        .trim_start();
    let rest = rest.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].to_owned())
}

/// Index entries appear as a bare stem (`[[alpha-note]]`), a wiki-relative path
/// (`topics/auth`), or a workspace-relative one (`wiki/topics/auth`). All three
/// spellings name the same page.
fn references_page(reference: &str, page: &str, display_root: &str) -> bool {
    let reference = reference.trim_end_matches(".md");
    let page = page.trim_end_matches(".md");
    let unprefixed = display_root
        .is_empty()
        .then_some(reference)
        .or_else(|| reference.strip_prefix(&format!("{display_root}/")))
        .unwrap_or(reference);
    unprefixed == page || unprefixed == stem(page) || reference == stem(page)
}

fn has_overview_section(index: &str) -> bool {
    index.lines().any(|line| {
        line.trim_start()
            .to_ascii_lowercase()
            .starts_with("## overview")
    })
}

/// Workspace-relative `.md` pages that the root hub is expected to catalogue.
fn durable_pages(wiki_root: &Path) -> Vec<String> {
    let mut pages = Vec::new();
    collect_pages(wiki_root, wiki_root, &mut pages);
    pages.sort();
    pages
}

fn collect_pages(wiki_root: &Path, directory: &Path, pages: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            if !name.starts_with('.') && !EXCLUDED_DIRECTORIES.contains(&name.as_str()) {
                collect_pages(wiki_root, &path, pages);
            }
            continue;
        }
        if !name.ends_with(".md") || DURABLE_EXCLUSIONS.contains(&name.as_str()) {
            continue;
        }
        pages.push(relative(wiki_root, &path));
    }
}

/// Pages generated from another source of truth. They are linted for their own
/// integrity but are never expected in `index.md`.
fn is_derived(page: &str) -> bool {
    page.starts_with("code/") || page.starts_with("log-archive/")
}

const CODE_HUB: &str = "code/_index.md";

/// The code graph reaches the root through exactly one reserved hub page.
/// Ordinary code pages belong in that hub, never in `index.md`.
fn lint_code_hub(
    wiki_root: &Path,
    display_root: &str,
    pages: &[String],
    references: &[Reference],
    index_display: &str,
    findings: &mut Vec<Finding>,
) {
    let code_pages: Vec<&String> = pages
        .iter()
        .filter(|page| page.starts_with("code/") && *page != CODE_HUB)
        .collect();
    if code_pages.is_empty() {
        return;
    }

    let hub_display = join_display(display_root, CODE_HUB);
    let Ok(hub) = fs::read_to_string(wiki_root.join(CODE_HUB)) else {
        findings.push(
            Finding::file(
                "MEM014",
                "code-hub-missing",
                Severity::Warning,
                &hub_display,
                "Code pages exist without a `code/_index.md` hub",
            )
            .with_evidence("code_pages", &code_pages.len().to_string()),
        );
        return;
    };

    let root_links = references
        .iter()
        .filter(|reference| is_hub_reference(&reference.target, display_root))
        .count();
    if root_links != 1 {
        findings.push(
            Finding::file(
                "MEM015",
                "code-hub-not-root-linked",
                Severity::Warning,
                index_display,
                "`index.md` must link the code hub exactly once",
            )
            .with_evidence("links", &root_links.to_string()),
        );
    }

    // Dangling hub entries are LMD001's job; this rule only asks whether every
    // active code page is reachable from the hub.
    let listed = index_references(&hub);
    for page in &code_pages {
        let stem = stem(page);
        let present = listed.iter().any(|entry| {
            references_page(&entry.target, page, display_root)
                || entry.target.trim_end_matches(".md") == stem
        });
        if !present {
            findings.push(
                Finding::file(
                    "MEM016",
                    "code-hub-missing-page",
                    Severity::Warning,
                    &hub_display,
                    "Code page is not listed in the complete code hub",
                )
                .with_target(&stem),
            );
        }
    }
}

/// `code/_index`, `wiki/code/_index`, or the bare reserved stem, with or
/// without the `.md` suffix.
fn is_hub_reference(target: &str, display_root: &str) -> bool {
    let target = target.trim_end_matches(".md");
    let unprefixed = target
        .strip_prefix(&format!("{display_root}/"))
        .unwrap_or(target);
    unprefixed == "code/_index" || unprefixed == "_index"
}

/// Decide whether an index entry points at a code page, and say how we know.
/// An explicit `code/…` path is unambiguous; a bare stem only counts when it
/// resolves uniquely, or when the surrounding index section names the code graph.
fn resolve_code_page(
    reference: &Reference,
    pages: &[String],
    display_root: &str,
) -> Option<(String, &'static str)> {
    let target = reference.target.trim_end_matches(".md");
    let unprefixed = target
        .strip_prefix(&format!("{display_root}/"))
        .unwrap_or(target);

    // The reserved hub link is the one code entry the root index must carry.
    if is_hub_reference(&reference.target, display_root) {
        return None;
    }
    if unprefixed.starts_with("code/") {
        let page = format!("{unprefixed}.md");
        return pages.contains(&page).then_some((page, "explicit"));
    }
    if unprefixed.contains('/') {
        return None;
    }

    let matches: Vec<&String> = pages
        .iter()
        .filter(|page| stem(page) == unprefixed)
        .collect();
    let code: Vec<&&String> = matches.iter().filter(|page| is_derived(page)).collect();
    if code.is_empty() {
        return None;
    }
    if matches.len() == 1 {
        return Some((code[0].to_string(), "shorthand-unique"));
    }
    if reference.section.to_ascii_lowercase().contains("code") {
        return Some((code[0].to_string(), "section-context"));
    }
    None
}

/// An `index.md` entry plus the section heading it appeared under, which is what
/// disambiguates a shorthand stem shared by a prose page and a code page.
struct Reference {
    target: String,
    section: String,
}

/// `[[wikilink]]` targets and relative Markdown link destinations in `index.md`.
fn index_references(index: &str) -> Vec<Reference> {
    let mut references: Vec<Reference> = Vec::new();
    let mut section = String::new();
    for line in index.lines() {
        if let Some(heading) = line.trim_start().strip_prefix('#') {
            section = heading.trim_start_matches('#').trim().to_owned();
            continue;
        }
        let characters: Vec<char> = line.chars().collect();
        let mut position = 0;
        while position < characters.len() {
            if characters[position] == '[' && characters.get(position + 1) == Some(&'[') {
                if let Some(end) = find_from(&characters, position + 2, "]]") {
                    let target: String = characters[position + 2..end].iter().collect();
                    push_reference(&mut references, &target, &section);
                    position = end + 2;
                    continue;
                }
            }
            if characters[position] == '(' {
                if let Some(end) = find_from(&characters, position + 1, ")") {
                    let target: String = characters[position + 1..end].iter().collect();
                    if target.ends_with(".md") && !target.contains("://") {
                        push_reference(&mut references, &target, &section);
                    }
                    position = end + 1;
                    continue;
                }
            }
            position += 1;
        }
    }
    references
}

fn push_reference(references: &mut Vec<Reference>, target: &str, section: &str) {
    let target = target
        .split('|')
        .next()
        .unwrap_or_default()
        .split('#')
        .next()
        .unwrap_or_default()
        .trim();
    if target.is_empty() {
        return;
    }
    let target = target.strip_prefix("./").unwrap_or(target).to_owned();
    if references.iter().any(|entry| entry.target == target) {
        return;
    }
    references.push(Reference {
        target,
        section: section.to_owned(),
    });
}

fn find_from(haystack: &[char], start: usize, needle: &str) -> Option<usize> {
    let needle: Vec<char> = needle.chars().collect();
    (start..haystack.len().saturating_sub(needle.len() - 1))
        .find(|index| haystack[*index..*index + needle.len()] == needle[..])
}

fn stem(page: &str) -> String {
    page.rsplit('/')
        .next()
        .unwrap_or(page)
        .trim_end_matches(".md")
        .to_owned()
}

fn relative(base: &Path, path: &Path) -> String {
    let base = fs::canonicalize(base).unwrap_or_else(|_| base.to_path_buf());
    let path = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    path.strip_prefix(&base)
        .map(|value| value.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| path.to_string_lossy().replace('\\', "/"))
}

fn join_display(root: &str, page: &str) -> String {
    if root.is_empty() {
        page.to_owned()
    } else {
        format!("{root}/{page}")
    }
}
