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
}
