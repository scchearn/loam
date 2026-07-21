// The native SHA-256 replaces a `sha256sum` subprocess on the codegraph hot
// path, so it is checked against fixed SHA-256 vectors on files that cross the
// 64-byte block and 64 KiB buffer boundaries without requiring platform tools.
use std::fs;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

/// The binary's own hashing is only reachable through codegraph, so parity is
/// asserted through `codegraph diff`: pages carrying known hashes must be
/// reported current by the native hasher.
#[test]
fn native_hash_matches_known_vectors_across_buffer_boundaries() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("loam-sha256-{nonce}"));
    let codebase = root.join("code");
    let wiki = root.join("wiki");
    fs::create_dir_all(codebase.join("src")).expect("src");
    fs::create_dir_all(wiki.join("code")).expect("code pages");
    fs::write(wiki.join("SCHEMA.md"), "").expect("schema");

    // Empty, sub-block, exact block, block+1, multi-block, and past the 64 KiB
    // read buffer: every padding and chunking edge in one fixture.
    let cases = [
        (
            0usize,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        ),
        (
            1,
            "6e340b9cffb37a989ca544e6bb780a2c78901d3fb33738768511a30617afa01d",
        ),
        (
            55,
            "463eb28e72f82e0a96c0a4cc53690c571281131f672aa229e0d45ae59b598b59",
        ),
        (
            56,
            "da2ae4d6b36748f2a318f23e7ab1dfdf45acdc9d049bd80e59de82a60895f562",
        ),
        (
            63,
            "29af2686fd53374a36b0846694cc342177e428d1647515f078784d69cdb9e488",
        ),
        (
            64,
            "fdeab9acf3710362bd2658cdc9a29e8f9c757fcf9811603a8c447cd1d9151108",
        ),
        (
            65,
            "4bfd2c8b6f1eec7a2afeb48b934ee4b2694182027e6d0fc075074f2fabb31781",
        ),
        (
            1000,
            "4e4c294b331f7a2099a379bec34b9f9fc03dc46ab465d998f4d683da53487e6d",
        ),
        (
            65536,
            "4b640d85ab3ba30fd02c9fc9db4a8928f416322ad27022ea58a65aaee68a4df2",
        ),
        (
            65537,
            "237356e18b503616912abb8ffaed3a72591e397d4ac294c4637917d48a3f529d",
        ),
        (
            200_000,
            "e24bc62381f1224fbbb74688663f8f9743b9680b193edd666835e97b06e730eb",
        ),
    ];
    for (index, (size, hash)) in cases.iter().enumerate() {
        let name = format!("file_{index}.rs");
        let body: Vec<u8> = (0..*size).map(|byte| (byte % 251) as u8).collect();
        let path = codebase.join("src").join(&name);
        fs::write(&path, &body).expect("source");
        fs::write(
            wiki.join("code").join(format!("page-{index}.md")),
            format!(
                "---\nsource_path: src/{name}\ningested_at: \"1\"\nsource_size: \"{size}\"\ncontent_hash: \"{hash}\"\n---\n"
            ),
        )
        .expect("page");
    }

    let binary = std::env::var("CARGO_BIN_EXE_loam").expect("cargo should provide the loam binary");
    let output = Command::new(&binary)
        .args([
            "codegraph",
            "diff",
            codebase.to_str().unwrap(),
            wiki.to_str().unwrap(),
            "--strict",
        ])
        .output()
        .expect("loam should run");
    let text = String::from_utf8(output.stdout).expect("UTF-8");
    fs::remove_dir_all(&root).ok();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // Empty files are excluded from the walk, so they never appear either way.
    assert_eq!(
        text.trim(),
        "[]",
        "native hashing disagreed with known vectors: {text}"
    );
}

#[test]
fn native_hash_detects_a_single_changed_byte() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("loam-sha256-diff-{nonce}"));
    let codebase = root.join("code");
    let wiki = root.join("wiki");
    fs::create_dir_all(codebase.join("src")).expect("src");
    fs::create_dir_all(wiki.join("code")).expect("code pages");
    fs::write(wiki.join("SCHEMA.md"), "").expect("schema");

    let path = codebase.join("src/edited.rs");
    fs::write(&path, "fn value() -> u8 { 1 }\n").expect("source");
    let hash = "79c818189c3bf5c191ae5231ba3fe18c5416c0ecee924e9e6c35d3c04f2b1a14";
    fs::write(&path, "fn value() -> u8 { 2 }\n").expect("edited source");
    fs::write(
        wiki.join("code/edited.md"),
        format!(
            "---\nsource_path: src/edited.rs\ningested_at: \"1\"\nsource_size: \"23\"\ncontent_hash: \"{hash}\"\n---\n"
        ),
    )
    .expect("page");

    let binary = std::env::var("CARGO_BIN_EXE_loam").expect("cargo should provide the loam binary");
    let output = Command::new(&binary)
        .args([
            "codegraph",
            "diff",
            codebase.to_str().unwrap(),
            wiki.to_str().unwrap(),
            "--strict",
        ])
        .output()
        .expect("loam should run");
    let text = String::from_utf8(output.stdout).expect("UTF-8");
    fs::remove_dir_all(&root).ok();

    assert!(text.contains("\"reason\":\"stale\""), "{text}");
}
