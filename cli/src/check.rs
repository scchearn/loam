//! `loam check versions` — offline version agreement.
//!
//! Loam ships two independently released products from one repository, so
//! there are two version domains and agreement is asserted *within* each, never
//! across them:
//!
//! - **plugin** (`package.json`, both `.claude-plugin/marketplace.json` fields,
//!   `.codex-plugin/plugin.json`, `.cursor-plugin/plugin.json`) — what the
//!   harnesses display and resolve. Released as a `v<version>` tag.
//! - **runtime** (`cli/Cargo.toml`, `skills/loam-using/scripts/CLI_VERSION`) —
//!   what the launcher downloads. Released as a `cli-v<version>` tag, and only
//!   `cli-v*` triggers the dist build.
//!
//! `CLI_VERSION` is not merely compared: the launcher interpolates it into the
//! release URL and the on-disk runtime path, so it must name a published
//! `cli-v<version>` release. That is the constraint the runtime group protects.
//!
//! Network manifest resolution deliberately stays in
//! `bin/check-release-resolution.sh`: a pre-commit gate must never depend on a
//! reachable GitHub Release.

use std::fs;
use std::path::Path;

use crate::json;

const PLUGIN_REFERENCE: &str = "package.json";
const RUNTIME_REFERENCE: &str = "cli/Cargo.toml";
const CLI_VERSION_FILE: &str = "skills/loam-using/scripts/CLI_VERSION";

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
    eprintln!(
        "Usage: loam check versions <repo-root> [--plugin | --runtime]\n\n  Asserts one version within each domain. The plugin and runtime versions\n  are released independently and are never compared to each other.\n  Default checks both."
    );
}

#[derive(Clone, Copy, PartialEq)]
enum Domain {
    Plugin,
    Runtime,
}

impl Domain {
    fn label(self) -> &'static str {
        match self {
            Domain::Plugin => "plugin",
            Domain::Runtime => "runtime",
        }
    }
}

fn versions(args: impl Iterator<Item = String>) -> i32 {
    let mut root = None;
    let mut only = None;
    for arg in args {
        match arg.as_str() {
            "--plugin" if only.is_none() => only = Some(Domain::Plugin),
            "--runtime" if only.is_none() => only = Some(Domain::Runtime),
            value if value.starts_with('-') => {
                usage();
                return 1;
            }
            value if root.is_none() => root = Some(value.to_owned()),
            _ => {
                usage();
                return 1;
            }
        }
    }

    let Some(root) = root else {
        usage();
        return 1;
    };
    let root = Path::new(&root);
    if !root.is_dir() {
        eprintln!(
            "version agreement: FAIL: repo root not found: {}",
            root.display()
        );
        return 1;
    }

    let mut failed = false;
    for domain in [Domain::Plugin, Domain::Runtime] {
        if only.is_some_and(|selected| selected != domain) {
            continue;
        }
        if !check_domain(root, domain) {
            failed = true;
        }
    }

    if failed {
        1
    } else {
        0
    }
}

fn check_domain(root: &Path, domain: Domain) -> bool {
    let (reference_label, sources) = match domain {
        Domain::Plugin => (
            PLUGIN_REFERENCE,
            vec![
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
            ],
        ),
        Domain::Runtime => (RUNTIME_REFERENCE, vec![read_cli_version(root)]),
    };

    let reference = match domain {
        Domain::Plugin => read_json_field(root, PLUGIN_REFERENCE, &["version"]),
        Domain::Runtime => read_cargo_version(root).map(|(_, value)| value),
    };
    let reference = match reference {
        Ok(value) => value,
        Err(error) => {
            eprintln!("version agreement: FAIL: {error}");
            return false;
        }
    };

    // Every mismatch is reported, so one bump never hides another.
    let mut ok = true;
    for source in sources {
        match source {
            Ok((label, value)) => {
                if value != reference {
                    eprintln!(
                        "version agreement: FAIL: {label} is {value}, {reference_label} is {reference} \u{2014} the {} version must be one value",
                        domain.label()
                    );
                    ok = false;
                }
            }
            Err(error) => {
                eprintln!("version agreement: FAIL: {error}");
                ok = false;
            }
        }
    }

    if ok {
        println!("version agreement: {} PASS ({reference})", domain.label());
    }
    ok
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
    const LABEL: &str = "cli/Cargo.toml version";
    let content = fs::read_to_string(root.join(RUNTIME_REFERENCE))
        .map_err(|error| format!("cannot read {RUNTIME_REFERENCE}: {error}"))?;
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
    Err(format!("{RUNTIME_REFERENCE} has no [package] version"))
}

fn read_cli_version(root: &Path) -> Result<(String, String), String> {
    let content = fs::read_to_string(root.join(CLI_VERSION_FILE))
        .map_err(|error| format!("cannot read {CLI_VERSION_FILE}: {error}"))?;
    let value = content.trim();
    if value.is_empty() {
        return Err(format!("{CLI_VERSION_FILE} is empty"));
    }
    Ok((CLI_VERSION_FILE.to_owned(), value.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(label: &str) -> std::path::PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "loam-check-{label}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock should be after epoch")
                .as_nanos()
        ));
        fs::create_dir_all(directory.join("cli")).expect("fixture should be created");
        directory
    }

    #[test]
    fn cargo_version_ignores_dependency_versions() {
        let directory = fixture("cargo-version");
        fs::write(
            directory.join("cli/Cargo.toml"),
            "[package]\nname = \"loam\"\nversion = \"0.9.0\"\n\n[dependencies]\nchrono = { version = \"0.4\" }\n",
        )
        .expect("fixture should be written");

        let (label, value) = read_cargo_version(&directory).expect("version should be found");

        assert_eq!(label, "cli/Cargo.toml version");
        assert_eq!(value, "0.9.0");
    }

    #[test]
    fn a_cargo_file_without_a_package_version_is_an_error() {
        let directory = fixture("cargo-missing");
        fs::write(
            directory.join("cli/Cargo.toml"),
            "[dependencies]\nchrono = { version = \"0.4\" }\n",
        )
        .expect("fixture should be written");

        assert!(read_cargo_version(&directory).is_err());
    }
}
