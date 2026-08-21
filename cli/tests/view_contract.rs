// The Loam View producer contract (T7): every fixture under
// cli/tests/fixtures/view/ produces a normalized snapshot that byte-for-byte
// matches its checked-in expected/<fixture>.json. This is the cross-platform
// semantic-identity claim from specs/loam-view.md -- normalized output must
// be identical whether this runs on Linux, macOS, or Windows CI -- and it
// supersedes the spec's sh/ps1 parity suite (see the plan's Decisions log).
//
// `generate_expected_fixtures` (ignored by default) regenerates the
// expected/*.json files from the current producer. After running it, review
// the diff by hand against specs/loam-view.md before committing -- these
// files are the contract, not a snapshot dump.
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

/// Every workspace under cli/tests/fixtures/view/, in the order documented by
/// its README.md. Kept as an explicit list (not a directory scan) so adding a
/// fixture without adding contract coverage fails loudly -- see
/// `view_contract_fixture_directories_match_the_covered_set`.
const FIXTURES: &[&str] = &[
    "sparse",
    "healthy",
    "code-drift",
    "broken-links",
    "malformed",
    "degraded",
    "chronicle",
];

fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/view")
}

fn expected_path(name: &str) -> PathBuf {
    fixtures_root()
        .join("expected")
        .join(format!("{name}.json"))
}

/// A fresh nonce directory per call, so parallel tests never collide.
fn isolated_dir(prefix: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be after epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("{prefix}-{nonce}"));
    fs::create_dir_all(&dir).expect("isolated directory should be created");
    dir
}

/// Copies `name` into a fresh, non-git-controlled workspace whose directory
/// name is exactly the fixture name (nested under a nonce parent for
/// parallel safety) -- so `workspace.name` in the snapshot is the fixture
/// name itself rather than a nonce-suffixed temp path.
fn copy_fixture(name: &str) -> PathBuf {
    let source = fixtures_root().join(name);
    let dest = isolated_dir("loam-view-contract").join(name);
    copy_dir_recursive(&source, &dest);
    dest
}

fn copy_dir_recursive(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).expect("fixture directory should be created");
    for entry in fs::read_dir(src).expect("fixture directory should be readable") {
        let entry = entry.expect("fixture entry should be readable");
        let dst_path = dst.join(entry.file_name());
        if entry
            .file_type()
            .expect("file type should be readable")
            .is_dir()
        {
            copy_dir_recursive(&entry.path(), &dst_path);
        } else {
            fs::copy(entry.path(), &dst_path).expect("fixture file should be copied");
        }
    }
}

/// Runs `loam state --view <workspace>` with `LOAM_CONFIG_DIR` pinned to an
/// isolated temp root -- this producer doesn't read the federation/enrollment
/// registry today, but every child-process test in this repo pins it anyway
/// so that never becomes an accidental hermeticity gap.
fn view_snapshot(workspace: &Path) -> Output {
    let binary = std::env::var("CARGO_BIN_EXE_loam").expect("cargo should provide the loam binary");
    let config_dir = isolated_dir("loam-view-contract-cfg");
    let output = Command::new(binary)
        .args(["state", "--view", workspace.to_str().unwrap()])
        .env("LOAM_CONFIG_DIR", &config_dir)
        .output()
        .expect("loam should run");
    fs::remove_dir_all(&config_dir).ok();
    output
}

/// Returns the end index of the JSON value (string, object, array, or bare
/// number/literal) starting at byte offset `start` in `json`.
fn skip_json_value(json: &str, start: usize) -> usize {
    let bytes = json.as_bytes();
    let mut i = start;
    match bytes[i] {
        b'"' => {
            i += 1;
            while bytes[i] != b'"' {
                if bytes[i] == b'\\' {
                    i += 1;
                }
                i += 1;
            }
            i + 1
        }
        open @ (b'{' | b'[') => {
            let close = if open == b'{' { b'}' } else { b']' };
            let mut depth = 0i32;
            loop {
                match bytes[i] {
                    b'"' => {
                        i += 1;
                        while bytes[i] != b'"' {
                            if bytes[i] == b'\\' {
                                i += 1;
                            }
                            i += 1;
                        }
                    }
                    c if c == open => depth += 1,
                    c if c == close => {
                        depth -= 1;
                        if depth == 0 {
                            return i + 1;
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
        }
        _ => {
            while i < bytes.len() && !matches!(bytes[i], b',' | b'}' | b']') {
                i += 1;
            }
            i
        }
    }
}

/// Finds the next `"key":<value>` at or after byte offset `from` and returns
/// the byte range of `<value>` (which may be a string, number, object, or
/// array).
fn find_value_span(json: &str, key: &str, from: usize) -> Option<(usize, usize)> {
    let needle = format!("\"{key}\":");
    let key_pos = json[from..].find(&needle)? + from;
    let value_start = key_pos + needle.len();
    Some((value_start, skip_json_value(json, value_start)))
}

/// Replaces every `"key":<value>` occurrence in `json` with `"key":<replacement>`.
/// Handles repeats: `generated_at` (top-level, and echoed into every
/// relationship's `rule`), `duration_ms` (one per probe), and `age_minutes`/
/// `age_days` (one per hint/signal that reports a checkpoint or lint-marker
/// age) each occur more than once per snapshot.
fn normalize_field(mut json: String, key: &str, replacement: &str) -> String {
    let mut search_from = 0;
    while let Some((value_start, value_end)) = find_value_span(&json, key, search_from) {
        json.replace_range(value_start..value_end, replacement);
        search_from = value_start + replacement.len();
    }
    json
}

/// Replaces the `inner_key` value nested inside the *first* object found at
/// `"outer_key":{...}`, leaving every other object's `inner_key` (a much more
/// common field name, e.g. `value`) untouched.
fn normalize_nested_value(
    json: &str,
    outer_key: &str,
    inner_key: &str,
    replacement: &str,
) -> String {
    let Some((outer_start, outer_end)) = find_value_span(json, outer_key, 0) else {
        return json.to_owned();
    };
    let inner = normalize_field(
        json[outer_start..outer_end].to_owned(),
        inner_key,
        replacement,
    );
    format!("{}{inner}{}", &json[..outer_start], &json[outer_end..])
}

/// Strips every field this contract does not claim identity over:
/// - `generated_at` (wall-clock, top-level and echoed into relationship rules)
/// - probe `duration_ms` (wall-clock; only `codegraph`'s is ever non-zero)
/// - `workspace.root` (an absolute, machine-specific path)
/// - `workspace.platform` (the one field expected to genuinely differ per OS leg)
/// - `age_minutes` (hint evidence: real-clock minutes since a fixture's fixed
///   checkpoint timestamp -- ticks every run)
/// - `age_days` (the `memory-lint` signal's evidence, and the `value` inside
///   the `wiki.lint_age_days` metric: real-clock days since a fixture's fixed
///   lint-marker date)
///
/// `workspace.git` and `capabilities.qmd`/probe `qmd` are deliberately left
/// untouched: fixtures are copied into a fresh non-git temp directory that is
/// never registered with qmd (see `copy_fixture`), so both are already
/// deterministic -- "unavailable"/"not a git repository" and "absent"/"no qmd
/// config found" -- regardless of what's installed on the machine running the
/// test. `view_inventory.rs` already asserts the qmd literal on this same
/// copy-to-temp-dir pattern.
fn normalize_snapshot(json: &str) -> String {
    let json = normalize_field(json.to_owned(), "generated_at", "\"<generated_at>\"");
    let json = normalize_field(json, "duration_ms", "0");
    let json = normalize_field(json, "root", "\"<root>\"");
    let json = normalize_field(json, "platform", "\"<platform>\"");
    let json = normalize_field(json, "age_minutes", "0");
    let json = normalize_field(json, "age_days", "0");
    normalize_nested_value(&json, "wiki.lint_age_days", "value", "0")
}

fn run_contract(name: &str) -> String {
    let workspace = copy_fixture(name);
    let output = view_snapshot(&workspace);
    fs::remove_dir_all(&workspace).ok();
    assert!(
        output.status.success(),
        "loam state --view should succeed for fixture `{name}`: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    normalize_snapshot(&String::from_utf8(output.stdout).expect("snapshot stdout should be UTF-8"))
}

fn assert_contract(name: &str) {
    let actual = run_contract(name);
    let path = expected_path(name);
    let expected = fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "missing expected fixture at {}; run `cargo test --manifest-path cli/Cargo.toml --test view_contract -- --ignored generate_expected_fixtures` and review the diff",
            path.display()
        )
    });
    assert_eq!(
        actual.trim_end(),
        expected.trim_end(),
        "normalized snapshot for fixture `{name}` drifted from the checked-in contract at {}",
        path.display()
    );
}

#[test]
fn view_contract_fixture_directories_match_the_covered_set() {
    let mut found: Vec<String> = fs::read_dir(fixtures_root())
        .expect("fixtures/view should be readable")
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry
                .file_type()
                .map(|file_type| file_type.is_dir())
                .unwrap_or(false)
        })
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name != "expected")
        .collect();
    found.sort();

    let mut expected: Vec<String> = FIXTURES.iter().map(|name| (*name).to_owned()).collect();
    expected.sort();

    assert_eq!(
        found, expected,
        "a fixture directory was added or removed under cli/tests/fixtures/view/ without updating FIXTURES in view_contract.rs"
    );
}

#[test]
fn view_contract_sparse() {
    assert_contract("sparse");
}

#[test]
fn view_contract_healthy() {
    assert_contract("healthy");
}

#[test]
fn view_contract_code_drift() {
    assert_contract("code-drift");
}

#[test]
fn view_contract_broken_links() {
    assert_contract("broken-links");
}

#[test]
fn view_contract_malformed() {
    assert_contract("malformed");
}

#[test]
fn view_contract_degraded() {
    assert_contract("degraded");
}

#[test]
fn view_contract_chronicle() {
    assert_contract("chronicle");
}

#[test]
#[ignore = "regenerates cli/tests/fixtures/view/expected/*.json from the current producer -- run explicitly, then review the diff against specs/loam-view.md before committing"]
fn generate_expected_fixtures() {
    let dir = fixtures_root().join("expected");
    fs::create_dir_all(&dir).expect("expected/ directory should be creatable");
    for name in FIXTURES {
        let normalized = run_contract(name);
        fs::write(dir.join(format!("{name}.json")), format!("{normalized}\n"))
            .expect("expected fixture should be writable");
    }
}
