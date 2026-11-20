//! Seals: content-bound, signed claims over marker spans.
//!
//! This module is public so items consumed only by tests until the CLI verbs
//! land do not trip `dead_code`.

pub mod canon;
pub mod evidence;
pub mod policy;
pub mod record;
pub mod sshsig;
pub mod trust;
pub mod verdict;
pub mod verify;

use crate::markers::{self, Kind};
use policy::Level;
use std::fs;
use std::path::{Path, PathBuf};

/// Exit codes (spec D8): 0 ok, 1 refused or non-citable, 2 usage or
/// malformed input. Deliberately unlike `lib.rs`, where usage exits 1.
pub const EXIT_OK: i32 = 0;
pub const EXIT_REFUSED: i32 = 1;
pub const EXIT_USAGE: i32 = 2;

pub fn run(mut args: impl Iterator<Item = String>) -> i32 {
    match args.next().as_deref() {
        Some("hash") => hash(args),
        Some("trust") => trust::run(args),
        Some("verify") => verify_command(args),
        Some("status") => status_command(args),
        _ => usage(),
    }
}

fn usage() -> i32 {
    eprintln!(
        "Usage:\n  loam seal hash <file> <anchor>\n  loam seal verify [<page>[#^anchor]] [--json] [--now <stamp>]\n  loam seal status [--json] [--now <stamp>]\n  loam seal trust init|add <principal> <pubkey>|show"
    );
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

/// The wiki the command runs against: the nearest `wiki/` directory at or
/// above the current directory.
fn find_wiki(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .map(|dir| dir.join("wiki"))
        .find(|candidate| candidate.is_dir())
}

struct ReadOptions {
    selector: verify::Selector,
    json: bool,
    now: chrono::DateTime<chrono::FixedOffset>,
}

fn parse_read_options(
    verb: &str,
    args: impl Iterator<Item = String>,
    allow_selector: bool,
) -> Result<ReadOptions, String> {
    let mut selector = None;
    let mut json = false;
    let mut now = None;
    let mut args = args;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--json" => json = true,
            "--now" => {
                let stamp = args.next().ok_or("--now needs a timestamp")?;
                now = Some(policy::parse_stamp(&stamp).ok_or_else(|| {
                    format!("--now `{stamp}` must be `YYYY-MM-DD HH:MM +HH:MM` or RFC 3339 with an offset")
                })?);
            }
            flag if flag.starts_with("--") => return Err(format!("unknown option {flag}")),
            _ if allow_selector && selector.is_none() => {
                selector = Some(verify::Selector::parse(&arg)?);
            }
            _ => return Err(format!("unexpected argument `{arg}` for loam seal {verb}")),
        }
    }
    Ok(ReadOptions {
        selector: selector.unwrap_or_default(),
        json,
        now: now.unwrap_or_else(|| {
            let utc = chrono::FixedOffset::east_opt(0).expect("zero offset is valid");
            chrono::Utc::now().with_timezone(&utc)
        }),
    })
}

/// Verifies every seal in the nearest wiki. `Err` carries an exit code
/// after the diagnostic has been printed.
fn verify_wiki(verb: &str, options: &ReadOptions) -> Result<Vec<verify::Report>, i32> {
    let fail = |message: String| {
        eprintln!("loam seal {verb}: {message}");
        EXIT_USAGE
    };
    let cwd = std::env::current_dir()
        .map_err(|e| fail(format!("cannot read the current directory: {e}")))?;
    let wiki = find_wiki(&cwd)
        .ok_or_else(|| fail(format!("no wiki/ directory at or above {}", cwd.display())))?;
    let env = verify::Env::load(&wiki, trust::seal_dir().ok(), options.now).map_err(&fail)?;
    verify::verify_all(&env).map_err(fail)
}

fn level_text(level: Option<Level>) -> &'static str {
    level.map_or("none", Level::as_str)
}

/// `loam seal verify [<page>[#^anchor]] [--json] [--now <stamp>]`.
fn verify_command(args: impl Iterator<Item = String>) -> i32 {
    let options = match parse_read_options("verify", args, true) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("loam seal verify: {message}");
            return usage();
        }
    };
    let reports = match verify_wiki("verify", &options) {
        Ok(reports) => reports,
        Err(code) => return code,
    };
    let selected: Vec<&verify::Report> = reports
        .iter()
        .filter(|report| report.matches(&options.selector))
        .collect();
    let targeted = options.selector != verify::Selector::default();
    if targeted && selected.is_empty() {
        eprintln!("loam seal verify: no seal record matches the selector");
        return EXIT_USAGE;
    }
    for report in &selected {
        if options.json {
            println!("{}", report.to_value().to_json());
        } else {
            print_human(report);
        }
    }
    if !options.json && selected.is_empty() {
        println!("no seals");
    }
    if selected.iter().all(|report| report.is_citable()) {
        EXIT_OK
    } else {
        EXIT_REFUSED
    }
}

fn print_human(report: &verify::Report) {
    println!(
        "{}  {}  {}  {}  {}",
        report.id,
        report.outcome.verdict.as_str(),
        level_text(report.outcome.effective_level),
        report.subject.as_deref().unwrap_or("-"),
        report.cite_as().unwrap_or_else(|| "not citable".to_owned()),
    );
    let codes = &report.outcome.codes;
    for code in codes.failure.iter().chain(&codes.informational) {
        println!("    {code}");
    }
    if let Some(location) = &report.likely_location {
        println!("    likely location: {location}");
    }
}

/// `loam seal status [--json]`: counts per verdict.
fn status_command(args: impl Iterator<Item = String>) -> i32 {
    let options = match parse_read_options("status", args, false) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("loam seal status: {message}");
            return usage();
        }
    };
    let reports = match verify_wiki("status", &options) {
        Ok(reports) => reports,
        Err(code) => return code,
    };
    let counts = verify::Counts::of(&reports);
    if options.json {
        let fields = counts
            .entries()
            .iter()
            .map(|(name, count)| {
                (
                    (*name).to_owned(),
                    crate::json::Value::Number(count.to_string()),
                )
            })
            .collect();
        println!("{}", crate::json::Value::Object(fields).to_json());
    } else {
        for (name, count) in counts.entries() {
            println!("{name}: {count}");
        }
    }
    EXIT_OK
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

    use super::evidence::{Code, Kind, Recheck};
    use super::policy::{self, Level, SignatureStanding};
    use super::verdict::fixture::{at, cited, note, primary, verified, Case};
    use super::verdict::{code, Verdict};
    use crate::json::Value;

    fn codes(outcome: &super::verdict::Outcome) -> Vec<&'static str> {
        let c = &outcome.codes;
        c.success
            .iter()
            .chain(&c.informational)
            .chain(&c.failure)
            .copied()
            .collect()
    }

    /// S6 (a): note-only and unsealed-page evidence never rises above
    /// unsealed.
    #[test]
    fn level_ceiling_note_only_is_unverifiable() {
        let mut c = Case::citable();
        c.requested = Level::Evidence;
        c.evidence = vec![note()];
        let got = c.run();
        assert_eq!(got.verdict, Verdict::Unverifiable);
        assert_eq!(got.effective_level, None);
        assert!(!got.codes.success.contains(&code::EVIDENCE_CURRENT));

        // A wiki page is zero-weight too (the resolver assigns the weight).
        let mut page = primary(Kind::File);
        page.weight = super::evidence::Weight::Zero;
        c.evidence = vec![page, note()];
        assert_eq!(c.run().effective_level, None);
    }

    /// A fresh seal without `review_by` raises nothing that lint surfaces.
    #[test]
    fn fresh_seal_without_review_by_has_no_lint_code() {
        let c = Case::citable();
        let got = c.run();
        assert_eq!(got.verdict, Verdict::Citable);
        for code in got.codes.informational.iter().chain(&got.codes.failure) {
            assert_ne!(code::lint_code(code), Some("SEL006"), "{code}");
        }
    }

    /// Levels are cumulative: a canonical-only grant reaches canonical.
    #[test]
    fn level_ceiling_canonical_only_grant_implies_audited() {
        let mut c = Case::citable();
        c.requested = Level::Canonical;
        c.signature = verified("carol@example.com", "sk-ssh-ed25519@openssh.com");
        c.policy.canonical_principals = vec!["carol@example.com".into()];
        assert_eq!(c.run().effective_level, Some(Level::Canonical));
    }

    /// S6 (b): canonical requested by a principal granted only audited.
    #[test]
    fn level_ceiling_overclaim_reports_sel008() {
        let mut c = Case::citable();
        c.requested = Level::Canonical;
        let got = c.run();
        assert_eq!(got.verdict, Verdict::Citable);
        assert_eq!(got.effective_level, Some(Level::Audited));
        assert_eq!(got.requested_level, Level::Canonical);
        assert!(got
            .codes
            .informational
            .contains(&code::LEVEL_ABOVE_ATTAINABLE));
        assert_eq!(
            code::lint_code(code::LEVEL_ABOVE_ATTAINABLE),
            Some("SEL008")
        );
    }

    /// Ceilings per D2: no signature gives evidence; an unlisted signer gives
    /// evidence; canonical needs the canonical list and `min_signers`.
    #[test]
    fn level_ceiling_by_signer() {
        let mut c = Case::citable();
        c.requested = Level::Canonical;

        c.signature = SignatureStanding::Unsigned;
        assert_eq!(c.run().effective_level, Some(Level::Evidence));

        c.signature = verified("alice@example.com", "ssh-ed25519");
        assert_eq!(c.run().effective_level, Some(Level::Evidence));

        c.policy.audited_principals.push("alice@example.com".into());
        assert_eq!(c.run().effective_level, Some(Level::Audited));

        c.policy.canonical_principals = vec!["alice@example.com".into()];
        let got = c.run();
        assert_eq!(got.effective_level, Some(Level::Canonical));
        assert!(got.codes.informational.contains(&code::UNATTENDED_KEY));

        // A hardware-backed key carries no unattended note.
        c.signature = verified("alice@example.com", "sk-ssh-ed25519@openssh.com");
        let got = c.run();
        assert_eq!(got.effective_level, Some(Level::Canonical));
        assert!(!got.codes.informational.contains(&code::UNATTENDED_KEY));

        // min_signers needs a second distinct canonical signer.
        c.policy.canonical_min_signers = 2;
        assert_eq!(c.run().effective_level, Some(Level::Audited));
        c.cosigners = vec!["alice@example.com".into()];
        assert_eq!(
            c.run().effective_level,
            Some(Level::Audited),
            "same signer twice"
        );
        c.cosigners = vec!["bob@example.com".into()];
        assert_eq!(
            c.run().effective_level,
            Some(Level::Audited),
            "bob not listed"
        );
        c.policy.canonical_principals.push("bob@example.com".into());
        assert_eq!(c.run().effective_level, Some(Level::Canonical));
    }

    /// Audited needs every item to resolve; evidence needs only one primary.
    #[test]
    fn level_ceiling_all_items_must_resolve_for_audited() {
        let mut c = Case::citable();
        let mut orphan = primary(Kind::Commit);
        orphan.code = Code::Orphaned;
        c.evidence = vec![primary(Kind::Blob), orphan];
        let got = c.run();
        assert_eq!(got.verdict, Verdict::Citable);
        assert_eq!(got.effective_level, Some(Level::Evidence));
        assert!(got
            .codes
            .informational
            .contains(&code::LEVEL_ABOVE_ATTAINABLE));
    }

    /// S6 (c): a derived seal takes the minimum level of what it cites.
    #[test]
    fn level_ceiling_derived_takes_minimum() {
        let mut c = Case::citable();
        c.requested = Level::Audited;
        c.evidence = vec![cited(Some(Level::Evidence))];
        let got = c.run();
        assert_eq!(got.effective_level, Some(Level::Evidence));
        assert!(got.codes.informational.contains(&code::CAPPED_DERIVED));

        c.evidence = vec![cited(Some(Level::Canonical)), cited(Some(Level::Audited))];
        assert_eq!(c.run().effective_level, Some(Level::Audited));

        c.evidence = vec![primary(Kind::Blob), cited(Some(Level::Evidence))];
        assert_eq!(c.run().effective_level, Some(Level::Evidence));
    }

    /// Pure `redaction_cap`: an audited signer with `redacted:["token"]`
    /// ends at evidence.
    #[test]
    fn level_ceiling_redaction_cap() {
        let redacted = vec![Value::String("token".into())];
        assert_eq!(policy::redaction_cap(&redacted), Some(Level::Evidence));
        assert_eq!(policy::redaction_cap(&[]), None);

        let mut c = Case::citable();
        c.redacted = redacted;
        let got = c.run();
        assert_eq!(got.verdict, Verdict::Citable);
        assert_eq!(got.effective_level, Some(Level::Evidence));
        assert!(got.codes.informational.contains(&code::CAPPED_REDACTION));
    }

    /// With the verifier unavailable the verdict is not refused but the
    /// level is no higher than evidence, with the informational code.
    #[test]
    fn level_ceiling_verifier_unavailable() {
        let mut c = Case::citable();
        c.signature = SignatureStanding::VerifierUnavailable;
        c.requested = Level::Canonical;
        let got = c.run();
        assert_eq!(got.verdict, Verdict::Citable);
        assert_eq!(got.effective_level, Some(Level::Evidence));
        assert!(codes(&got).contains(&code::VERIFIER_UNAVAILABLE));
        assert!(got.codes.failure.is_empty());
    }

    /// S8: evidence drift is stale with SEL005.
    #[test]
    fn stale_evidence_drift() {
        let mut c = Case::citable();
        let mut drifted = primary(Kind::Blob);
        drifted.code = Code::Drifted;
        c.evidence = vec![drifted];
        let got = c.run();
        assert_eq!(got.verdict, Verdict::Stale);
        assert!(got.codes.failure.contains(&code::EVIDENCE_DRIFTED));
        assert_eq!(code::lint_code(code::EVIDENCE_DRIFTED), Some("SEL005"));

        let mut gone = primary(Kind::Blob);
        gone.code = Code::Unresolvable;
        c.evidence = vec![gone];
        assert_eq!(c.run().verdict, Verdict::Stale);
    }

    /// S8: a passed `review_by` is stale with SEL006; a future one is not.
    #[test]
    fn stale_review_by() {
        let mut c = Case::citable();
        c.review_by = Some(at("2026-12-01 00:00 +02:00"));
        c.now = at("2026-11-30 23:59 +02:00");
        assert_eq!(c.run().verdict, Verdict::Citable);
        c.now = at("2026-12-01 00:01 +02:00");
        let got = c.run();
        assert_eq!(got.verdict, Verdict::Stale);
        assert!(got.codes.failure.contains(&code::REVIEW_BY_PASSED));
        assert_eq!(code::lint_code(code::REVIEW_BY_PASSED), Some("SEL006"));
    }

    /// S8: a due `every:<dur>` recheck is stale; before it is due, citable.
    #[test]
    fn stale_recheck_due() {
        let mut c = Case::citable();
        let mut item = primary(Kind::Blob);
        item.recheck = Recheck::Every("30d".into());
        c.evidence = vec![item];
        c.now = at("2026-12-19 14:30 +02:00");
        assert_eq!(c.run().verdict, Verdict::Citable);
        c.now = at("2026-12-20 14:30 +02:00");
        let got = c.run();
        assert_eq!(got.verdict, Verdict::Stale);
        assert!(got.codes.failure.contains(&code::RECHECK_DUE));
    }

    /// S8: `review_by` of 2099 is treated as expiring at the 180-day cap.
    #[test]
    fn stale_review_by_2099_expires_at_cap() {
        let mut c = Case::citable();
        c.review_by = Some(at("2099-01-01 00:00 +00:00"));
        // time_verified 2026-11-20 14:30 +02:00 plus 180 days.
        c.now = at("2027-05-19 14:29 +02:00");
        let got = c.run();
        assert_eq!(got.verdict, Verdict::Citable);
        assert!(got.codes.informational.contains(&code::REVIEW_BY_CLAMPED));

        c.now = at("2027-05-19 14:31 +02:00");
        let got = c.run();
        assert_eq!(got.verdict, Verdict::Stale);
        assert!(got.codes.failure.contains(&code::REVIEW_BY_CAPPED));

        // The cap follows the reader's policy, not a constant.
        c.policy.max_review = "30d".into();
        c.now = at("2026-12-21 00:00 +02:00");
        assert_eq!(c.run().verdict, Verdict::Stale);
    }

    /// Equivalent instants in different offsets give the same outcome.
    #[test]
    fn stale_is_offset_independent() {
        let mut c = Case::citable();
        c.review_by = Some(at("2026-12-01 00:00 +02:00"));
        c.now = at("2026-12-01 00:01 +02:00");
        let a = c.run();
        c.now = at("2026-11-30 22:01 +00:00");
        c.review_by = Some(at("2026-11-30 22:00 +00:00"));
        assert_eq!(a, c.run());
        assert_eq!(a.verdict, Verdict::Stale);
    }

    /// A seal with no `review_by` expires at the reader's `max_review` cap.
    #[test]
    fn stale_missing_review_by_expires_at_cap() {
        let mut c = Case::citable();
        c.now = at("2027-05-19 14:29 +02:00");
        let got = c.run();
        assert_eq!(got.verdict, Verdict::Citable);
        assert!(got.codes.informational.contains(&code::REVIEW_BY_CLAMPED));
        c.now = at("2027-05-19 14:31 +02:00");
        let got = c.run();
        assert_eq!(got.verdict, Verdict::Stale);
        assert!(got.codes.failure.contains(&code::REVIEW_BY_CAPPED));
    }

    /// A forward-dated `time_verified` cannot buy a long window.
    #[test]
    fn stale_time_verified_in_future() {
        let mut c = Case::citable();
        c.time_verified = at("2099-01-01 00:00 +00:00");
        c.review_by = Some(at("2099-02-01 00:00 +00:00"));
        let got = c.run();
        assert_eq!(got.verdict, Verdict::Stale);
        assert!(got.codes.failure.contains(&code::TIME_VERIFIED_FUTURE));
        assert_eq!(code::lint_code(code::TIME_VERIFIED_FUTURE), Some("SEL006"));
        // Within clock skew is fine.
        let mut c = Case::citable();
        c.time_verified = at("2026-11-20 14:34 +02:00");
        c.now = at("2026-11-20 14:30 +02:00");
        assert_eq!(c.run().verdict, Verdict::Citable);
    }

    /// Untrusted dates and durations never panic the verifier.
    #[test]
    fn stale_overflow_does_not_panic() {
        let mut c = Case::citable();
        c.policy.max_review = "4294967295d".into();
        c.review_by = Some(at("2099-01-01 00:00 +00:00"));
        let _ = c.run();

        let mut c = Case::citable();
        c.time_verified = at("+262000-01-01 00:00 +00:00");
        c.review_by = Some(at("2099-01-01 00:00 +00:00"));
        let mut item = primary(Kind::Blob);
        item.recheck = Recheck::Every("9999w".into());
        c.evidence = vec![item];
        assert_eq!(c.run().verdict, Verdict::Stale);
    }

    /// Exact boundaries: `review_by` lapses strictly after the instant, a
    /// recheck is due at the instant.
    #[test]
    fn stale_boundaries() {
        let mut c = Case::citable();
        c.review_by = Some(at("2026-12-01 00:00 +02:00"));
        c.now = at("2026-12-01 00:00 +02:00");
        assert_eq!(c.run().verdict, Verdict::Citable);

        let mut c = Case::citable();
        let mut item = primary(Kind::Blob);
        item.recheck = Recheck::Every("30d".into());
        c.evidence = vec![item];
        c.now = at("2026-12-20 14:30 +02:00");
        assert_eq!(c.run().verdict, Verdict::Stale);
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

    // ----------------------------------------------------------- verify

    use super::sshsig;
    use super::trust;
    use super::verify::{self, Env};
    use std::path::Path;
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicU64, Ordering};

    static WORLDS: AtomicU64 = AtomicU64::new(0);

    /// A temp workspace (`wiki/`, evidence source files) plus a pinned
    /// trust directory standing in for `<configRoot>/seal`.
    struct World {
        root: PathBuf,
        wiki: PathBuf,
        trust_dir: PathBuf,
        bin: Option<PathBuf>,
    }

    impl World {
        fn new() -> World {
            let n = WORLDS.fetch_add(1, Ordering::SeqCst);
            let root =
                std::env::temp_dir().join(format!("loam-seal-verify-{}-{n}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            let wiki = root.join("wiki");
            let trust_dir = root.join("config").join("seal");
            fs::create_dir_all(&wiki).unwrap();
            fs::create_dir_all(&trust_dir).unwrap();
            fs::write(root.join("run.log"), b"all tests pass").unwrap();
            World {
                root,
                wiki,
                trust_dir,
                bin: sshsig::keygen_for_tests(),
            }
        }

        fn make_key(&self, name: &str) -> (PathBuf, String) {
            let bin = self.bin.as_ref().unwrap();
            let key = self.root.join(name);
            let out = Command::new(bin)
                .args(["-q", "-t", "ed25519", "-N", "", "-C", name, "-f"])
                .arg(&key)
                .output()
                .unwrap();
            assert!(out.status.success(), "ssh-keygen failed");
            let public = fs::read_to_string(key.with_extension("pub")).unwrap();
            let mut parts = public.split_whitespace();
            let line = format!("{} {}", parts.next().unwrap(), parts.next().unwrap());
            (key, line)
        }

        fn allow(&self, lines: &[String]) {
            fs::write(
                self.trust_dir.join(trust::ALLOWED_FILE),
                lines.join("\n") + "\n",
            )
            .unwrap();
        }

        fn policy(&self, audited: &[&str]) {
            let policy = trust::Policy {
                audited_principals: audited.iter().map(|p| (*p).to_owned()).collect(),
                ..trust::Policy::default()
            };
            fs::write(
                self.trust_dir.join(trust::POLICY_FILE),
                policy.to_json().to_json(),
            )
            .unwrap();
        }

        fn env(&self, now: &str) -> Env {
            let mut env = Env::load(
                &self.wiki,
                Some(self.trust_dir.clone()),
                super::policy::parse_stamp(now).unwrap(),
            )
            .unwrap();
            env.repo_root = self.root.clone();
            env.keygen = self
                .bin
                .clone()
                .unwrap_or_else(|| self.root.join("no-such-keygen"));
            env
        }

        fn reports(&self, now: &str) -> Vec<verify::Report> {
            verify::verify_all(&self.env(now)).unwrap()
        }

        fn only(&self, now: &str) -> verify::Report {
            let mut reports = self.reports(now);
            assert_eq!(reports.len(), 1, "{reports:?}");
            reports.remove(0)
        }

        fn page_path(&self, page: &str) -> PathBuf {
            self.wiki.join(format!("{page}.md"))
        }

        /// Writes the page and a record for it; returns the record id.
        fn seal(&self, spec: &Spec, sig: Sig<'_>) -> String {
            let text = format!(
                "# Alpha\n\n<!-- loam:seal {} -->\n{}\n<!-- /loam:seal -->\n",
                spec.anchor, spec.body
            );
            let path = self.page_path(spec.page);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, &text).unwrap();
            let scan = crate::markers::scan(&text);
            let marker = &scan.markers[0];
            let content = &text.as_bytes()[marker.content.clone()];
            let canonical =
                String::from_utf8(super::canon::canonicalize(content).unwrap()).unwrap();

            let ctx = Ctx {
                repo_root: self.root.clone(),
                wiki: self.wiki.clone(),
                retrieved_at: STAMP.to_owned(),
                capture_max_bytes: 262_144,
            };
            let items = [evidence::parse("capture:run.log").unwrap()];
            let resolved = evidence::resolve_all(&items, &ctx, &AttestOptions::default()).unwrap();
            evidence::store_captures(&self.wiki, &resolved).unwrap();

            let (id, bytes) = record::build(&record::NewRecord {
                kind: record::RecordKind::Seal,
                page: spec.page.to_owned(),
                anchor: spec.anchor.to_owned(),
                subject_sha256: super::canon::digest(content).unwrap(),
                locator: record::soft_locator(&["Alpha".to_owned()], &canonical),
                content: canonical,
                requested_level: Some(spec.requested.to_owned()),
                claim: Some("TLS order".to_owned()),
                procedure_uri: "loam://seal/procedure/v1".to_owned(),
                principal: spec.principal.clone(),
                acted_on_behalf_of: None,
                generator: record::Generator {
                    name: "loam".to_owned(),
                    version: "1.0.0".to_owned(),
                    harness: "test".to_owned(),
                },
                time_verified: spec.time_verified.to_owned(),
                review_by: spec.review_by.map(str::to_owned),
                evidence: resolved.iter().map(|r| r.item.to_value()).collect(),
                redacted: evidence::redacted_values(&spec.redacted),
                parent: None,
                status: None,
            })
            .unwrap();
            record::write_record(&self.wiki, &bytes).unwrap();
            match sig {
                Sig::None => {}
                Sig::Raw(text) => record::write_sig(&self.wiki, &id, text.as_bytes()).unwrap(),
                Sig::Key { key, namespace } => {
                    let armored = self.sign(key, namespace, &bytes);
                    record::write_sig(&self.wiki, &id, armored.as_bytes()).unwrap();
                }
            }
            id
        }

        fn sign(&self, key: &Path, namespace: &str, data: &[u8]) -> String {
            let bin = self.bin.as_ref().unwrap();
            if namespace == sshsig::NAMESPACE {
                return sshsig::sign(bin, key, data).unwrap();
            }
            let mut child = Command::new(bin)
                .args(["-Y", "sign", "-n", namespace, "-f"])
                .arg(key)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            std::io::Write::write_all(&mut child.stdin.take().unwrap(), data).unwrap();
            String::from_utf8(child.wait_with_output().unwrap().stdout).unwrap()
        }
    }

    impl Drop for World {
        fn drop(&mut self) {
            for _ in 0..10 {
                if fs::remove_dir_all(&self.root).is_ok() {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    }

    struct Spec {
        page: &'static str,
        anchor: &'static str,
        body: &'static str,
        requested: &'static str,
        principal: String,
        time_verified: &'static str,
        review_by: Option<&'static str>,
        redacted: Vec<String>,
    }

    impl Spec {
        fn new(principal: &str) -> Spec {
            Spec {
                page: "alpha",
                anchor: "s-trust-order",
                body: "TLS tries the bundled rung first.\n\nThen the system store.",
                requested: "evidence",
                principal: principal.to_owned(),
                time_verified: STAMP,
                review_by: None,
                redacted: Vec::new(),
            }
        }
    }

    enum Sig<'a> {
        None,
        Raw(&'a str),
        Key { key: &'a Path, namespace: &'a str },
    }

    const NOW: &str = "2026-11-21 10:00 +02:00";
    const PRINCIPAL: &str = "agent.one@local";

    fn allowed_line(principal: &str, public: &str, window: &str) -> String {
        format!("{principal} namespaces=\"loam-seal\",valid-after=\"20200101\"{window} {public}")
    }

    /// Stat snapshot of every file under `dirs`: path, length and mtime.
    fn snapshot(dirs: &[&Path]) -> Vec<(PathBuf, u64, Option<std::time::SystemTime>)> {
        fn walk(dir: &Path, out: &mut Vec<(PathBuf, u64, Option<std::time::SystemTime>)>) {
            let Ok(entries) = fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let meta = entry.metadata().unwrap();
                out.push((entry.path(), meta.len(), meta.modified().ok()));
                if meta.is_dir() {
                    walk(&entry.path(), out);
                }
            }
        }
        let mut out = Vec::new();
        for dir in dirs {
            walk(dir, &mut out);
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    fn failures(report: &verify::Report) -> Vec<&'static str> {
        report.outcome.codes.failure.clone()
    }

    /// Without a capable ssh-keygen a signed seal cannot be checked: it
    /// must degrade to the evidence level with the informational code.
    fn assert_degraded(world: &World) {
        world.allow(&[]);
        world.policy(&[PRINCIPAL]);
        let mut spec = Spec::new(PRINCIPAL);
        spec.requested = "audited";
        let id = world.seal(
            &spec,
            Sig::Raw("-----BEGIN SSH SIGNATURE-----\nAAAA\n-----END SSH SIGNATURE-----\n"),
        );
        let report = world.only(NOW);
        assert_eq!(report.id, id);
        assert_eq!(report.outcome.verdict, Verdict::Citable);
        assert!(report.outcome.effective_level <= Some(Level::Evidence));
        assert!(report
            .outcome
            .codes
            .informational
            .contains(&code::VERIFIER_UNAVAILABLE));
    }

    /// S2: an unchanged, signed seal is citable, cites as `seal:<id>`, and
    /// verification touches nothing under `wiki/` or the trust directory.
    #[test]
    fn verify_citable() {
        let world = World::new();
        if world.bin.is_none() {
            return assert_degraded(&world);
        }
        let (key, public) = world.make_key("one");
        world.allow(&[allowed_line(PRINCIPAL, &public, "")]);
        world.policy(&[PRINCIPAL]);
        let mut spec = Spec::new(PRINCIPAL);
        spec.requested = "audited";
        let id = world.seal(
            &spec,
            Sig::Key {
                key: &key,
                namespace: sshsig::NAMESPACE,
            },
        );

        let before = snapshot(&[&world.wiki, &world.trust_dir]);
        let report = world.only(NOW);
        assert_eq!(snapshot(&[&world.wiki, &world.trust_dir]), before);

        assert_eq!(report.id, id);
        assert_eq!(report.outcome.verdict, Verdict::Citable, "{report:?}");
        assert_eq!(report.outcome.effective_level, Some(Level::Audited));
        assert_eq!(report.cite_as(), Some(format!("seal:{id}")));
        assert_eq!(report.signer.as_deref(), Some(PRINCIPAL));
        assert!(report
            .outcome
            .codes
            .success
            .contains(&code::SIGNATURE_VALID));
        assert!(report
            .outcome
            .codes
            .success
            .contains(&code::BINDING_MATCHES));

        let value = report.to_value();
        assert_eq!(value.get("seal").and_then(Value::as_str), Some(id.as_str()));
        assert_eq!(
            value.get("verdict").and_then(Value::as_str),
            Some("citable")
        );
        assert_eq!(
            value.get("cite_as").and_then(Value::as_str),
            Some(format!("seal:{id}").as_str())
        );
        assert_eq!(
            value.get("effective_level").and_then(Value::as_str),
            Some("audited")
        );
        assert_eq!(
            value.get("requested_level").and_then(Value::as_str),
            Some("audited")
        );
        assert_eq!(value.get("signer").and_then(Value::as_str), Some(PRINCIPAL));
        assert_eq!(
            value.get("time_verified").and_then(Value::as_str),
            Some(STAMP)
        );
        for key in ["success", "informational", "failure"] {
            assert!(
                value.get("codes").and_then(|c| c.get(key)).is_some(),
                "{key}"
            );
        }
    }

    /// An unsigned evidence-level seal is citable; it needs no key.
    #[test]
    fn verify_unsigned_evidence_seal_is_citable() {
        let world = World::new();
        world.seal(&Spec::new(PRINCIPAL), Sig::None);
        let report = world.only(NOW);
        assert_eq!(report.outcome.verdict, Verdict::Citable);
        assert_eq!(report.outcome.effective_level, Some(Level::Evidence));
        assert_eq!(report.signer, None);
    }

    /// S3: editing a span line drifts the seal, and the record still holds
    /// the as-sealed bytes.
    #[test]
    fn drift() {
        let world = World::new();
        let id = world.seal(&Spec::new(PRINCIPAL), Sig::None);
        let record_before = fs::read(record::record_path(&world.wiki, &id)).unwrap();

        let page = world.page_path("alpha");
        let edited = fs::read_to_string(&page)
            .unwrap()
            .replace("bundled rung first", "system store first");
        fs::write(&page, edited).unwrap();

        let report = world.only(NOW);
        assert_eq!(report.outcome.verdict, Verdict::Drifted);
        assert!(failures(&report).contains(&code::BINDING_MISMATCH));
        assert_eq!(report.cite_as(), None);
        assert_eq!(
            fs::read(record::record_path(&world.wiki, &id)).unwrap(),
            record_before
        );
        let view = record::well_formed(&id, &record_before).unwrap();
        assert!(view.content.unwrap().contains("bundled rung first"));
    }

    /// A trailing-space or CRLF change does not drift (canon), a reflow does.
    #[test]
    fn drift_ignores_canonical_noise() {
        let world = World::new();
        world.seal(&Spec::new(PRINCIPAL), Sig::None);
        let page = world.page_path("alpha");
        let text = fs::read_to_string(&page).unwrap();
        fs::write(&page, text.replace('\n', "  \r\n")).unwrap();
        assert_eq!(world.only(NOW).outcome.verdict, Verdict::Citable);
    }

    /// S5: hand-made and mismatched signatures are forged or untrusted.
    #[test]
    fn forgery() {
        let world = World::new();
        if world.bin.is_none() {
            return assert_degraded(&world);
        }
        let (key_a, public_a) = world.make_key("a");
        let (_key_b, public_b) = world.make_key("b");
        let (key_c, _public_c) = world.make_key("c");
        world.allow(&[
            allowed_line("agent.a@local", &public_a, ""),
            allowed_line("agent.b@local", &public_b, ""),
        ]);
        world.policy(&["agent.a@local", "agent.b@local"]);
        let ns = sshsig::NAMESPACE;

        let verdict_of = |spec: &Spec, sig: Sig<'_>| -> verify::Report {
            let world_dir = &world;
            for entry in fs::read_dir(record::records_dir(&world_dir.wiki))
                .into_iter()
                .flatten()
            {
                fs::remove_file(entry.unwrap().path()).unwrap();
            }
            world_dir.seal(spec, sig);
            world_dir.only(NOW)
        };

        // Hand-written signature bytes.
        let report = verdict_of(
            &Spec::new("agent.a@local"),
            Sig::Raw("-----BEGIN SSH SIGNATURE-----\nAAAA\n-----END SSH SIGNATURE-----\n"),
        );
        assert_eq!(report.outcome.verdict, Verdict::Forged);
        assert_eq!(report.cite_as(), None);

        // Key A signing a record that names principal B.
        let report = verdict_of(
            &Spec::new("agent.b@local"),
            Sig::Key {
                key: &key_a,
                namespace: ns,
            },
        );
        assert_eq!(report.outcome.verdict, Verdict::Forged);
        assert!(report
            .outcome
            .codes
            .failure
            .contains(&code::PRINCIPAL_MISMATCH));

        // The right key in the wrong namespace.
        let report = verdict_of(
            &Spec::new("agent.a@local"),
            Sig::Key {
                key: &key_a,
                namespace: "git",
            },
        );
        assert_eq!(report.outcome.verdict, Verdict::Forged);
        assert!(report
            .outcome
            .codes
            .failure
            .contains(&code::NAMESPACE_WRONG));

        // A genuine signature by a key this reader does not trust.
        let report = verdict_of(
            &Spec::new("agent.c@local"),
            Sig::Key {
                key: &key_c,
                namespace: ns,
            },
        );
        assert_eq!(report.outcome.verdict, Verdict::Untrusted);
        assert!(report
            .outcome
            .codes
            .failure
            .contains(&code::UNTRUSTED_SIGNER));

        // A signature that does not cover these record bytes.
        let report = verdict_of(
            &Spec::new("agent.a@local"),
            Sig::Raw(&world.sign(&key_a, ns, b"some other payload")),
        );
        assert_eq!(report.outcome.verdict, Verdict::Forged);
        assert!(report
            .outcome
            .codes
            .failure
            .contains(&code::SIGNATURE_INVALID));

        // A record filed under a name that is not the hash of its bytes.
        for entry in fs::read_dir(record::records_dir(&world.wiki)).unwrap() {
            fs::remove_file(entry.unwrap().path()).unwrap();
        }
        let id = world.seal(&Spec::new("agent.a@local"), Sig::None);
        let from = record::record_path(&world.wiki, &id);
        let renamed = record::records_dir(&world.wiki).join("0123456789abcdef0123.json");
        fs::rename(&from, &renamed).unwrap();
        let report = world.only(NOW);
        assert_eq!(report.id, "0123456789abcdef0123");
        assert_eq!(report.outcome.verdict, Verdict::Untrusted);
        assert!(report.outcome.codes.failure.contains(&code::MALFORMED));
        assert_eq!(report.cite_as(), None);
    }

    /// S10: a closed `valid-before` keeps old seals valid at `time_verified`;
    /// a window that closed earlier, or a revoked key, is untrusted.
    #[test]
    fn rotation() {
        let world = World::new();
        if world.bin.is_none() {
            return assert_degraded(&world);
        }
        let (key, public) = world.make_key("one");
        world.policy(&[PRINCIPAL]);
        let spec = Spec::new(PRINCIPAL);
        let sealed = |world: &World| {
            for entry in fs::read_dir(record::records_dir(&world.wiki))
                .into_iter()
                .flatten()
            {
                fs::remove_file(entry.unwrap().path()).unwrap();
            }
            world.seal(
                &spec,
                Sig::Key {
                    key: &key,
                    namespace: sshsig::NAMESPACE,
                },
            )
        };

        // Window closed after the seal was made: still valid.
        world.allow(&[allowed_line(
            PRINCIPAL,
            &public,
            ",valid-before=\"20261201Z\"",
        )]);
        sealed(&world);
        let report = world.only("2027-01-10 10:00 +02:00");
        assert_eq!(report.outcome.verdict, Verdict::Citable, "{report:?}");
        assert_eq!(report.signer.as_deref(), Some(PRINCIPAL));

        // Window closed before the seal claims to have been made.
        world.allow(&[allowed_line(
            PRINCIPAL,
            &public,
            ",valid-before=\"20260101Z\"",
        )]);
        let report = world.only(NOW);
        assert_eq!(report.outcome.verdict, Verdict::Untrusted);
        assert_eq!(report.cite_as(), None);

        // Revoked key.
        world.allow(&[allowed_line(PRINCIPAL, &public, "")]);
        assert_eq!(world.only(NOW).outcome.verdict, Verdict::Citable);
        fs::write(
            world.trust_dir.join(trust::REVOKED_FILE),
            format!("{public}\n"),
        )
        .unwrap();
        let report = world.only(NOW);
        assert_eq!(report.outcome.verdict, Verdict::Untrusted);
        assert!(report.outcome.codes.failure.contains(&code::SIGNER_REVOKED));
    }

    /// Moving a page keeps the verdict and adds `seal.subject.moved`; a span
    /// whose markers were stripped is drifted with a likely location.
    #[test]
    fn moved() {
        let world = World::new();
        world.seal(&Spec::new(PRINCIPAL), Sig::None);
        let report = world.only(NOW);
        assert!(!report
            .outcome
            .codes
            .informational
            .contains(&verify::SUBJECT_MOVED));
        assert_eq!(report.current_page.as_deref(), Some("alpha"));

        let from = world.page_path("alpha");
        let to = world.page_path("archive/beta");
        fs::create_dir_all(to.parent().unwrap()).unwrap();
        fs::rename(&from, &to).unwrap();

        let report = world.only(NOW);
        assert_eq!(report.outcome.verdict, Verdict::Citable);
        assert!(report
            .outcome
            .codes
            .informational
            .contains(&verify::SUBJECT_MOVED));
        assert_eq!(report.current_page.as_deref(), Some("archive/beta"));
        assert!(report.cite_as().is_some());

        // Markers stripped: never validated, only located.
        let stripped = fs::read_to_string(&to)
            .unwrap()
            .replace("<!-- loam:seal s-trust-order -->\n", "")
            .replace("<!-- /loam:seal -->\n", "");
        fs::write(&to, stripped).unwrap();
        let report = world.only(NOW);
        assert_eq!(report.outcome.verdict, Verdict::Drifted);
        assert!(failures(&report).contains(&code::SPAN_MISSING));
        assert_eq!(report.likely_location.as_deref(), Some("archive/beta"));
        assert_eq!(report.cite_as(), None);
    }

    /// S20: an audited signer whose seal declares `redacted:["token"]`
    /// verifies at effective `evidence`.
    #[test]
    fn captures_redacted() {
        let world = World::new();
        if world.bin.is_none() {
            return assert_degraded(&world);
        }
        let (key, public) = world.make_key("one");
        world.allow(&[allowed_line(PRINCIPAL, &public, "")]);
        world.policy(&[PRINCIPAL]);
        let mut spec = Spec::new(PRINCIPAL);
        spec.requested = "audited";
        spec.redacted = vec!["token".to_owned()];
        world.seal(
            &spec,
            Sig::Key {
                key: &key,
                namespace: sshsig::NAMESPACE,
            },
        );
        let report = world.only(NOW);
        assert_eq!(report.outcome.verdict, Verdict::Citable, "{report:?}");
        assert_eq!(report.outcome.effective_level, Some(Level::Evidence));
        assert_eq!(report.outcome.requested_level, Level::Audited);
        assert!(report
            .outcome
            .codes
            .informational
            .contains(&code::CAPPED_REDACTION));
    }

    /// A stub `LOAM_SSH_KEYGEN` that cannot verify caps the seal at evidence
    /// without refusing it.
    #[test]
    fn verifier_unavailable() {
        let _guard = crate::env_lock();
        let world = World::new();
        let stub = sshsig::test_support::stub_without_y(&world.root);
        world.allow(&[]);
        world.policy(&[PRINCIPAL]);
        let mut spec = Spec::new(PRINCIPAL);
        spec.requested = "audited";
        world.seal(
            &spec,
            Sig::Raw("-----BEGIN SSH SIGNATURE-----\nAAAA\n-----END SSH SIGNATURE-----\n"),
        );

        let previous = std::env::var_os("LOAM_SSH_KEYGEN");
        std::env::set_var("LOAM_SSH_KEYGEN", &stub);
        let env = Env::load(
            &world.wiki,
            Some(world.trust_dir.clone()),
            super::policy::parse_stamp(NOW).unwrap(),
        );
        match previous {
            Some(value) => std::env::set_var("LOAM_SSH_KEYGEN", value),
            None => std::env::remove_var("LOAM_SSH_KEYGEN"),
        }
        let mut env = env.unwrap();
        env.repo_root = world.root.clone();
        assert_eq!(env.keygen, stub, "LOAM_SSH_KEYGEN selects the verifier");

        let reports = verify::verify_all(&env).unwrap();
        let report = &reports[0];
        assert_eq!(report.outcome.verdict, Verdict::Citable);
        assert_eq!(report.outcome.effective_level, Some(Level::Evidence));
        assert!(report
            .outcome
            .codes
            .informational
            .contains(&code::VERIFIER_UNAVAILABLE));
        assert!(report.outcome.codes.failure.is_empty());
    }

    /// A cited seal that is not citable makes the citing seal stale; a
    /// citable one caps the citing seal at its level.
    #[test]
    fn verify_follows_cited_seals() {
        let world = World::new();
        let cited = world.seal(&Spec::new(PRINCIPAL), Sig::None);

        let ctx = Ctx {
            repo_root: world.root.clone(),
            wiki: world.wiki.clone(),
            retrieved_at: STAMP.to_owned(),
            capture_max_bytes: 262_144,
        };
        let item = evidence::parse(&format!("seal:{cited}")).unwrap();
        let resolved = evidence::resolve_all(&[item], &ctx, &AttestOptions::default()).unwrap();
        fs::write(
            world.page_path("beta"),
            "# Beta\n\n<!-- loam:seal s-beta-claim -->\nBeta rests on alpha.\n<!-- /loam:seal -->\n",
        )
        .unwrap();
        let text = fs::read_to_string(world.page_path("beta")).unwrap();
        let scan = crate::markers::scan(&text);
        let content = &text.as_bytes()[scan.markers[0].content.clone()];
        let (id, bytes) = record::build(&record::NewRecord {
            kind: record::RecordKind::Seal,
            page: "beta".to_owned(),
            anchor: "s-beta-claim".to_owned(),
            subject_sha256: super::canon::digest(content).unwrap(),
            locator: record::soft_locator(&[], "Beta rests on alpha."),
            content: "Beta rests on alpha.\n".to_owned(),
            requested_level: Some("evidence".to_owned()),
            claim: Some("Beta claim".to_owned()),
            procedure_uri: "loam://seal/procedure/v1".to_owned(),
            principal: PRINCIPAL.to_owned(),
            acted_on_behalf_of: None,
            generator: record::Generator {
                name: "loam".to_owned(),
                version: "1.0.0".to_owned(),
                harness: "test".to_owned(),
            },
            time_verified: STAMP.to_owned(),
            review_by: None,
            evidence: resolved.iter().map(|r| r.item.to_value()).collect(),
            redacted: Vec::new(),
            parent: None,
            status: None,
        })
        .unwrap();
        record::write_record(&world.wiki, &bytes).unwrap();

        let by_id = |reports: &[verify::Report], wanted: &str| {
            reports
                .iter()
                .find(|r| r.id == wanted)
                .unwrap()
                .outcome
                .verdict
        };
        let reports = world.reports(NOW);
        assert_eq!(by_id(&reports, &cited), Verdict::Citable);
        assert_eq!(by_id(&reports, &id), Verdict::Citable);

        // Drift the cited seal: the citing seal goes stale.
        let page = world.page_path("alpha");
        let edited = fs::read_to_string(&page)
            .unwrap()
            .replace("bundled", "other");
        fs::write(&page, edited).unwrap();
        let reports = world.reports(NOW);
        assert_eq!(by_id(&reports, &cited), Verdict::Drifted);
        assert_eq!(by_id(&reports, &id), Verdict::Stale);
    }

    /// The selector narrows by page, stem and anchor.
    #[test]
    fn selector_matching() {
        let world = World::new();
        world.seal(&Spec::new(PRINCIPAL), Sig::None);
        let report = world.only(NOW);
        for arg in [
            "alpha",
            "alpha.md",
            "alpha#^s-trust-order",
            "#^s-trust-order",
        ] {
            assert!(
                report.matches(&verify::Selector::parse(arg).unwrap()),
                "{arg}"
            );
        }
        for arg in ["beta", "alpha#^s-other"] {
            assert!(
                !report.matches(&verify::Selector::parse(arg).unwrap()),
                "{arg}"
            );
        }
        assert!(verify::Selector::parse("alpha#^").is_err());
        assert!(verify::Selector::parse("").is_err());
    }
}
