//! Seals: content-bound, signed claims over marker spans.
//!
//! This module is public so items consumed only by tests until the CLI verbs
//! land do not trip `dead_code`.

pub mod canon;

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
        _ => usage(),
    }
}

fn usage() -> i32 {
    eprintln!("Usage:\n  loam seal hash <file> <anchor>");
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
