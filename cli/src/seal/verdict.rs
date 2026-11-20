//! The verdict engine (spec D2, D6, D7): one verdict per seal per read, with
//! success, informational and failure code arrays. A pure function of
//! well-formedness, binding, signature, trust, evidence, freshness and
//! lifecycle inputs with an injected `now`; nothing is read from markers,
//! from the record's claimed level, or from the clock.
//!
//! # Precedence
//!
//! The first matching row wins:
//!
//! 1. `untrusted`: the record is not well-formed or its id does not match
//!    its bytes.
//! 2. `forged`: bad signature, wrong namespace, unknown signer, or
//!    `verifier.principal` differing from the signer the key matched (the
//!    harness name is diagnostic only).
//! 3. `untrusted`: the signing key is revoked.
//! 4. `retracted`: a retract status record applies.
//! 5. `drifted`: the span's current canonical hash differs from the sealed
//!    one, or the span is gone.
//! 6. `stale`: evidence drifted or no longer resolves, a cited seal is no
//!    longer citable, or `review_by` / a recheck has lapsed.
//! 7. `unverifiable`: not even `evidence` is attainable (note-only,
//!    orphaned or otherwise unresolvable primary evidence).
//! 8. `citable`.

use crate::seal::evidence::{Code as EvidenceCode, Kind};
use crate::seal::policy::{
    self, Cap, CeilingInputs, EvidenceState, Expiry, ForgeReason, FreshnessInputs, Level,
    SignatureStanding,
};
use crate::seal::trust::Policy;
use chrono::{DateTime, FixedOffset};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Citable,
    Drifted,
    Stale,
    Unverifiable,
    Untrusted,
    Forged,
    Retracted,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Citable => "citable",
            Verdict::Drifted => "drifted",
            Verdict::Stale => "stale",
            Verdict::Unverifiable => "unverifiable",
            Verdict::Untrusted => "untrusted",
            Verdict::Forged => "forged",
            Verdict::Retracted => "retracted",
        }
    }
}

/// Whether the span's current canonical bytes still match the sealed hash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Binding {
    Matches,
    Mismatch,
    /// The anchor no longer resolves to a well-formed seal span.
    SpanMissing,
}

/// Lifecycle folded from status records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lifecycle {
    Active,
    Disputed,
    Superseded,
    Retracted,
}

pub struct Inputs<'a> {
    /// Record parsed against the schema and its id equals the hash of its bytes.
    pub well_formed: bool,
    pub binding: Binding,
    pub signature: SignatureStanding,
    pub requested: Level,
    pub evidence: &'a [EvidenceState],
    pub redacted: &'a [crate::json::Value],
    pub lifecycle: Lifecycle,
    pub demoted_to: Option<Level>,
    pub cosigners: &'a [String],
    pub time_verified: DateTime<FixedOffset>,
    pub review_by: Option<DateTime<FixedOffset>>,
    pub now: DateTime<FixedOffset>,
    pub policy: &'a Policy,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Codes {
    pub success: Vec<&'static str>,
    pub informational: Vec<&'static str>,
    pub failure: Vec<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub verdict: Verdict,
    /// `None` when not even `evidence` is attainable (unsealed).
    pub effective_level: Option<Level>,
    pub requested_level: Level,
    pub codes: Codes,
}

pub mod code {
    pub const MALFORMED: &str = "seal.record.malformed";
    pub const SIGNATURE_VALID: &str = "seal.signature.valid";
    pub const SIGNATURE_INVALID: &str = "seal.signature.invalid";
    pub const NAMESPACE_WRONG: &str = "seal.signature.namespace";
    pub const SIGNER_UNKNOWN: &str = "seal.signature.unknown_signer";
    pub const PRINCIPAL_MISMATCH: &str = "seal.signature.principal_mismatch";
    pub const VERIFIER_UNAVAILABLE: &str = "seal.signature.verifier_unavailable";
    pub const SIGNER_REVOKED: &str = "seal.signer.revoked";
    pub const BINDING_MATCHES: &str = "seal.binding.matches";
    pub const BINDING_MISMATCH: &str = "seal.binding.mismatch";
    pub const SPAN_MISSING: &str = "seal.binding.span_missing";
    pub const EVIDENCE_CURRENT: &str = "seal.evidence.current";
    pub const EVIDENCE_DRIFTED: &str = "seal.evidence.drifted";
    pub const EVIDENCE_UNRESOLVABLE: &str = "seal.evidence.unresolvable";
    pub const EVIDENCE_ORPHANED: &str = "seal.evidence.orphaned";
    pub const EVIDENCE_INSUFFICIENT: &str = "seal.evidence.insufficient";
    pub const CITED_NOT_CITABLE: &str = "seal.evidence.cited_not_citable";
    pub const REVIEW_BY_PASSED: &str = "seal.freshness.review_by_passed";
    pub const REVIEW_BY_CAPPED: &str = "seal.freshness.review_by_capped";
    pub const RECHECK_DUE: &str = "seal.freshness.recheck_due";
    pub const TIME_VERIFIED_FUTURE: &str = "seal.freshness.time_verified_future";
    pub const FRESH: &str = "seal.freshness.current";
    pub const LEVEL_ABOVE_ATTAINABLE: &str = "seal.level.requested_above_attainable";
    pub const CAPPED_DERIVED: &str = "seal.level.capped_derived";
    pub const CAPPED_REDACTION: &str = "seal.level.capped_redaction";
    pub const CAPPED_DEMOTED: &str = "seal.level.capped_demoted";
    pub const UNATTENDED_KEY: &str = "seal.canonical.unattended_key";
    pub const DISPUTED: &str = "seal.disputed";
    pub const SUPERSEDED: &str = "seal.superseded";
    pub const RETRACTED: &str = "seal.retracted";

    /// The lint code a verify code reports under (spec D9), or `None` for
    /// codes lint does not surface.
    pub fn lint_code(code: &str) -> Option<&'static str> {
        Some(match code {
            MALFORMED | SIGNATURE_INVALID | NAMESPACE_WRONG | SIGNER_UNKNOWN
            | PRINCIPAL_MISMATCH | SIGNER_REVOKED => "SEL003",
            BINDING_MISMATCH | SPAN_MISSING => "SEL004",
            EVIDENCE_DRIFTED | EVIDENCE_UNRESOLVABLE => "SEL005",
            REVIEW_BY_PASSED | REVIEW_BY_CAPPED | RECHECK_DUE | TIME_VERIFIED_FUTURE => "SEL006",
            CITED_NOT_CITABLE | RETRACTED => "SEL007",
            LEVEL_ABOVE_ATTAINABLE
            | CAPPED_DERIVED
            | CAPPED_REDACTION
            | CAPPED_DEMOTED
            | UNATTENDED_KEY
            | VERIFIER_UNAVAILABLE => "SEL008",
            _ => return None,
        })
    }
}

fn outcome(verdict: Verdict, requested: Level, effective: Option<Level>, codes: Codes) -> Outcome {
    Outcome {
        verdict,
        effective_level: effective,
        requested_level: requested,
        codes,
    }
}

/// Compute the verdict for one seal. See the module docs for precedence.
pub fn evaluate(input: &Inputs<'_>) -> Outcome {
    let mut codes = Codes::default();
    let requested = input.requested;

    if !input.well_formed {
        codes.failure.push(code::MALFORMED);
        return outcome(Verdict::Untrusted, requested, None, codes);
    }

    match &input.signature {
        SignatureStanding::Forged(reason) => {
            codes.failure.push(match reason {
                ForgeReason::BadSignature => code::SIGNATURE_INVALID,
                ForgeReason::WrongNamespace => code::NAMESPACE_WRONG,
                ForgeReason::UnknownSigner => code::SIGNER_UNKNOWN,
            });
            return outcome(Verdict::Forged, requested, None, codes);
        }
        SignatureStanding::Verified {
            matched_principal,
            claimed_principal,
            ..
        } if matched_principal != claimed_principal => {
            codes.failure.push(code::PRINCIPAL_MISMATCH);
            return outcome(Verdict::Forged, requested, None, codes);
        }
        SignatureStanding::Revoked => {
            codes.failure.push(code::SIGNER_REVOKED);
            return outcome(Verdict::Untrusted, requested, None, codes);
        }
        _ => {}
    }

    let ceiling = policy::ceiling(&CeilingInputs {
        binding_matches: input.binding == Binding::Matches,
        evidence: input.evidence,
        signature: &input.signature,
        cosigners: input.cosigners,
        redacted: input.redacted,
        demoted_to: input.demoted_to,
        policy: input.policy,
    });
    let effective = policy::effective(requested, &ceiling);

    if input.lifecycle == Lifecycle::Retracted {
        codes.failure.push(code::RETRACTED);
        return outcome(Verdict::Retracted, requested, effective, codes);
    }

    if input.binding != Binding::Matches {
        codes.failure.push(match input.binding {
            Binding::SpanMissing => code::SPAN_MISSING,
            _ => code::BINDING_MISMATCH,
        });
        return outcome(Verdict::Drifted, requested, effective, codes);
    }
    codes.success.push(code::BINDING_MATCHES);

    if matches!(input.signature, SignatureStanding::Verified { .. }) {
        codes.success.push(code::SIGNATURE_VALID);
    }
    if input.signature == SignatureStanding::VerifierUnavailable {
        codes.informational.push(code::VERIFIER_UNAVAILABLE);
    }

    // Evidence and freshness.
    let mut stale = false;
    for item in input.evidence {
        match item.code {
            EvidenceCode::Drifted => push_unique(&mut codes.failure, code::EVIDENCE_DRIFTED),
            EvidenceCode::Unresolvable => {
                push_unique(&mut codes.failure, code::EVIDENCE_UNRESOLVABLE)
            }
            EvidenceCode::Orphaned => {
                push_unique(&mut codes.informational, code::EVIDENCE_ORPHANED)
            }
            EvidenceCode::Current => {}
        }
        if matches!(
            item.code,
            EvidenceCode::Drifted | EvidenceCode::Unresolvable
        ) {
            stale = true;
        }
        if item.kind == Kind::Seal && item.cited.is_none() && item.code == EvidenceCode::Current {
            push_unique(&mut codes.failure, code::CITED_NOT_CITABLE);
            stale = true;
        }
    }
    if !stale && !input.evidence.is_empty() {
        codes.success.push(code::EVIDENCE_CURRENT);
    }

    let rechecks: Vec<_> = input.evidence.iter().map(|e| e.recheck.clone()).collect();
    let fresh = policy::freshness(&FreshnessInputs {
        time_verified: input.time_verified,
        review_by: input.review_by,
        rechecks: &rechecks,
        now: input.now,
        max_review_days: input.policy.max_review_days(),
    });
    for expiry in &fresh.expired {
        stale = true;
        push_unique(
            &mut codes.failure,
            match expiry {
                Expiry::ReviewByPassed => code::REVIEW_BY_PASSED,
                Expiry::ReviewByCapped => code::REVIEW_BY_CAPPED,
                Expiry::RecheckDue(_) => code::RECHECK_DUE,
                Expiry::TimeVerifiedInFuture => code::TIME_VERIFIED_FUTURE,
            },
        );
    }
    if fresh.review_by_capped && !fresh.expired.contains(&Expiry::ReviewByCapped) {
        codes.informational.push(code::REVIEW_BY_CAPPED);
    }
    if fresh.expired.is_empty() {
        codes.success.push(code::FRESH);
    }

    // Caps and overclaims are informational on every path that gets here.
    for cap in &ceiling.caps {
        match cap {
            Cap::Derived(_) => push_unique(&mut codes.informational, code::CAPPED_DERIVED),
            Cap::Redaction => push_unique(&mut codes.informational, code::CAPPED_REDACTION),
            Cap::Demoted(_) => push_unique(&mut codes.informational, code::CAPPED_DEMOTED),
            Cap::VerifierUnavailable => {}
        }
    }
    if ceiling.unattended_key && effective == Some(Level::Canonical) {
        codes.informational.push(code::UNATTENDED_KEY);
    }
    if let Some(level) = effective {
        if requested > level {
            codes.informational.push(code::LEVEL_ABOVE_ATTAINABLE);
        }
    }
    match input.lifecycle {
        Lifecycle::Disputed => codes.informational.push(code::DISPUTED),
        Lifecycle::Superseded => codes.informational.push(code::SUPERSEDED),
        _ => {}
    }

    if stale {
        return outcome(Verdict::Stale, requested, effective, codes);
    }
    if effective.is_none() {
        codes.failure.push(code::EVIDENCE_INSUFFICIENT);
        return outcome(Verdict::Unverifiable, requested, None, codes);
    }
    outcome(Verdict::Citable, requested, effective, codes)
}

fn push_unique(list: &mut Vec<&'static str>, item: &'static str) {
    if !list.contains(&item) {
        list.push(item);
    }
}

/// Owned fixture for tests here and in `seal.rs`.
#[cfg(test)]
pub(crate) mod fixture {
    use super::*;
    use crate::seal::evidence::{Recheck, Weight};

    pub const STAMP: &str = "2026-11-20 14:30 +02:00";
    pub const AGENT: &str = "agent.abc@local";

    pub fn at(stamp: &str) -> DateTime<FixedOffset> {
        policy::parse_stamp(stamp).unwrap()
    }

    pub fn primary(kind: Kind) -> EvidenceState {
        EvidenceState {
            kind,
            weight: Weight::Primary,
            code: EvidenceCode::Current,
            recheck: Recheck::OnChange,
            cited: None,
        }
    }

    pub fn note() -> EvidenceState {
        EvidenceState {
            kind: Kind::Note,
            weight: Weight::Zero,
            code: EvidenceCode::Current,
            recheck: Recheck::OnChange,
            cited: None,
        }
    }

    pub fn cited(level: Option<Level>) -> EvidenceState {
        EvidenceState {
            kind: Kind::Seal,
            weight: Weight::Derived,
            code: EvidenceCode::Current,
            recheck: Recheck::OnChange,
            cited: level,
        }
    }

    pub fn verified(principal: &str, key_type: &str) -> SignatureStanding {
        SignatureStanding::Verified {
            matched_principal: principal.to_owned(),
            claimed_principal: principal.to_owned(),
            key_type: key_type.to_owned(),
        }
    }

    pub struct Case {
        pub well_formed: bool,
        pub binding: Binding,
        pub signature: SignatureStanding,
        pub requested: Level,
        pub evidence: Vec<EvidenceState>,
        pub redacted: Vec<crate::json::Value>,
        pub lifecycle: Lifecycle,
        pub demoted_to: Option<Level>,
        pub cosigners: Vec<String>,
        pub time_verified: DateTime<FixedOffset>,
        pub review_by: Option<DateTime<FixedOffset>>,
        pub now: DateTime<FixedOffset>,
        pub policy: Policy,
    }

    impl Case {
        /// A citable audited seal by an agent key, verified the same minute.
        pub fn citable() -> Case {
            Case {
                well_formed: true,
                binding: Binding::Matches,
                signature: verified(AGENT, "ssh-ed25519"),
                requested: Level::Audited,
                evidence: vec![primary(Kind::Blob)],
                redacted: Vec::new(),
                lifecycle: Lifecycle::Active,
                demoted_to: None,
                cosigners: Vec::new(),
                time_verified: at(STAMP),
                review_by: None,
                now: at(STAMP),
                policy: Policy::default(),
            }
        }

        pub fn run(&self) -> Outcome {
            evaluate(&Inputs {
                well_formed: self.well_formed,
                binding: self.binding,
                signature: self.signature.clone(),
                requested: self.requested,
                evidence: &self.evidence,
                redacted: &self.redacted,
                lifecycle: self.lifecycle,
                demoted_to: self.demoted_to,
                cosigners: &self.cosigners,
                time_verified: self.time_verified,
                review_by: self.review_by,
                now: self.now,
                policy: &self.policy,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fixture::*;
    use super::*;

    /// Every precedence row, including rows that collide: each case sets the
    /// named failure plus every lower-priority one, and must still yield the
    /// higher row's verdict.
    #[test]
    fn precedence() {
        let everything_wrong = || {
            let mut c = Case::citable();
            c.binding = Binding::Mismatch;
            c.evidence = vec![{
                let mut e = primary(Kind::Blob);
                e.code = EvidenceCode::Drifted;
                e
            }];
            c.lifecycle = Lifecycle::Retracted;
            c.review_by = Some(at("2026-11-21 00:00 +02:00"));
            c.now = at("2027-01-01 00:00 +02:00");
            c
        };
        type Tweak = fn(&mut Case);
        let rows: [(&str, Tweak, Verdict, &str); 9] = [
            (
                "malformed beats forged",
                |c| {
                    c.well_formed = false;
                    c.signature = SignatureStanding::Forged(ForgeReason::BadSignature);
                },
                Verdict::Untrusted,
                code::MALFORMED,
            ),
            (
                "forged beats revoked",
                |c| c.signature = SignatureStanding::Forged(ForgeReason::WrongNamespace),
                Verdict::Forged,
                code::NAMESPACE_WRONG,
            ),
            (
                "principal mismatch is forged",
                |c| {
                    c.signature = SignatureStanding::Verified {
                        matched_principal: AGENT.into(),
                        claimed_principal: "someone.else@local".into(),
                        key_type: "ssh-ed25519".into(),
                    }
                },
                Verdict::Forged,
                code::PRINCIPAL_MISMATCH,
            ),
            (
                "revoked beats retracted",
                |c| c.signature = SignatureStanding::Revoked,
                Verdict::Untrusted,
                code::SIGNER_REVOKED,
            ),
            (
                "retracted beats drifted",
                |_| {},
                Verdict::Retracted,
                code::RETRACTED,
            ),
            (
                "drifted beats stale",
                |c| c.lifecycle = Lifecycle::Active,
                Verdict::Drifted,
                code::BINDING_MISMATCH,
            ),
            (
                "stale beats unverifiable",
                |c| {
                    c.lifecycle = Lifecycle::Active;
                    c.binding = Binding::Matches;
                },
                Verdict::Stale,
                code::EVIDENCE_DRIFTED,
            ),
            (
                "unverifiable beats citable",
                |c| {
                    c.lifecycle = Lifecycle::Active;
                    c.binding = Binding::Matches;
                    c.evidence = vec![note()];
                    c.review_by = None;
                },
                Verdict::Unverifiable,
                code::EVIDENCE_INSUFFICIENT,
            ),
            (
                "citable",
                |c| {
                    *c = Case::citable();
                },
                Verdict::Citable,
                code::BINDING_MATCHES,
            ),
        ];
        for (name, tweak, verdict, expected_code) in rows {
            let mut case = everything_wrong();
            tweak(&mut case);
            let got = case.run();
            assert_eq!(got.verdict, verdict, "{name}");
            let all: Vec<_> = got
                .codes
                .failure
                .iter()
                .chain(&got.codes.success)
                .copied()
                .collect();
            assert!(all.contains(&expected_code), "{name}: {:?}", got.codes);
        }
    }

    #[test]
    fn missing_span_is_drifted() {
        let mut c = Case::citable();
        c.binding = Binding::SpanMissing;
        let got = c.run();
        assert_eq!(got.verdict, Verdict::Drifted);
        assert_eq!(got.codes.failure, vec![code::SPAN_MISSING]);
    }

    #[test]
    fn orphaned_commit_is_unverifiable_not_forged() {
        let mut c = Case::citable();
        let mut e = primary(Kind::Commit);
        e.code = EvidenceCode::Orphaned;
        c.evidence = vec![e];
        let got = c.run();
        assert_eq!(got.verdict, Verdict::Unverifiable);
        assert_eq!(got.effective_level, None);
    }

    #[test]
    fn cited_seal_not_citable_fails_with_sel007() {
        let mut c = Case::citable();
        c.evidence = vec![primary(Kind::Blob), cited(None)];
        let got = c.run();
        assert_eq!(got.verdict, Verdict::Stale);
        assert!(got.codes.failure.contains(&code::CITED_NOT_CITABLE));
        assert_eq!(code::lint_code(code::CITED_NOT_CITABLE), Some("SEL007"));
    }

    #[test]
    fn disputed_keeps_level() {
        let mut c = Case::citable();
        c.lifecycle = Lifecycle::Disputed;
        let got = c.run();
        assert_eq!(got.verdict, Verdict::Citable);
        assert_eq!(got.effective_level, Some(Level::Audited));
        assert!(got.codes.informational.contains(&code::DISPUTED));
    }

    #[test]
    fn demote_caps_level() {
        let mut c = Case::citable();
        c.demoted_to = Some(Level::Evidence);
        let got = c.run();
        assert_eq!(got.effective_level, Some(Level::Evidence));
        assert!(got
            .codes
            .informational
            .contains(&code::LEVEL_ABOVE_ATTAINABLE));
    }
}
