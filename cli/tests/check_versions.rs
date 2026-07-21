//! `loam check versions` — offline version agreement across two independent
//! release domains.
//!
//! Plugin (`package.json`, both marketplace fields, Codex, Cursor) ships as
//! `v<version>`; runtime (`cli/Cargo.toml`, `CLI_VERSION`) ships as
//! `cli-v<version>`. Agreement is asserted *within* each domain and never
//! across them — a plugin-only change must not force a runtime release.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const PLUGIN: &str = "0.8.3";
const RUNTIME: &str = "0.9.0";

fn temporary_root(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be after epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("loam-versions-{label}-{nonce}"));
    fs::create_dir_all(&path).expect("temporary root should be created");
    path
}

fn write(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("parent directory should be created");
    }
    fs::write(path, content).expect("file should be written");
}

/// A fixture where each domain agrees internally and the two differ from each
/// other — the normal decoupled state, not an error.
fn agreeing_root(label: &str) -> PathBuf {
    let root = temporary_root(label);
    write(
        &root.join("package.json"),
        &format!(
            "{{\n  \"name\": \"loam\",\n  \"version\": \"{PLUGIN}\",\n  \"type\": \"module\"\n}}\n"
        ),
    );
    write(
        &root.join(".claude-plugin/marketplace.json"),
        &format!("{{\n  \"name\": \"loam\",\n  \"metadata\": {{\n    \"description\": \"d\",\n    \"version\": \"{PLUGIN}\"\n  }},\n  \"plugins\": [\n    {{\n      \"name\": \"loam\",\n      \"version\": \"{PLUGIN}\"\n    }}\n  ]\n}}\n"),
    );
    write(
        &root.join(".codex-plugin/plugin.json"),
        &format!("{{\n  \"name\": \"loam\",\n  \"version\": \"{PLUGIN}\"\n}}\n"),
    );
    write(
        &root.join(".cursor-plugin/plugin.json"),
        &format!("{{\n  \"name\": \"loam\",\n  \"version\": \"{PLUGIN}\"\n}}\n"),
    );
    write(
        &root.join("cli/Cargo.toml"),
        &format!("[package]\nname = \"loam\"\nversion = \"{RUNTIME}\"\nedition = \"2021\"\n\n[dependencies]\nchrono = {{ version = \"0.4\" }}\n"),
    );
    write(
        &root.join("skills/loam-using/scripts/CLI_VERSION"),
        &format!("{RUNTIME}\n"),
    );
    root
}

fn check(root: &Path, extra: &[&str]) -> (i32, String, String) {
    let binary = std::env::var("CARGO_BIN_EXE_loam").expect("cargo should provide the loam binary");
    let mut arguments = vec!["check", "versions", root.to_str().unwrap()];
    arguments.extend_from_slice(extra);
    let output = Command::new(binary)
        .args(&arguments)
        .output()
        .expect("loam should run");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn independent_domain_versions_pass_without_being_compared() {
    let root = agreeing_root("agree");

    let (code, stdout, stderr) = check(&root, &[]);

    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(stderr, "");
    assert!(
        stdout.contains(&format!("version agreement: plugin PASS ({PLUGIN})\n")),
        "stdout: {stdout}"
    );
    assert!(
        stdout.contains(&format!("version agreement: runtime PASS ({RUNTIME})\n")),
        "stdout: {stdout}"
    );
}

#[test]
fn each_domain_can_be_checked_alone() {
    let root = agreeing_root("selectors");

    let (code, stdout, _) = check(&root, &["--plugin"]);
    assert_eq!(code, 0);
    assert!(
        stdout.contains(&format!("plugin PASS ({PLUGIN})")),
        "{stdout}"
    );
    assert!(!stdout.contains("runtime"), "{stdout}");

    let (code, stdout, _) = check(&root, &["--runtime"]);
    assert_eq!(code, 0);
    assert!(
        stdout.contains(&format!("runtime PASS ({RUNTIME})")),
        "{stdout}"
    );
    assert!(!stdout.contains("plugin"), "{stdout}");
}

/// The whole point of the split: these two values are unequal and that is fine.
#[test]
fn a_plugin_version_never_has_to_equal_the_runtime_version() {
    let root = agreeing_root("cross-domain");
    write(
        &root.join("skills/loam-using/scripts/CLI_VERSION"),
        "2.5.1\n",
    );
    write(
        &root.join("cli/Cargo.toml"),
        "[package]\nname = \"loam\"\nversion = \"2.5.1\"\nedition = \"2021\"\n",
    );

    let (code, stdout, stderr) = check(&root, &[]);

    assert_eq!(code, 0, "cross-domain difference must not fail: {stderr}");
    assert!(
        stdout.contains(&format!("plugin PASS ({PLUGIN})")),
        "{stdout}"
    );
    assert!(stdout.contains("runtime PASS (2.5.1)"), "{stdout}");
}

/// Each plugin value drifted on its own so no failure can mask another.
#[test]
fn any_drifted_plugin_value_fails_and_names_itself() {
    let cases: [(&str, &str, &str); 4] = [
        (
            "marketplace-metadata",
            ".claude-plugin/marketplace.json",
            ".claude-plugin/marketplace.json metadata.version",
        ),
        (
            "marketplace-plugin",
            ".claude-plugin/marketplace.json",
            ".claude-plugin/marketplace.json plugins[0].version",
        ),
        (
            "codex",
            ".codex-plugin/plugin.json",
            ".codex-plugin/plugin.json version",
        ),
        (
            "cursor",
            ".cursor-plugin/plugin.json",
            ".cursor-plugin/plugin.json version",
        ),
    ];

    for (label, file, expected) in cases {
        let root = agreeing_root(label);
        let path = root.join(file);
        let original = fs::read_to_string(&path).expect("fixture should be readable");
        let drifted = match label {
            "marketplace-metadata" => original.replacen(PLUGIN, "0.1.0", 1),
            "marketplace-plugin" => {
                let split = original.find("plugins").expect("plugins key should exist");
                let (head, tail) = original.split_at(split);
                format!("{head}{}", tail.replacen(PLUGIN, "0.1.0", 1))
            }
            _ => original.replace(PLUGIN, "0.1.0"),
        };
        write(&path, &drifted);

        let (code, _, stderr) = check(&root, &[]);

        assert_eq!(code, 1, "{label} drift should fail");
        assert!(
            stderr.contains(expected),
            "{label}: stderr should name {expected}, got: {stderr}"
        );
        assert!(
            stderr.contains("the plugin version must be one value"),
            "{label}: stderr should name the domain, got: {stderr}"
        );
    }
}

#[test]
fn a_drifted_cli_version_fails_the_runtime_domain() {
    let root = agreeing_root("runtime-drift");
    write(
        &root.join("skills/loam-using/scripts/CLI_VERSION"),
        "0.8.2\n",
    );

    let (code, _, stderr) = check(&root, &[]);

    assert_eq!(code, 1);
    assert!(
        stderr.contains("skills/loam-using/scripts/CLI_VERSION is 0.8.2"),
        "stderr: {stderr}"
    );
    assert!(
        stderr.contains("the runtime version must be one value"),
        "stderr: {stderr}"
    );
}

/// A runtime failure must not be reported when only the plugin was requested.
#[test]
fn a_selector_scopes_the_failure() {
    let root = agreeing_root("scoped-failure");
    write(
        &root.join("skills/loam-using/scripts/CLI_VERSION"),
        "0.8.2\n",
    );

    let (plugin_code, _, _) = check(&root, &["--plugin"]);
    let (runtime_code, _, _) = check(&root, &["--runtime"]);

    assert_eq!(plugin_code, 0, "plugin domain is still healthy");
    assert_eq!(runtime_code, 1, "runtime domain is broken");
}

#[test]
fn every_drifted_value_is_reported_not_just_the_first() {
    let root = agreeing_root("all-drift");
    write(
        &root.join(".codex-plugin/plugin.json"),
        "{\n  \"name\": \"loam\",\n  \"version\": \"0.1.0\"\n}\n",
    );
    write(
        &root.join(".cursor-plugin/plugin.json"),
        "{\n  \"name\": \"loam\",\n  \"version\": \"0.1.0\"\n}\n",
    );

    let (code, _, stderr) = check(&root, &[]);

    assert_eq!(code, 1);
    assert!(
        stderr.contains(".codex-plugin/plugin.json"),
        "stderr: {stderr}"
    );
    assert!(
        stderr.contains(".cursor-plugin/plugin.json"),
        "stderr: {stderr}"
    );
}

#[test]
fn a_missing_file_is_a_hard_failure() {
    let root = agreeing_root("missing");
    fs::remove_file(root.join(".codex-plugin/plugin.json")).expect("fixture should be removable");

    let (code, _, stderr) = check(&root, &[]);

    assert_eq!(code, 1);
    assert!(
        stderr.contains(".codex-plugin/plugin.json"),
        "stderr: {stderr}"
    );
}

#[test]
fn malformed_json_is_reported_rather_than_silently_skipped() {
    let root = agreeing_root("malformed");
    write(&root.join("package.json"), "{ this is not json\n");

    let (code, _, stderr) = check(&root, &[]);

    assert_eq!(code, 1);
    assert!(stderr.contains("package.json"), "stderr: {stderr}");
}

#[test]
fn a_missing_root_is_reported() {
    let root = temporary_root("no-root").join("nope");

    let (code, _, stderr) = check(&root, &[]);

    assert_eq!(code, 1);
    assert!(stderr.contains("not found"), "stderr: {stderr}");
}

#[test]
fn surrounding_whitespace_in_cli_version_is_tolerated() {
    let root = agreeing_root("whitespace");
    write(
        &root.join("skills/loam-using/scripts/CLI_VERSION"),
        &format!("  {RUNTIME}  \r\n"),
    );

    let (code, stdout, stderr) = check(&root, &["--runtime"]);

    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(
        stdout,
        format!("version agreement: runtime PASS ({RUNTIME})\n")
    );
}

#[test]
fn conflicting_selectors_are_rejected() {
    let root = agreeing_root("both-selectors");

    let (code, _, _) = check(&root, &["--plugin", "--runtime"]);

    assert_eq!(code, 1);
}

#[test]
fn the_real_repository_agrees_within_each_domain() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("cli/ should have a parent");

    let (code, stdout, stderr) = check(root, &[]);

    assert_eq!(code, 0, "each domain must ship one version: {stderr}");
    assert!(stdout.contains("plugin PASS ("), "{stdout}");
    assert!(stdout.contains("runtime PASS ("), "{stdout}");
}
