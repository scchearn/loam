//! `loam seal mark`: status records (spec D5, S12).
//!
//! A status record is a signed `kind: status` record that points at a seal
//! with `updates: <id>`. It never edits the seal it names: records are
//! append-only, and verification folds the status records that themselves
//! verify into the seal's lifecycle. Dispute is open to any trusted signer
//! and is a flag only. Demote, supersede and retract need authority: the
//! signer's policy ceiling must reach the seal's level, or the signer must be
//! the principal who sealed it. Anyone else's attempt is written, and read
//! back, as a dispute.

use crate::seal::attest::{
    ambient_context, is_record_id, refuse, signing_identity, stamp, AttestError, Context,
    PROCEDURE_URI,
};
use crate::seal::policy::{self, Level};
use crate::seal::record::{self, Generator, Locator, NewRecord, RecordKind, StatusInfo};
use crate::seal::sshsig;
use crate::seal::trust::Policy;
use crate::seal::verdict::{code, Lifecycle, Verdict};
use crate::seal::verify::{self, Env, Overlay, Report};
use chrono::{DateTime, FixedOffset};
use std::path::Path;

/// The four status types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    Dispute,
    Demote,
    Supersede,
    Retract,
}

impl Mark {
    pub fn as_str(self) -> &'static str {
        match self {
            Mark::Dispute => "dispute",
            Mark::Demote => "demote",
            Mark::Supersede => "supersede",
            Mark::Retract => "retract",
        }
    }

    pub fn parse(value: &str) -> Option<Mark> {
        match value {
            "dispute" => Some(Mark::Dispute),
            "demote" => Some(Mark::Demote),
            "supersede" => Some(Mark::Supersede),
            "retract" => Some(Mark::Retract),
            _ => None,
        }
    }
}

/// Whether `signer` may demote, supersede or retract a seal sealed by
/// `sealer` whose level (before any status record) is `level`.
pub fn may_act(policy: &Policy, signer: &str, sealer: &str, level: Level) -> bool {
    signer == sealer || policy::principal_ceiling(policy, signer) >= level
}

/// One status record that verified, ready to fold.
#[derive(Debug, Clone)]
pub struct Applied {
    pub id: String,
    pub mark: Mark,
    /// A demote's target level.
    pub to: Option<Level>,
    /// The principal the signature matched.
    pub signer: String,
    pub at: DateTime<FixedOffset>,
}

/// The seal's lifecycle and level cap after every verified status record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Folded {
    pub lifecycle: Lifecycle,
    pub demoted_to: Option<Level>,
}

fn rank(lifecycle: Lifecycle) -> u8 {
    match lifecycle {
        Lifecycle::Active => 0,
        Lifecycle::Disputed => 1,
        Lifecycle::Superseded => 2,
        Lifecycle::Retracted => 3,
    }
}

/// Folds `applied` in time order. A record the signer had no authority for
/// counts as a dispute; the strongest lifecycle wins and demotions take the
/// lowest level. The result does not depend on the order of equal stamps.
pub fn fold(applied: &mut [Applied], policy: &Policy, sealer: &str, base: Level) -> Folded {
    applied.sort_by(|a, b| a.at.cmp(&b.at).then_with(|| a.id.cmp(&b.id)));
    let mut out = Folded {
        lifecycle: Lifecycle::Active,
        demoted_to: None,
    };
    let mut raise = |lifecycle: Lifecycle| {
        if rank(lifecycle) > rank(out.lifecycle) {
            out.lifecycle = lifecycle;
        }
    };
    let mut demoted: Option<Level> = None;
    for status in applied.iter() {
        let authorised = may_act(policy, &status.signer, sealer, base);
        match status.mark {
            Mark::Dispute => raise(Lifecycle::Disputed),
            Mark::Demote => match status.to.filter(|_| authorised) {
                Some(level) => demoted = Some(demoted.map_or(level, |d| d.min(level))),
                None => raise(Lifecycle::Disputed),
            },
            Mark::Supersede if authorised => raise(Lifecycle::Superseded),
            Mark::Retract if authorised => raise(Lifecycle::Retracted),
            Mark::Supersede | Mark::Retract => raise(Lifecycle::Disputed),
        }
    }
    out.demoted_to = demoted;
    out
}

/// A parsed `loam seal mark` request.
#[derive(Debug, Clone)]
pub struct Request {
    /// The seal record id the status applies to.
    pub id: String,
    /// `dispute`, `demote`, `supersede` or `retract`.
    pub kind: String,
    pub reason: String,
    /// A demote's target level; defaults to one below the effective level.
    pub to: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Marked {
    /// Id of the status record written.
    pub id: String,
    pub requested: Mark,
    /// What was written: the request, or a dispute when the signer lacked authority.
    pub recorded: Mark,
    pub downgraded: bool,
    /// The target seal as it now verifies.
    pub report: Report,
}

fn one_below(level: Level) -> Option<Level> {
    match level {
        Level::Canonical => Some(Level::Audited),
        Level::Audited => Some(Level::Evidence),
        Level::Evidence => None,
    }
}

/// Writes one signed status record for the seal `request.id`. Nothing is
/// written unless the record would take effect when read back, and no
/// existing file is ever modified.
pub fn mark(ctx: &Context, request: &Request) -> Result<Marked, AttestError> {
    let usage = AttestError::Usage;
    let requested = Mark::parse(&request.kind).ok_or_else(|| {
        usage(format!(
            "`{}` must be dispute, demote, supersede or retract",
            request.kind
        ))
    })?;
    if request.reason.trim().is_empty() {
        return Err(usage("--reason must not be empty".to_owned()));
    }
    if !is_record_id(&request.id) {
        return Err(usage(format!(
            "`{}` is not a {}-hex seal id",
            request.id,
            record::ID_LEN
        )));
    }
    let to = match (&request.to, requested) {
        (None, _) => None,
        (Some(text), Mark::Demote) => Some(Level::parse(text).ok_or_else(|| {
            usage(format!(
                "--to `{text}` must be evidence, audited or canonical"
            ))
        })?),
        (Some(_), _) => return Err(usage("--to applies only to demote".to_owned())),
    };

    let mut env = Env::load(&ctx.wiki, Some(ctx.trust_dir.clone()), ctx.now)
        .map_err(|e| refuse(format!("cannot load trust: {e}")))?;
    env.repo_root = ctx.repo_root.clone();
    env.keygen = ctx.keygen.clone();

    let reports = verify::verify_all(&env).map_err(|e| refuse(e.to_string()))?;
    let target = reports
        .iter()
        .find(|report| report.id == request.id)
        .ok_or_else(|| usage(format!("{} is not a seal record in this wiki", request.id)))?;
    let loaded = record::load_all(&ctx.wiki).map_err(|e| refuse(e.to_string()))?;
    let view = loaded
        .iter()
        .find(|l| l.id == request.id)
        .and_then(|l| l.view.as_ref().ok())
        .ok_or_else(|| usage(format!("seal {} is not a well-formed record", request.id)))?;
    let (page, _) = view
        .subject_name
        .split_once("#^")
        .unwrap_or((view.subject_name.as_str(), ""));

    let to = match requested {
        Mark::Demote => {
            let current = target.outcome.effective_level.ok_or_else(|| {
                usage(format!(
                    "seal {} has no attainable level to demote; use dispute or retract",
                    request.id
                ))
            })?;
            let to = match to {
                Some(level) => level,
                None => one_below(current).ok_or_else(|| {
                    usage(format!(
                        "seal {} is already at evidence, the lowest level; use retract",
                        request.id
                    ))
                })?,
            };
            if to >= current {
                return Err(usage(format!(
                    "--to {} does not lower seal {} below its effective level {}",
                    to.as_str(),
                    request.id,
                    current.as_str()
                )));
            }
            Some(to)
        }
        _ => None,
    };

    let (key, principal) = signing_identity(ctx)?;
    let base = target.base_level.unwrap_or(Level::Evidence);
    let sufficient =
        requested == Mark::Dispute || may_act(&env.trust.policy, &principal, &view.principal, base);
    let recorded = if sufficient { requested } else { Mark::Dispute };
    let downgraded = recorded != requested;

    let (id, bytes) = record::build(&NewRecord {
        kind: RecordKind::Status,
        page: page.to_owned(),
        anchor: view.anchor.clone(),
        subject_sha256: view.subject_sha256.clone(),
        locator: Locator {
            heading_path: Vec::new(),
            prefix: String::new(),
            suffix: String::new(),
        },
        content: String::new(),
        requested_level: to
            .filter(|_| recorded == Mark::Demote)
            .map(|l| l.as_str().to_owned()),
        claim: None,
        procedure_uri: PROCEDURE_URI.to_owned(),
        principal,
        acted_on_behalf_of: None,
        generator: Generator {
            name: "loam".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            harness: ctx
                .harness
                .clone()
                .filter(|h| !h.trim().is_empty())
                .unwrap_or_else(|| "unknown".to_owned()),
        },
        time_verified: stamp(&ctx.now),
        review_by: None,
        evidence: Vec::new(),
        redacted: Vec::new(),
        parent: None,
        status: Some(StatusInfo {
            type_: recorded.as_str().to_owned(),
            updates: request.id.clone(),
            reason: request.reason.trim().to_owned(),
        }),
    })
    .map_err(|e| refuse(format!("cannot build the record: {e}")))?;
    if record::record_path(&ctx.wiki, &id).exists() {
        return Err(refuse(format!(
            "status record {id} already exists (the same status was recorded in the same minute); nothing written"
        )));
    }
    let signature = sshsig::sign(&ctx.keygen, &key, &bytes)
        .map_err(|e| refuse(format!("signing failed: {e}")))?;

    // Rehearse in memory: a status the reader would ignore (for instance a
    // key this wiki's allowed_signers does not list) is refused, not stored.
    let overlay = Overlay {
        record: Some((id.clone(), bytes.clone())),
        signature: Some(signature.clone()),
        ..Overlay::default()
    };
    let rehearsal = verify::verify_overlay(&env, &overlay, &request.id)
        .map_err(|e| refuse(format!("round-trip verification failed: {e}")))?;
    if !took_effect(&rehearsal, recorded) {
        return Err(refuse(format!(
            "the {} would have no effect on seal {} (is this instance's key in allowed_signers?); nothing written",
            recorded.as_str(),
            request.id
        )));
    }

    record::write_record(&ctx.wiki, &bytes)
        .map_err(|e| refuse(format!("cannot write the record: {e}")))?;
    if let Err(e) = record::write_sig(&ctx.wiki, &id, signature.as_bytes()) {
        return Err(refuse(format!(
            "status record {id} was written without its signature and is ignored until signed: {e}"
        )));
    }
    let report = verify::verify_overlay(&env, &Overlay::default(), &request.id)
        .map_err(|e| refuse(format!("status {id} written but not re-verified: {e}")))?;
    Ok(Marked {
        id,
        requested,
        recorded,
        downgraded,
        report,
    })
}

fn took_effect(report: &Report, recorded: Mark) -> bool {
    let codes = &report.outcome.codes.informational;
    let retracted = report.outcome.verdict == Verdict::Retracted;
    let superseded = codes.contains(&code::SUPERSEDED);
    match recorded {
        Mark::Dispute => report.is_disputed() || retracted || superseded,
        Mark::Demote => codes.contains(&code::CAPPED_DEMOTED),
        Mark::Supersede => superseded || retracted,
        Mark::Retract => retracted,
    }
}

// ---------------------------------------------------------------- CLI

fn usage_text() -> i32 {
    eprintln!(
        "Usage: loam seal mark <id> <dispute|demote|supersede|retract> --reason <text> [--to <evidence|audited>]"
    );
    crate::seal::EXIT_USAGE
}

pub fn run(args: impl Iterator<Item = String>) -> i32 {
    use crate::seal::{EXIT_OK, EXIT_REFUSED, EXIT_USAGE};
    let mut args = args;
    let (mut id, mut kind, mut reason, mut to) = (None, None, None, None);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--reason" | "--to" => {
                let Some(value) = args.next() else {
                    eprintln!("loam seal mark: {arg} needs a value");
                    return usage_text();
                };
                if arg == "--reason" {
                    reason = Some(value);
                } else {
                    to = Some(value);
                }
            }
            flag if flag.starts_with("--") => {
                eprintln!("loam seal mark: unknown option {flag}");
                return usage_text();
            }
            _ if id.is_none() => id = Some(arg),
            _ if kind.is_none() => kind = Some(arg),
            _ => {
                eprintln!("loam seal mark: unexpected argument `{arg}`");
                return usage_text();
            }
        }
    }
    let (Some(id), Some(kind), Some(reason)) = (id, kind, reason) else {
        eprintln!("loam seal mark: <id>, a status type and --reason are required");
        return usage_text();
    };
    let Ok(cwd) = std::env::current_dir() else {
        eprintln!("loam seal mark: cannot read the current directory");
        return EXIT_USAGE;
    };
    let Some(wiki) = crate::seal::find_wiki(&cwd) else {
        eprintln!(
            "loam seal mark: no wiki/ directory at or above {}",
            Path::new(&cwd).display()
        );
        return EXIT_USAGE;
    };
    let ctx = match ambient_context(wiki) {
        Ok(ctx) => ctx,
        Err(message) => {
            eprintln!("loam seal mark: {message}");
            return EXIT_REFUSED;
        }
    };
    match mark(
        &ctx,
        &Request {
            id,
            kind,
            reason,
            to,
        },
    ) {
        Ok(done) => {
            let text = |s: &str| crate::json::Value::String(s.to_owned());
            let out = crate::json::Value::Object(vec![
                ("status_record".to_owned(), text(&done.id)),
                ("type".to_owned(), text(done.recorded.as_str())),
                ("requested".to_owned(), text(done.requested.as_str())),
                (
                    "downgraded".to_owned(),
                    crate::json::Value::Bool(done.downgraded),
                ),
                ("seal".to_owned(), done.report.to_value()),
            ]);
            if done.downgraded {
                eprintln!(
                    "loam seal mark: your ceiling does not reach this seal's level, so the {} was recorded as a dispute",
                    done.requested.as_str()
                );
            }
            println!("{}", out.to_json());
            EXIT_OK
        }
        Err(AttestError::Usage(message)) => {
            eprintln!("loam seal mark: {message}");
            EXIT_USAGE
        }
        Err(AttestError::Refused(lines)) => {
            for line in lines {
                eprintln!("loam seal mark: {line}");
            }
            EXIT_REFUSED
        }
    }
}
