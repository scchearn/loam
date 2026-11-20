//! Level ceilings and freshness (spec D2, D5, D7): pure functions of the
//! evidence states, the signature standing, the reader's policy and an
//! injected clock. Nothing here reads a marker, a record's claimed level or
//! the wall clock: levels are computed, never typed.
//!
//! `effective_level = min(requested, attainable)`; [`ceiling`] computes
//! `attainable` (with every cap applied) and [`freshness`] decides expiry.
//! [`crate::seal::verdict`] combines the two into one verdict.

use crate::json::Value;
use crate::seal::evidence::{Code, Kind, Recheck, Weight};
use crate::seal::trust::Policy;
use chrono::{DateTime, Duration, FixedOffset};

/// Cumulative seal level: `evidence < audited < canonical`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Level {
    Evidence,
    Audited,
    Canonical,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Evidence => "evidence",
            Level::Audited => "audited",
            Level::Canonical => "canonical",
        }
    }

    pub fn parse(value: &str) -> Option<Level> {
        match value {
            "evidence" => Some(Level::Evidence),
            "audited" => Some(Level::Audited),
            "canonical" => Some(Level::Canonical),
            _ => None,
        }
    }
}

/// Parse a stamp for injected clocks (`--now`) and policy tests: loam's canonical `YYYY-MM-DD HH:MM ±HH:MM` form, the
/// unambiguous `±HHMM` variant, or RFC 3339. Offset-less stamps are rejected.
/// Record stamps are validated elsewhere and must not go through this parser,
/// whose accepted forms are deliberately broader.
pub fn parse_stamp(stamp: &str) -> Option<DateTime<FixedOffset>> {
    DateTime::parse_from_str(stamp, "%Y-%m-%d %H:%M %:z")
        .or_else(|_| DateTime::parse_from_str(stamp, "%Y-%m-%d %H:%M %z"))
        .or_else(|_| DateTime::parse_from_rfc3339(stamp))
        .ok()
}

/// One evidence item as the verifier saw it on this read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceState {
    pub kind: Kind,
    pub weight: Weight,
    pub code: Code,
    pub recheck: Recheck,
    /// For `seal:<id>` items: the cited seal's effective level, or `None`
    /// when the cited seal is not citable (retracted, drifted, unknown).
    pub cited: Option<Level>,
}

impl EvidenceState {
    fn is_derived(&self) -> bool {
        self.kind == Kind::Seal
    }

    /// Resolves and (for derived items) the cited seal still holds.
    pub(crate) fn holds(&self) -> bool {
        self.code == Code::Current && (!self.is_derived() || self.cited.is_some())
    }

    /// A citable cited seal (`seal:`) counts as a primary basis: derived seals
    /// cap at the cited level rather than being refused.
    fn counts_toward_evidence(&self) -> bool {
        match self.weight {
            Weight::Zero => false,
            Weight::Primary => self.code == Code::Current,
            Weight::Derived => self.holds(),
        }
    }
}

/// What the signature check established.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignatureStanding {
    /// No signature present (an `evidence`-level seal needs none).
    Unsigned,
    /// A valid SSHSIG in the `loam-seal` namespace from a key in the
    /// reader's `allowed_signers`.
    Verified {
        /// The principal `ssh-keygen -Y find-principals` matched.
        matched_principal: String,
        /// `verifier.principal` as recorded in the statement.
        claimed_principal: String,
        /// Key type of the signing key, e.g. `ssh-ed25519`, `sk-ssh-ed25519@openssh.com`.
        key_type: String,
    },
    /// Bad signature, wrong namespace, or an unknown signer.
    Forged(ForgeReason),
    /// The signer's key is listed in the reader's `revoked` file.
    Revoked,
    /// No usable `ssh-keygen -Y` on this machine: the signature could not be
    /// checked either way.
    VerifierUnavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForgeReason {
    BadSignature,
    WrongNamespace,
    UnknownSigner,
}

/// Why a level fell below the evidence-derived base.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cap {
    /// A cited seal sits at this level.
    Derived(Level),
    /// The capture declares `redacted[]` fields.
    Redaction,
    /// No verifier on this machine.
    VerifierUnavailable,
    /// A demote status record.
    Demoted(Level),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ceiling {
    /// `None`: not even `evidence` is attainable (unsealed).
    pub level: Option<Level>,
    pub caps: Vec<Cap>,
    /// Canonical granted to a key that is not hardware-backed (`sk-*`).
    pub unattended_key: bool,
}

pub struct CeilingInputs<'a> {
    pub binding_matches: bool,
    pub evidence: &'a [EvidenceState],
    pub signature: &'a SignatureStanding,
    /// Other distinct principals whose valid signatures cover the same seal
    /// (for `min_signers`).
    pub cosigners: &'a [String],
    pub redacted: &'a [Value],
    pub demoted_to: Option<Level>,
    pub policy: &'a Policy,
}

/// `Some(Level::Evidence)` when the capture declares redactions: the
/// redacted bytes cannot be re-audited, so no signer can lift it higher.
pub fn redaction_cap(redacted: &[Value]) -> Option<Level> {
    if redacted.is_empty() {
        None
    } else {
        Some(Level::Evidence)
    }
}

/// `*` and `?` glob match (ssh pattern subset) over whole strings.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) && p[pi] != '*' {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

fn listed(patterns: &[String], principal: &str) -> bool {
    patterns.iter().any(|p| glob_match(p, principal))
}

fn is_hardware_key(key_type: &str) -> bool {
    key_type.starts_with("sk-")
}

/// Compute the highest level the evidence, signature and policy support,
/// then apply every cap. Does not look at the requested level.
pub fn ceiling(input: &CeilingInputs<'_>) -> Ceiling {
    let mut caps = Vec::new();
    let mut unattended_key = false;

    let has_basis =
        input.binding_matches && input.evidence.iter().any(|e| e.counts_toward_evidence());
    if !has_basis {
        return Ceiling {
            level: None,
            caps,
            unattended_key,
        };
    }
    let mut level = Level::Evidence;

    let all_hold = input.evidence.iter().all(|e| e.holds());
    if let SignatureStanding::Verified {
        matched_principal,
        key_type,
        ..
    } = input.signature
    {
        // Levels are cumulative: a canonical grant implies audited, so a
        // principal listed only for canonical still reaches audited.
        let canonical_listed = listed(&input.policy.canonical_principals, matched_principal);
        if all_hold
            && (canonical_listed || listed(&input.policy.audited_principals, matched_principal))
        {
            level = Level::Audited;
            if canonical_listed {
                let mut signers: Vec<&str> = vec![matched_principal.as_str()];
                for other in input.cosigners {
                    if listed(&input.policy.canonical_principals, other)
                        && !signers.contains(&other.as_str())
                    {
                        signers.push(other);
                    }
                }
                if signers.len() as u32 >= input.policy.canonical_min_signers {
                    level = Level::Canonical;
                    unattended_key = !is_hardware_key(key_type);
                }
            }
        }
    }

    fn apply(level: &mut Level, caps: &mut Vec<Cap>, limit: Level, why: Cap) {
        if limit < *level {
            *level = limit;
            caps.push(why);
        }
    }
    if let Some(lowest) = input
        .evidence
        .iter()
        .filter(|e| e.is_derived())
        .filter_map(|e| e.cited)
        .min()
    {
        apply(&mut level, &mut caps, lowest, Cap::Derived(lowest));
    }
    if let Some(limit) = redaction_cap(input.redacted) {
        apply(&mut level, &mut caps, limit, Cap::Redaction);
    }
    if *input.signature == SignatureStanding::VerifierUnavailable {
        caps.push(Cap::VerifierUnavailable);
        level = level.min(Level::Evidence);
    }
    if let Some(limit) = input.demoted_to {
        apply(&mut level, &mut caps, limit, Cap::Demoted(limit));
    }
    if level < Level::Canonical {
        unattended_key = false;
    }

    Ceiling {
        level: Some(level),
        caps,
        unattended_key,
    }
}

/// `min(requested, attainable)`; `None` when nothing is attainable.
pub fn effective(requested: Level, ceiling: &Ceiling) -> Option<Level> {
    ceiling.level.map(|attainable| requested.min(attainable))
}

/// Why a seal is past its freshness window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expiry {
    /// `review_by` has passed.
    ReviewByPassed,
    /// `review_by` was beyond `max_review`; the cap has passed.
    ReviewByCapped,
    /// An `every:<dur>` recheck is due.
    RecheckDue(String),
    /// `time_verified` is later than `now` (beyond clock skew).
    TimeVerifiedInFuture,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Freshness {
    pub expired: Vec<Expiry>,
    /// `review_by` exceeds `max_review`; it is treated as expiring at the cap.
    pub review_by_capped: bool,
}

pub struct FreshnessInputs<'a> {
    pub time_verified: DateTime<FixedOffset>,
    pub review_by: Option<DateTime<FixedOffset>>,
    pub rechecks: &'a [Recheck],
    pub now: DateTime<FixedOffset>,
    pub max_review_days: u32,
}

/// Clock skew tolerated before a `time_verified` counts as forward-dated.
const FUTURE_SKEW_MINUTES: i64 = 5;

/// The freshness window is anchored on `time_verified`, which a writer
/// declares, so the reader's policy bounds it from both sides:
///
/// - a `time_verified` more than a few minutes after `now` is stale
///   ([`Expiry::TimeVerifiedInFuture`]); a forward-dated record cannot buy
///   itself a long window;
/// - a `review_by` never outlives `time_verified + max_review`; an absent
///   `review_by` is treated as expiring at that cap (flagged capped), so
///   omitting the field does not escape the reader's `max_review`;
/// - a recheck is due once `time_verified + interval` is reached (`>=`),
///   while `review_by` and the cap lapse only once strictly passed (`>`).
///
/// Date arithmetic is checked: an unrepresentable cap is treated as
/// unbounded and an unrepresentable recheck deadline as not yet due, so
/// untrusted input cannot panic the verifier.
pub fn freshness(input: &FreshnessInputs<'_>) -> Freshness {
    let mut expired = Vec::new();
    let in_future = input
        .now
        .checked_add_signed(Duration::minutes(FUTURE_SKEW_MINUTES))
        .is_some_and(|limit| input.time_verified > limit);
    if in_future {
        expired.push(Expiry::TimeVerifiedInFuture);
    }
    let cap = input
        .time_verified
        .checked_add_signed(Duration::days(i64::from(input.max_review_days)));
    let (deadline, review_by_capped) = match (input.review_by, cap) {
        (Some(review_by), Some(cap)) if review_by > cap => (Some(cap), true),
        (Some(review_by), _) => (Some(review_by), false),
        (None, Some(cap)) => (Some(cap), true),
        (None, None) => (None, false),
    };
    if let Some(deadline) = deadline {
        if input.now > deadline {
            expired.push(if review_by_capped {
                Expiry::ReviewByCapped
            } else {
                Expiry::ReviewByPassed
            });
        }
    }
    for recheck in input.rechecks {
        let due = recheck
            .interval()
            .and_then(|interval| input.time_verified.checked_add_signed(interval))
            .is_some_and(|at| input.now >= at);
        if due {
            let label = recheck.as_string();
            if !expired.contains(&Expiry::RecheckDue(label.clone())) {
                expired.push(Expiry::RecheckDue(label));
            }
        }
    }
    Freshness {
        expired,
        review_by_capped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob() {
        assert!(glob_match("agent.*@*", "agent.abc@local"));
        assert!(!glob_match("agent.*@*", "alice@example.com"));
        assert!(glob_match("a?c", "abc"));
        assert!(!glob_match("a?c", "ac"));
        assert!(glob_match("*", ""));
        assert!(glob_match("alice@x", "alice@x"));
        assert!(!glob_match("alice@x", "alice@xy"));
    }

    #[test]
    fn stamps() {
        assert!(parse_stamp("2026-11-20 14:30 +02:00").is_some());
        assert!(parse_stamp("2026-11-20 14:30 +0200").is_some());
        assert!(parse_stamp("2026-11-20 14:30").is_none());
    }
}
