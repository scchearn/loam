//! Read-only seal verification (spec D2, D6, D8).
//!
//! Loads every record under `wiki/.seals/records/`, finds each live span by
//! anchor across all pages, binds it through `loam.canon/v1`, checks the
//! SSHSIG with the reader's own `ssh-keygen`, re-resolves the evidence and
//! hands everything to the verdict engine. Nothing here writes under `wiki/`
//! or the trust directory, and nothing ever refuses or reverts a page edit.

use std::collections::BTreeSet;

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
use crate::seal_cache::{self, Ctl, PageEntry};
use chrono::{DateTime, FixedOffset};
use std::cell::{Cell, RefCell};
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
    /// Where the derived seal cache lives (`<configRoot>/cache/seals`);
    /// `None` disables it.
    pub cache_dir: Option<PathBuf>,
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
        let cache_dir = cache_dir_beside(trust_dir.as_deref());
        Ok(Env {
            wiki: wiki.to_path_buf(),
            repo_root,
            trust,
            trust_dir,
            keygen: sshsig::keygen_binary(),
            now,
            cache_dir,
        })
    }

    /// The environment for the cache-only fast path: nothing here spawns a
    /// process. `keygen` is a placeholder the fast path never runs, and the
    /// repository root is found by walking up to a `.git`, not by asking git.
    pub fn load_fast(
        wiki: &Path,
        trust_dir: Option<PathBuf>,
        cache_dir: Option<PathBuf>,
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
        let repo_root = wiki
            .ancestors()
            .find(|dir| dir.join(".git").exists())
            .or_else(|| wiki.parent())
            .unwrap_or(wiki)
            .to_path_buf();
        Ok(Env {
            wiki: wiki.to_path_buf(),
            repo_root,
            trust,
            trust_dir,
            keygen: PathBuf::new(),
            now,
            cache_dir,
        })
    }
}

/// `<configRoot>/cache/seals` for a trust directory of `<configRoot>/seal`.
/// A relative root disables the cache: it would resolve against the working
/// directory, possibly inside `wiki/`.
pub(crate) fn cache_dir_beside(trust_dir: Option<&Path>) -> Option<PathBuf> {
    trust_dir
        .and_then(Path::parent)
        .filter(|root| root.is_absolute())
        .map(|root| root.join("cache").join("seals"))
}

/// Work the fast path declined to do, for `state`'s `diag`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Diag {
    /// Page bytes hashed this run.
    pub hashed_bytes: u64,
    /// Changed pages left unhashed because the budget was spent.
    pub over_budget: u64,
    /// Signature or evidence lookups with no usable cache entry.
    pub no_cache: u64,
}

/// Whether verification reads and writes the derived cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Off,
    /// Recompute everything, then refill the cache.
    Full,
    /// Answer from the cache only; unanswerable seals are `unchecked`.
    Fast,
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
    /// Set only by the fast path: an input (signature, evidence or page
    /// digest) was not in the derived cache, so no verdict was reached.
    pub unchecked: bool,
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
        if let Some(child) = &self.superseded_by {
            // Informational only: the verdict and failure codes are untouched,
            // so a consumer can exclude superseded drift from its counts.
            if let Some((_, Value::Object(codes))) =
                fields.iter_mut().find(|(key, _)| key == "codes")
            {
                if let Some((_, Value::Array(info))) =
                    codes.iter_mut().find(|(key, _)| key == "informational")
                {
                    info.push(text(code::SUPERSEDED_BY_RESEAL));
                }
            }
            fields.push(("superseded_by".to_owned(), text(child)));
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

/// Every seal span in `files`, plus the pages the fast path left unhashed
/// (budget spent), whose spans are therefore unknown.
fn collect_spans(
    files: &[(String, PathBuf)],
    overlay: Option<&Overlay>,
    ctl: Option<&Ctl>,
) -> (Vec<Span>, Vec<String>) {
    let mut spans = Vec::new();
    let mut unhashed = Vec::new();
    for (name, path) in files {
        let replaced = overlay
            .and_then(|o| o.page.as_ref())
            .filter(|(page, _)| page == name)
            .map(|(_, bytes)| bytes.clone());
        // Stamp before reading: a write that lands after the stamp changes
        // the next stamp, never hides behind this one.
        let stamp = ctl.and_then(|_| seal_cache::file_fingerprint(path));
        if let (Some(ctl), Some((fingerprint, _))) = (ctl, &stamp) {
            if ctl.fast {
                if let Some(entry) = ctl.cached_page(name, fingerprint) {
                    for (anchor, digest) in &entry.spans {
                        spans.push(Span {
                            name: name.clone(),
                            anchor: anchor.clone(),
                            digest: digest.clone(),
                        });
                    }
                    ctl.put_page(name, entry);
                    continue;
                }
                let size = fs::metadata(path).map_or(0, |m| m.len());
                if !ctl.spend(size) {
                    unhashed.push(name.clone());
                    continue;
                }
            }
        }
        let Some(bytes) = replaced.or_else(|| fs::read(path).ok()) else {
            continue;
        };
        let Ok(source) = std::str::from_utf8(&bytes) else {
            continue;
        };
        let first = spans.len();
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
        if let (Some(ctl), Some((fingerprint, newest))) = (ctl, stamp) {
            ctl.put_page(
                name,
                PageEntry {
                    stamp: seal_cache::Stamp {
                        fingerprint,
                        newest,
                        checked_at: seal_cache::now_ns(),
                    },
                    spans: spans[first..]
                        .iter()
                        .map(|s| (s.anchor.clone(), s.digest.clone()))
                        .collect(),
                },
            );
        }
    }
    (spans, unhashed)
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

impl Overlay {
    fn is_empty(&self) -> bool {
        self.page.is_none()
            && self.record.is_none()
            && self.signature.is_none()
            && self.captures.is_empty()
    }
}

/// Verifies every seal record in the wiki, sorted by id. Reads no cache and
/// writes none.
pub fn verify_all(env: &Env) -> Result<Vec<Report>, String> {
    verify_with(env, None, Mode::Off).map(|(reports, _)| reports)
}

/// [`verify_all`], then refills the derived cache under `env.cache_dir` with
/// the signature, evidence and page results it computed.
pub fn verify_and_cache(env: &Env) -> Result<Vec<Report>, String> {
    verify_with(env, None, Mode::Full).map(|(reports, _)| reports)
}

/// Verifies from the derived cache alone: no `ssh-keygen`, no `git`, at most
/// [`seal_cache::HASH_BUDGET`] bytes of pages hashed. A seal whose inputs are
/// not all cached comes back with `unchecked` set.
pub fn verify_fast(env: &Env) -> Result<(Vec<Report>, Diag), String> {
    verify_with(env, None, Mode::Fast)
}

/// Verifies the single seal `id` as it would stand once `overlay` is applied.
pub fn verify_overlay(env: &Env, overlay: &Overlay, id: &str) -> Result<Report, String> {
    // An empty overlay is a plain verification of what is on disk (the check
    // after a write), so it refills the cache; a real one is hypothetical
    // and must never be cached.
    let mode = if overlay.is_empty() {
        Mode::Full
    } else {
        Mode::Off
    };
    verify_with(env, Some(overlay), mode)?
        .0
        .into_iter()
        .find(|report| report.id == id)
        .ok_or_else(|| format!("seal {id} did not verify as a seal record"))
}

fn verify_with(
    env: &Env,
    overlay: Option<&Overlay>,
    mode: Mode,
) -> Result<(Vec<Report>, Diag), String> {
    let ctl = (mode != Mode::Off).then(|| {
        Ctl::open(
            env.cache_dir.as_deref(),
            &env.wiki,
            env.trust_dir.as_deref(),
            mode == Mode::Fast,
        )
    });
    let verifier = build_verifier(env, overlay, ctl.as_ref())?;
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
    drop(verifier);
    let diag = ctl.as_ref().map(Ctl::diag).unwrap_or_default();
    if let Some(ctl) = ctl {
        ctl.finish();
    }
    Ok((reports, diag))
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
            && !report.unchecked
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
    let by_id = |id: &str| snapshot.iter().find(|other| other.id == id);
    for report in reports.iter_mut().filter(|report| drifts(report)) {
        // Walk the re-seal chain (each hop a same-subject child naming the
        // previous record as `parent`). A sound node ends its branch: it
        // supersedes the ancestor only if its level is high enough. Drifted
        // or otherwise unsound nodes are passed through, so a chain of
        // re-seals after repeated edits still resolves.
        let mut visited: BTreeSet<&str> = BTreeSet::new();
        visited.insert(report.id.as_str());
        let mut found: Option<String> = None;
        let mut blocked = false;
        for (first, first_parent, subject) in &children {
            if first_parent != &report.id || report.subject.as_ref() != Some(subject) {
                continue;
            }
            let mut stack: Vec<&str> = vec![first.as_str()];
            let mut hit = false;
            while let Some(node) = stack.pop() {
                if !visited.insert(node) {
                    continue;
                }
                let Some(other) = by_id(node) else { continue };
                blocked |= other.unchecked;
                if sound(other) {
                    if other
                        .base_level
                        .is_some_and(|level| level >= report.outcome.requested_level)
                    {
                        hit = true;
                        break;
                    }
                    continue;
                }
                for (next, parent, next_subject) in &children {
                    if parent == node && next_subject == subject {
                        stack.push(next.as_str());
                    }
                }
            }
            if hit {
                found = Some(first.clone());
                break;
            }
        }
        // A re-seal the fast path could not settle might supersede this
        // record, so this one cannot be called drifted either.
        report.unchecked |= found.is_none() && blocked;
        report.superseded_by = found;
    }
}

fn build_verifier<'a>(
    env: &'a Env,
    overlay: Option<&'a Overlay>,
    ctl: Option<&'a Ctl>,
) -> Result<Verifier<'a>, String> {
    let files = markdown_files(&env.wiki)?;
    let (spans, unhashed) = collect_spans(&files, overlay, ctl);
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
        ctl,
        unhashed,
        miss: Cell::new(false),
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
    let verifier = build_verifier(env, None, None)?;
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
    /// The derived cache for this run, when one is in use.
    ctl: Option<&'a Ctl>,
    /// Pages the fast path did not hash; their spans are unknown.
    unhashed: Vec<String>,
    /// Set when an input this seal needs was not in the cache.
    miss: Cell<bool>,
}

impl Verifier<'_> {
    fn report(&self, id: &str, stack: &mut Vec<String>) -> Option<Report> {
        if let Some(found) = self.cache.borrow().get(id) {
            if found.unchecked {
                self.miss.set(true);
            }
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
            unchecked: false,
        }
    }

    /// Runs [`Self::compute_inner`] and records whether any input it (or a
    /// seal it cites) needed was missing from the cache.
    fn compute(&self, loaded: &Loaded, stack: &mut Vec<String>) -> Report {
        let outer = self.miss.replace(false);
        let mut report = self.compute_inner(loaded, stack);
        report.unchecked = self.miss.get();
        self.miss.set(outer || report.unchecked);
        report
    }

    fn compute_inner(&self, loaded: &Loaded, stack: &mut Vec<String>) -> Report {
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
        // A page the fast path left unhashed may hold this span (or the moved
        // copy), so a binding settled without it is not settled.
        if self.ctl.is_some()
            && (self.unhashed.iter().any(|name| same_page(name, &page))
                || (!self.unhashed.is_empty()
                    && (binding == Binding::SpanMissing || (moved && binding != Binding::Matches))))
        {
            self.miss.set(true);
        }
        let likely_location = (binding == Binding::SpanMissing && self.ctl.is_none_or(|c| !c.fast))
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
                let (code, weight) = self.evidence_check(item, &ctx);
                let mut check = evidence::Check {
                    code,
                    weight,
                    detail: String::new(),
                    local_path: None,
                };
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
            unchecked: false,
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
        // Keyed by everything the result depends on: the record, its
        // signature file and the trust files. A change to any is a miss.
        let key = self.ctl.map(|ctl| {
            format!(
                "{}:{}:{}",
                crate::sha256::hex(&loaded.bytes),
                crate::sha256::hex(signature.as_bytes()),
                ctl.trust_digest
            )
        });
        if let (Some(ctl), Some(key)) = (self.ctl, &key) {
            if ctl.fast {
                return match ctl.cached_signature(key) {
                    Some(standing) => Self::with_signer(standing),
                    None => {
                        ctl.note_miss();
                        self.miss.set(true);
                        (SignatureStanding::VerifierUnavailable, None)
                    }
                };
            }
        }
        if sshsig::capability_of(&self.env.keygen) != Capability::Available {
            return (SignatureStanding::VerifierUnavailable, None);
        }
        let standing = self.check_signature(&signature, view, &loaded.bytes);
        if let (Some(ctl), Some(key)) = (self.ctl, &key) {
            ctl.put_signature(key, &standing);
        }
        Self::with_signer(standing)
    }

    /// One evidence item's result code and weight. With a cache, a file,
    /// blob, capture or commit result is reused only while its inputs'
    /// `{mtime, size}` (or the repository's refs) are unchanged; the fast path
    /// recomputes only file and capture digests in process, and reports a seal
    /// `unchecked` otherwise when it must.
    fn evidence_check(
        &self,
        item: &evidence::Item,
        ctx: &Ctx,
    ) -> (evidence::Code, evidence::Weight) {
        use evidence::{Code, Kind};
        let weight = evidence::weight_of(item, ctx);
        let Some(ctl) = self.ctl else {
            return (evidence::check(item, ctx).code, weight);
        };
        // Resolved from files the verifier already reads, never by a process.
        if matches!(item.kind, Kind::Note | Kind::Seal) {
            return (evidence::check_code(item, ctx).0, weight);
        }
        let key = match item.kind {
            Kind::Commit => format!(
                "commit:{}@{}",
                item.reference,
                item.git_ref.as_deref().unwrap_or("")
            ),
            _ => format!(
                "{}:{}:{}",
                item.kind.as_str(),
                item.reference,
                item.git_blob
                    .as_deref()
                    .or(item.sha256.as_deref())
                    .unwrap_or("")
            ),
        };
        let stamp = if item.kind == Kind::Commit {
            seal_cache::refs_fingerprint(&ctx.repo_root)
        } else {
            match evidence::local_input(item, ctx) {
                // An invalid recorded path resolves (to unresolvable) without
                // touching the file system or git.
                None => return (evidence::check_code(item, ctx).0, weight),
                Some(path) if !path.is_file() => {
                    return (evidence::check_code(item, ctx).0, weight)
                }
                Some(path) => seal_cache::file_fingerprint(&path),
            }
        };
        if ctl.fast {
            if let Some((fingerprint, _)) = &stamp {
                if let Some(code) = ctl.cached_evidence(&key, fingerprint) {
                    return (code, weight);
                }
            }
            // File and capture evidence are re-hashed in process (no spawn,
            // charged to the hash budget) when the stamp is racy or changed,
            // so evidence stored moments before a check recovers on its own.
            // Only an entry that exists is revisited: a cold cache stays
            // unchecked.
            if matches!(item.kind, Kind::File | Kind::Capture) && ctl.has_evidence(&key) {
                if let Some((fingerprint, newest)) = stamp {
                    let size = evidence::local_input(item, ctx)
                        .and_then(|path| fs::metadata(path).ok())
                        .map_or(0, |meta| meta.len());
                    if ctl.spend(size) {
                        let code = evidence::check_code(item, ctx).0;
                        ctl.put_evidence(&key, fingerprint, newest, code);
                        return (code, weight);
                    }
                }
            }
            ctl.note_miss();
            self.miss.set(true);
            return (Code::Unresolvable, weight);
        }
        // Stamped before the check, so a change during it is never hidden.
        let code = evidence::check(item, ctx).code;
        if let Some((fingerprint, newest)) = stamp {
            ctl.put_evidence(&key, fingerprint, newest, code);
        }
        (code, weight)
    }

    fn with_signer(standing: SignatureStanding) -> (SignatureStanding, Option<String>) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seal::verdict::Codes;

    #[test]
    fn a_superseded_report_exposes_the_reseal_in_json_without_touching_verdict() {
        let report = Report {
            id: "old".to_owned(),
            subject: Some("alpha#^s-one".to_owned()),
            page: Some("alpha".to_owned()),
            anchor: Some("s-one".to_owned()),
            current_page: Some("alpha".to_owned()),
            outcome: Outcome {
                verdict: Verdict::Drifted,
                effective_level: None,
                requested_level: Level::Evidence,
                codes: Codes {
                    success: vec![],
                    informational: vec![],
                    failure: vec![code::BINDING_MISMATCH],
                },
            },
            base_level: None,
            signer: None,
            time_verified: None,
            likely_location: None,
            superseded_by: Some("child1".to_owned()),
            unchecked: false,
        };
        let value = report.to_value();
        let info = value.get("codes").and_then(|c| c.get("informational"));
        let listed: Vec<&str> = info
            .and_then(Value::as_array)
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert_eq!(listed, vec!["seal.superseded_by_reseal"]);
        assert_eq!(
            value.get("superseded_by").and_then(Value::as_str),
            Some("child1")
        );
        assert_eq!(
            value.get("verdict").and_then(Value::as_str),
            Some("drifted")
        );
    }
}
