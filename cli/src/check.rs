//! `loam check versions` — offline product-version agreement.
//!
//! Loam ships one product version across seven places. The Bash gate compared
//! four of them and needed `python3` to read JSON, which is how the Codex and
//! Cursor manifests drifted to `0.1.0` unnoticed. Network manifest resolution
//! deliberately stays in `bin/check-release-resolution.sh`: a pre-commit gate
//! must never depend on a reachable GitHub Release.

use std::fs;
use std::path::Path;

use crate::json;

pub fn run(mut args: impl Iterator<Item = String>) -> i32 {
    match args.next().as_deref() {
        Some("versions") => versions(args),
        _ => {
            usage();
            1
        }
    }
}

fn usage() {
    eprintln!("Usage: loam check versions <repo-root>");
}

fn versions(mut args: impl Iterator<Item = String>) -> i32 {
    let Some(root) = args.next() else {
        usage();
        return 1;
    };
    if args.next().is_some() {
        usage();
        return 1;
    }
    let root = Path::new(&root);
    if !root.is_dir() {
        eprintln!(
            "version agreement: FAIL: repo root not found: {}",
            root.display()
        );
        return 1;
    }

    // `package.json` is the reference; everything else must equal it.
    let reference = match read_json_field(root, "package.json", &["version"]) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("version agreement: FAIL: {error}");
            return 1;
        }
    };

    let sources: [Result<(String, String), String>; 6] = [
        read_json_field(
            root,
            ".claude-plugin/marketplace.json",
            &["metadata", "version"],
        )
        .map(|value| {
            (
                ".claude-plugin/marketplace.json metadata.version".to_owned(),
                value,
            )
        }),
        read_marketplace_plugin(root),
        read_json_field(root, ".codex-plugin/plugin.json", &["version"])
            .map(|value| (".codex-plugin/plugin.json version".to_owned(), value)),
        read_json_field(root, ".cursor-plugin/plugin.json", &["version"])
            .map(|value| (".cursor-plugin/plugin.json version".to_owned(), value)),
        read_cargo_version(root),
        read_cli_version(root),
    ];

    // Every mismatch is reported, so one bump never hides another.
    let mut failed = false;
    for source in sources {
        match source {
            Ok((label, value)) => {
                if value != reference {
                    eprintln!(
                        "version agreement: FAIL: {label} is {value}, package.json is {reference} \u{2014} one product version is required"
                    );
                    failed = true;
                }
            }
            Err(error) => {
                eprintln!("version agreement: FAIL: {error}");
                failed = true;
            }
        }
    }

    if failed {
        return 1;
    }
    println!("version agreement: PASS ({reference})");
    0
}

fn read_json_field(root: &Path, relative: &str, path: &[&str]) -> Result<String, String> {
    let full = root.join(relative);
    let content =
        fs::read_to_string(&full).map_err(|error| format!("cannot read {relative}: {error}"))?;
    let document =
        json::parse(&content).map_err(|error| format!("cannot parse {relative}: {error}"))?;
    let mut current = &document;
    for key in path {
        current = current
            .get(key)
            .ok_or_else(|| format!("{relative} has no {}", path.join(".")))?;
    }
    current
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("{relative} {} is not a string", path.join(".")))
}

fn read_marketplace_plugin(root: &Path) -> Result<(String, String), String> {
    const RELATIVE: &str = ".claude-plugin/marketplace.json";
    const LABEL: &str = ".claude-plugin/marketplace.json plugins[0].version";
    let full = root.join(RELATIVE);
    let content =
        fs::read_to_string(&full).map_err(|error| format!("cannot read {RELATIVE}: {error}"))?;
    let document =
        json::parse(&content).map_err(|error| format!("cannot parse {RELATIVE}: {error}"))?;
    let value = document
        .get("plugins")
        .and_then(|plugins| plugins.at(0))
        .and_then(|plugin| plugin.get("version"))
        .and_then(json::Value::as_str)
        .ok_or_else(|| format!("{RELATIVE} has no plugins[0].version"))?;
    Ok((LABEL.to_owned(), value.to_owned()))
}

/// The `version` key of the `[package]` table, so a dependency's version can
/// never be mistaken for the crate's own.
fn read_cargo_version(root: &Path) -> Result<(String, String), String> {
    const RELATIVE: &str = "cli/Cargo.toml";
    const LABEL: &str = "cli/Cargo.toml version";
    let content = fs::read_to_string(root.join(RELATIVE))
        .map_err(|error| format!("cannot read {RELATIVE}: {error}"))?;
    let mut in_package = false;
    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_package = line == "[package]";
            continue;
        }
        if !in_package {
            continue;
        }
        let Some(rest) = line.strip_prefix("version") else {
            continue;
        };
        let Some(rest) = rest.trim_start().strip_prefix('=') else {
            continue;
        };
        let value = rest.trim().trim_matches('"');
        if value.is_empty() {
            continue;
        }
        return Ok((LABEL.to_owned(), value.to_owned()));
    }
    Err(format!("{RELATIVE} has no [package] version"))
}

fn read_cli_version(root: &Path) -> Result<(String, String), String> {
    const RELATIVE: &str = "skills/loam-using/scripts/CLI_VERSION";
    let content = fs::read_to_string(root.join(RELATIVE))
        .map_err(|error| format!("cannot read {RELATIVE}: {error}"))?;
    let value = content.trim();
    if value.is_empty() {
        return Err(format!("{RELATIVE} is empty"));
    }
    Ok((RELATIVE.to_owned(), value.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cargo_version_ignores_dependency_versions() {
        let directory = std::env::temp_dir().join(format!(
            "loam-cargo-version-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock should be after epoch")
                .as_nanos()
        ));
        fs::create_dir_all(directory.join("cli")).expect("fixture should be created");
        fs::write(
            directory.join("cli/Cargo.toml"),
            "[package]\nname = \"loam\"\nversion = \"0.8.2\"\n\n[dependencies]\nchrono = { version = \"0.4\" }\n",
        )
        .expect("fixture should be written");

        let (label, value) = read_cargo_version(&directory).expect("version should be found");

        assert_eq!(label, "cli/Cargo.toml version");
        assert_eq!(value, "0.8.2");
    }

    #[test]
    fn a_cargo_file_without_a_package_version_is_an_error() {
        let directory = std::env::temp_dir().join(format!(
            "loam-cargo-missing-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock should be after epoch")
                .as_nanos()
        ));
        fs::create_dir_all(directory.join("cli")).expect("fixture should be created");
        fs::write(
            directory.join("cli/Cargo.toml"),
            "[dependencies]\nchrono = { version = \"0.4\" }\n",
        )
        .expect("fixture should be written");

        assert!(read_cargo_version(&directory).is_err());
    }
}
