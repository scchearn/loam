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
    /// The principal the signature verified as, when it did.
    pub signer: Option<String>,
    pub time_verified: Option<String>,
    /// Where the sealed text probably is now, when its markers are gone.
    /// A hint only; never used to validate.
    pub likely_location: Option<String>,
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

fn markdown_files(wiki: &Path) -> Result<Vec<(String, PathBuf)>, String> {
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

fn collect_spans(files: &[(String, PathBuf)]) -> Vec<Span> {
    let mut spans = Vec::new();
    for (name, path) in files {
        let Ok(bytes) = fs::read(path) else { continue };
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

/// Verifies every seal record in the wiki, sorted by id.
pub fn verify_all(env: &Env) -> Result<Vec<Report>, String> {
    let files = markdown_files(&env.wiki)?;
    let spans = collect_spans(&files);
    let records = record::load_all(&env.wiki)?;
    let verifier = Verifier {
        env,
        files,
        spans,
        records,
        cache: RefCell::new(HashMap::new()),
    };
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
    Ok(reports)
}

struct Verifier<'a> {
    env: &'a Env,
    files: Vec<(String, PathBuf)>,
    spans: Vec<Span>,
    records: Vec<Loaded>,
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
            signer: None,
            time_verified: view.map(|v| v.time_verified.clone()),
            likely_location: None,
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

        // Binding.
        let candidates: Vec<&Span> = self
            .spans
            .iter()
            .filter(|s| s.anchor == view.anchor)
            .collect();
        let bound = |s: &&&Span| s.digest.as_deref() == Some(view.subject_sha256.as_str());
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
            Some(span) if span.digest.as_deref() == Some(view.subject_sha256.as_str()) => {
                Binding::Matches
            }
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
                let check = evidence::check(item, &ctx);
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

        let mut outcome = verdict::evaluate(&Inputs {
            well_formed: true,
            binding,
            signature,
            requested,
            evidence: &states,
            redacted: &view.redacted,
            lifecycle: Lifecycle::Active,
            demoted_to: None,
            cosigners: &[],
            time_verified,
            review_by,
            now: self.env.now,
            policy: &self.env.trust.policy,
        });
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
            signer,
            time_verified: Some(view.time_verified.clone()),
            likely_location,
        }
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
        let signature = match fs::metadata(&sig_file) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return (SignatureStanding::Unsigned, None)
            }
            Ok(meta) if meta.len() <= MAX_SIGNATURE_BYTES => fs::read_to_string(&sig_file).ok(),
            _ => None,
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
