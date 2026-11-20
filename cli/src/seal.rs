//! Seals: content-bound, signed claims over marker spans.
//!
//! This module is public so items consumed only by tests until the CLI verbs
//! land do not trip `dead_code`.

pub mod attest;
pub mod canon;
pub mod carry;
pub mod evidence;
pub mod policy;
pub mod record;
pub mod sshsig;
pub mod status;
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
        Some("attest") => attest::run(args),
        Some("mark") => status::run(args),
        Some("carry") => carry::run(args),
        Some("verify") => verify_command(args),
        Some("status") => status_command(args),
        _ => usage(),
    }
}

fn usage() -> i32 {
    eprintln!(
        "Usage:\n  loam seal hash <file> <anchor>\n  loam seal attest <page>#^<anchor> --level <l> --claim <text> --evidence <kind:ref>... (see `loam seal attest` for the optional flags)\n  loam seal mark <id> dispute|demote|supersede|retract --reason <text> [--to <level>]\n  loam seal carry <id>\n  loam seal verify [<page>[#^anchor]] [--json] [--now <stamp>]\n  loam seal status [--json] [--now <stamp>]\n  loam seal trust init|add <principal> <pubkey>|show"
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
pub(crate) fn find_wiki(start: &Path) -> Option<PathBuf> {
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
fn verify_wiki(
    verb: &str,
    options: &ReadOptions,
) -> Result<(Vec<verify::Report>, verify::Selector), i32> {
    let fail = |message: String| {
        eprintln!("loam seal {verb}: {message}");
        EXIT_USAGE
    };
    let cwd = std::env::current_dir()
        .map_err(|e| fail(format!("cannot read the current directory: {e}")))?;
    let wiki = find_wiki(&cwd)
        .ok_or_else(|| fail(format!("no wiki/ directory at or above {}", cwd.display())))?;
    let env = verify::Env::load(&wiki, trust::seal_dir().ok(), options.now).map_err(&fail)?;
    let selector = options.selector.clone().resolve(&wiki, &cwd);
    let reports = verify::verify_all(&env).map_err(fail)?;
    Ok((reports, selector))
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
    let (reports, selector) = match verify_wiki("verify", &options) {
        Ok(found) => found,
        Err(code) => return code,
    };
    let selected: Vec<&verify::Report> = reports
        .iter()
        .filter(|report| report.matches(&selector))
        .collect();
    let targeted = selector != verify::Selector::default();
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
        Ok((reports, _)) => reports,
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
        // Fixed dates only: the window is closed on the real clock but was
        // open at the seal's `time_verified`.
        let sealed = |world: &World, time_verified: &'static str| {
            for entry in fs::read_dir(record::records_dir(&world.wiki))
                .into_iter()
                .flatten()
            {
                fs::remove_file(entry.unwrap().path()).unwrap();
            }
            let mut spec = Spec::new(PRINCIPAL);
            spec.time_verified = time_verified;
            world.seal(
                &spec,
                Sig::Key {
                    key: &key,
                    namespace: sshsig::NAMESPACE,
                },
            )
        };
        let window = ",valid-before=\"20260601Z\"";
        world.allow(&[allowed_line(PRINCIPAL, &public, window)]);

        // Window closed after the seal was made: still valid.
        sealed(&world, "2026-01-10 10:00 +00:00");
        let report = world.only("2026-01-11 10:00 +00:00");
        assert_eq!(report.outcome.verdict, Verdict::Citable, "{report:?}");
        assert_eq!(report.signer.as_deref(), Some(PRINCIPAL));

        // Seal claims a time after the window closed.
        sealed(&world, "2026-07-10 10:00 +00:00");
        let report = world.only("2026-07-11 10:00 +00:00");
        assert_eq!(report.outcome.verdict, Verdict::Untrusted);
        assert_eq!(report.cite_as(), None);

        // Revoked key.
        sealed(&world, STAMP);
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

        let env = Env::load(
            &world.wiki,
            Some(world.trust_dir.clone()),
            super::policy::parse_stamp(NOW).unwrap(),
        );
        let mut env = env.unwrap();
        env.repo_root = world.root.clone();
        env.keygen = stub;

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

    // ----------------------------------------------------------- attest

    use super::attest::{self, AttestError, Request};

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env(
                "GIT_CONFIG_GLOBAL",
                if cfg!(windows) { "NUL" } else { "/dev/null" },
            )
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
            .output()
            .expect("git must be installed to run these tests");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    impl World {
        /// A signing identity under the trust directory, trusted by an
        /// `allowed_signers` window that opens long before `NOW`. `None`
        /// when no capable ssh-keygen exists here.
        fn attest_context(&self) -> Option<attest::Context> {
            let bin = self.bin.clone()?;
            let report = trust::init(&self.trust_dir, &trust::Enrolled::default(), &bin).unwrap();
            let public = fs::read_to_string(self.trust_dir.join(trust::PUBLIC_FILE)).unwrap();
            let mut parts = public.split_whitespace();
            let line = format!("{} {}", parts.next().unwrap(), parts.next().unwrap());
            self.allow(&[allowed_line(&report.principal, &line, "")]);
            Some(attest::Context {
                wiki: self.wiki.clone(),
                repo_root: self.root.clone(),
                trust_dir: self.trust_dir.clone(),
                keygen: bin,
                now: super::policy::parse_stamp(NOW).unwrap(),
                org: None,
                harness: Some("test".to_owned()),
            })
        }

        fn page_with(&self, name: &str, text: &str) -> PathBuf {
            let path = self.page_path(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, text).unwrap();
            path
        }

        fn init_repo(&self) {
            git(&self.root, &["init", "-q"]);
            git(&self.root, &["symbolic-ref", "HEAD", "refs/heads/main"]);
            fs::write(self.root.join("a.txt"), "alpha\n").unwrap();
            git(&self.root, &["add", "a.txt", "run.log"]);
            git(&self.root, &["commit", "-q", "-m", "first"]);
        }
    }

    fn request(evidence: &[&str]) -> Request {
        Request {
            page: "alpha".to_owned(),
            anchor: "s-trust-order".to_owned(),
            level: "evidence".to_owned(),
            claim: "TLS order".to_owned(),
            evidence: evidence.iter().map(|e| (*e).to_owned()).collect(),
            ..Request::default()
        }
    }

    const UNMARKED: &str =
        "# Alpha\n\nTLS tries the bundled rung first.\nThen the system store. ^s-trust-order\n\nTail.\n";

    fn has_no_seal_store(world: &World) -> bool {
        !record::seals_dir(&world.wiki).exists()
    }

    /// AC-S1: an un-marked `^s-` block gains markers, a record named by the
    /// sha256 prefix and its signature, and verifies citable.
    #[test]
    fn attest_roundtrip() {
        let world = World::new();
        let Some(ctx) = world.attest_context() else {
            // No capable ssh-keygen: attest must refuse with the setup hint.
            let ctx = attest::Context {
                wiki: world.wiki.clone(),
                repo_root: world.root.clone(),
                trust_dir: world.trust_dir.clone(),
                keygen: world.root.join("no-such-keygen"),
                now: super::policy::parse_stamp(NOW).unwrap(),
                org: None,
                harness: None,
            };
            world.page_with("alpha", UNMARKED);
            let AttestError::Refused(lines) =
                attest::attest(&ctx, &request(&["capture:run.log"])).unwrap_err()
            else {
                panic!("expected a refusal");
            };
            assert!(lines.join("\n").contains("npx @scchearn/loam setup"));
            return;
        };
        let page = world.page_with("alpha", UNMARKED);
        let done = attest::attest(&ctx, &request(&["capture:run.log"])).unwrap();

        let record_bytes = fs::read(record::record_path(&world.wiki, &done.id)).unwrap();
        assert_eq!(done.id, crate::sha256::hex(&record_bytes)[..20]);
        assert_eq!(done.id, record::id_of(&record_bytes));
        assert!(record::sig_path(&world.wiki, &done.id).is_file());
        assert!(done.inserted_markers);
        assert_eq!(done.report.outcome.verdict, Verdict::Citable);
        assert_eq!(done.report.outcome.effective_level, Some(Level::Evidence));
        assert_eq!(done.report.cite_as(), Some(format!("seal:{}", done.id)));
        assert_eq!(
            fs::read_to_string(&page).unwrap(),
            "# Alpha\n\n<!-- loam:seal s-trust-order -->\nTLS tries the bundled rung first.\nThen the system store.\n<!-- /loam:seal -->\n\nTail.\n"
        );
        let view = record::well_formed(&done.id, &record_bytes).unwrap();
        assert_eq!(view.claim.as_deref(), Some("TLS order"));
        assert_eq!(view.requested_level.as_deref(), Some("evidence"));
        assert!(view.principal.starts_with("agent."));
        let generator = view
            .document
            .get("predicate")
            .unwrap()
            .get("generator")
            .unwrap();
        assert_eq!(
            generator.get("harness").and_then(Value::as_str),
            Some("test")
        );
        assert_eq!(
            generator.get("version").and_then(Value::as_str),
            Some(env!("CARGO_PKG_VERSION"))
        );

        // A fresh verifier agrees, and the captured evidence is stored.
        let again = world.only(NOW);
        assert_eq!(again.id, done.id);
        assert_eq!(again.outcome.verdict, Verdict::Citable);
        assert_eq!(again.signer, Some(view.principal.clone()));
        assert!(
            record::evidence_path(&world.wiki, &crate::sha256::hex(b"all tests pass")).is_file()
        );

        // The same claim in the same minute is refused, not overwritten.
        let AttestError::Refused(lines) =
            attest::attest(&ctx, &request(&["capture:run.log"])).unwrap_err()
        else {
            panic!("expected a refusal");
        };
        assert!(lines.join("\n").contains("already exists"), "{lines:?}");
        assert_eq!(
            fs::read(record::record_path(&world.wiki, &done.id)).unwrap(),
            record_bytes
        );
    }

    /// Existing markers are used as they are: the page is not touched, and a
    /// CRLF page keeps its line endings.
    #[test]
    fn attest_uses_existing_markers_and_preserves_crlf() {
        let world = World::new();
        let Some(ctx) = world.attest_context() else {
            return;
        };
        let marked =
            "# Alpha\r\n\r\n<!-- loam:seal s-trust-order -->\r\nBundled first.\r\n<!-- /loam:seal -->\r\n";
        let page = world.page_with("alpha", marked);
        let done = attest::attest(&ctx, &request(&["capture:run.log"])).unwrap();
        assert!(!done.inserted_markers);
        assert_eq!(fs::read(&page).unwrap(), marked.as_bytes());

        let world = World::new();
        let ctx = world.attest_context().unwrap();
        let crlf = UNMARKED.replace('\n', "\r\n");
        let page = world.page_with("alpha", &crlf);
        attest::attest(&ctx, &request(&["capture:run.log"])).unwrap();
        let after = fs::read_to_string(&page).unwrap();
        assert_eq!(
            after,
            "# Alpha\r\n\r\n<!-- loam:seal s-trust-order -->\r\nTLS tries the bundled rung first.\r\nThen the system store.\r\n<!-- /loam:seal -->\r\n\r\nTail.\r\n"
        );
        assert!(!after.replace("\r\n", "").contains('\n'));
    }

    /// AC-S4 (runtime half), AC-S19: every unresolvable item is refused, all
    /// of them are named, and the page and the seal store are untouched.
    #[test]
    fn attest_refuses() {
        let world = World::new();
        let Some(ctx) = world.attest_context() else {
            return;
        };
        world.init_repo();
        git(&world.root, &["checkout", "-q", "-b", "feature"]);
        fs::write(world.root.join("b.txt"), "b").unwrap();
        git(&world.root, &["add", "b.txt"]);
        git(&world.root, &["commit", "-q", "-m", "branch only"]);
        let branch_only = git(&world.root, &["rev-parse", "HEAD"]);
        git(&world.root, &["checkout", "-q", "main"]);
        fs::write(world.root.join("a.txt"), "edited after commit\n").unwrap();
        fs::write(world.root.join("big.bin"), vec![b'x'; 300_000]).unwrap();

        let page = world.page_with("alpha", UNMARKED);
        let before = fs::read(&page).unwrap();
        let commit = format!("commit:{branch_only}");
        let cases: [(&str, Vec<&str>, usize); 5] = [
            ("missing blob", vec!["blob:nope.txt"], 1),
            ("unreachable commit", vec![commit.as_str()], 1),
            ("over-cap capture", vec!["capture:big.bin"], 1),
            ("sha mismatch", vec!["blob:a.txt"], 1),
            (
                "several at once",
                vec!["blob:nope.txt", "capture:big.bin", "capture:run.log"],
                2,
            ),
        ];
        for (name, evidence, failing) in cases {
            let AttestError::Refused(lines) =
                attest::attest(&ctx, &request(&evidence)).unwrap_err()
            else {
                panic!("{name}: expected a refusal");
            };
            assert_eq!(lines.len(), failing, "{name}: {lines:?}");
            assert_eq!(fs::read(&page).unwrap(), before, "{name}: page changed");
            assert!(has_no_seal_store(&world), "{name}: wrote under .seals");
        }

        // Usage-class problems also write nothing.
        let mut bad = request(&["capture:run.log"]);
        bad.anchor = "s-no-such-anchor".to_owned();
        assert!(matches!(
            attest::attest(&ctx, &bad).unwrap_err(),
            AttestError::Usage(_)
        ));
        let mut bad = request(&["capture:run.log"]);
        bad.page = "missing".to_owned();
        assert!(matches!(
            attest::attest(&ctx, &bad).unwrap_err(),
            AttestError::Usage(_)
        ));
        let mut bad = request(&["capture:run.log"]);
        bad.level = "gold".to_owned();
        assert!(matches!(
            attest::attest(&ctx, &bad).unwrap_err(),
            AttestError::Usage(_)
        ));
        assert!(matches!(
            attest::attest(&ctx, &request(&[])).unwrap_err(),
            AttestError::Usage(_)
        ));
        let mut bad = request(&["capture:run.log"]);
        bad.review_by = Some("2026-11-20 14:30 +02:00".to_owned());
        assert!(matches!(
            attest::attest(&ctx, &bad).unwrap_err(),
            AttestError::Usage(_)
        ));
        assert_eq!(fs::read(&page).unwrap(), before);
        assert!(has_no_seal_store(&world));

        // A note-only seal would not be citable: refused before any write.
        let AttestError::Refused(lines) =
            attest::attest(&ctx, &request(&["note:because I said so"])).unwrap_err()
        else {
            panic!("expected a refusal");
        };
        assert!(
            lines.join("\n").contains("would not be citable"),
            "{lines:?}"
        );
        assert_eq!(fs::read(&page).unwrap(), before);
        assert!(has_no_seal_store(&world));

        // No signing identity: refused with the setup hint.
        let mut no_identity = ctx.clone();
        no_identity.trust_dir = world.root.join("nowhere");
        let AttestError::Refused(lines) =
            attest::attest(&no_identity, &request(&["capture:run.log"])).unwrap_err()
        else {
            panic!("expected a refusal");
        };
        assert!(lines.join("\n").contains("npx @scchearn/loam setup"));
        assert_eq!(fs::read(&page).unwrap(), before);
        assert!(has_no_seal_store(&world));
    }

    /// `--parent` records the parent's id and verdict at attest time and
    /// keeps the provenance of an unchanged capture.
    #[test]
    fn parent_chain() {
        let world = World::new();
        let Some(mut ctx) = world.attest_context() else {
            return;
        };
        world.page_with("alpha", UNMARKED);
        let first = attest::attest(&ctx, &request(&["capture:run.log"])).unwrap();

        ctx.now = super::policy::parse_stamp("2026-11-21 11:00 +02:00").unwrap();
        let mut again = request(&["capture:run.log"]);
        again.parent = Some(first.id.clone());
        let second = attest::attest(&ctx, &again).unwrap();
        assert_ne!(second.id, first.id);

        let bytes = fs::read(record::record_path(&world.wiki, &second.id)).unwrap();
        let view = record::well_formed(&second.id, &bytes).unwrap();
        let parent = view.parent.expect("parent recorded");
        assert_eq!(parent.id, first.id);
        assert_eq!(parent.verdict, "citable");
        let capture = &view.evidence[0];
        assert_eq!(
            capture.get("retrieved_at").and_then(Value::as_str),
            Some("2026-11-21 10:00 +02:00"),
            "an unchanged capture keeps its first retrieval time"
        );

        // The first record is untouched and both stay citable.
        assert!(record::record_path(&world.wiki, &first.id).is_file());
        let reports = world.reports("2026-11-21 12:00 +02:00");
        assert_eq!(reports.len(), 2);
        assert!(reports.iter().all(verify::Report::is_citable));

        // An unknown or malformed parent is a usage error that writes nothing.
        for parent in ["0123456789abcdef0123", "not-an-id"] {
            let mut bad = request(&["capture:run.log"]);
            bad.parent = Some(parent.to_owned());
            ctx.now = super::policy::parse_stamp("2026-11-21 12:30 +02:00").unwrap();
            assert!(matches!(
                attest::attest(&ctx, &bad).unwrap_err(),
                AttestError::Usage(_)
            ));
        }
        assert_eq!(world.reports(NOW).len(), 2);

        // A parent that seals a different anchor is a usage error that
        // writes nothing.
        let page = world.page_path("alpha");
        let other_text = format!(
            "{}\nOther claim. ^s-other-anchor\n",
            fs::read_to_string(&page).unwrap()
        );
        fs::write(&page, other_text).unwrap();
        let mut other = request(&["capture:run.log"]);
        other.anchor = "s-other-anchor".to_owned();
        ctx.now = super::policy::parse_stamp("2026-11-21 13:00 +02:00").unwrap();
        let other_id = attest::attest(&ctx, &other).unwrap().id;
        let page_before = fs::read(&page).unwrap();
        let count = world.reports(NOW).len();
        let mut crossed = request(&["capture:run.log"]);
        crossed.parent = Some(other_id);
        ctx.now = super::policy::parse_stamp("2026-11-21 13:30 +02:00").unwrap();
        assert!(matches!(
            attest::attest(&ctx, &crossed).unwrap_err(),
            AttestError::Usage(_)
        ));
        assert_eq!(fs::read(&page).unwrap(), page_before);
        assert_eq!(world.reports(NOW).len(), count);

        // The same anchor on another page is a different span: refused too.
        let beta = world.page_with("beta", UNMARKED);
        let mut on_beta = request(&["capture:run.log"]);
        on_beta.page = "beta".to_owned();
        ctx.now = super::policy::parse_stamp("2026-11-21 14:00 +02:00").unwrap();
        let beta_id = attest::attest(&ctx, &on_beta).unwrap().id;
        let alpha_before = fs::read(&page).unwrap();
        let beta_before = fs::read(&beta).unwrap();
        let count = world.reports(NOW).len();
        let mut cross_page = request(&["capture:run.log"]);
        cross_page.parent = Some(beta_id);
        ctx.now = super::policy::parse_stamp("2026-11-21 14:30 +02:00").unwrap();
        match attest::attest(&ctx, &cross_page).unwrap_err() {
            AttestError::Usage(message) => {
                assert!(message.contains("alpha#^s-trust-order"), "{message}");
            }
            other => panic!("expected usage, got {other:?}"),
        }
        assert_eq!(fs::read(&page).unwrap(), alpha_before);
        assert_eq!(fs::read(&beta).unwrap(), beta_before);
        assert_eq!(world.reports(NOW).len(), count);
    }

    /// Re-sealing a drifted span with `--parent` records the parent as
    /// drifted; `--review-by` and `--redacted` land in the record and the
    /// redaction caps the level at evidence.
    #[test]
    fn parent_after_drift_review_by_and_redacted() {
        let world = World::new();
        let Some(mut ctx) = world.attest_context() else {
            return;
        };
        let page = world.page_with("alpha", UNMARKED);
        let first = attest::attest(&ctx, &request(&["capture:run.log"])).unwrap();

        let drifted = fs::read_to_string(&page)
            .unwrap()
            .replace("bundled rung", "bundled rung (edited)");
        fs::write(&page, drifted).unwrap();
        ctx.now = super::policy::parse_stamp("2026-11-21 11:00 +02:00").unwrap();
        let mut again = request(&["capture:run.log"]);
        again.parent = Some(first.id.clone());
        again.review_by = Some("2026-12-01 09:00 +02:00".to_owned());
        again.redacted = vec!["api token".to_owned()];
        again.level = "audited".to_owned();
        let second = attest::attest(&ctx, &again).unwrap();

        let bytes = fs::read(record::record_path(&world.wiki, &second.id)).unwrap();
        let view = record::well_formed(&second.id, &bytes).unwrap();
        let parent = view.parent.expect("parent recorded");
        assert_eq!(parent.id, first.id);
        assert_eq!(parent.verdict, "drifted");
        assert_eq!(view.review_by.as_deref(), Some("2026-12-01 09:00 +02:00"));
        assert_eq!(view.redacted.len(), 1);
        let report = world
            .reports("2026-11-21 12:00 +02:00")
            .into_iter()
            .find(|r| r.id == second.id)
            .expect("second report");
        assert!(report
            .outcome
            .codes
            .informational
            .contains(&super::verdict::code::CAPPED_REDACTION));
        assert_eq!(
            report.outcome.effective_level,
            Some(super::policy::Level::Evidence)
        );
    }

    // ----------------------------------------------------------- status

    use super::status::{self, Mark};

    /// A second signing identity in its own trust directory. Both
    /// directories trust both keys and share one policy, as two enrolled
    /// instances of one org would.
    fn second_signer(world: &World, first: &attest::Context) -> attest::Context {
        let dir = world.root.join("config2").join("seal");
        fs::create_dir_all(&dir).unwrap();
        let report = trust::init(&dir, &trust::Enrolled::default(), &first.keygen).unwrap();
        let public = fs::read_to_string(dir.join(trust::PUBLIC_FILE)).unwrap();
        let mut parts = public.split_whitespace();
        let line = format!("{} {}", parts.next().unwrap(), parts.next().unwrap());
        let allowed = world.trust_dir.join(trust::ALLOWED_FILE);
        let mut text = fs::read_to_string(&allowed).unwrap();
        text.push_str(&allowed_line(&report.principal, &line, ""));
        text.push('\n');
        fs::write(&allowed, text).unwrap();
        attest::Context {
            trust_dir: dir,
            ..first.clone()
        }
    }

    fn share_trust(world: &World, other: &attest::Context) {
        if other.trust_dir == world.trust_dir {
            return;
        }
        for name in [trust::ALLOWED_FILE, trust::POLICY_FILE] {
            let from = world.trust_dir.join(name);
            if from.is_file() {
                fs::copy(&from, other.trust_dir.join(name)).unwrap();
            }
        }
    }

    fn mark(kind: &str, id: &str) -> status::Request {
        status::Request {
            id: id.to_owned(),
            kind: kind.to_owned(),
            reason: format!("{kind} for the test"),
            to: None,
        }
    }

    fn report_for(world: &World, now: &str, id: &str) -> verify::Report {
        world
            .reports(now)
            .into_iter()
            .find(|r| r.id == id)
            .expect("report")
    }

    /// AC-S12: dispute and retraction fold in, an insufficient retract
    /// becomes a dispute, citing seals fail with SEL007, originals never change.
    #[test]
    fn status_records() {
        let world = World::new();
        let Some(mut one) = world.attest_context() else {
            // No capable ssh-keygen: marking must refuse, not panic.
            let ctx = attest::Context {
                wiki: world.wiki.clone(),
                repo_root: world.root.clone(),
                trust_dir: world.trust_dir.clone(),
                keygen: world.root.join("no-such-keygen"),
                now: super::policy::parse_stamp(NOW).unwrap(),
                org: None,
                harness: None,
            };
            // An empty wiki has no such seal, so this is a usage error; the
            // point is that it returns an error instead of panicking.
            assert!(matches!(
                status::mark(&ctx, &mark("retract", "0123456789abcdef0123")).unwrap_err(),
                AttestError::Usage(_)
            ));
            return;
        };
        world.page_with("alpha", UNMARKED);
        world.page_with("beta", "# Beta\n\nBeta rests on alpha. ^s-beta-claim\n");
        let mut audited = request(&["capture:run.log"]);
        audited.level = "audited".to_owned();
        let x = attest::attest(&one, &audited).unwrap();
        assert_eq!(x.report.outcome.effective_level, Some(Level::Audited));
        let principal_one = x.report.signer.clone().unwrap();
        world.policy(&[principal_one.as_str()]);
        share_trust(&world, &one);

        let mut cite = request(&["capture:run.log"]);
        cite.page = "beta".to_owned();
        cite.anchor = "s-beta-claim".to_owned();
        cite.evidence.push(format!("seal:{}", x.id));
        let y = attest::attest(&one, &cite).unwrap();
        assert!(y.report.is_citable());

        let original = fs::read(record::record_path(&world.wiki, &x.id)).unwrap();
        let two = second_signer(&world, &one);
        share_trust(&world, &two);
        let at = |ctx: &mut attest::Context, stamp: &str| {
            ctx.now = super::policy::parse_stamp(stamp).unwrap();
        };

        // Anyone trusted may dispute: a flag only, the level is unchanged.
        let mut two = two;
        at(&mut two, "2026-11-21 11:00 +02:00");
        let done = status::mark(&two, &mark("dispute", &x.id)).unwrap();
        assert_eq!(done.recorded, Mark::Dispute);
        assert!(!done.downgraded);
        let now = "2026-11-21 11:05 +02:00";
        let report = report_for(&world, now, &x.id);
        assert!(report.is_disputed());
        assert_eq!(report.outcome.verdict, Verdict::Citable);
        assert_eq!(report.outcome.effective_level, Some(Level::Audited));
        assert!(report_for(&world, now, &y.id).is_citable());

        // A retract by a principal below the seal's level is a dispute.
        at(&mut two, "2026-11-21 11:10 +02:00");
        let done = status::mark(&two, &mark("retract", &x.id)).unwrap();
        assert_eq!(done.recorded, Mark::Dispute);
        assert_eq!(done.requested, Mark::Retract);
        assert!(done.downgraded);
        let now = "2026-11-21 11:15 +02:00";
        let report = report_for(&world, now, &x.id);
        assert_eq!(report.outcome.verdict, Verdict::Citable);
        assert!(report.is_disputed());

        // The same signer may not demote or supersede either.
        at(&mut two, "2026-11-21 11:16 +02:00");
        assert!(
            status::mark(&two, &mark("demote", &x.id))
                .unwrap()
                .downgraded
        );
        assert!(
            status::mark(&two, &mark("supersede", &x.id))
                .unwrap()
                .downgraded
        );

        // The original sealer demotes: a level cap, still citable.
        at(&mut one, "2026-11-21 11:20 +02:00");
        let mut demote = mark("demote", &x.id);
        demote.to = Some("evidence".to_owned());
        let done = status::mark(&one, &demote).unwrap();
        assert_eq!(done.recorded, Mark::Demote);
        let report = report_for(&world, "2026-11-21 11:25 +02:00", &x.id);
        assert_eq!(report.outcome.effective_level, Some(Level::Evidence));
        assert!(report
            .outcome
            .codes
            .informational
            .contains(&super::verdict::code::CAPPED_DEMOTED));
        // Demoting to the level it already has, or below evidence, is refused.
        at(&mut one, "2026-11-21 11:26 +02:00");
        assert!(matches!(
            status::mark(&one, &demote).unwrap_err(),
            AttestError::Usage(_)
        ));

        // A sufficient retract: X is retracted, Y fails with SEL007.
        at(&mut one, "2026-11-21 11:30 +02:00");
        let done = status::mark(&one, &mark("retract", &x.id)).unwrap();
        assert_eq!(done.recorded, Mark::Retract);
        assert!(!done.downgraded);
        let now = "2026-11-21 11:35 +02:00";
        let report = report_for(&world, now, &x.id);
        assert_eq!(report.outcome.verdict, Verdict::Retracted);
        assert!(!report.is_citable() && report.cite_as().is_none());
        let citing = report_for(&world, now, &y.id);
        assert!(!citing.is_citable());
        assert!(citing
            .outcome
            .codes
            .failure
            .contains(&super::verdict::code::CITED_NOT_CITABLE));
        assert_eq!(
            super::verdict::code::lint_code(super::verdict::code::CITED_NOT_CITABLE),
            Some("SEL007")
        );

        // Append-only: the original bytes never changed and nothing was lost.
        assert_eq!(
            fs::read(record::record_path(&world.wiki, &x.id)).unwrap(),
            original
        );
        let records = fs::read_dir(record::records_dir(&world.wiki))
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".json")
            })
            .count();
        assert_eq!(records, 2 + 6);

        // Unknown and malformed targets are usage errors that write nothing.
        for bad in ["0123456789abcdef0123", "nope", y.id.as_str()] {
            at(&mut one, "2026-11-21 12:00 +02:00");
            let outcome = status::mark(&one, &mark("dispute", bad));
            if bad == y.id {
                assert!(outcome.is_ok());
            } else {
                assert!(
                    matches!(outcome.unwrap_err(), AttestError::Usage(_)),
                    "{bad}"
                );
            }
        }
        let mut empty = mark("dispute", &x.id);
        empty.reason = "  ".to_owned();
        assert!(matches!(
            status::mark(&one, &empty).unwrap_err(),
            AttestError::Usage(_)
        ));
        assert!(matches!(
            status::mark(&one, &mark("bogus", &x.id)).unwrap_err(),
            AttestError::Usage(_)
        ));
    }

    /// Authority does not shrink when a span drifts: a low-ceiling signer
    /// cannot edit an audited span and then retract the seal, and a dispute
    /// on a drifted seal is written, verifies and is reported.
    #[test]
    fn status_records_on_drifted_seal() {
        let world = World::new();
        let Some(one) = world.attest_context() else {
            return;
        };
        world.page_with("alpha", UNMARKED);
        let mut audited = request(&["capture:run.log"]);
        audited.level = "audited".to_owned();
        let x = attest::attest(&one, &audited).unwrap();
        let principal_one = x.report.signer.clone().unwrap();
        world.policy(&[principal_one.as_str()]);
        share_trust(&world, &one);
        let mut two = second_signer(&world, &one);
        share_trust(&world, &two);

        // Edit the sealed span: the seal drifts.
        world.page_with(
            "alpha",
            "# Alpha\n\nTLS now tries something else.\nThen the system store. ^s-trust-order\n\nTail.\n",
        );
        two.now = super::policy::parse_stamp("2026-11-21 11:00 +02:00").unwrap();
        let done = status::mark(&two, &mark("retract", &x.id)).unwrap();
        assert_eq!(done.recorded, Mark::Dispute);
        assert!(done.downgraded);
        let report = report_for(&world, "2026-11-21 11:05 +02:00", &x.id);
        assert_eq!(report.outcome.verdict, Verdict::Drifted);
        assert!(report.is_disputed());

        // The sealer's own retract still takes effect on the drifted seal.
        let mut one = one;
        one.now = super::policy::parse_stamp("2026-11-21 11:10 +02:00").unwrap();
        let done = status::mark(&one, &mark("retract", &x.id)).unwrap();
        assert_eq!(done.recorded, Mark::Retract);
        let report = report_for(&world, "2026-11-21 11:15 +02:00", &x.id);
        assert_eq!(report.outcome.verdict, Verdict::Retracted);
    }

    /// A status record nobody trusts, or that names no signature, changes nothing.
    #[test]
    fn status_records_need_a_verified_signature() {
        let world = World::new();
        let Some(one) = world.attest_context() else {
            return;
        };
        world.page_with("alpha", UNMARKED);
        let x = attest::attest(&one, &request(&["capture:run.log"])).unwrap();

        // Hand-written, unsigned retract.
        let (id, bytes) = record::build(&record::NewRecord {
            kind: record::RecordKind::Status,
            page: "alpha".to_owned(),
            anchor: "s-trust-order".to_owned(),
            subject_sha256: "0".repeat(64),
            locator: record::soft_locator(&[], "x"),
            content: String::new(),
            requested_level: None,
            claim: None,
            procedure_uri: attest::PROCEDURE_URI.to_owned(),
            principal: x.report.signer.clone().unwrap(),
            acted_on_behalf_of: None,
            generator: record::Generator {
                name: "loam".to_owned(),
                version: "1.0.0".to_owned(),
                harness: "test".to_owned(),
            },
            time_verified: "2026-11-21 10:30 +02:00".to_owned(),
            review_by: None,
            evidence: Vec::new(),
            redacted: Vec::new(),
            parent: None,
            status: Some(record::StatusInfo {
                type_: "retract".to_owned(),
                updates: x.id.clone(),
                reason: "forged".to_owned(),
            }),
        })
        .unwrap();
        record::write_record(&world.wiki, &bytes).unwrap();
        let report = report_for(&world, "2026-11-21 11:00 +02:00", &x.id);
        assert_eq!(report.outcome.verdict, Verdict::Citable);
        assert!(!report.is_disputed(), "{id}");
    }

    // ------------------------------------------------------------ carry

    use super::carry;

    const LINKED: &str = "# Alpha\n\nTLS tries [[old-name]] first.\nThen [[topics/old-name|the store]] and [[old-name#Heading]]. ^s-trust-order\n\nTail.\n";

    fn commit_all(world: &World, message: &str) {
        git(&world.root, &["add", "wiki", "run.log", "a.txt"]);
        git(&world.root, &["commit", "-q", "-m", message]);
    }

    fn carry_request(id: &str) -> carry::Request {
        carry::Request { id: id.to_owned() }
    }

    fn refused_text(err: AttestError) -> String {
        match err {
            AttestError::Refused(lines) => lines.join("\n"),
            AttestError::Usage(message) => panic!("expected a refusal, got usage: {message}"),
        }
    }

    /// AC-S11: a git-renamed link target is carried, a semantic edit is
    /// refused, and an untracked page is refused with the re-seal hint.
    #[test]
    fn carry() {
        let world = World::new();
        let Some(mut one) = world.attest_context() else {
            // No capable ssh-keygen: carry must refuse, not panic.
            let ctx = attest::Context {
                wiki: world.wiki.clone(),
                repo_root: world.root.clone(),
                trust_dir: world.trust_dir.clone(),
                keygen: world.root.join("no-such-keygen"),
                now: super::policy::parse_stamp(NOW).unwrap(),
                org: None,
                harness: None,
            };
            assert!(carry::carry(&ctx, &carry_request("0123456789abcdef0123")).is_err());
            return;
        };
        world.init_repo();
        world.page_with("topics/old-name", "# Old name\n\nTarget page.\n");
        world.page_with("alpha", LINKED);
        let mut audited = request(&["capture:run.log"]);
        audited.level = "audited".to_owned();
        let x = attest::attest(&one, &audited).unwrap();
        let principal = x.report.signer.clone().unwrap();
        world.policy(&[principal.as_str()]);
        share_trust(&world, &one);
        assert_eq!(x.report.outcome.effective_level, Some(Level::Audited));
        commit_all(&world, "seal");
        let sealed_text = fs::read_to_string(world.page_path("alpha")).unwrap();
        assert!(sealed_text.contains("loam:seal"));
        let original = fs::read(record::record_path(&world.wiki, &x.id)).unwrap();
        let at = |ctx: &mut attest::Context, stamp: &str| {
            ctx.now = super::policy::parse_stamp(stamp).unwrap();
        };

        // Nothing drifted yet: nothing to carry.
        at(&mut one, "2026-11-21 10:05 +02:00");
        assert!(carry::carry(&one, &carry_request(&x.id)).is_err());

        // Obsidian renames the page and rewrites both link forms.
        git(
            &world.root,
            &["mv", "wiki/topics/old-name.md", "wiki/topics/new-name.md"],
        );
        let retargeted = sealed_text
            .replace("[[old-name]]", "[[new-name]]")
            .replace("[[topics/old-name|", "[[topics/new-name|")
            .replace("[[old-name#Heading]]", "[[new-name#Heading]]");
        world.page_with("alpha", &retargeted);
        commit_all(&world, "rename");
        let drifted = report_for(&world, "2026-11-21 10:06 +02:00", &x.id);
        assert_eq!(drifted.outcome.verdict, Verdict::Drifted);

        // A one-word edit riding along with the retarget is refused.
        let edited = retargeted.replace("first", "second");
        world.page_with("alpha", &edited);
        commit_all(&world, "edit");
        at(&mut one, "2026-11-21 10:10 +02:00");
        let before = fs::read_dir(record::records_dir(&world.wiki))
            .unwrap()
            .count();
        let text = refused_text(carry::carry(&one, &carry_request(&x.id)).unwrap_err());
        assert!(text.contains("re-seal with loam seal attest"), "{text}");
        assert!(text.contains(&x.id), "{text}");
        assert_eq!(
            fs::read_dir(record::records_dir(&world.wiki))
                .unwrap()
                .count(),
            before,
            "a refusal writes nothing"
        );

        // A retarget no recorded rename explains is refused too.
        world.page_with(
            "alpha",
            &retargeted.replace("[[new-name]]", "[[elsewhere]]"),
        );
        commit_all(&world, "wrong target");
        assert!(carry::carry(&one, &carry_request(&x.id)).is_err());

        // An alias or fragment change is not a target-only change.
        world.page_with("alpha", &retargeted.replace("the store", "the cache"));
        commit_all(&world, "alias");
        assert!(carry::carry(&one, &carry_request(&x.id)).is_err());
        world.page_with("alpha", &retargeted.replace("#Heading", "#Other"));
        commit_all(&world, "fragment");
        assert!(carry::carry(&one, &carry_request(&x.id)).is_err());

        // The pure retarget is carried at the prior level, by the sealer only.
        world.page_with("alpha", &retargeted);
        commit_all(&world, "pure retarget");
        let mut two = second_signer(&world, &one);
        share_trust(&world, &two);
        at(&mut two, "2026-11-21 10:15 +02:00");
        let text = refused_text(carry::carry(&two, &carry_request(&x.id)).unwrap_err());
        assert!(text.contains("may not carry"), "{text}");
        assert_eq!(
            report_for(&world, "2026-11-21 10:16 +02:00", &x.id)
                .outcome
                .verdict,
            Verdict::Drifted
        );
        at(&mut one, "2026-11-21 10:20 +02:00");
        let done = carry::carry(&one, &carry_request(&x.id)).unwrap();
        assert!(done.report.is_citable(), "{:?}", done.report.outcome);
        assert_eq!(done.report.outcome.effective_level, Some(Level::Audited));
        let now = "2026-11-21 10:25 +02:00";
        let report = report_for(&world, now, &x.id);
        assert!(report.is_citable());
        assert_eq!(report.outcome.effective_level, Some(Level::Audited));
        assert_eq!(
            fs::read(record::record_path(&world.wiki, &x.id)).unwrap(),
            original,
            "the original record is never edited"
        );
        let update = record::load_all(&world.wiki)
            .unwrap()
            .into_iter()
            .find(|l| l.id == done.id)
            .and_then(|l| l.view.ok())
            .unwrap();
        assert_eq!(update.kind, record::RecordKind::Update);
        let info = update.status.unwrap();
        assert_eq!(info.type_, "link-retarget");
        assert_eq!(info.updates, x.id);

        // A second rename carries on top of the first update.
        git(
            &world.root,
            &["mv", "wiki/topics/new-name.md", "wiki/topics/newest.md"],
        );
        let again = retargeted
            .replace("[[new-name]]", "[[newest]]")
            .replace("[[topics/new-name|", "[[topics/newest|")
            .replace("[[new-name#Heading]]", "[[newest#Heading]]");
        world.page_with("alpha", &again);
        commit_all(&world, "second rename");
        assert_eq!(
            report_for(&world, "2026-11-21 10:30 +02:00", &x.id)
                .outcome
                .verdict,
            Verdict::Drifted
        );
        at(&mut one, "2026-11-21 10:40 +02:00");
        carry::carry(&one, &carry_request(&x.id)).unwrap();
        assert!(report_for(&world, "2026-11-21 10:45 +02:00", &x.id).is_citable());

        // Untracked memory: a page git does not know is refused with the hint.
        world.page_with("beta", "# Beta\n\nBeta links [[old-name]]. ^s-beta-claim\n");
        let mut beta = request(&["capture:run.log"]);
        beta.page = "beta".to_owned();
        beta.anchor = "s-beta-claim".to_owned();
        at(&mut one, "2026-11-21 11:00 +02:00");
        let y = attest::attest(&one, &beta).unwrap();
        world.page_with(
            "beta",
            &fs::read_to_string(world.page_path("beta"))
                .unwrap()
                .replace("[[old-name]]", "[[newest]]"),
        );
        at(&mut one, "2026-11-21 11:10 +02:00");
        let text = refused_text(carry::carry(&one, &carry_request(&y.id)).unwrap_err());
        assert!(
            text.contains("untracked memory: re-seal with loam seal attest"),
            "{text}"
        );

        // A tracked page whose record was never committed is refused the same way.
        git(&world.root, &["add", "-f", "wiki/beta.md"]);
        git(&world.root, &["commit", "-q", "-m", "beta page only"]);
        let text = refused_text(carry::carry(&one, &carry_request(&y.id)).unwrap_err());
        assert!(text.contains("untracked memory"), "{text}");

        // Bad ids are usage errors.
        assert!(matches!(
            carry::carry(&one, &carry_request("nope")).unwrap_err(),
            AttestError::Usage(_)
        ));
        assert!(matches!(
            carry::carry(&one, &carry_request("0123456789abcdef0123")).unwrap_err(),
            AttestError::Usage(_)
        ));
    }
}
