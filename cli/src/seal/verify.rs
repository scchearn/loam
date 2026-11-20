//! Read-only seal verification (spec D2, D6, D8).
//!
//! Loads every record under `wiki/.seals/records/`, finds each live span by
//! anchor across all pages, binds it through `loam.canon/v1`, checks the
//! SSHSIG with the reader's own `ssh-keygen`, re-resolves the evidence and
//! hands everything to the verdict engine. Nothing here writes under `wiki/`
//! or the trust directory, and nothing ever refuses or reverts a page edit.

use crate::json::Value;
use crate::markers::{self, Kind as MarkerKind};
use crate::seal::canon;
use crate::seal::evidence::{self, Ctx};
use crate::seal::policy::{self, EvidenceState, ForgeReason, Level, SignatureStanding};
use crate::seal::record::{self, Loaded, View};
use crate::seal::sshsig::{self, Capability, SshsigError};
use crate::seal::status;
use crate::seal::trust::{self, Trust};
use crate::seal::verdict::{self, code, Binding, Inputs, Lifecycle, Outcome, Verdict};
use chrono::{DateTime, FixedOffset};
use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Informational: the span now lives on a different page than the one the
/// record names. The verdict is unchanged.
pub const SUBJECT_MOVED: &str = "seal.subject.moved";

/// Deepest `seal:<id>` citation chain followed before the chain is treated as
/// unresolvable (a cycle guard as much as a depth bound).
const MAX_CITE_DEPTH: usize = 16;
const MAX_SIGNATURE_BYTES: u64 = 64 * 1024;

/// Everything a verification run reads from outside the wiki.
pub struct Env {
    pub wiki: PathBuf,
    /// Blob and file evidence resolves against this, not the wiki root.
    pub repo_root: PathBuf,
    pub trust: Trust,
    /// `<configRoot>/seal`; `None` when no config root resolves, in which
    /// case no key is trusted.
    pub trust_dir: Option<PathBuf>,
    pub keygen: PathBuf,
    pub now: DateTime<FixedOffset>,
}

impl Env {
    /// Builds the environment for `wiki`, loading trust from `trust_dir`.
    pub fn load(
        wiki: &Path,
        trust_dir: Option<PathBuf>,
        now: DateTime<FixedOffset>,
    ) -> Result<Env, String> {
        let trust = match &trust_dir {
            Some(dir) => trust::load(dir).map_err(|e| e.to_string())?,
            None => Trust {
                signers: Vec::new(),
                revoked: Vec::new(),
                policy: trust::Policy::default(),
            },
        };
        let repo_root = evidence::repo_root(wiki)
            .or_else(|| wiki.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| wiki.to_path_buf());
        Ok(Env {
            wiki: wiki.to_path_buf(),
            repo_root,
            trust,
            trust_dir,
            keygen: sshsig::keygen_binary(),
            now,
        })
    }
}

/// One seal's result.
#[derive(Debug, Clone)]
pub struct Report {
    pub id: String,
    /// The record's `<page>#^<anchor>` subject name; `None` when malformed.
    pub subject: Option<String>,
    /// The page name the record names.
    pub page: Option<String>,
    pub anchor: Option<String>,
    /// The page the live span was found on, when it was found.
    pub current_page: Option<String>,
    pub outcome: Outcome,
    /// The level the evidence and signature support before any status record
    /// applies: what decides who may demote, supersede or retract.
    pub base_level: Option<Level>,
    /// The principal the signature verified as, when it did.
    pub signer: Option<String>,
    pub time_verified: Option<String>,
    /// Where the sealed text probably is now, when its markers are gone.
    /// A hint only; never used to validate.
    pub likely_location: Option<String>,
    /// The id of a later, well-formed seal of the same subject that names this
    /// record as its `parent` and binds to the live span. Set only on a record
    /// that drifted; a re-seal is the remedy for drift, so such a record is
    /// history rather than a defect.
    pub superseded_by: Option<String>,
}

impl Report {
    pub fn is_citable(&self) -> bool {
        self.outcome.verdict == Verdict::Citable
    }

    /// `seal:<id>`, only for a seal that may be cited.
    pub fn cite_as(&self) -> Option<String> {
        self.is_citable().then(|| format!("seal:{}", self.id))
    }

    pub fn is_disputed(&self) -> bool {
        self.outcome.codes.informational.contains(&code::DISPUTED)
    }

    /// Whether this seal is selected by `<page>[#^anchor]`.
    pub fn matches(&self, selector: &Selector) -> bool {
        let page_ok = |name: &str| selector.page.as_ref().is_none_or(|p| page_matches(name, p));
        let on_page = self.page.as_deref().is_some_and(page_ok)
            || self.current_page.as_deref().is_some_and(page_ok);
        let on_anchor = selector
            .anchor
            .as_ref()
            .is_none_or(|a| self.anchor.as_ref() == Some(a));
        on_page && on_anchor
    }

    /// The D8 JSON object (plus `subject` and, when known, `likely_location`).
    pub fn to_value(&self) -> Value {
        let text = |s: &str| Value::String(s.to_owned());
        let optional = |s: &Option<String>| s.as_deref().map_or(Value::Null, text);
        let list = |codes: &[&'static str]| Value::Array(codes.iter().map(|c| text(c)).collect());
        let codes = &self.outcome.codes;
        let mut fields = vec![
            ("seal".to_owned(), text(&self.id)),
            ("verdict".to_owned(), text(self.outcome.verdict.as_str())),
            (
                "effective_level".to_owned(),
                self.outcome
                    .effective_level
                    .map_or(Value::Null, |l| text(l.as_str())),
            ),
            (
                "requested_level".to_owned(),
                text(self.outcome.requested_level.as_str()),
            ),
            ("signer".to_owned(), optional(&self.signer)),
            ("time_verified".to_owned(), optional(&self.time_verified)),
            (
                "codes".to_owned(),
                Value::Object(vec![
                    ("success".to_owned(), list(&codes.success)),
                    ("informational".to_owned(), list(&codes.informational)),
                    ("failure".to_owned(), list(&codes.failure)),
                ]),
            ),
            (
                "cite_as".to_owned(),
                self.cite_as().as_deref().map_or(Value::Null, text),
            ),
            ("subject".to_owned(), optional(&self.subject)),
        ];
        if let Some(location) = &self.likely_location {
            fields.push(("likely_location".to_owned(), text(location)));
        }
        Value::Object(fields)
    }
}

/// `<page>[#^anchor]` as typed on the command line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Selector {
    /// The page argument as typed, kept so [`Selector::resolve`] can treat it
    /// as a filesystem path.
    raw_page: Option<String>,
    pub page: Option<String>,
    pub anchor: Option<String>,
}

impl Selector {
    pub fn parse(arg: &str) -> Result<Selector, String> {
        let (page, anchor) = match arg.split_once('#') {
            Some((page, anchor)) => (page, Some(anchor.trim_start_matches('^'))),
            None => (arg, None),
        };
        if anchor.is_some_and(str::is_empty) {
            return Err(format!("`{arg}` names an empty anchor"));
        }
        let page = page.trim();
        let page = (!page.is_empty()).then(|| normalize_page(page));
        if page.is_none() && anchor.is_none() {
            return Err("empty page selector".to_owned());
        }
        Ok(Selector {
            raw_page: (page.is_some())
                .then(|| arg.split('#').next().unwrap_or("").trim().to_owned()),
            page,
            anchor: anchor.map(str::to_owned),
        })
    }
}

impl Selector {
    /// Resolves the page the way `attest` will: a path naming an existing
    /// file (absolute, or relative to `cwd`) becomes its wiki-relative name;
    /// otherwise a leading `wiki/` is dropped.
    pub fn resolve(mut self, wiki: &Path, cwd: &Path) -> Selector {
        let Some(raw) = self.raw_page.take() else {
            return self;
        };
        let typed = Path::new(&raw);
        let joined = if typed.is_absolute() {
            typed.to_path_buf()
        } else {
            cwd.join(typed)
        };
        let canon = |p: &Path| fs::canonicalize(p).map(crate::enrollment::dos_compatible);
        if joined.is_file() {
            if let (Ok(file), Ok(root)) = (canon(&joined), canon(wiki)) {
                if let Ok(rel) = file.strip_prefix(&root) {
                    let name = rel.to_string_lossy().replace('\\', "/");
                    self.page = Some(normalize_page(&name));
                    return self;
                }
            }
        }
        if let Some(page) = &self.page {
            if let Some(rest) = page.strip_prefix("wiki/") {
                self.page = Some(rest.to_owned());
            }
        }
        self
    }
}

fn normalize_page(page: &str) -> String {
    let forward = page.replace('\\', "/");
    let trimmed = forward.trim_start_matches("./");
    trimmed.strip_suffix(".md").unwrap_or(trimmed).to_owned()
}

/// A selector page (`a/b`, `b`, `b.md`) names a recorded or current page
/// (`a/b`): equal, or the selector is its bare file stem.
fn page_matches(name: &str, wanted: &str) -> bool {
    name == wanted || (!wanted.contains('/') && stem_of(name) == wanted)
}

fn stem_of(name: &str) -> &str {
    name.rsplit('/').next().unwrap_or(name)
}

/// A recorded page name is satisfied by the span's page when it equals the
/// page's wiki-relative name, or (a bare name) its file stem.
fn same_page(current: &str, recorded: &str) -> bool {
    if recorded.contains('/') {
        current == recorded
    } else {
        stem_of(current) == recorded
    }
}

struct Span {
    /// Wiki-relative page name: forward slashes, no `.md`.
    name: String,
    anchor: String,
    /// `None` when the span is not valid UTF-8.
    digest: Option<String>,
}

pub(super) fn markdown_files(wiki: &Path) -> Result<Vec<(String, PathBuf)>, String> {
    fn walk(dir: &Path, prefix: &str, out: &mut Vec<(String, PathBuf)>) -> Result<(), String> {
        let entries = fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("{}: {e}", dir.display()))?;
            let file_name = entry.file_name();
            let Some(name) = file_name.to_str() else {
                continue;
            };
            if name.starts_with('.') {
                continue;
            }
            // `file_type` does not follow symlinks, so a link loop cannot hang
            // the walk and a link out of the wiki is not read.
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                walk(&entry.path(), &format!("{prefix}{name}/"), out)?;
            } else if kind.is_file() {
                if let Some(stem) = name.strip_suffix(".md") {
                    out.push((format!("{prefix}{stem}"), entry.path()));
                }
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    walk(wiki, "", &mut files)?;
    files.sort();
    Ok(files)
}

fn collect_spans(files: &[(String, PathBuf)], overlay: Option<&Overlay>) -> Vec<Span> {
    let mut spans = Vec::new();
    for (name, path) in files {
        let replaced = overlay
            .and_then(|o| o.page.as_ref())
            .filter(|(page, _)| page == name)
            .map(|(_, bytes)| bytes.clone());
        let Some(bytes) = replaced.or_else(|| fs::read(path).ok()) else {
            continue;
        };
        let Ok(source) = std::str::from_utf8(&bytes) else {
            continue;
        };
        for marker in markers::scan(source)
            .markers
            .iter()
            .filter(|m| m.kind == MarkerKind::Seal)
        {
            spans.push(Span {
                name: name.clone(),
                anchor: marker.id.clone(),
                digest: canon::digest(&bytes[marker.content.clone()]).ok(),
            });
        }
    }
    spans
}

/// In-memory state verified in place of what is on disk, so `attest` can
/// prove its round trip before it writes anything: the page as it will be,
/// the record and its signature as they will be, and the captures it will
/// store (content-addressed, so correct by construction).
#[derive(Debug, Default, Clone)]
pub struct Overlay {
    /// Wiki-relative page name (no `.md`) and its replacement bytes.
    pub page: Option<(String, Vec<u8>)>,
    /// Record id and exact bytes.
    pub record: Option<(String, Vec<u8>)>,
    /// Armored signature for the overlay record.
    pub signature: Option<String>,
    /// sha256 digests of captures about to be stored.
    pub captures: Vec<String>,
}

/// Verifies every seal record in the wiki, sorted by id.
pub fn verify_all(env: &Env) -> Result<Vec<Report>, String> {
    verify_with(env, None)
}

/// Verifies the single seal `id` as it would stand once `overlay` is applied.
pub fn verify_overlay(env: &Env, overlay: &Overlay, id: &str) -> Result<Report, String> {
    verify_with(env, Some(overlay))?
        .into_iter()
        .find(|report| report.id == id)
        .ok_or_else(|| format!("seal {id} did not verify as a seal record"))
}

fn verify_with(env: &Env, overlay: Option<&Overlay>) -> Result<Vec<Report>, String> {
    let verifier = build_verifier(env, overlay)?;
    let mut reports = Vec::new();
    for loaded in &verifier.records {
        let is_seal = match &loaded.view {
            Ok(view) => view.kind == record::RecordKind::Seal,
            Err(_) => true,
        };
        if is_seal {
            if let Some(report) = verifier.report(&loaded.id, &mut Vec::new()) {
                reports.push(report);
            }
        }
    }
    mark_superseded(&verifier, &mut reports);
    Ok(reports)
}

/// Flags each drifted record that a sound child re-seal replaces. The child
/// must itself bind to the live span, so a planted or broken child cannot hide
/// real drift, and its own level must be at least the parent's so a weaker
/// re-seal cannot silence drift on an audited or canonical record.
fn mark_superseded(verifier: &Verifier<'_>, reports: &mut [Report]) {
    let drifts = |report: &Report| {
        let failure = &report.outcome.codes.failure;
        failure.contains(&code::BINDING_MISMATCH) || failure.contains(&code::SPAN_MISSING)
    };
    let sound = |report: &Report| {
        let failure = &report.outcome.codes.failure;
        !failure.contains(&code::MALFORMED)
            && !failure.contains(&code::SIGNATURE_INVALID)
            && !drifts(report)
            && !matches!(
                report.outcome.verdict,
                Verdict::Forged | Verdict::Untrusted | Verdict::Retracted
            )
    };
    let children: Vec<(String, String, String)> = verifier
        .records
        .iter()
        .filter_map(|loaded| {
            let view = loaded.view.as_ref().ok()?;
            if view.kind != record::RecordKind::Seal {
                return None;
            }
            let parent = view.parent.as_ref()?;
            Some((
                loaded.id.clone(),
                parent.id.clone(),
                view.subject_name.clone(),
            ))
        })
        .collect();
    let snapshot: Vec<Report> = reports.to_vec();
    for report in reports.iter_mut().filter(|report| drifts(report)) {
        let found = children.iter().find(|(child, parent, subject)| {
            parent == &report.id
                && report.subject.as_ref() == Some(subject)
                && snapshot.iter().any(|other| {
                    &other.id == child
                        && sound(other)
                        && other
                            .base_level
                            .is_some_and(|level| level >= report.outcome.requested_level)
                })
        });
        if let Some((child, _, _)) = found {
            report.superseded_by = Some(child.clone());
        }
    }
}

fn build_verifier<'a>(env: &'a Env, overlay: Option<&'a Overlay>) -> Result<Verifier<'a>, String> {
    let files = markdown_files(&env.wiki)?;
    let spans = collect_spans(&files, overlay);
    let mut records = record::load_all(&env.wiki)?;
    if let Some((id, bytes)) = overlay.and_then(|o| o.record.as_ref()) {
        records.retain(|loaded| &loaded.id != id);
        records.push(Loaded {
            id: id.clone(),
            bytes: bytes.clone(),
            view: record::well_formed(id, bytes),
        });
        records.sort_by(|a, b| a.id.cmp(&b.id));
    }
    Ok(Verifier {
        env,
        files,
        spans,
        records,
        overlay,
        cache: RefCell::new(HashMap::new()),
    })
}

/// A verified `link-retarget` update: the span the seal is bound to now.
#[derive(Debug, Clone)]
pub struct AppliedUpdate {
    pub id: String,
    /// `loam.canon/v1` digest of the updated span.
    pub digest: String,
    /// When the update was verified; orders updates oldest first.
    pub at: DateTime<FixedOffset>,
}

/// Every verified `link-retarget` update for the seal `id`.
pub fn updates_for(env: &Env, id: &str) -> Result<Vec<AppliedUpdate>, String> {
    let verifier = build_verifier(env, None)?;
    let Some(loaded) = verifier.records.iter().find(|l| l.id == id) else {
        return Ok(Vec::new());
    };
    let Ok(view) = &loaded.view else {
        return Ok(Vec::new());
    };
    let requested = view
        .requested_level
        .as_deref()
        .and_then(Level::parse)
        .unwrap_or(Level::Evidence);
    Ok(verifier.verified_updates(id, view, requested))
}

struct Verifier<'a> {
    env: &'a Env,
    files: Vec<(String, PathBuf)>,
    spans: Vec<Span>,
    records: Vec<Loaded>,
    overlay: Option<&'a Overlay>,
    cache: RefCell<HashMap<String, Report>>,
}

impl Verifier<'_> {
    fn report(&self, id: &str, stack: &mut Vec<String>) -> Option<Report> {
        if let Some(found) = self.cache.borrow().get(id) {
            return Some(found.clone());
        }
        if stack.iter().any(|seen| seen == id) || stack.len() >= MAX_CITE_DEPTH {
            return None;
        }
        let loaded = self.records.iter().find(|l| l.id == id)?;
        stack.push(id.to_owned());
        let report = self.compute(loaded, stack);
        stack.pop();
        self.cache
            .borrow_mut()
            .insert(id.to_owned(), report.clone());
        Some(report)
    }

    fn malformed(&self, loaded: &Loaded, view: Option<&View>) -> Report {
        let mut codes = verdict::Codes::default();
        codes.failure.push(code::MALFORMED);
        Report {
            id: loaded.id.clone(),
            subject: view.map(|v| v.subject_name.clone()),
            page: None,
            anchor: view.map(|v| v.anchor.clone()),
            current_page: None,
            outcome: Outcome {
                verdict: Verdict::Untrusted,
                effective_level: None,
                requested_level: Level::Evidence,
                codes,
            },
            base_level: None,
            signer: None,
            time_verified: view.map(|v| v.time_verified.clone()),
            likely_location: None,
            superseded_by: None,
        }
    }

    fn compute(&self, loaded: &Loaded, stack: &mut Vec<String>) -> Report {
        let Ok(view) = &loaded.view else {
            return self.malformed(loaded, None);
        };
        let requested = view
            .requested_level
            .as_deref()
            .and_then(Level::parse)
            .unwrap_or(Level::Evidence);
        let time_verified = policy::parse_stamp(&view.time_verified);
        let review_by = match view.review_by.as_deref() {
            Some(stamp) => policy::parse_stamp(stamp).map(Some),
            None => Some(None),
        };
        let items = evidence::items_from_values(&view.evidence);
        let (Some(time_verified), Some(review_by), Ok(items)) = (time_verified, review_by, items)
        else {
            return self.malformed(loaded, Some(view));
        };

        let page = view
            .subject_name
            .split_once("#^")
            .map_or(view.subject_name.as_str(), |(page, _)| page)
            .to_owned();

        // Binding. A verified `link-retarget` update moves the digest the
        // span must match; the sealed digest itself stays acceptable, so
        // undoing the retarget does not drift the seal.
        let carried = self.verified_updates(&loaded.id, view, requested);
        let accepts = |digest: Option<&str>| {
            digest
                .is_some_and(|d| d == view.subject_sha256 || carried.iter().any(|u| u.digest == d))
        };
        let candidates: Vec<&Span> = self
            .spans
            .iter()
            .filter(|s| s.anchor == view.anchor)
            .collect();
        let bound = |s: &&&Span| accepts(s.digest.as_deref());
        // The recorded page wins, so an edit there is not hidden by a verbatim
        // copy of the anchor elsewhere; a moved page falls back to the digest.
        let picked = candidates
            .iter()
            .find(|s| same_page(&s.name, &page))
            .or_else(|| candidates.iter().find(bound))
            .or_else(|| candidates.first())
            .copied();
        let binding = match picked {
            None => Binding::SpanMissing,
            Some(span) if accepts(span.digest.as_deref()) => Binding::Matches,
            Some(_) => Binding::Mismatch,
        };
        let moved = picked.is_some_and(|s| !same_page(&s.name, &page));
        let likely_location = (binding == Binding::SpanMissing)
            .then(|| self.locate(view))
            .flatten();

        // Signature.
        let (signature, signer) = self.signature(loaded, view);

        // Evidence.
        let ctx = Ctx {
            repo_root: self.env.repo_root.clone(),
            wiki: self.env.wiki.clone(),
            retrieved_at: String::new(),
            capture_max_bytes: self.env.trust.policy.capture_max_bytes,
        };
        let states: Vec<EvidenceState> = items
            .iter()
            .map(|item| {
                let mut check = evidence::check(item, &ctx);
                let pending = item.kind == evidence::Kind::Capture
                    && item.sha256.as_ref().is_some_and(|digest| {
                        self.overlay
                            .is_some_and(|o| o.captures.iter().any(|c| c == digest))
                    });
                if pending {
                    check.code = evidence::Code::Current;
                }
                let cited = (item.kind == evidence::Kind::Seal)
                    .then(|| self.report(&item.reference, stack))
                    .flatten()
                    .filter(Report::is_citable)
                    .and_then(|r| r.outcome.effective_level);
                EvidenceState {
                    kind: item.kind,
                    weight: check.weight,
                    code: check.code,
                    recheck: item.recheck.clone(),
                    cited,
                }
            })
            .collect();

        let evaluate = |lifecycle: Lifecycle, demoted_to: Option<Level>| {
            verdict::evaluate(&Inputs {
                well_formed: true,
                binding,
                signature: signature.clone(),
                requested,
                evidence: &states,
                redacted: &view.redacted,
                lifecycle,
                demoted_to,
                cosigners: &[],
                time_verified,
                review_by,
                now: self.env.now,
                policy: &self.env.trust.policy,
            })
        };
        // Status records apply only when they themselves verify; whether a
        // signer may demote, supersede or retract is judged against the level
        // the seal had before any status record (spec D5).
        let mut applied = self.applied_statuses(&loaded.id, view);
        let base = evaluate(Lifecycle::Active, None);
        let base_level = base.effective_level;
        let mut outcome = if applied.is_empty() {
            base
        } else {
            let folded = status::fold(
                &mut applied,
                &self.env.trust.policy,
                &view.principal,
                status::authority_level(&self.env.trust.policy, requested, &view.principal),
            );
            evaluate(folded.lifecycle, folded.demoted_to)
        };
        if moved {
            outcome.codes.informational.push(SUBJECT_MOVED);
        }

        Report {
            id: loaded.id.clone(),
            subject: Some(view.subject_name.clone()),
            page: Some(page),
            anchor: Some(view.anchor.clone()),
            current_page: picked.map(|s| s.name.clone()),
            outcome,
            base_level,
            signer,
            time_verified: Some(view.time_verified.clone()),
            likely_location,
            superseded_by: None,
        }
    }

    /// Every `link-retarget` update for `target` that itself verifies, oldest
    /// first. Each is:
    /// signed by a trusted signer who is its claimed principal and who may
    /// act on the seal (the sealer, or a ceiling that reaches its level), on
    /// the seal's anchor, with a digest that is the hash of its own content.
    /// An update only rebinds the span; it never changes evidence or level.
    fn verified_updates(
        &self,
        target: &str,
        target_view: &View,
        requested: Level,
    ) -> Vec<AppliedUpdate> {
        let policy = &self.env.trust.policy;
        let ceiling = status::authority_level(policy, requested, &target_view.principal);
        let mut found: Vec<AppliedUpdate> = Vec::new();
        for loaded in &self.records {
            let Ok(view) = &loaded.view else { continue };
            let Some(info) = view
                .status
                .as_ref()
                .filter(|_| view.kind == record::RecordKind::Update)
            else {
                continue;
            };
            if info.updates != target
                || info.type_ != record::UPDATE_TYPE_LINK_RETARGET
                || view.anchor != target_view.anchor
            {
                continue;
            }
            let Some(content) = view.content.as_deref() else {
                continue;
            };
            if crate::sha256::hex(content.as_bytes()) != view.subject_sha256 {
                continue;
            }
            // An update is only a retarget if it differs from the as-sealed
            // text by link targets alone; a signature cannot widen that.
            let Some(sealed) = target_view.content.as_deref() else {
                continue;
            };
            if crate::seal::carry::link_changes(sealed, content).is_err() {
                continue;
            }
            let (standing, _) = self.signature(loaded, view);
            let SignatureStanding::Verified {
                matched_principal,
                claimed_principal,
                ..
            } = standing
            else {
                continue;
            };
            if matched_principal != claimed_principal
                || !status::may_act(policy, &matched_principal, &target_view.principal, ceiling)
            {
                continue;
            }
            let Some(at) = policy::parse_stamp(&view.time_verified) else {
                continue;
            };
            let candidate = AppliedUpdate {
                id: loaded.id.clone(),
                digest: view.subject_sha256.clone(),
                at,
            };
            found.push(candidate);
        }
        found.sort_by(|a, b| (a.at, &a.id).cmp(&(b.at, &b.id)));
        found
    }

    /// Status records that name `target`, sit on its anchor and carry a
    /// signature that verifies as a trusted signer's own.
    fn applied_statuses(&self, target: &str, target_view: &View) -> Vec<status::Applied> {
        let mut applied = Vec::new();
        for loaded in &self.records {
            let Ok(view) = &loaded.view else { continue };
            let Some(info) = view
                .status
                .as_ref()
                .filter(|_| view.kind == record::RecordKind::Status)
            else {
                continue;
            };
            if info.updates != target || view.anchor != target_view.anchor {
                continue;
            }
            let (standing, _) = self.signature(loaded, view);
            let SignatureStanding::Verified {
                matched_principal,
                claimed_principal,
                ..
            } = standing
            else {
                continue;
            };
            if matched_principal != claimed_principal {
                continue;
            }
            let (Some(mark), Some(at)) = (
                status::Mark::parse(&info.type_),
                policy::parse_stamp(&view.time_verified),
            ) else {
                continue;
            };
            applied.push(status::Applied {
                id: loaded.id.clone(),
                mark,
                to: view.requested_level.as_deref().and_then(Level::parse),
                signer: matched_principal,
                at,
            });
        }
        applied
    }

    /// Reports the page whose canonical text still holds both edges of the
    /// recorded locator. A hint for a human; never evidence.
    fn locate(&self, view: &View) -> Option<String> {
        let locator = locator_of(view)?;
        if locator.0.is_empty() || locator.1.is_empty() {
            return None;
        }
        self.files.iter().find_map(|(name, path)| {
            let bytes = fs::read(path).ok()?;
            let text = String::from_utf8(canon::canonicalize(&bytes).ok()?).ok()?;
            (text.contains(&locator.0) && text.contains(&locator.1)).then(|| name.clone())
        })
    }

    fn signature(&self, loaded: &Loaded, view: &View) -> (SignatureStanding, Option<String>) {
        let sig_file = record::sig_path(&self.env.wiki, &loaded.id);
        let pending = self
            .overlay
            .and_then(|o| o.record.as_ref().zip(o.signature.as_ref()))
            .filter(|((id, _), _)| id == &loaded.id)
            .map(|(_, signature)| signature.clone());
        let signature = match pending {
            Some(signature) => Some(signature),
            None => match fs::metadata(&sig_file) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    return (SignatureStanding::Unsigned, None)
                }
                Ok(meta) if meta.len() <= MAX_SIGNATURE_BYTES => fs::read_to_string(&sig_file).ok(),
                _ => None,
            },
        };
        let Some(signature) = signature else {
            return (SignatureStanding::Forged(ForgeReason::BadSignature), None);
        };
        if sshsig::capability_of(&self.env.keygen) != Capability::Available {
            return (SignatureStanding::VerifierUnavailable, None);
        }
        let standing = self.check_signature(&signature, view, &loaded.bytes);
        let signer = match &standing {
            SignatureStanding::Verified {
                matched_principal, ..
            } => Some(matched_principal.clone()),
            _ => None,
        };
        (standing, signer)
    }

    fn check_signature(&self, signature: &str, view: &View, data: &[u8]) -> SignatureStanding {
        use SignatureStanding as S;
        // A signature made for another namespace is a forgery attempt even when
        // the key is trusted: report it as such.
        if namespace_of(signature).is_some_and(|ns| ns != sshsig::NAMESPACE) {
            return S::Forged(ForgeReason::WrongNamespace);
        }
        let Ok(verify_time) = sshsig::verify_time_utc(&view.time_verified) else {
            return S::Forged(ForgeReason::BadSignature);
        };
        let bin = &self.env.keygen;
        let allowed = self
            .env
            .trust_dir
            .as_ref()
            .map(|dir| dir.join(trust::ALLOWED_FILE))
            .filter(|path| path.is_file());

        // Any failure that is not ssh-keygen refusing (spawn, scratch files)
        // means the check could not be made either way.
        let unavailable = |e: &SshsigError| !matches!(e, SshsigError::Rejected(_));
        let not_trusted = || match sshsig::check_novalidate(bin, signature, data) {
            Ok(()) => S::Untrusted,
            Err(e) if unavailable(&e) => S::VerifierUnavailable,
            Err(_) => S::Forged(ForgeReason::BadSignature),
        };

        let Some(allowed) = allowed else {
            return not_trusted();
        };
        let principals = match sshsig::find_principals(bin, &allowed, signature, &verify_time) {
            Ok(principals) => principals,
            Err(e) if unavailable(&e) => return S::VerifierUnavailable,
            Err(_) => return not_trusted(),
        };
        let matched = if principals.contains(&view.principal) {
            view.principal.clone()
        } else if let Some(first) = principals.first() {
            first.clone()
        } else {
            return not_trusted();
        };
        let verified =
            match sshsig::verify(bin, &allowed, &matched, signature, data, &verify_time, None) {
                Ok(verified) => verified,
                Err(e) if unavailable(&e) => return S::VerifierUnavailable,
                Err(_) => return not_trusted(),
            };

        if !self.env.trust.revoked.is_empty() {
            if let Some(revoked) = self
                .env
                .trust_dir
                .as_ref()
                .map(|dir| dir.join(trust::REVOKED_FILE))
            {
                match sshsig::verify(
                    bin,
                    &allowed,
                    &matched,
                    signature,
                    data,
                    &verify_time,
                    Some(&revoked),
                ) {
                    Ok(_) => {}
                    Err(e) if unavailable(&e) => return S::VerifierUnavailable,
                    Err(_) => return S::Revoked,
                }
            }
        }

        S::Verified {
            key_type: self.key_type(&matched, verified.fingerprint.as_deref()),
            matched_principal: matched,
            claimed_principal: view.principal.clone(),
        }
    }

    /// The signing key's type, from the `allowed_signers` line whose
    /// fingerprint matches what ssh-keygen reported.
    fn key_type(&self, principal: &str, fingerprint: Option<&str>) -> String {
        let for_principal = || {
            self.env
                .trust
                .signers
                .iter()
                .filter(move |s| s.principals.split(',').any(|p| p.trim() == principal))
        };
        for_principal()
            .find(|s| fingerprint.is_some() && trust::fingerprint(&s.key).as_deref() == fingerprint)
            .or_else(|| for_principal().next())
            .map_or_else(|| "ssh-ed25519".to_owned(), |s| s.key_type.clone())
    }
}

/// The recorded locator's prefix and suffix, read from the document.
fn locator_of(view: &View) -> Option<(String, String)> {
    let locator = view.document.get("predicate")?.get("locator")?;
    Some((
        locator.get("prefix")?.as_str()?.to_owned(),
        locator.get("suffix")?.as_str()?.to_owned(),
    ))
}

/// The namespace named inside an armored SSHSIG, read only to tell a
/// wrong-namespace forgery from a bad signature in the diagnostics. The
/// verdict itself always comes from `ssh-keygen`.
fn namespace_of(armored: &str) -> Option<String> {
    let body: String = armored
        .lines()
        .filter(|line| !line.starts_with("-----"))
        .collect();
    let blob = crate::provisioning::base64_decode(&body)?;
    let mut rest = blob.strip_prefix(b"SSHSIG".as_slice())?;
    let mut take = |len: usize| -> Option<&[u8]> {
        let (head, tail) = rest.split_at_checked(len)?;
        rest = tail;
        Some(head)
    };
    take(4)?; // version
    let key_len = u32::from_be_bytes(take(4)?.try_into().ok()?) as usize;
    take(key_len)?; // public key
    let ns_len = u32::from_be_bytes(take(4)?.try_into().ok()?) as usize;
    String::from_utf8(take(ns_len)?.to_vec()).ok()
}

/// Counts per verdict for `seal status`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Counts {
    pub citable: usize,
    pub drifted: usize,
    pub stale: usize,
    pub unverifiable: usize,
    pub untrusted: usize,
    pub forged: usize,
    pub retracted: usize,
    /// Seals carrying `seal.disputed`; counted again under their verdict.
    pub disputed: usize,
}

impl Counts {
    pub fn of(reports: &[Report]) -> Counts {
        let mut counts = Counts::default();
        for report in reports {
            match report.outcome.verdict {
                Verdict::Citable => counts.citable += 1,
                Verdict::Drifted => counts.drifted += 1,
                Verdict::Stale => counts.stale += 1,
                Verdict::Unverifiable => counts.unverifiable += 1,
                Verdict::Untrusted => counts.untrusted += 1,
                Verdict::Forged => counts.forged += 1,
                Verdict::Retracted => counts.retracted += 1,
            }
            if report.is_disputed() {
                counts.disputed += 1;
            }
        }
        counts
    }

    pub fn entries(&self) -> [(&'static str, usize); 8] {
        [
            ("citable", self.citable),
            ("drifted", self.drifted),
            ("stale", self.stale),
            ("unverifiable", self.unverifiable),
            ("untrusted", self.untrusted),
            ("forged", self.forged),
            ("retracted", self.retracted),
            ("disputed", self.disputed),
        ]
    }
}
