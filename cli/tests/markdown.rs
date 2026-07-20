use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

fn temporary_workspace() -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be after epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("loam-markdown-{nonce}"));
    fs::create_dir_all(&path).expect("temporary workspace should be created");
    path
}

fn write_file(workspace: &Path, relative: &str, content: &str) {
    let path = workspace.join(relative);
    fs::create_dir_all(path.parent().expect("fixture file should have a parent"))
        .expect("fixture directory should be created");
    fs::write(path, content).expect("fixture file should be written");
}

fn run_lint(workspace: &Path) -> Output {
    let binary = std::env::var("CARGO_BIN_EXE_loam").expect("cargo should provide the loam binary");
    Command::new(binary)
        .args(["lint", "--only", "markdown", workspace.to_str().unwrap()])
        .output()
        .expect("loam markdown lint should run")
}

#[test]
fn markdown_lint_clean_workspace_emits_nothing() {
    let workspace = temporary_workspace();
    write_file(
        &workspace,
        "wiki/index.md",
        "# Index\n\n- [x] A completed task\n\nA clean root with no active internal links.\n",
    );

    let output = run_lint(&workspace);
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");

    assert_eq!(
        output.status.code(),
        Some(0),
        "output: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}

#[test]
fn markdown_lint_reports_missing_and_ambiguous_documents() {
    let workspace = temporary_workspace();
    write_file(
        &workspace,
        "wiki/index.md",
        "# Index\n\n[[missing]]\n[[note]]\n",
    );
    write_file(&workspace, "wiki/one/note.md", "# One\n");
    write_file(&workspace, "wiki/two/note.md", "# Two\n");

    let output = run_lint(&workspace);
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");
    let stdout = String::from_utf8(output.stdout).expect("lint output should be UTF-8");

    assert_eq!(output.status.code(), Some(2));
    assert!(stdout.contains("\"rule\":\"LMD001\""), "output: {stdout}");
    assert!(stdout.contains("\"rule\":\"LMD002\""), "output: {stdout}");
}

#[test]
fn markdown_lint_reports_heading_and_link_rules() {
    let workspace = temporary_workspace();
    write_file(
        &workspace,
        "wiki/index.md",
        "# Index\n\n## Duplicate\n## Duplicate\n\n[empty]()\n[missing](#no-such-heading)\n[undefined][missing-ref]\n[[target#Missing]]\n[[target#Repeated]]\n",
    );
    write_file(
        &workspace,
        "wiki/target.md",
        "# Target\n\n## Repeated\n## Repeated\n",
    );

    let output = run_lint(&workspace);
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");
    let stdout = String::from_utf8(output.stdout).expect("lint output should be UTF-8");

    assert_eq!(output.status.code(), Some(2));
    for rule in ["MD024", "MD042", "MD051", "MD052", "LMD003", "LMD004"] {
        assert!(
            stdout.contains(&format!("\"rule\":\"{rule}\"")),
            "missing {rule} in output: {stdout}"
        );
    }
}

#[test]
fn markdown_lint_is_deterministic_and_read_only() {
    let workspace = temporary_workspace();
    write_file(&workspace, "wiki/index.md", "# Index\n\n[[missing]]\n");
    let before = fs::read(workspace.join("wiki/index.md")).expect("fixture should be readable");

    let first = run_lint(&workspace);
    let second = run_lint(&workspace);
    let after = fs::read(workspace.join("wiki/index.md")).expect("fixture should be readable");
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");

    assert_eq!(first.status.code(), Some(2));
    assert_eq!(second.status.code(), Some(2));
    assert_eq!(first.stdout, second.stdout);
    assert_eq!(before, after);
    assert!(first.stderr.is_empty());
}

#[test]
fn markdown_lint_keeps_shorthand_namespaces_separate() {
    let workspace = temporary_workspace();
    write_file(&workspace, "wiki/index.md", "# Wiki\n\n[[note]]\n");
    write_file(&workspace, "wiki/note.md", "# Note\n");
    write_file(&workspace, "goals/note.md", "# Goal note\n");

    let output = run_lint(&workspace);
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");

    assert_eq!(
        output.status.code(),
        Some(0),
        "output: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(output.stdout.is_empty());
}

#[test]
fn markdown_lint_ignores_code_external_and_archived_links() {
    let workspace = temporary_workspace();
    write_file(
        &workspace,
        "wiki/index.md",
        "# Index\n\n[[active-missing]]\n`[[inline-missing]]`\n\n```md\n[[fenced-missing]]\n[bad](#missing)\n```\n\n[external](https://example.com/missing)\n[[archived]]\n[[.archive/archived.md]]\n",
    );
    write_file(&workspace, "wiki/.archive/archived.md", "# Archived\n");

    let output = run_lint(&workspace);
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");

    assert_eq!(output.status.code(), Some(2));
    let stdout = String::from_utf8(output.stdout).expect("lint output should be UTF-8");
    assert!(stdout.contains("active-missing"), "output: {stdout}");
    assert!(!stdout.contains("inline-missing"), "output: {stdout}");
    assert!(!stdout.contains("fenced-missing"), "output: {stdout}");
    assert!(!stdout.contains("archived"), "output: {stdout}");
}

#[test]
fn markdown_lint_rejects_invalid_arguments() {
    let binary = std::env::var("CARGO_BIN_EXE_loam").expect("cargo should provide the loam binary");
    let output = Command::new(binary)
        .args(["lint", "--only", "markdown"])
        .output()
        .expect("loam should run");

    assert_eq!(output.status.code(), Some(1));
}

#[test]
fn markdown_lint_resolves_relative_fragments_and_custom_anchors() {
    let workspace = temporary_workspace();
    write_file(
        &workspace,
        "wiki/index.md",
        "# Index\n\n[heading](sub/target.md#target-heading)\n[unicode](sub/target.md#cafe%CC%81)\n[custom](sub/target.md#custom-id)\n[same](#index)\n[line](#L1)\n[goal](../goals/note.md)\n",
    );
    write_file(
        &workspace,
        "wiki/sub/target.md",
        "# Target Heading\n\n## Cafe\u{301}\n\n## Custom {#custom-id}\n",
    );
    write_file(&workspace, "goals/note.md", "# Goal\n");

    let output = run_lint(&workspace);
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");

    assert_eq!(
        output.status.code(),
        Some(0),
        "output: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(output.stdout.is_empty());
}

#[test]
fn markdown_lint_reports_workspace_escape_and_emits_fixed_schema() {
    let workspace = temporary_workspace();
    write_file(
        &workspace,
        "wiki/index.md",
        "# Index\n\n[escape](../../outside.md)\n",
    );

    let output = run_lint(&workspace);
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");
    let stdout = String::from_utf8(output.stdout).expect("lint output should be UTF-8");
    let record = stdout.lines().next().expect("finding should be emitted");

    assert_eq!(output.status.code(), Some(2));
    for field in [
        "file",
        "line",
        "column",
        "end_line",
        "end_column",
        "rule",
        "rule_names",
        "description",
        "detail",
        "context",
        "severity",
        "target",
        "candidates",
    ] {
        assert!(
            record.contains(&format!("\"{field}\":")),
            "missing {field}: {record}"
        );
    }
    assert!(record.contains("\"rule\":\"LMD001\""), "record: {record}");
}

#[test]
fn markdown_lint_does_not_duplicate_same_line_wikilinks() {
    let workspace = temporary_workspace();
    write_file(&workspace, "wiki/index.md", "# Index\n\n[[one]] [[two]]\n");

    let output = run_lint(&workspace);
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");

    assert_eq!(output.status.code(), Some(2));
    assert_eq!(String::from_utf8_lossy(&output.stdout).lines().count(), 2);
}

#[test]
fn markdown_lint_resolves_root_relative_wikilink_paths_with_optional_extension() {
    let workspace = temporary_workspace();
    write_file(
        &workspace,
        "wiki/index.md",
        "# Index\n\n[[sub/target]]\n[[sub/target.md]]\n",
    );
    write_file(&workspace, "wiki/sub/target.md", "# Target\n");

    let output = run_lint(&workspace);
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");

    assert_eq!(
        output.status.code(),
        Some(0),
        "output: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn markdown_lint_uses_lmd003_for_missing_cross_document_fragments() {
    let workspace = temporary_workspace();
    write_file(
        &workspace,
        "wiki/index.md",
        "# Index\n\n[missing](target.md#missing)\n",
    );
    write_file(&workspace, "wiki/target.md", "# Target\n");

    let output = run_lint(&workspace);
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");
    let stdout = String::from_utf8(output.stdout).expect("lint output should be UTF-8");

    assert_eq!(output.status.code(), Some(2));
    assert!(stdout.contains("\"rule\":\"LMD003\""), "output: {stdout}");
    assert!(!stdout.contains("\"rule\":\"MD051\""), "output: {stdout}");
}

#[test]
fn markdown_lint_ignores_escaped_wikilinks() {
    let workspace = temporary_workspace();
    write_file(&workspace, "wiki/index.md", "# Index\n\n\\[[not-a-link]]\n");

    let output = run_lint(&workspace);
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");

    assert_eq!(
        output.status.code(),
        Some(0),
        "output: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn markdown_lint_does_not_treat_data_id_or_non_anchor_name_as_fragments() {
    let workspace = temporary_workspace();
    write_file(
        &workspace,
        "wiki/index.md",
        "# Index\n\n<div data-id=\"data-value\" name=\"div-name\" id=\"real-id\"></div>\n<a name=\"real-name\"></a>\n\n[data](#data-value)\n[name](#div-name)\n[real-id](#real-id)\n[real-name](#real-name)\n",
    );

    let output = run_lint(&workspace);
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");
    let stdout = String::from_utf8(output.stdout).expect("lint output should be UTF-8");

    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        stdout
            .lines()
            .filter(|line| line.contains("\"rule\":\"MD051\""))
            .count(),
        2,
        "output: {stdout}"
    );
}

#[test]
fn markdown_lint_preserves_markdownlint_default_rule_boundaries() {
    let workspace = temporary_workspace();
    write_file(
        &workspace,
        "wiki/index.md",
        "# Index\n\n[empty](#)\n[undefined][]\n[shortcut]\n[x][]\n[line](#L20)\n[range](#L19C5-L21C11)\n",
    );

    let output = run_lint(&workspace);
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");
    let stdout = String::from_utf8(output.stdout).expect("lint output should be UTF-8");

    assert_eq!(output.status.code(), Some(2));
    assert_eq!(stdout.matches("\"rule\":\"MD042\"").count(), 1, "{stdout}");
    assert_eq!(stdout.matches("\"rule\":\"MD052\"").count(), 1, "{stdout}");
    assert!(!stdout.contains("shortcut"), "output: {stdout}");
    assert!(!stdout.contains("\"target\":\"x\""), "output: {stdout}");
}

#[cfg(unix)]
#[test]
fn markdown_lint_skips_symlinked_roots() {
    use std::os::unix::fs::symlink;

    let workspace = temporary_workspace();
    let outside = temporary_workspace();
    write_file(&outside, "index.md", "# Outside\n\n[[missing]]\n");
    symlink(&outside, workspace.join("wiki")).expect("root symlink should be created");

    let output = run_lint(&workspace);
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");
    fs::remove_dir_all(&outside).expect("outside fixture should be removed");

    assert_eq!(
        output.status.code(),
        Some(0),
        "output: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(output.stdout.is_empty());
}
