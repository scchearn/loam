use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const M1: &str = "---\ntitle: T\n---\n\n# Page\n\n\
<!-- loam:class c-payroll-notes local-only -->\n\
Payroll notes.\n\
\n\
<!-- loam:seal s-tls-rung-order -->\n\
TLS tries the bundled rung first.\n\
<!-- /loam:seal -->\n\
<!-- /loam:class -->\n\
\n\
- [ ] Q: Who signs off hosting? ^q-hosting-signoff\n";

const NOW: &str = "2026-07-20 12:00 +02:00";

fn temporary_workspace(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be after epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("loam-lint-markers-{label}-{nonce}"));
    fs::create_dir_all(&path).expect("temporary workspace should be created");
    path
}

fn cleanup(path: &Path) {
    // Windows can hold a directory briefly after a child exits (EBUSY).
    fs::remove_dir_all(path).ok();
}

fn write(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("parent directory should be created");
    }
    fs::write(path, content).expect("file should be written");
}

fn hermetic(workspace: &Path) -> Command {
    let binary = std::env::var("CARGO_BIN_EXE_loam").expect("cargo should provide the loam binary");
    let mut command = Command::new(binary);
    command
        .env_remove("HCOM_TOOL")
        .env_remove("HCOM_INSTALL_DIR")
        .env("PATH", "")
        .env("HOME", workspace)
        .env("USERPROFILE", workspace)
        .env("LOAM_CONFIG_DIR", workspace.join(".loam-config"))
        .env("LOAM_WORKSPACES", "off");
    command
}

fn lint(workspace: &Path, only: Option<&str>) -> (Option<i32>, Vec<String>) {
    let mut command = hermetic(workspace);
    command.arg("lint");
    if let Some(only) = only {
        command.args(["--only", only]);
    }
    let output = command
        .arg(workspace)
        .args(["--now", NOW])
        .output()
        .expect("loam should run");
    let stdout = String::from_utf8(output.stdout).expect("findings should be UTF-8");
    (
        output.status.code(),
        stdout.lines().map(str::to_owned).collect(),
    )
}

fn field(line: &str, name: &str) -> String {
    let marker = format!("\"{name}\":\"");
    let start = line.find(&marker).expect("finding should carry field") + marker.len();
    let rest = &line[start..];
    rest[..rest.find('"').expect("value should be terminated")].to_owned()
}

fn rules(lines: &[String]) -> Vec<String> {
    lines.iter().map(|line| field(line, "rule")).collect()
}

fn state(workspace: &Path) -> String {
    let output = hermetic(workspace)
        .args(["state", "--fast"])
        .arg(workspace)
        .output()
        .expect("loam should run");
    String::from_utf8(output.stdout).expect("state output should be UTF-8")
}

fn base_workspace(label: &str) -> PathBuf {
    let workspace = temporary_workspace(label);
    write(&workspace.join("wiki/index.md"), "# Index\n\n- [[page]]\n");
    workspace
}

#[test]
fn m1_markdown_lint_accepts_every_marker_kind() {
    let workspace = base_workspace("m1");
    write(&workspace.join("wiki/page.md"), M1);
    let (code, lines) = lint(&workspace, Some("markdown"));
    cleanup(&workspace);

    // The class span is reserved and reported once as info, which never
    // fails a run; nothing else is reported.
    assert_eq!(code, Some(0), "{lines:?}");
    assert_eq!(rules(&lines), vec!["MRK010"], "{lines:?}");
    assert_eq!(field(&lines[0], "severity"), "info");
}

#[test]
fn m2_malformed_markers_are_errors_and_exit_2() {
    let cases: [(&str, &str, &str); 4] = [
        (
            "<!-- loam:seal s-aa-bb -->\nbody\n",
            "MRK001",
            "markdown",
        ),
        (
            "<!-- loam:seal s-aa-bb -->\n<!-- loam:seal s-cc-dd -->\nx\n<!-- /loam:seal -->\n<!-- /loam:seal -->\n",
            "MRK002",
            "markdown",
        ),
        (
            "<!-- loam:seal s-aa-bb -->\none\n<!-- /loam:seal -->\n<!-- loam:seal s-aa-bb -->\ntwo\n<!-- /loam:seal -->\n",
            "MRK003",
            "markdown",
        ),
        (
            "<!-- loam:seal s-aa-bb level=canonical -->\nbody\n<!-- /loam:seal -->\n",
            "SEL001",
            "seals",
        ),
    ];
    for (index, (page, rule, domain)) in cases.iter().enumerate() {
        let workspace = base_workspace(&format!("m2-{index}"));
        write(&workspace.join("wiki/page.md"), page);
        let (code, lines) = lint(&workspace, None);
        let (_, only_domain) = lint(&workspace, Some(domain));
        let other = if *domain == "seals" {
            "markdown"
        } else {
            "seals"
        };
        let (_, only_other) = lint(&workspace, Some(other));
        cleanup(&workspace);

        assert_eq!(code, Some(2), "{rule}: {lines:?}");
        let hit = lines
            .iter()
            .find(|line| field(line, "rule") == *rule)
            .unwrap_or_else(|| panic!("{rule} missing: {lines:?}"));
        assert_eq!(field(hit, "severity"), "error", "{rule}");
        assert_eq!(field(hit, "domain"), *domain, "{rule}");
        assert!(
            rules(&only_domain).contains(&rule.to_string()),
            "{rule} should be selectable under --only {domain}: {only_domain:?}"
        );
        assert!(
            !rules(&only_other).contains(&rule.to_string()),
            "{rule} leaked under --only {other}: {only_other:?}"
        );
    }
}

#[test]
fn m3_class_marker_is_info_once_and_changes_nothing() {
    let plain = "# Page\n\nFirst paragraph.\n\nSecond paragraph.\n\nSee [[ghost-page]].\n";
    let wrapped = "# Page\n\n<!-- loam:class c-first-note project -->\nFirst paragraph.\n<!-- /loam:class -->\n\n<!-- loam:class c-second-note hub-promotable -->\nSecond paragraph.\n<!-- /loam:class -->\n\nSee [[ghost-page]].\n";

    let workspace = base_workspace("m3");
    write(&workspace.join("wiki/page.md"), plain);
    write(&workspace.join("wiki/other.md"), plain);
    let (code_before, before) = lint(&workspace, None);
    let state_before = state(&workspace);

    write(&workspace.join("wiki/page.md"), wrapped);
    write(&workspace.join("wiki/other.md"), wrapped);
    let (code_after, after) = lint(&workspace, None);
    let state_after = state(&workspace);
    cleanup(&workspace);

    assert_eq!(code_before, Some(2));
    assert_eq!(code_after, Some(2));
    let info: Vec<_> = after
        .iter()
        .filter(|line| field(line, "rule") == "MRK010")
        .collect();
    assert_eq!(info.len(), 1, "exactly one MRK010 per run: {after:?}");
    assert_eq!(field(info[0], "severity"), "info");
    assert_eq!(
        field(info[0], "file"),
        "wiki/other.md",
        "first class span in sorted path order"
    );
    let mut rest: Vec<_> = after
        .iter()
        .filter(|line| field(line, "rule") != "MRK010")
        .map(|line| (field(line, "rule"), field(line, "file")))
        .collect();
    let mut baseline: Vec<_> = before
        .iter()
        .map(|line| (field(line, "rule"), field(line, "file")))
        .collect();
    rest.sort();
    baseline.sort();
    assert_eq!(rest, baseline);
    assert_eq!(
        state_before, state_after,
        "state hints and counts unchanged"
    );
}

#[test]
fn info_only_output_exits_zero() {
    let workspace = base_workspace("info-only");
    write(&workspace.join("wiki/page.md"), M1);
    let (code, lines) = lint(&workspace, Some("markdown"));
    cleanup(&workspace);
    assert_eq!(code, Some(0), "{lines:?}");
    assert!(!lines.is_empty());
}
