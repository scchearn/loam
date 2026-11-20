//! Seals: content-bound, signed claims over marker spans.
//!
//! This module is public so items consumed only by tests until the CLI verbs
//! land do not trip `dead_code`.

pub mod canon;
pub mod evidence;
pub mod record;
pub mod sshsig;
pub mod trust;

use crate::markers::{self, Kind};
use std::fs;

/// Exit codes (spec D8): 0 ok, 1 refused or non-citable, 2 usage or
/// malformed input. Deliberately unlike `lib.rs`, where usage exits 1.
pub const EXIT_OK: i32 = 0;
pub const EXIT_REFUSED: i32 = 1;
pub const EXIT_USAGE: i32 = 2;

pub fn run(mut args: impl Iterator<Item = String>) -> i32 {
    match args.next().as_deref() {
        Some("hash") => hash(args),
        Some("trust") => trust::run(args),
        _ => usage(),
    }
}

fn usage() -> i32 {
    eprintln!("Usage:\n  loam seal hash <file> <anchor>\n  loam seal trust init|add <principal> <pubkey>|show");
    EXIT_USAGE
}

/// `loam seal hash <file> <anchor>`: print the `loam.canon/v1` digest of the
/// bytes strictly between the seal span's marker lines.
fn hash(args: impl Iterator<Item = String>) -> i32 {
    let args: Vec<String> = args.collect();
    let [file, anchor] = args.as_slice() else {
        return usage();
    };
    let bytes = match fs::read(file) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("loam seal hash: cannot read {file}: {error}");
            return EXIT_USAGE;
        }
    };
    let source = match std::str::from_utf8(&bytes) {
        Ok(source) => source,
        Err(error) => {
            eprintln!("loam seal hash: {file} is not valid UTF-8 ({error})");
            return EXIT_USAGE;
        }
    };
    let id = anchor.trim_start_matches('#').trim_start_matches('^');
    let scan = markers::scan(source);
    let Some(marker) = scan
        .markers
        .iter()
        .find(|marker| marker.kind == Kind::Seal && marker.id == id)
    else {
        eprintln!("loam seal hash: no well-formed seal span ^{id} in {file}");
        return EXIT_USAGE;
    };
    match canon::digest(&bytes[marker.content.clone()]) {
        Ok(digest) => {
            println!("{digest}");
            EXIT_OK
        }
        Err(error) => {
            eprintln!("loam seal hash: span ^{id}: {error}");
            EXIT_USAGE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::evidence::{self, AttestOptions, Ctx, FailKind};
    use super::record;
    use std::fs;
    use std::path::PathBuf;

    const STAMP: &str = "2026-11-20 14:30 +02:00";

    struct Dir(PathBuf);
    impl Dir {
        fn new() -> Dir {
            let dir =
                std::env::temp_dir().join(format!("loam-seal-captures-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(dir.join("wiki")).unwrap();
            Dir(dir)
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            for _ in 0..10 {
                if fs::remove_dir_all(&self.0).is_ok() {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    }

    /// S20: a capture under the cap is stored with origin and retrieved_at,
    /// survives a round trip through a record, and an over-cap capture is
    /// refused with nothing written; declared redactions land in `redacted[]`.
    #[test]
    fn captures() {
        let dir = Dir::new();
        let wiki = dir.0.join("wiki");
        let ctx = Ctx {
            repo_root: dir.0.clone(),
            wiki: wiki.clone(),
            retrieved_at: STAMP.to_owned(),
            capture_max_bytes: 16,
        };
        fs::write(dir.0.join("run.log"), b"all tests pass").unwrap();
        fs::write(dir.0.join("huge.log"), vec![b'x'; 17]).unwrap();

        let over = [evidence::parse("capture:huge.log").unwrap()];
        let refused = evidence::resolve_all(&over, &ctx, &AttestOptions::default()).unwrap_err();
        assert_eq!(refused[0].kind, FailKind::OverCap);
        assert!(
            !record::seals_dir(&wiki).exists(),
            "refusal must write nothing"
        );

        let items = [evidence::parse("capture:run.log").unwrap()];
        let options = AttestOptions {
            origin: Some("ci job 7"),
        };
        let resolved = evidence::resolve_all(&items, &ctx, &options).unwrap();
        evidence::store_captures(&wiki, &resolved).unwrap();
        let digest = resolved[0].item.sha256.clone().unwrap();
        assert_eq!(
            fs::read(record::evidence_path(&wiki, &digest)).unwrap(),
            b"all tests pass"
        );

        let value = resolved[0].item.to_value();
        assert_eq!(
            value.get("origin").and_then(|v| v.as_str()),
            Some("ci job 7")
        );
        assert_eq!(
            value.get("retrieved_at").and_then(|v| v.as_str()),
            Some(STAMP)
        );
        let reparsed =
            evidence::Item::from_value(&crate::json::parse(&value.to_json()).unwrap()).unwrap();
        assert_eq!(
            evidence::check(&reparsed, &ctx).code,
            evidence::Code::Current
        );

        let redacted = evidence::redacted_values(&["token".to_owned()]);
        assert_eq!(redacted[0].as_str(), Some("token"));
    }
}
