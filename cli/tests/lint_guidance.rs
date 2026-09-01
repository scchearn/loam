use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const MAP_OPEN: &str =
    "<!-- loam:memory-map · generated from wiki/index.md · do not edit by hand -->";
const MAP_CLOSE: &str = "<!-- /loam:memory-map -->";

fn temporary_workspace(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be after epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("loam-gdn-{label}-{nonce}"));
    fs::create_dir_all(&path).expect("temporary workspace should be created");
    path
}

fn write(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("parent directory should be created");
    }
    fs::write(path, content).expect("file should be written");
}

/// A wiki-bearing workspace with two topics and one entity.
fn workspace_with_wiki(label: &str) -> PathBuf {
    let workspace = temporary_workspace(label);
    write(
        &workspace.join("wiki/index.md"),
        "# Index\n\n## Overview\n\nThe hub.\n",
    );
    write(&workspace.join("wiki/topics/alpha.md"), "# Alpha\n");
    write(&workspace.join("wiki/topics/beta.md"), "# Beta\n");
    write(&workspace.join("wiki/entities/gamma.md"), "# Gamma\n");
    workspace
}

fn fresh_region() -> String {
    format!("{MAP_OPEN}\nTopics (2): alpha · beta\nEntities (1): gamma\n{MAP_CLOSE}")
}

fn lint(workspace: &Path, extra: &[&str]) -> (i32, String) {
    let binary = std::env::var("CARGO_BIN_EXE_loam").expect("cargo should provide the loam binary");
    let mut arguments = vec!["lint", "--only", "guidance"];
    arguments.extend_from_slice(extra);
    arguments.push(workspace.to_str().unwrap());
    let output = Command::new(binary)
        .args(&arguments)
        .output()
        .expect("loam should run");
    assert!(
        output.stderr.is_empty(),
        "unexpected stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    (
        output.status.code().expect("loam should exit normally"),
        String::from_utf8(output.stdout).expect("findings should be UTF-8"),
    )
}

fn rules(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| {
            let marker = "\"rule\":\"";
            let start = line.find(marker).expect("finding should carry a rule") + marker.len();
            let rest = &line[start..];
            rest[..rest.find('"').expect("rule should be terminated")].to_owned()
        })
        .collect()
}

#[test]
fn guidance_reports_a_missing_map_in_a_wiki_bearing_workspace() {
    let workspace = workspace_with_wiki("missing");
    write(
        &workspace.join("AGENTS.md"),
        "# Guide\n\n## Commands\n\nRun it.\n",
    );

    let (code, stdout) = lint(&workspace, &[]);
    assert_eq!(rules(&stdout), vec!["GDN001".to_owned()]);
    assert!(stdout.contains("\"domain\":\"guidance\""));
    assert!(stdout.contains("\"severity\":\"warning\""));
    assert_eq!(code, 2);
}

#[test]
fn guidance_stays_silent_without_a_wiki() {
    let workspace = temporary_workspace("no-wiki");
    write(&workspace.join("AGENTS.md"), "# Guide\n");

    let (code, stdout) = lint(&workspace, &[]);
    assert_eq!(stdout, "");
    assert_eq!(code, 0);
}

#[test]
fn guidance_stays_silent_for_a_current_map() {
    let workspace = workspace_with_wiki("fresh");
    write(
        &workspace.join("AGENTS.md"),
        &format!("# Guide\n\n## Memory\n\nProse.\n\n{}\n", fresh_region()),
    );

    let (code, stdout) = lint(&workspace, &[]);
    assert_eq!(stdout, "");
    assert_eq!(code, 0);
}

#[test]
fn guidance_reports_a_stale_map_when_a_page_is_added() {
    let workspace = workspace_with_wiki("stale-added");
    write(
        &workspace.join("AGENTS.md"),
        &format!("# Guide\n\n{}\n", fresh_region()),
    );
    write(&workspace.join("wiki/topics/delta.md"), "# Delta\n");

    let (code, stdout) = lint(&workspace, &[]);
    assert_eq!(rules(&stdout), vec!["GDN002".to_owned()]);
    assert!(stdout.contains("\"added\":\"delta\""), "{stdout}");
    assert_eq!(code, 2);
}

#[test]
fn guidance_reports_a_stale_map_when_a_page_is_removed() {
    let workspace = workspace_with_wiki("stale-removed");
    write(
        &workspace.join("AGENTS.md"),
        &format!("# Guide\n\n{}\n", fresh_region()),
    );
    fs::remove_file(workspace.join("wiki/entities/gamma.md")).expect("page should be removed");

    let (code, stdout) = lint(&workspace, &[]);
    assert_eq!(rules(&stdout), vec!["GDN002".to_owned()]);
    assert!(stdout.contains("\"removed\":\"gamma\""), "{stdout}");
    assert_eq!(code, 2);
}

#[test]
fn guidance_ignores_a_cosmetic_reflow_of_the_region() {
    let workspace = workspace_with_wiki("reflow");
    write(
        &workspace.join("AGENTS.md"),
        &format!("# Guide\n\n{MAP_OPEN}\n\nTopics (2):  alpha ·  beta\nEntities (1):   gamma\n\n{MAP_CLOSE}\n"),
    );

    let (code, stdout) = lint(&workspace, &[]);
    assert_eq!(stdout, "");
    assert_eq!(code, 0);
}

#[test]
fn guidance_reports_a_drifted_claude_shim() {
    let workspace = workspace_with_wiki("shim");
    write(
        &workspace.join("AGENTS.md"),
        &format!("# Guide\n\n{}\n", fresh_region()),
    );
    write(&workspace.join("CLAUDE.md"), "# Project\n\nOwn content.\n");

    let (code, stdout) = lint(&workspace, &[]);
    assert_eq!(rules(&stdout), vec!["GDN003".to_owned()]);
    assert_eq!(code, 2);
}

#[test]
fn guidance_accepts_an_intact_claude_shim() {
    let workspace = workspace_with_wiki("shim-ok");
    write(
        &workspace.join("AGENTS.md"),
        &format!("# Guide\n\n{}\n", fresh_region()),
    );
    write(&workspace.join("CLAUDE.md"), "@AGENTS.md\n");

    let (code, stdout) = lint(&workspace, &[]);
    assert_eq!(stdout, "");
    assert_eq!(code, 0);
}

#[test]
fn guidance_fix_inserts_the_section_and_preserves_the_existing_file() {
    let workspace = workspace_with_wiki("fix-insert");
    let original = "# Guide\n\n## Commands\n\nRun it.\n";
    write(&workspace.join("AGENTS.md"), original);

    let (code, stdout) = lint(&workspace, &["--fix"]);
    assert_eq!(stdout, "");
    assert_eq!(code, 0);

    let updated =
        fs::read_to_string(workspace.join("AGENTS.md")).expect("guide should be readable");
    assert!(updated.starts_with(original), "{updated}");
    assert!(updated.contains("## Memory"));
    assert!(updated.contains(&fresh_region()), "{updated}");
}

#[test]
fn guidance_fix_replaces_only_the_bytes_between_the_markers() {
    let workspace = workspace_with_wiki("fix-region");
    let head = "# Guide\n\n## Memory\n\nHand-written prose.\n\n";
    let tail = "\n\n## Commands\n\nRun it.\n";
    write(
        &workspace.join("AGENTS.md"),
        &format!("{head}{MAP_OPEN}\nTopics (1): stale-only\n{MAP_CLOSE}{tail}"),
    );

    let (code, _) = lint(&workspace, &["--fix"]);
    assert_eq!(code, 0);

    let updated =
        fs::read_to_string(workspace.join("AGENTS.md")).expect("guide should be readable");
    assert_eq!(updated, format!("{head}{}{tail}", fresh_region()));
}

#[test]
fn guidance_fix_is_idempotent_and_leaves_the_domain_clean() {
    let workspace = workspace_with_wiki("fix-twice");
    write(&workspace.join("AGENTS.md"), "# Guide\n");

    lint(&workspace, &["--fix"]);
    let once = fs::read_to_string(workspace.join("AGENTS.md")).expect("guide should be readable");
    lint(&workspace, &["--fix"]);
    let twice = fs::read_to_string(workspace.join("AGENTS.md")).expect("guide should be readable");
    assert_eq!(once, twice);

    let (code, stdout) = lint(&workspace, &[]);
    assert_eq!(stdout, "");
    assert_eq!(code, 0);
}

#[test]
fn guidance_fix_preserves_crlf_line_endings() {
    let workspace = workspace_with_wiki("fix-crlf");
    write(
        &workspace.join("AGENTS.md"),
        "# Guide\r\n\r\n## Commands\r\n",
    );

    let (code, _) = lint(&workspace, &["--fix"]);
    assert_eq!(code, 0);

    let updated =
        fs::read_to_string(workspace.join("AGENTS.md")).expect("guide should be readable");
    assert!(!updated.replace("\r\n", "").contains('\n'), "{updated:?}");
    assert!(updated.contains(MAP_OPEN));
}

#[test]
fn guidance_fix_without_a_wiki_writes_nothing() {
    let workspace = temporary_workspace("fix-no-wiki");
    let original = "# Guide\n\n## Commands\n\nRun it.\n";
    write(&workspace.join("AGENTS.md"), original);

    let (code, _) = lint(&workspace, &["--fix"]);
    assert_eq!(code, 0);
    assert_eq!(
        fs::read_to_string(workspace.join("AGENTS.md")).expect("guide should be readable"),
        original
    );
}

#[test]
fn guidance_fix_never_writes_to_the_claude_shim() {
    let workspace = workspace_with_wiki("fix-shim");
    write(&workspace.join("AGENTS.md"), "# Guide\n");
    write(&workspace.join("CLAUDE.md"), "# Drifted\n");

    lint(&workspace, &["--fix"]);
    assert_eq!(
        fs::read_to_string(workspace.join("CLAUDE.md")).expect("shim should be readable"),
        "# Drifted\n"
    );
}

#[test]
fn guidance_only_selects_the_guidance_domain() {
    let workspace = workspace_with_wiki("only");
    // `alpha`/`beta`/`gamma` are unreferenced by `index.md`, which the memory
    // domain would flag; `--only guidance` must not surface those.
    write(
        &workspace.join("AGENTS.md"),
        &format!("{}\n", fresh_region()),
    );

    let (code, stdout) = lint(&workspace, &[]);
    assert_eq!(stdout, "");
    assert_eq!(code, 0);
}

/// The reviewer's blocker repro: an unbalanced opening marker must never let a
/// later `--fix` swallow the prose below it.
#[test]
fn guidance_fix_preserves_prose_below_an_orphan_opening_marker() {
    let workspace = workspace_with_wiki("orphan-open");
    let original = format!(
        "# Guide\n\n{MAP_OPEN}\nTopics (1): alpha\n\n## Commands\n\nRun it.\n\n## Style\n\nTabs.\n"
    );
    write(&workspace.join("AGENTS.md"), &original);

    lint(&workspace, &["--fix"]);
    let once = fs::read_to_string(workspace.join("AGENTS.md")).expect("guide should be readable");
    lint(&workspace, &["--fix"]);
    let twice = fs::read_to_string(workspace.join("AGENTS.md")).expect("guide should be readable");

    assert_eq!(once, twice, "second --fix must be a no-op");
    assert!(
        once.starts_with(&original),
        "prose above was rewritten: {once}"
    );
    assert!(once.contains("## Commands\n\nRun it."), "{once}");
    assert!(once.contains("## Style\n\nTabs."), "{once}");
    assert_eq!(once.matches(MAP_CLOSE).count(), 1);
}

/// A page pushed out of view by truncation was not removed; the finding must
/// not name it.
#[test]
fn guidance_stale_never_blames_a_page_that_still_exists() {
    let workspace = workspace_with_wiki("truncated-blame");
    fs::remove_file(workspace.join("wiki/topics/alpha.md")).expect("page should be removed");
    fs::remove_file(workspace.join("wiki/topics/beta.md")).expect("page should be removed");
    fs::remove_file(workspace.join("wiki/entities/gamma.md")).expect("page should be removed");
    for index in 1..=31 {
        write(
            &workspace.join(format!("wiki/topics/topic-{index:02}.md")),
            "# Topic\n",
        );
    }
    write(&workspace.join("AGENTS.md"), "# Guide\n");
    lint(&workspace, &["--fix"]);

    write(&workspace.join("wiki/topics/aaa.md"), "# Aaa\n");
    let (code, stdout) = lint(&workspace, &[]);

    assert_eq!(rules(&stdout), vec!["GDN002".to_owned()]);
    assert_eq!(code, 2);
    assert!(!stdout.contains("topic-30"), "blamed a live page: {stdout}");
    assert!(!stdout.contains("\"removed\""), "{stdout}");
    assert!(stdout.contains("\"topics\":\"31 → 32\""), "{stdout}");
}

/// A page added past the truncation threshold is invisible to the slug diff;
/// the finding must still say something actionable.
#[test]
fn guidance_stale_is_actionable_past_the_truncation_threshold() {
    let workspace = workspace_with_wiki("truncated-tail");
    fs::remove_file(workspace.join("wiki/entities/gamma.md")).expect("page should be removed");
    fs::remove_file(workspace.join("wiki/topics/alpha.md")).expect("page should be removed");
    fs::remove_file(workspace.join("wiki/topics/beta.md")).expect("page should be removed");
    for index in 1..=31 {
        write(
            &workspace.join(format!("wiki/topics/topic-{index:02}.md")),
            "# Topic\n",
        );
    }
    write(&workspace.join("AGENTS.md"), "# Guide\n");
    lint(&workspace, &["--fix"]);

    write(&workspace.join("wiki/topics/zzz.md"), "# Zzz\n");
    let (_, stdout) = lint(&workspace, &[]);

    assert_eq!(rules(&stdout), vec!["GDN002".to_owned()]);
    assert!(
        !stdout.contains("\"evidence\":{}"),
        "empty evidence: {stdout}"
    );
    assert!(stdout.contains("\"topics\":\"31 → 32\""), "{stdout}");
}

#[test]
fn guidance_fix_creates_a_missing_guidance_file() {
    let workspace = workspace_with_wiki("fix-create");

    let (code, stdout) = lint(&workspace, &[]);
    assert_eq!(rules(&stdout), vec!["GDN001".to_owned()]);
    assert!(stdout.contains("`AGENTS.md` does not exist"), "{stdout}");
    assert_eq!(code, 2);

    let (code, stdout) = lint(&workspace, &["--fix"]);
    assert_eq!(stdout, "");
    assert_eq!(code, 0);
    assert_eq!(
        fs::read_to_string(workspace.join("AGENTS.md")).expect("guide should be readable"),
        format!(
            "## Memory\n\nThis project keeps a **Loam memory** — agent-owned markdown in `wiki/`. Consult it\nbefore non-trivial work and keep it current. Start at `wiki/index.md`.\n\n{}\n",
            fresh_region()
        )
    );
}

/// Mirror of the orphan-open blocker: a stray closing marker must not make
/// every `--fix` append another section.
#[test]
fn guidance_fix_is_idempotent_below_an_orphan_closing_marker() {
    let workspace = workspace_with_wiki("orphan-close");
    let original = format!("# Guide\n\n{MAP_CLOSE}\n\n## Commands\n\nHuman prose.\n");
    write(&workspace.join("AGENTS.md"), &original);

    lint(&workspace, &["--fix"]);
    let once = fs::read_to_string(workspace.join("AGENTS.md")).expect("guide should be readable");
    lint(&workspace, &["--fix"]);
    lint(&workspace, &["--fix"]);
    let thrice = fs::read_to_string(workspace.join("AGENTS.md")).expect("guide should be readable");

    assert_eq!(once, thrice, "repeated --fix must not append again");
    assert!(once.starts_with(&original), "prose was rewritten: {once}");
    assert_eq!(once.matches("## Memory").count(), 1, "{once}");
    assert_eq!(once.matches(MAP_OPEN).count(), 1, "{once}");

    let (code, stdout) = lint(&workspace, &[]);
    assert_eq!(stdout, "", "the finding never cleared");
    assert_eq!(code, 0);
}
