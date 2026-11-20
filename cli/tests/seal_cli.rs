use std::path::PathBuf;
use std::process::{Command, Output};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/seal")
        .join(name)
}

fn loam(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_loam"))
        .args(args)
        .env("LOAM_WORKSPACES", "off")
        .output()
        .expect("loam should run")
}

fn hash(name: &str, anchor: &str) -> Output {
    loam(&["seal", "hash", fixture(name).to_str().unwrap(), anchor])
}

fn digest(name: &str, anchor: &str) -> String {
    let output = hash(name, anchor);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

#[test]
fn hash_prints_the_canon_digest_of_a_span_found_by_anchor() {
    let value = digest("lf.md", "s-trust-rung-order");
    assert_eq!(value.len(), 64);
    assert!(value.bytes().all(|b| b.is_ascii_hexdigit()));
    let expected = loam::sha256::hex(
        b"TLS tries the bundled rung first.\n\nThen it falls back to the system store.\n",
    );
    assert_eq!(value, expected);
}

#[test]
fn anchor_may_carry_a_caret_prefix() {
    assert_eq!(
        digest("lf.md", "^s-trust-rung-order"),
        digest("lf.md", "s-trust-rung-order")
    );
}

#[test]
fn crlf_trailing_space_and_blank_lines_hash_like_the_lf_page() {
    assert_eq!(
        digest("crlf.md", "s-trust-rung-order"),
        digest("lf.md", "s-trust-rung-order")
    );
}

#[test]
fn reflowed_paragraph_hashes_differently() {
    assert_ne!(
        digest("reflowed.md", "s-trust-rung-order"),
        digest("lf.md", "s-trust-rung-order")
    );
}

#[test]
fn unknown_anchor_exits_2() {
    assert_eq!(hash("lf.md", "s-nope").status.code(), Some(2));
}

#[test]
fn invalid_utf8_exits_2() {
    assert_eq!(
        hash("invalid-utf8.md", "s-bad-bytes").status.code(),
        Some(2)
    );
}

#[test]
fn missing_file_exits_2() {
    let output = loam(&["seal", "hash", "does-not-exist.md", "s-x"]);
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn usage_errors_exit_2() {
    assert_eq!(loam(&["seal"]).status.code(), Some(2));
    assert_eq!(
        loam(&["seal", "hash", "only-one-arg"]).status.code(),
        Some(2)
    );
    assert_eq!(loam(&["seal", "verify"]).status.code(), Some(2));
}
