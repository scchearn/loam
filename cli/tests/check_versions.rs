//! `loam check versions` — offline product-version agreement.
//!
//! Replaces the `python3`-dependent half of `bin/check-release-resolution.sh`
//! and widens it from four values to seven: both Claude marketplace fields and
//! the Codex and Cursor manifests were previously unchecked, which is how they
//! drifted to `0.1.0`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const VERSION: &str = "0.8.2";

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

/// Every version-bearing file the release gate covers, all in agreement.
fn agreeing_root(label: &str) -> PathBuf {
    let root = temporary_root(label);
    write(
        &root.join("package.json"),
        &format!("{{\n  \"name\": \"loam\",\n  \"version\": \"{VERSION}\",\n  \"type\": \"module\"\n}}\n"),
    );
    write(
        &root.join(".claude-plugin/marketplace.json"),
        &format!("{{\n  \"name\": \"loam\",\n  \"metadata\": {{\n    \"description\": \"d\",\n    \"version\": \"{VERSION}\"\n  }},\n  \"plugins\": [\n    {{\n      \"name\": \"loam\",\n      \"version\": \"{VERSION}\"\n    }}\n  ]\n}}\n"),
    );
    write(
        &root.join(".codex-plugin/plugin.json"),
        &format!("{{\n  \"name\": \"loam\",\n  \"version\": \"{VERSION}\"\n}}\n"),
    );
    write(
        &root.join(".cursor-plugin/plugin.json"),
        &format!("{{\n  \"name\": \"loam\",\n  \"version\": \"{VERSION}\"\n}}\n"),
    );
    write(
        &root.join("cli/Cargo.toml"),
        &format!("[package]\nname = \"loam\"\nversion = \"{VERSION}\"\nedition = \"2021\"\n\n[dependencies]\nchrono = {{ version = \"0.4\" }}\n"),
    );
    write(
        &root.join("skills/loam-using/scripts/CLI_VERSION"),
        &format!("{VERSION}\n"),
    );
    root
}

fn check(root: &Path) -> (i32, String, String) {
    let binary = std::env::var("CARGO_BIN_EXE_loam").expect("cargo should provide the loam binary");
    let output = Command::new(binary)
        .args(["check", "versions", root.to_str().unwrap()])
        .output()
        .expect("loam should run");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn seven_agreeing_versions_pass() {
    let root = agreeing_root("agree");

    let (code, stdout, stderr) = check(&root);

    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(stdout, format!("version agreement: PASS ({VERSION})\n"));
    assert_eq!(stderr, "");
}

/// Each value is drifted on its own so no failure can mask another.
#[test]
fn any_single_drifted_value_fails_and_names_itself() {
    let cases: [(&str, &str, &str); 6] = [
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
        ("cargo", "cli/Cargo.toml", "cli/Cargo.toml version"),
        (
            "cli-version",
            "skills/loam-using/scripts/CLI_VERSION",
            "skills/loam-using/scripts/CLI_VERSION",
        ),
    ];

    for (label, file, expected) in cases {
        let root = agreeing_root(label);
        let path = root.join(file);
        let original = fs::read_to_string(&path).expect("fixture should be readable");
        // Replace only the occurrence this case targets.
        let drifted = match label {
            "marketplace-metadata" => original.replacen(VERSION, "0.1.0", 1),
            "marketplace-plugin" => {
                let split = original.find("plugins").expect("plugins key should exist");
                let (head, tail) = original.split_at(split);
                format!("{head}{}", tail.replacen(VERSION, "0.1.0", 1))
            }
            _ => original.replace(VERSION, "0.1.0"),
        };
        write(&path, &drifted);

        let (code, _, stderr) = check(&root);

        assert_eq!(code, 1, "{label} drift should fail");
        assert!(
            stderr.contains(expected),
            "{label}: stderr should name {expected}, got: {stderr}"
        );
        assert!(
            stderr.contains("0.1.0") && stderr.contains(VERSION),
            "{label}: stderr should show both values, got: {stderr}"
        );
    }
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

    let (code, _, stderr) = check(&root);

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

    let (code, _, stderr) = check(&root);

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

    let (code, _, stderr) = check(&root);

    assert_eq!(code, 1);
    assert!(stderr.contains("package.json"), "stderr: {stderr}");
}

#[test]
fn a_missing_root_is_reported() {
    let root = temporary_root("no-root").join("nope");

    let (code, _, stderr) = check(&root);

    assert_eq!(code, 1);
    assert!(stderr.contains("not found"), "stderr: {stderr}");
}

#[test]
fn surrounding_whitespace_in_cli_version_is_tolerated() {
    let root = agreeing_root("whitespace");
    write(
        &root.join("skills/loam-using/scripts/CLI_VERSION"),
        &format!("  {VERSION}  \r\n"),
    );

    let (code, stdout, stderr) = check(&root);

    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(stdout, format!("version agreement: PASS ({VERSION})\n"));
}

#[test]
fn the_real_repository_agrees_on_one_product_version() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("cli/ should have a parent");

    let (code, stdout, stderr) = check(root);

    assert_eq!(
        code, 0,
        "the repository must ship one product version: {stderr}"
    );
    assert!(stdout.starts_with("version agreement: PASS ("), "{stdout}");
}
