use std::fs;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn temporary_wiki() -> std::path::PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be after epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("loam-datecheck-{nonce}"));
    fs::create_dir_all(&path).expect("temporary wiki should be created");
    path
}

fn drifted_note(wiki: &std::path::Path) {
    fs::write(
        wiki.join("note.md"),
        "---\ncreated_at: 2026-06-26 11:07\nupdated_at: 2026-06-26 11:07 UTC\n---\n- Captured: 2026-06-26 11:07\n- 2026-06-26 - decision\n",
    )
    .expect("drifted note should be written");
}

#[test]
fn datecheck_check_reports_drift_and_exits_two() {
    let wiki = temporary_wiki();
    drifted_note(&wiki);

    let binary = std::env::var("CARGO_BIN_EXE_loam").expect("cargo should provide the loam binary");
    let output = Command::new(&binary)
        .args([
            "datecheck",
            "check",
            wiki.to_str().unwrap(),
            "--offset",
            "+02:00",
        ])
        .output()
        .expect("loam should run");
    fs::remove_dir_all(&wiki).expect("temporary wiki should be removed");

    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        String::from_utf8(output.stdout).expect("datecheck output should be UTF-8"),
        concat!(
            "{\"file\":\"note.md\",\"line\":2,\"field\":\"created_at\",\"value\":\"2026-06-26 11:07\",\"issue\":\"missing_offset\",\"fix\":\"add +02:00\"}\n",
            "{\"file\":\"note.md\",\"line\":3,\"field\":\"updated_at\",\"value\":\"2026-06-26 11:07 UTC\",\"issue\":\"legacy_tz\",\"fix\":\"replace with +02:00\"}\n",
            "{\"file\":\"note.md\",\"line\":5,\"field\":\"Captured\",\"value\":\"2026-06-26 11:07\",\"issue\":\"missing_offset\",\"fix\":\"add +02:00\"}\n",
            "{\"file\":\"note.md\",\"line\":6,\"field\":\"decisions_log\",\"value\":\" - \",\"issue\":\"wrong_separator\",\"fix\":\"use em-dash —\"}\n",
        )
    );
}

#[test]
fn datecheck_fix_is_idempotent_and_normalizes_the_file() {
    let wiki = temporary_wiki();
    drifted_note(&wiki);
    let binary = std::env::var("CARGO_BIN_EXE_loam").expect("cargo should provide the loam binary");

    let first = Command::new(&binary)
        .args([
            "datecheck",
            "fix",
            wiki.to_str().unwrap(),
            "--offset",
            "+02:00",
        ])
        .output()
        .expect("loam should run");
    assert!(first.status.success());
    assert_eq!(
        String::from_utf8(first.stdout).expect("fix output should be UTF-8"),
        "note.md\n{\"mode\":\"fix\",\"offset\":\"+02:00\",\"files_fixed\":1}\n"
    );
    assert_eq!(
        fs::read_to_string(wiki.join("note.md")).expect("fixed note should be readable"),
        "---\ncreated_at: 2026-06-26 11:07 +02:00\nupdated_at: 2026-06-26 11:07 +02:00\n---\n- Captured: 2026-06-26 11:07 +02:00\n- 2026-06-26 — decision\n"
    );

    let check = Command::new(&binary)
        .args([
            "datecheck",
            "check",
            wiki.to_str().unwrap(),
            "--offset",
            "+02:00",
        ])
        .output()
        .expect("loam should run");
    assert!(check.status.success());
    assert!(check.stdout.is_empty());

    let second = Command::new(&binary)
        .args([
            "datecheck",
            "fix",
            wiki.to_str().unwrap(),
            "--offset",
            "+02:00",
        ])
        .output()
        .expect("loam should run");
    fs::remove_dir_all(&wiki).expect("temporary wiki should be removed");

    assert!(second.status.success());
    assert_eq!(
        String::from_utf8(second.stdout).expect("fix output should be UTF-8"),
        "{\"mode\":\"fix\",\"offset\":\"+02:00\",\"files_fixed\":0}\n"
    );
}

#[test]
fn datecheck_ignores_unicode_bullets_without_panicking() {
    let wiki = temporary_wiki();
    fs::write(
        wiki.join("note.md"),
        "- protect → shield, boundary, watch mark\n",
    )
    .expect("unicode note should be written");
    let binary = std::env::var("CARGO_BIN_EXE_loam").expect("cargo should provide the loam binary");

    let output = Command::new(binary)
        .args([
            "datecheck",
            "check",
            wiki.to_str().unwrap(),
            "--offset",
            "+02:00",
        ])
        .output()
        .expect("loam should run");
    fs::remove_dir_all(&wiki).expect("temporary wiki should be removed");

    assert!(output.status.success());
    assert!(output.stdout.is_empty());
}
