use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const NOW: &str = "2026-07-20 12:00 +02:00";

fn temporary_workspace(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be after epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("loam-mem-{label}-{nonce}"));
    fs::create_dir_all(&path).expect("temporary workspace should be created");
    path
}

fn write(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("parent directory should be created");
    }
    fs::write(path, content).expect("file should be written");
}

/// A wiki that is healthy for every rule under test, so each fixture only has to
/// introduce the one defect it cares about.
fn healthy_wiki(workspace: &Path) {
    write(
        &workspace.join("wiki/index.md"),
        "# Index\n\n## Overview\n\nThe hub.\n\n- [[alpha-note]]\n",
    );
    write(
        &workspace.join("wiki/alpha-note.md"),
        "# Alpha Note\n\nBody text.\n",
    );
}

fn lint(workspace: &Path) -> (i32, String) {
    let binary = std::env::var("CARGO_BIN_EXE_loam").expect("cargo should provide the loam binary");
    let output = Command::new(binary)
        .args([
            "lint",
            "--only",
            "memory",
            workspace.to_str().unwrap(),
            "--now",
            NOW,
        ])
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
fn healthy_wiki_reports_nothing_and_exits_zero() {
    let workspace = temporary_workspace("healthy");
    healthy_wiki(&workspace);

    let (code, stdout) = lint(&workspace);
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");

    assert_eq!(stdout, "", "healthy wiki should emit no findings");
    assert_eq!(code, 0, "healthy wiki should exit 0");
}

#[test]
fn legacy_overview_is_reported_with_the_versioned_schema() {
    let workspace = temporary_workspace("overview");
    healthy_wiki(&workspace);
    write(
        &workspace.join("wiki/overview.md"),
        "# Overview\n\nLegacy.\n",
    );

    let (code, stdout) = lint(&workspace);
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");

    assert_eq!(code, 2, "findings should exit 2: {stdout}");
    assert_eq!(rules(&stdout), vec!["MEM002"], "output: {stdout}");
    let line = stdout.lines().next().expect("one finding");
    for field in [
        "\"schema_version\":\"1\"",
        "\"domain\":\"memory\"",
        "\"rule\":\"MEM002\"",
        "\"rule_names\":[\"MEM002\",\"legacy-overview-present\"]",
        "\"severity\":\"warning\"",
        "\"file\":\"wiki/overview.md\"",
        "\"line\":0",
        "\"column\":0",
        "\"evidence\":{",
        "\"target\":null",
    ] {
        assert!(line.contains(field), "missing {field} in: {line}");
    }
}

#[test]
fn missing_overview_section_in_index_is_reported() {
    let workspace = temporary_workspace("no-overview");
    healthy_wiki(&workspace);
    write(
        &workspace.join("wiki/index.md"),
        "# Index\n\n- [[alpha-note]]\n",
    );

    let (code, stdout) = lint(&workspace);
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");

    assert_eq!(code, 2, "output: {stdout}");
    assert_eq!(rules(&stdout), vec!["MEM001"], "output: {stdout}");
}

#[test]
fn index_membership_gaps_and_dangling_entries_are_reported() {
    let workspace = temporary_workspace("index");
    healthy_wiki(&workspace);
    write(
        &workspace.join("wiki/orphan-note.md"),
        "# Orphan\n\nBody.\n",
    );
    write(
        &workspace.join("wiki/index.md"),
        "# Index\n\n## Overview\n\nThe hub.\n\n- [[alpha-note]]\n- [[ghost-note]]\n",
    );

    let (code, stdout) = lint(&workspace);
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");

    assert_eq!(code, 2, "output: {stdout}");
    // Emission order is the sort contract: file first, so index.md precedes orphan-note.md.
    assert_eq!(rules(&stdout), vec!["MEM004", "MEM003"], "output: {stdout}");
    assert!(
        stdout.contains("\"target\":\"ghost-note\""),
        "dangling entry should name its target: {stdout}"
    );
}

#[test]
fn workspace_relative_index_references_resolve() {
    let workspace = temporary_workspace("workspace-relative");
    healthy_wiki(&workspace);
    write(
        &workspace.join("wiki/topics/auth.md"),
        "# Auth\n\nBody text.\n",
    );
    write(
        &workspace.join("wiki/index.md"),
        "# Index\n\n## Overview\n\nHub.\n\n- [[alpha-note]]\n- [[wiki/topics/auth|auth]]\n",
    );

    let (code, stdout) = lint(&workspace);
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");

    assert_eq!(
        stdout, "",
        "a `wiki/`-prefixed index reference should resolve"
    );
    assert_eq!(code, 0);
}

#[test]
fn derived_pages_are_exempt_from_index_membership() {
    let workspace = temporary_workspace("derived");
    healthy_wiki(&workspace);
    write(
        &workspace.join("wiki/code/lexer.md"),
        "---\nsource_path: src/lexer.rs\nsource_size: 10\ncontent_hash: abc\n---\n\n# Lexer\n\nBody.\n",
    );
    write(
        &workspace.join("wiki/log-archive/2026-06.md"),
        "# June\n\nRotated log body.\n",
    );

    let (code, stdout) = lint(&workspace);
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");

    // The hub obligation (MEM014) is the only thing derived pages owe here.
    assert!(
        !stdout.contains("MEM003"),
        "derived pages must not require a root index entry: {stdout}"
    );
    assert_eq!(rules(&stdout), vec!["MEM014"], "output: {stdout}");
    assert_eq!(code, 2);
}

#[test]
fn prose_pages_still_require_an_index_entry() {
    let workspace = temporary_workspace("prose-still");
    healthy_wiki(&workspace);
    write(&workspace.join("wiki/topics/auth.md"), "# Auth\n\nBody.\n");
    write(
        &workspace.join("wiki/root-note.md"),
        "# Root Note\n\nBody.\n",
    );

    let (code, stdout) = lint(&workspace);
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");

    assert_eq!(code, 2, "output: {stdout}");
    assert_eq!(rules(&stdout), vec!["MEM003", "MEM003"], "output: {stdout}");
    assert!(
        stdout.contains("root-note"),
        "root prose stays eligible: {stdout}"
    );
}

#[test]
fn indexed_code_page_is_reported_for_both_link_spellings() {
    let workspace = temporary_workspace("code-indexed");
    healthy_wiki(&workspace);
    for (path, source) in [("lexer", "src/lexer.rs"), ("parser", "src/parser.rs")] {
        write(
            &workspace.join(format!("wiki/code/{path}.md")),
            &format!(
                "---\nsource_path: {source}\nsource_size: 10\ncontent_hash: abc\n---\n\n# {path}\n\nBody.\n"
            ),
        );
    }
    write(
        &workspace.join("wiki/index.md"),
        "# Index\n\n## Overview\n\nHub.\n\n- [[alpha-note]]\n- [[lexer]]\n- [[wiki/code/parser|parser]]\n",
    );

    let (code, stdout) = lint(&workspace);
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");

    assert_eq!(code, 2, "output: {stdout}");
    assert_eq!(
        rules(&stdout),
        vec!["MEM014", "MEM013", "MEM013"],
        "output: {stdout}"
    );
    assert!(
        stdout.contains("\"resolution\":\"shorthand-unique\""),
        "shorthand resolution should be explained: {stdout}"
    );
    assert!(
        stdout.contains("\"resolution\":\"explicit\""),
        "explicit resolution should be explained: {stdout}"
    );
    assert!(
        stdout.contains("\"code_page\":\"code/lexer.md\""),
        "evidence should name the resolved page: {stdout}"
    );
}

#[test]
fn ambiguous_shorthand_does_not_produce_a_code_page_finding() {
    let workspace = temporary_workspace("ambiguous");
    healthy_wiki(&workspace);
    write(
        &workspace.join("wiki/code/auth.md"),
        "---\nsource_path: src/auth.rs\nsource_size: 10\ncontent_hash: abc\n---\n\n# Auth\n\nBody.\n",
    );
    write(
        &workspace.join("wiki/topics/auth.md"),
        "# Auth\n\nProse body.\n",
    );
    write(
        &workspace.join("wiki/index.md"),
        "# Index\n\n## Overview\n\nHub.\n\n- [[alpha-note]]\n- [[auth]]\n",
    );

    let (code, stdout) = lint(&workspace);
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");

    assert!(
        !stdout.contains("MEM013"),
        "an ambiguous stem must not be called a code page: {stdout}"
    );
    assert_eq!(rules(&stdout), vec!["MEM014"], "output: {stdout}");
    assert_eq!(code, 2, "output: {stdout}");
}

#[test]
fn ambiguous_shorthand_under_a_code_section_is_resolved_by_context() {
    let workspace = temporary_workspace("section-context");
    healthy_wiki(&workspace);
    write(
        &workspace.join("wiki/code/auth.md"),
        "---\nsource_path: src/auth.rs\nsource_size: 10\ncontent_hash: abc\n---\n\n# Auth\n\nBody.\n",
    );
    write(
        &workspace.join("wiki/topics/auth.md"),
        "# Auth\n\nProse body.\n",
    );
    write(
        &workspace.join("wiki/index.md"),
        "# Index\n\n## Overview\n\nHub.\n\n- [[alpha-note]]\n- [[topics/auth]]\n\n## Code graph\n\n- [[auth]]\n",
    );

    let (code, stdout) = lint(&workspace);
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");

    assert_eq!(code, 2, "output: {stdout}");
    assert_eq!(rules(&stdout), vec!["MEM014", "MEM013"], "output: {stdout}");
    assert!(
        stdout.contains("\"resolution\":\"section-context\""),
        "section resolution should be explained: {stdout}"
    );
}

/// A wiki whose code graph is fully compliant: complete hub, single root link.
fn hubbed_wiki(workspace: &Path) {
    write(
        &workspace.join("wiki/index.md"),
        "# Index\n\n## Overview\n\nHub.\n\n- [[alpha-note]]\n- [[code/_index|Code graph]]\n",
    );
    write(
        &workspace.join("wiki/alpha-note.md"),
        "# Alpha Note\n\nBody text.\n",
    );
    for (page, source) in [("lexer", "src/lexer.rs"), ("parser", "src/parser.rs")] {
        write(
            &workspace.join(format!("wiki/code/{page}.md")),
            &format!(
                "---\nsource_path: {source}\nsource_size: 10\ncontent_hash: abc\n---\n\n# {page}\n\nBody.\n"
            ),
        );
    }
    write(
        &workspace.join("wiki/code/_index.md"),
        "# Code graph\n\n- [[lexer]]\n- [[parser]]\n",
    );
}

#[test]
fn compliant_code_hub_reports_nothing() {
    let workspace = temporary_workspace("hub-clean");
    hubbed_wiki(&workspace);

    let (code, stdout) = lint(&workspace);
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");

    assert_eq!(stdout, "", "a compliant code hub is silent");
    assert_eq!(code, 0);
}

#[test]
fn reserved_hub_link_is_exempt_from_the_code_page_rule() {
    let workspace = temporary_workspace("hub-exempt");
    hubbed_wiki(&workspace);
    write(
        &workspace.join("wiki/index.md"),
        "# Index\n\n## Overview\n\nHub.\n\n- [[alpha-note]]\n- [[code/_index|Code graph]]\n- [[lexer]]\n",
    );

    let (code, stdout) = lint(&workspace);
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");

    assert_eq!(code, 2, "output: {stdout}");
    assert_eq!(rules(&stdout), vec!["MEM013"], "output: {stdout}");
    assert!(
        stdout.contains("\"target\":\"lexer\""),
        "only the ordinary code link should be flagged: {stdout}"
    );
    assert!(
        stdout.contains("code hub"),
        "the message should direct the agent to the hub: {stdout}"
    );
}

#[test]
fn missing_code_hub_is_reported() {
    let workspace = temporary_workspace("hub-missing");
    hubbed_wiki(&workspace);
    fs::remove_file(workspace.join("wiki/code/_index.md")).expect("hub should be removed");

    let (code, stdout) = lint(&workspace);
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");

    assert_eq!(code, 2, "output: {stdout}");
    assert!(
        rules(&stdout).contains(&"MEM014".to_owned()),
        "output: {stdout}"
    );
    assert!(
        stdout.contains("\"code_pages\":\"2\""),
        "evidence should count the orphaned code pages: {stdout}"
    );
}

#[test]
fn code_hub_not_linked_from_root_is_reported() {
    let workspace = temporary_workspace("hub-unlinked");
    hubbed_wiki(&workspace);
    write(
        &workspace.join("wiki/index.md"),
        "# Index\n\n## Overview\n\nHub.\n\n- [[alpha-note]]\n",
    );

    let (code, stdout) = lint(&workspace);
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");

    assert_eq!(code, 2, "output: {stdout}");
    assert_eq!(rules(&stdout), vec!["MEM015"], "output: {stdout}");
}

#[test]
fn duplicate_root_hub_links_are_reported() {
    let workspace = temporary_workspace("hub-duplicated");
    hubbed_wiki(&workspace);
    write(
        &workspace.join("wiki/index.md"),
        "# Index\n\n## Overview\n\nHub.\n\n- [[alpha-note]]\n- [[code/_index|Code graph]]\n\n## Code\n\n- [[wiki/code/_index|Code graph again]]\n",
    );

    let (code, stdout) = lint(&workspace);
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");

    assert_eq!(code, 2, "output: {stdout}");
    assert_eq!(rules(&stdout), vec!["MEM015"], "output: {stdout}");
    assert!(
        stdout.contains("\"links\":\"2\""),
        "evidence should count the root links: {stdout}"
    );
}

#[test]
fn code_pages_absent_from_the_hub_are_reported() {
    let workspace = temporary_workspace("hub-incomplete");
    hubbed_wiki(&workspace);
    write(
        &workspace.join("wiki/code/_index.md"),
        "# Code graph\n\n- [[lexer]]\n",
    );

    let (code, stdout) = lint(&workspace);
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");

    assert_eq!(code, 2, "output: {stdout}");
    assert_eq!(rules(&stdout), vec!["MEM016"], "output: {stdout}");
    assert!(
        stdout.contains("\"target\":\"parser\""),
        "the absent page should be named: {stdout}"
    );
    assert!(
        !stdout.contains("\"target\":\"_index\""),
        "the hub must not demand its own membership: {stdout}"
    );
}

#[test]
fn wiki_without_code_pages_needs_no_hub() {
    let workspace = temporary_workspace("hub-not-needed");
    healthy_wiki(&workspace);

    let (code, stdout) = lint(&workspace);
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");

    assert_eq!(stdout, "", "no code pages means no hub obligations");
    assert_eq!(code, 0);
}

#[test]
fn misplaced_obsidian_config_is_reported() {
    let workspace = temporary_workspace("obsidian");
    healthy_wiki(&workspace);
    fs::create_dir_all(workspace.join("wiki/.obsidian")).expect("config dir should be created");

    let (code, stdout) = lint(&workspace);
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");

    assert_eq!(code, 2, "output: {stdout}");
    assert_eq!(rules(&stdout), vec!["MEM005"], "output: {stdout}");
}

#[test]
fn metadata_pointing_elsewhere_is_reported() {
    let workspace = temporary_workspace("metadata");
    healthy_wiki(&workspace);
    write(
        &workspace.join("wiki/.wiki-metadata.json"),
        "{\"retrieval\":{\"collection_path\":\"/somewhere/else\",\"collection_name\":\"x\"}}\n",
    );

    let (code, stdout) = lint(&workspace);
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");

    assert_eq!(code, 2, "output: {stdout}");
    assert_eq!(rules(&stdout), vec!["MEM006"], "output: {stdout}");
}

#[test]
fn stranded_code_pages_and_legacy_hash_fields_are_reported() {
    let workspace = temporary_workspace("code-pages");
    healthy_wiki(&workspace);
    write(
        &workspace.join("wiki/entities/parser.md"),
        "---\nsource_path: src/parser.rs\nsource_size: 120\ncontent_hash: abc123\n---\n\n# Parser\n\nBody.\n",
    );
    write(
        &workspace.join("wiki/code/lexer.md"),
        "---\nsource_path: src/lexer.rs\n---\n\n# Lexer\n\nBody.\n",
    );
    write(
        &workspace.join("wiki/index.md"),
        "# Index\n\n## Overview\n\nHub.\n\n- [[alpha-note]]\n- [[parser]]\n- [[lexer]]\n",
    );

    let (code, stdout) = lint(&workspace);
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");

    assert_eq!(code, 2, "output: {stdout}");
    // The fixture indexes `[[lexer]]`, a code page, so MEM013 fires alongside.
    assert_eq!(
        rules(&stdout),
        vec!["MEM014", "MEM008", "MEM007", "MEM013"],
        "output: {stdout}"
    );
    assert!(
        stdout.contains("\"severity\":\"info\""),
        "legacy hash fields are informational: {stdout}"
    );
}

#[test]
fn filename_and_checkpoint_conventions_are_reported() {
    let workspace = temporary_workspace("names");
    healthy_wiki(&workspace);
    write(
        &workspace.join("wiki/Not Kebab.md"),
        "# Not Kebab\n\nBody.\n",
    );
    write(
        &workspace.join("wiki/checkpoints/checkpoint-2026-07-20-1030-some-slug.md"),
        "# Checkpoint\n\nBody.\n",
    );
    write(
        &workspace.join("wiki/checkpoints/checkpoint-2026-07-20-1130.md"),
        "# Checkpoint\n\nBody.\n",
    );
    write(
        &workspace.join("wiki/index.md"),
        "# Index\n\n## Overview\n\nHub.\n\n- [[alpha-note]]\n- [[Not Kebab]]\n",
    );

    let (code, stdout) = lint(&workspace);
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");

    assert_eq!(code, 2, "output: {stdout}");
    assert_eq!(rules(&stdout), vec!["MEM009", "MEM010"], "output: {stdout}");
}

#[test]
fn empty_pages_and_oversized_logs_are_reported() {
    let workspace = temporary_workspace("empty-page");
    healthy_wiki(&workspace);
    write(
        &workspace.join("wiki/placeholder.md"),
        "---\ntitle: Placeholder\n---\n\n# Placeholder\n\n## Notes\n",
    );
    write(
        &workspace.join("wiki/log.md"),
        &"## [2026-07-20] entry\n".repeat(501),
    );
    write(
        &workspace.join("wiki/index.md"),
        "# Index\n\n## Overview\n\nHub.\n\n- [[alpha-note]]\n- [[placeholder]]\n",
    );

    let (code, stdout) = lint(&workspace);
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");

    assert_eq!(code, 2, "output: {stdout}");
    assert_eq!(rules(&stdout), vec!["MEM012", "MEM011"], "output: {stdout}");
}

#[test]
fn repeated_runs_are_byte_identical() {
    let workspace = temporary_workspace("determinism");
    healthy_wiki(&workspace);
    write(
        &workspace.join("wiki/overview.md"),
        "# Overview\n\nLegacy.\n",
    );
    write(
        &workspace.join("wiki/orphan-note.md"),
        "# Orphan\n\nBody.\n",
    );

    let first = lint(&workspace);
    let second = lint(&workspace);
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");

    assert_eq!(first.0, second.0);
    assert_eq!(first.1, second.1);
}

#[test]
fn no_wiki_but_goals_present_still_lints_goals() {
    let workspace = temporary_workspace("goals-only");
    write(
        &workspace.join("goals/INDEX.md"),
        "# Goals\n\n| Slug | Title | Status |\n|---|---|---|\n",
    );

    let (code, stdout) = lint(&workspace);
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");

    assert_eq!(code, 0, "an empty goals dir is healthy: {stdout}");
    assert_eq!(stdout, "", "output: {stdout}");
}

#[test]
fn workspace_with_neither_wiki_nor_goals_exits_zero_quietly() {
    let workspace = temporary_workspace("empty");

    let (code, stdout) = lint(&workspace);
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");

    assert_eq!(code, 0);
    assert_eq!(stdout, "");
}
