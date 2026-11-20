//! Derived seal cache (spec D8: "derived cache only").
//!
//! Lives at `<configRoot>/cache/seals/<sha256(wiki root)>.json`, keyed the way
//! the workspaces store is so two worktrees never collide. It holds three
//! maps and nothing authoritative; deleting it only costs time:
//!
//! - **pages**: per page `{mtime, size}` and the `loam.canon/v1` digest of
//!   every seal span on it, so an unchanged page is never re-hashed;
//! - **signatures**: the outcome of `ssh-keygen`, keyed
//!   `<record sha256>:<signature sha256>:<trust digest>` so any change to the
//!   record, its signature file or the trust files misses;
//! - **evidence**: the code a local file or commit item resolved to, valid
//!   only while the file's `{mtime, size}` (or the repository's refs) are
//!   unchanged.
//!
//! The fast path (`loam state --fast`) answers from this cache alone: it never
//! spawns `ssh-keygen` or `git`, and anything it cannot answer is reported as
//! `unchecked`, never guessed. Full runs (`seal verify`, `seal status`, the
//! post-write check of `attest`/`mark`/`carry`, full `state`) recompute
//! everything and rewrite the cache, so normal use leaves it warm.
//!
//! A stamp is trusted only if the file was already at least [`RACY_NS`] old
//! when the entry was written. A file rewritten within one timestamp tick
//! with the same size would otherwise be indistinguishable from the cached one
//! (FAT keeps two-second granularity), the same "racy" window git guards.

use crate::json::{self, Value};
use crate::seal::evidence::Code;
use crate::seal::policy::{ForgeReason, SignatureStanding};
use crate::seal::trust;
use crate::seal::verify::{self, Diag, Env, Report};
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const SCHEMA: &str = "loam.seal-cache/v1";
/// Bytes of page content the fast path may hash per run.
pub const HASH_BUDGET: u64 = 512 * 1024;
/// A stamp younger than this when written is not trusted (see module docs).
pub const RACY_NS: u128 = 2_000_000_000;
/// A cache file larger than this is treated as corrupt.
const MAX_CACHE_BYTES: u64 = 32 * 1024 * 1024;
/// More ref files than this and the commit fingerprint is declined.
const MAX_REF_FILES: usize = 20_000;

// ------------------------------------------------------------------ stamps

/// What a cached result was computed from, plus when.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stamp {
    /// Opaque; equal fingerprints mean "unchanged".
    pub fingerprint: String,
    /// Newest modification time (ns since the epoch) the fingerprint covers.
    pub newest: u128,
    /// When the entry was written (ns since the epoch).
    pub checked_at: u128,
}

impl Stamp {
    fn trusted(&self, current: &str) -> bool {
        self.fingerprint == current && self.checked_at.saturating_sub(self.newest) >= RACY_NS
    }
}

#[cfg(test)]
thread_local! {
    /// A ticking fake clock for tests: each read returns the next integer.
    static TEST_CLOCK: Cell<Option<u128>> = const { Cell::new(None) };
}

pub fn now_ns() -> u128 {
    #[cfg(test)]
    if let Some(tick) = TEST_CLOCK.with(Cell::get) {
        TEST_CLOCK.with(|c| c.set(Some(tick + 1)));
        return tick;
    }
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos())
}

/// `{mtime}:{size}` and the mtime in ns; `None` when the file cannot be
/// statted or its time predates the epoch.
pub fn file_fingerprint(path: &Path) -> Option<(String, u128)> {
    let meta = fs::metadata(path).ok()?;
    let mtime = meta
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_nanos();
    Some((format!("{mtime}:{}", meta.len()), mtime))
}

/// Largest ref-related file the fingerprint will read; more and it declines.
const MAX_REF_BYTES: u64 = 8 * 1024 * 1024;

/// The content of `path`: `Some(None)` when absent, `None` when it cannot be
/// read or is too large to hash here.
fn ref_bytes(path: &Path) -> Option<Option<Vec<u8>>> {
    match fs::metadata(path) {
        Ok(meta) if meta.is_file() && meta.len() <= MAX_REF_BYTES => fs::read(path).ok().map(Some),
        Ok(_) => None,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(None),
        Err(_) => None,
    }
}

/// A fingerprint of what `allowed_ref` and commit reachability depend on,
/// read without spawning git, so a cached "commit is reachable" dies when the
/// allowed branch moves: `packed-refs`, `shallow`, the reftable table list,
/// `refs/remotes/origin/HEAD` and the ref it points at, `refs/heads/main`,
/// `refs/heads/master` and `refs/replace/*`. By content, not by stamp, so
/// there is no racy window and the second value is always 0. Unrelated refs
/// (other branches, tags, remotes, stash) and `HEAD` do not enter it. `None`
/// when the git directory cannot be understood (the caller then reports
/// `unchecked`).
pub fn refs_fingerprint(repo_root: &Path) -> Option<(String, u128)> {
    let dot_git = repo_root.join(".git");
    let commondir = if dot_git.is_dir() {
        dot_git
    } else {
        let text = fs::read_to_string(&dot_git).ok()?;
        let target = text.trim().strip_prefix("gitdir:")?.trim();
        let gitdir = repo_root.join(target);
        match fs::read_to_string(gitdir.join("commondir")) {
            Ok(rel) => gitdir.join(rel.trim()),
            Err(_) => gitdir,
        }
    };
    let mut parts: Vec<(String, String)> = Vec::new();
    let mut add = |label: String, path: &Path| -> Option<Option<Vec<u8>>> {
        let bytes = ref_bytes(path)?;
        let digest = bytes
            .as_deref()
            .map_or_else(|| "absent".to_owned(), crate::sha256::hex);
        parts.push((label, digest));
        Some(bytes)
    };
    add("packed-refs".to_owned(), &commondir.join("packed-refs"))?;
    add("shallow".to_owned(), &commondir.join("shallow"))?;
    // Reftable repositories keep refs here (`refs/heads` is a stub), and the
    // list is rewritten on every ref update.
    add(
        "reftable".to_owned(),
        &commondir.join("reftable").join("tables.list"),
    )?;
    add("main".to_owned(), &commondir.join("refs/heads/main"))?;
    add("master".to_owned(), &commondir.join("refs/heads/master"))?;
    let origin = add(
        "origin-head".to_owned(),
        &commondir.join("refs/remotes/origin/HEAD"),
    )?;
    if let Some(text) = origin.and_then(|b| String::from_utf8(b).ok()) {
        if let Some(target) = text.trim().strip_prefix("ref:").map(str::trim) {
            let safe = target.starts_with("refs/")
                && !target.contains("..")
                && !target.contains('\\')
                && !target.contains(':');
            if !safe {
                return None;
            }
            add(format!("origin-target={target}"), &commondir.join(target))?;
        }
    }
    let mut files = 0usize;
    if let Ok(entries) = fs::read_dir(commondir.join("refs").join("replace")) {
        for entry in entries.flatten() {
            files += 1;
            if files > MAX_REF_FILES {
                return None;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            add(format!("replace/{name}"), &entry.path())?;
        }
    }
    parts.sort();
    let mut text = String::new();
    for (label, digest) in &parts {
        text.push_str(label);
        text.push('=');
        text.push_str(digest);
        text.push('\n');
    }
    Some((crate::sha256::hex(text.as_bytes()), 0))
}

/// sha256 over the three trust files: for each, its name, byte length and
/// bytes (an absent file counts as empty). The length prefix keeps bytes
/// moved from one file to the next from cancelling out.
pub fn trust_digest(dir: Option<&Path>) -> String {
    let read = |name: &str| {
        dir.and_then(|d| fs::read(d.join(name)).ok())
            .unwrap_or_default()
    };
    let allowed = read(trust::ALLOWED_FILE);
    let revoked = read(trust::REVOKED_FILE);
    let policy = read(trust::POLICY_FILE);
    digest_of([&allowed[..], &revoked[..], &policy[..]])
}

/// The digest of the three trust files' bytes, in `allowed_signers`,
/// `revoked`, `policy.json` order.
pub fn digest_of(files: [&[u8]; 3]) -> String {
    let mut data = Vec::new();
    for (name, bytes) in [trust::ALLOWED_FILE, trust::REVOKED_FILE, trust::POLICY_FILE]
        .into_iter()
        .zip(files)
    {
        data.extend_from_slice(name.as_bytes());
        data.push(0);
        data.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
        data.extend_from_slice(bytes);
    }
    crate::sha256::hex(&data)
}

// ------------------------------------------------------------------- model

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageEntry {
    pub stamp: Stamp,
    /// `(anchor, canon digest)`; `None` when the span is not valid UTF-8.
    pub spans: Vec<(String, Option<String>)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceEntry {
    pub stamp: Stamp,
    pub code: Code,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SealCache {
    pub pages: BTreeMap<String, PageEntry>,
    pub signatures: BTreeMap<String, SignatureStanding>,
    pub evidence: BTreeMap<String, EvidenceEntry>,
}

fn text(s: &str) -> Value {
    Value::String(s.to_owned())
}

fn num(n: u128) -> Value {
    Value::Number(n.to_string())
}

fn stamp_fields(stamp: &Stamp) -> Vec<(String, Value)> {
    vec![
        ("fingerprint".to_owned(), text(&stamp.fingerprint)),
        ("newest".to_owned(), num(stamp.newest)),
        ("checked_at".to_owned(), num(stamp.checked_at)),
    ]
}

fn read_stamp(value: &Value) -> Option<Stamp> {
    let number = |key: &str| match value.get(key)? {
        Value::Number(n) => n.parse::<u128>().ok(),
        _ => None,
    };
    Some(Stamp {
        fingerprint: value.get("fingerprint")?.as_str()?.to_owned(),
        newest: number("newest")?,
        checked_at: number("checked_at")?,
    })
}

fn code_name(code: Code) -> &'static str {
    match code {
        Code::Current => "current",
        Code::Drifted => "drifted",
        Code::Unresolvable => "unresolvable",
        Code::Orphaned => "orphaned",
    }
}

fn code_from(name: &str) -> Option<Code> {
    Some(match name {
        "current" => Code::Current,
        "drifted" => Code::Drifted,
        "unresolvable" => Code::Unresolvable,
        "orphaned" => Code::Orphaned,
        _ => return None,
    })
}

/// `None` for a standing that must never be cached (the check could not be
/// made, so a later run on a capable machine must try again).
fn standing_value(standing: &SignatureStanding) -> Option<Value> {
    let fields = |kind: &str| vec![("standing".to_owned(), text(kind))];
    Some(Value::Object(match standing {
        SignatureStanding::Unsigned => fields("unsigned"),
        SignatureStanding::Untrusted => fields("untrusted"),
        SignatureStanding::Revoked => fields("revoked"),
        SignatureStanding::Forged(ForgeReason::BadSignature) => fields("forged_bad_signature"),
        SignatureStanding::Forged(ForgeReason::WrongNamespace) => fields("forged_wrong_namespace"),
        SignatureStanding::Verified {
            matched_principal,
            claimed_principal,
            key_type,
        } => {
            let mut out = fields("verified");
            out.push(("matched".to_owned(), text(matched_principal)));
            out.push(("claimed".to_owned(), text(claimed_principal)));
            out.push(("key_type".to_owned(), text(key_type)));
            out
        }
        SignatureStanding::VerifierUnavailable => return None,
    }))
}

fn read_standing(value: &Value) -> Option<SignatureStanding> {
    let field = |key: &str| value.get(key).and_then(Value::as_str).map(str::to_owned);
    Some(match value.get("standing")?.as_str()? {
        "unsigned" => SignatureStanding::Unsigned,
        "untrusted" => SignatureStanding::Untrusted,
        "revoked" => SignatureStanding::Revoked,
        "forged_bad_signature" => SignatureStanding::Forged(ForgeReason::BadSignature),
        "forged_wrong_namespace" => SignatureStanding::Forged(ForgeReason::WrongNamespace),
        "verified" => SignatureStanding::Verified {
            matched_principal: field("matched")?,
            claimed_principal: field("claimed")?,
            key_type: field("key_type")?,
        },
        _ => return None,
    })
}

impl SealCache {
    pub fn to_json(&self) -> String {
        let pages = self
            .pages
            .iter()
            .map(|(name, entry)| {
                let mut fields = stamp_fields(&entry.stamp);
                fields.push((
                    "spans".to_owned(),
                    Value::Array(
                        entry
                            .spans
                            .iter()
                            .map(|(anchor, digest)| {
                                Value::Array(vec![
                                    text(anchor),
                                    digest.as_deref().map_or(Value::Null, text),
                                ])
                            })
                            .collect(),
                    ),
                ));
                (name.clone(), Value::Object(fields))
            })
            .collect();
        let signatures = self
            .signatures
            .iter()
            .filter_map(|(key, standing)| Some((key.clone(), standing_value(standing)?)))
            .collect();
        let evidence = self
            .evidence
            .iter()
            .map(|(key, entry)| {
                let mut fields = stamp_fields(&entry.stamp);
                fields.push(("code".to_owned(), text(code_name(entry.code))));
                (key.clone(), Value::Object(fields))
            })
            .collect();
        Value::Object(vec![
            ("schema".to_owned(), text(SCHEMA)),
            ("pages".to_owned(), Value::Object(pages)),
            ("signatures".to_owned(), Value::Object(signatures)),
            ("evidence".to_owned(), Value::Object(evidence)),
        ])
        .to_json()
    }

    /// Anything unreadable, unknown or of another schema is an empty cache.
    pub fn parse(input: &str) -> SealCache {
        Self::try_parse(input).unwrap_or_default()
    }

    fn try_parse(input: &str) -> Option<SealCache> {
        let root = json::parse(input).ok()?;
        if root.get("schema")?.as_str()? != SCHEMA {
            return None;
        }
        let entries = |key: &str| match root.get(key) {
            Some(Value::Object(entries)) => Some(entries.clone()),
            _ => None,
        };
        let mut cache = SealCache::default();
        for (name, value) in entries("pages")? {
            let Some(stamp) = read_stamp(&value) else {
                continue;
            };
            let spans = value
                .get("spans")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .map(|pair| {
                            let anchor = pair.at(0)?.as_str()?.to_owned();
                            let digest = match pair.at(1)? {
                                Value::Null => None,
                                other => Some(other.as_str()?.to_owned()),
                            };
                            Some((anchor, digest))
                        })
                        .collect::<Option<Vec<_>>>()
                })
                .unwrap_or(None);
            if let Some(spans) = spans {
                cache.pages.insert(name, PageEntry { stamp, spans });
            }
        }
        for (key, value) in entries("signatures")? {
            if let Some(standing) = read_standing(&value) {
                cache.signatures.insert(key, standing);
            }
        }
        for (key, value) in entries("evidence")? {
            let code = value
                .get("code")
                .and_then(Value::as_str)
                .and_then(code_from);
            if let (Some(stamp), Some(code)) = (read_stamp(&value), code) {
                cache.evidence.insert(key, EvidenceEntry { stamp, code });
            }
        }
        Some(cache)
    }
}

// -------------------------------------------------------------------- store

/// `<cache_dir>/<sha256(wiki root)>.json`. The root is canonicalised (and
/// reduced to the form every tool accepts) so spellings of one wiki share a
/// file.
pub fn cache_file(cache_dir: &Path, wiki: &Path) -> PathBuf {
    let canonical = fs::canonicalize(wiki)
        .map(crate::enrollment::dos_compatible)
        .unwrap_or_else(|_| wiki.to_path_buf());
    cache_dir.join(format!("{}.json", crate::workspaces::key(&canonical)))
}

/// `<configRoot>/cache/seals`, refusing a relative or unresolved root as the
/// workspaces store does.
pub fn default_cache_dir() -> Option<PathBuf> {
    let root = crate::provisioning::configured_config_root()?;
    root.is_absolute().then(|| root.join("cache").join("seals"))
}

/// Loads the cache at `path`; missing, oversized or corrupt means empty.
pub fn load(path: &Path) -> SealCache {
    match fs::metadata(path) {
        Ok(meta) if meta.is_file() && meta.len() <= MAX_CACHE_BYTES => fs::read_to_string(path)
            .map(|text| SealCache::parse(&text))
            .unwrap_or_default(),
        _ => SealCache::default(),
    }
}

/// Atomic write: a temp file in the same directory, then a rename.
pub fn save(path: &Path, cache: &SealCache) -> std::io::Result<()> {
    let dir = path.parent().ok_or(std::io::ErrorKind::InvalidInput)?;
    fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(
        ".{}.{}.{:x}.tmp",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("cache"),
        std::process::id(),
        now_ns()
    ));
    let written = fs::write(&tmp, cache.to_json()).and_then(|()| fs::rename(&tmp, path));
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written
}

// ---------------------------------------------------------------------- ctl

/// One verification run's view of the cache.
///
/// Full mode ignores what is cached, recomputes everything, records the fresh
/// results and rewrites the file. Fast mode only reads (and refreshes page
/// digests within the hash budget).
pub struct Ctl {
    pub fast: bool,
    /// Digest of the trust bytes the verdicts were computed from.
    pub trust_digest: String,
    trust_dir: Option<PathBuf>,
    path: Option<PathBuf>,
    old: SealCache,
    new: RefCell<SealCache>,
    hashed_bytes: Cell<u64>,
    over_budget: Cell<u64>,
    no_cache: Cell<u64>,
}

impl Ctl {
    pub fn open(
        cache_dir: Option<&Path>,
        wiki: &Path,
        trust_dir: Option<&Path>,
        trust_digest: &str,
        fast: bool,
    ) -> Ctl {
        let path = cache_dir.map(|dir| cache_file(dir, wiki));
        let old = path.as_deref().map(load).unwrap_or_default();
        let mut new = SealCache::default();
        if fast {
            new.signatures = old.signatures.clone();
            new.evidence = old.evidence.clone();
        }
        Ctl {
            fast,
            trust_digest: trust_digest.to_owned(),
            trust_dir: trust_dir.map(Path::to_path_buf),
            path,
            old,
            new: RefCell::new(new),
            hashed_bytes: Cell::new(0),
            over_budget: Cell::new(0),
            no_cache: Cell::new(0),
        }
    }

    pub fn diag(&self) -> Diag {
        Diag {
            hashed_bytes: self.hashed_bytes.get(),
            over_budget: self.over_budget.get(),
            no_cache: self.no_cache.get(),
        }
    }

    pub fn note_miss(&self) {
        self.no_cache.set(self.no_cache.get() + 1);
    }

    /// Reserves `bytes` of the hash budget; `false` (and counted) when spent.
    pub fn spend(&self, bytes: u64) -> bool {
        let used = self.hashed_bytes.get();
        if used.saturating_add(bytes) > HASH_BUDGET {
            self.over_budget.set(self.over_budget.get() + 1);
            return false;
        }
        self.hashed_bytes.set(used + bytes);
        true
    }

    pub fn cached_page(&self, name: &str, fingerprint: &str) -> Option<PageEntry> {
        let entry = self.old.pages.get(name)?;
        entry.stamp.trusted(fingerprint).then(|| entry.clone())
    }

    pub fn put_page(&self, name: &str, entry: PageEntry) {
        self.new.borrow_mut().pages.insert(name.to_owned(), entry);
    }

    pub fn cached_signature(&self, key: &str) -> Option<SignatureStanding> {
        self.old.signatures.get(key).cloned()
    }

    pub fn put_signature(&self, key: &str, standing: &SignatureStanding) {
        if standing_value(standing).is_some() {
            self.new
                .borrow_mut()
                .signatures
                .insert(key.to_owned(), standing.clone());
        }
    }

    pub fn cached_evidence(&self, key: &str, fingerprint: &str) -> Option<Code> {
        let entry = self.old.evidence.get(key)?;
        entry.stamp.trusted(fingerprint).then_some(entry.code)
    }

    pub fn has_evidence(&self, key: &str) -> bool {
        self.old.evidence.contains_key(key)
    }

    /// Records `code` under `stamp`, which must have been taken (with its
    /// `checked_at` read from the clock) before the input was read.
    pub fn put_evidence(&self, key: &str, stamp: Stamp, code: Code) {
        self.new
            .borrow_mut()
            .evidence
            .insert(key.to_owned(), EvidenceEntry { stamp, code });
    }

    /// Writes the cache back. A fast run that learned nothing writes nothing.
    /// Failures are ignored: the cache is derived and best-effort.
    pub fn finish(self) {
        let Some(path) = &self.path else { return };
        let mut new = self.new.into_inner();
        // The trust files are read again by `ssh-keygen` and may have moved
        // since they were digested. If they did, none of this run's
        // signature results is known to describe the trust now on disk.
        if !self.fast && trust_digest(self.trust_dir.as_deref()) != self.trust_digest {
            new.signatures.clear();
        }
        if self.fast && new == self.old {
            return;
        }
        let _ = save(path, &new);
    }
}

// ------------------------------------------------------------------- counts

/// The `seals` object of `loam state`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SealCounts {
    pub citable: usize,
    pub drifted: usize,
    pub stale: usize,
    pub unverifiable: usize,
    pub untrusted: usize,
    pub forged: usize,
    pub retracted: usize,
    pub disputed: usize,
    pub unchecked: usize,
    pub diag: Diag,
}

impl SealCounts {
    /// Counts `reports`. A seal that a later re-seal replaces is history, not
    /// a defect, and is left out; one the fast path could not settle is
    /// `unchecked` and in no verdict bucket.
    pub fn of(reports: &[Report], diag: Diag) -> SealCounts {
        use crate::seal::verdict::Verdict;
        let mut counts = SealCounts {
            diag,
            ..SealCounts::default()
        };
        for report in reports.iter().filter(|r| r.superseded_by.is_none()) {
            if report.unchecked {
                counts.unchecked += 1;
                continue;
            }
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

    pub fn to_json(&self) -> String {
        let n = |v: usize| Value::Number(v.to_string());
        let d = |v: u64| Value::Number(v.to_string());
        Value::Object(vec![
            ("citable".to_owned(), n(self.citable)),
            ("drifted".to_owned(), n(self.drifted)),
            ("stale".to_owned(), n(self.stale)),
            ("unverifiable".to_owned(), n(self.unverifiable)),
            ("untrusted".to_owned(), n(self.untrusted)),
            ("forged".to_owned(), n(self.forged)),
            ("retracted".to_owned(), n(self.retracted)),
            ("disputed".to_owned(), n(self.disputed)),
            ("unchecked".to_owned(), n(self.unchecked)),
            (
                "diag".to_owned(),
                Value::Object(vec![
                    ("hashed_bytes".to_owned(), d(self.diag.hashed_bytes)),
                    ("over_budget".to_owned(), d(self.diag.over_budget)),
                    ("no_cache".to_owned(), d(self.diag.no_cache)),
                ]),
            ),
        ])
        .to_json()
    }
}

fn record_ids(wiki: &Path) -> usize {
    fs::read_dir(wiki.join(".seals").join("records")).map_or(0, |entries| {
        entries
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
            .count()
    })
}

/// Seal counts for `wiki`, using the configured trust and cache directories.
/// See [`seal_counts_with`].
pub fn seal_counts(wiki: &Path, fast: bool) -> SealCounts {
    let utc = chrono::FixedOffset::east_opt(0).expect("zero offset is valid");
    seal_counts_with(
        wiki,
        fast,
        trust::seal_dir().ok(),
        default_cache_dir(),
        chrono::Utc::now().with_timezone(&utc),
    )
}

/// Seal counts for `wiki`. `fast` answers from the cache alone (no
/// subprocess, bounded hashing); otherwise everything is verified and the
/// cache refilled. Never writes under `wiki/`, and never fails: whatever
/// cannot be settled is `unchecked`.
pub fn seal_counts_with(
    wiki: &Path,
    fast: bool,
    trust_dir: Option<PathBuf>,
    cache_dir: Option<PathBuf>,
    now: chrono::DateTime<chrono::FixedOffset>,
) -> SealCounts {
    let records = record_ids(wiki);
    if records == 0 {
        return SealCounts::default();
    }
    let unsettled = || SealCounts {
        unchecked: records,
        ..SealCounts::default()
    };
    let loaded = if fast {
        Env::load_fast(wiki, trust_dir, cache_dir, now)
    } else {
        Env::load(wiki, trust_dir, now).map(|mut env| {
            env.cache_dir = cache_dir;
            env
        })
    };
    let Ok(env) = loaded else {
        return unsettled();
    };
    let result = if fast {
        verify::verify_fast(&env)
    } else {
        verify::verify_and_cache(&env).map(|reports| (reports, Diag::default()))
    };
    match result {
        Ok((reports, diag)) => SealCounts::of(&reports, diag),
        Err(_) => unsettled(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stamp(fp: &str, newest: u128, checked_at: u128) -> Stamp {
        Stamp {
            fingerprint: fp.to_owned(),
            newest,
            checked_at,
        }
    }

    #[test]
    fn cache_round_trips_through_its_own_parser() {
        let mut cache = SealCache::default();
        cache.pages.insert(
            "a/b".to_owned(),
            PageEntry {
                stamp: stamp("1:2", 1, 9_000_000_000),
                spans: vec![
                    ("s-first-claim".to_owned(), Some("ab".repeat(32))),
                    ("s-second-claim".to_owned(), None),
                ],
            },
        );
        for standing in [
            SignatureStanding::Unsigned,
            SignatureStanding::Untrusted,
            SignatureStanding::Revoked,
            SignatureStanding::Forged(ForgeReason::BadSignature),
            SignatureStanding::Forged(ForgeReason::WrongNamespace),
            SignatureStanding::Verified {
                matched_principal: "me".to_owned(),
                claimed_principal: "me".to_owned(),
                key_type: "ssh-ed25519".to_owned(),
            },
        ] {
            let key = format!("k{}", cache.signatures.len());
            cache.signatures.insert(key, standing);
        }
        cache.evidence.insert(
            "file:x:y".to_owned(),
            EvidenceEntry {
                stamp: stamp("f", 5, 6),
                code: Code::Orphaned,
            },
        );
        assert_eq!(SealCache::parse(&cache.to_json()), cache);
    }

    #[test]
    fn an_unavailable_verifier_is_never_cached() {
        let mut cache = SealCache::default();
        cache
            .signatures
            .insert("k".to_owned(), SignatureStanding::VerifierUnavailable);
        assert!(SealCache::parse(&cache.to_json()).signatures.is_empty());
    }

    #[test]
    fn corrupt_or_foreign_input_is_an_empty_cache() {
        assert_eq!(SealCache::parse("not json"), SealCache::default());
        assert_eq!(
            SealCache::parse(r#"{"schema":"other","pages":{}}"#),
            SealCache::default()
        );
        assert_eq!(SealCache::parse(""), SealCache::default());
    }

    #[test]
    fn a_racy_or_changed_stamp_is_not_trusted() {
        let old = stamp("1:2", 100, 100 + RACY_NS);
        assert!(old.trusted("1:2"));
        assert!(!old.trusted("1:3"));
        let racy = stamp("1:2", 100, 100 + RACY_NS - 1);
        assert!(!racy.trusted("1:2"));
    }

    #[test]
    fn trust_digest_depends_on_every_file_and_on_where_bytes_sit() {
        let dir = std::env::temp_dir().join(format!("loam-trust-digest-{}", now_ns()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let empty = trust_digest(Some(&dir));
        assert_eq!(empty, trust_digest(None));
        fs::write(dir.join(trust::ALLOWED_FILE), "ab").unwrap();
        let a = trust_digest(Some(&dir));
        assert_ne!(a, empty);
        fs::remove_file(dir.join(trust::ALLOWED_FILE)).unwrap();
        fs::write(dir.join(trust::REVOKED_FILE), "ab").unwrap();
        assert_ne!(trust_digest(Some(&dir)), a);
        for _ in 0..10 {
            if fs::remove_dir_all(&dir).is_ok() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    #[test]
    fn save_is_atomic_and_load_reads_it_back() {
        let dir = std::env::temp_dir().join(format!("loam-cache-save-{}", now_ns()));
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("nested").join("c.json");
        let mut cache = SealCache::default();
        cache
            .signatures
            .insert("k".to_owned(), SignatureStanding::Untrusted);
        save(&path, &cache).unwrap();
        assert_eq!(load(&path), cache);
        let leftovers: Vec<_> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty());
        fs::write(&path, "{").unwrap();
        assert_eq!(load(&path), SealCache::default());
        for _ in 0..10 {
            if fs::remove_dir_all(&dir).is_ok() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    // --------------------------------------------------- cache-backed counts

    use super::fixture::{self, Fx, NOW};
    use crate::seal::{spawn_count, verify};

    fn fast(fx: &Fx) -> SealCounts {
        fx.counts(true)
    }

    #[test]
    fn a_wiki_with_no_seals_reports_all_zeros_and_writes_no_cache() {
        let fx = Fx::new("zero");
        fx.write_page("alpha", "s-first-claim", "Plain text.");
        fx.age();
        assert_eq!(fx.counts(true), SealCounts::default());
        assert_eq!(fx.counts(false), SealCounts::default());
        assert!(!fx.cache_dir.exists());
    }

    #[test]
    fn a_cold_fast_run_with_no_cache_is_unchecked_not_guessed() {
        let fx = Fx::new("cold");
        fx.seal("alpha", "s-first-claim", "Body.", &["file:run.log"], None);
        fx.age();
        let counts = fast(&fx);
        assert_eq!((counts.citable, counts.unchecked), (0, 1), "{counts:?}");
        assert!(counts.diag.no_cache > 0);
    }

    #[test]
    fn after_a_full_verify_a_cold_fast_run_is_citable_and_spawns_nothing() {
        let fx = Fx::new("warm");
        let signer = fx.signer();
        fx.seal(
            "alpha",
            "s-first-claim",
            "Body.",
            &["file:run.log", "capture:run.log"],
            signer.as_ref(),
        );
        fx.age();
        let full = fx.full();
        assert_eq!(full.len(), 1);
        assert!(full[0].is_citable(), "{:?}", full[0]);
        assert!(fx.cache_file().is_file(), "a full run fills the cache");

        let before = spawn_count();
        let counts = fast(&fx);
        assert_eq!(spawn_count(), before, "the fast path started a process");
        assert_eq!((counts.citable, counts.unchecked), (1, 0), "{counts:?}");
        assert_eq!(
            counts.diag.hashed_bytes, 0,
            "unchanged pages are not re-hashed"
        );
        assert_eq!(counts.diag.no_cache, 0);

        // Warm again: identical.
        assert_eq!(fast(&fx), counts);
    }

    #[test]
    fn seal_verify_status_and_the_post_write_check_all_fill_the_cache() {
        let fx = Fx::new("fills");
        fx.seal("alpha", "s-first-claim", "Body.", &["file:run.log"], None);
        fx.age();
        let env = fx.env();
        verify::verify_overlay(&env, &verify::Overlay::default(), &fx.only_id()).unwrap();
        assert!(fx.cache_file().is_file());
        let counts = fast(&fx);
        assert_eq!((counts.citable, counts.unchecked), (1, 0), "{counts:?}");
    }

    #[test]
    fn a_hypothetical_overlay_never_fills_the_cache() {
        let fx = Fx::new("overlay");
        fx.seal("alpha", "s-first-claim", "Body.", &["file:run.log"], None);
        fx.age();
        let overlay = verify::Overlay {
            page: Some(("alpha".to_owned(), b"unrelated".to_vec())),
            ..verify::Overlay::default()
        };
        let _ = verify::verify_overlay(&fx.env(), &overlay, &fx.only_id());
        assert!(!fx.cache_file().exists());
    }

    #[test]
    fn full_runs_write_only_under_the_cache_directory() {
        let fx = Fx::new("scope");
        let signer = fx.signer();
        fx.seal(
            "alpha",
            "s-first-claim",
            "Body.",
            &["file:run.log"],
            signer.as_ref(),
        );
        fx.age();
        let before = fx.snapshot(&[&fx.wiki, &fx.trust_dir]);
        fx.full();
        fx.counts(true);
        fx.counts(false);
        assert_eq!(fx.snapshot(&[&fx.wiki, &fx.trust_dir]), before);
    }

    #[test]
    fn changing_a_trust_file_invalidates_cached_signatures() {
        let fx = Fx::new("trust");
        let Some(signer) = fx.signer() else { return };
        fx.seal(
            "alpha",
            "s-first-claim",
            "Body.",
            &["file:run.log"],
            Some(&signer),
        );
        fx.age();
        fx.full();
        assert_eq!(fast(&fx).citable, 1);
        let allowed = fx.trust_dir.join(trust::ALLOWED_FILE);
        let mut text = fs::read_to_string(&allowed).unwrap();
        text.push_str("# reviewed\n");
        fs::write(&allowed, text).unwrap();
        let counts = fast(&fx);
        assert_eq!((counts.citable, counts.unchecked), (0, 1), "{counts:?}");
        // A different policy invalidates too.
        fx.full();
        assert_eq!(fast(&fx).citable, 1);
        fs::write(
            fx.trust_dir.join(trust::POLICY_FILE),
            trust::Policy {
                max_review: "7300d".to_owned(),
                ..trust::Policy::default()
            }
            .to_json()
            .to_json(),
        )
        .unwrap();
        assert_eq!(fast(&fx).unchecked, 1);
    }

    #[test]
    fn a_replaced_signature_or_record_is_never_a_stale_citable() {
        let fx = Fx::new("tamper");
        let Some(signer) = fx.signer() else { return };
        let id = fx.seal(
            "alpha",
            "s-first-claim",
            "Body.",
            &["file:run.log"],
            Some(&signer),
        );
        fx.age();
        fx.full();
        assert_eq!(fast(&fx).citable, 1);

        let sig = crate::seal::record::sig_path(&fx.wiki, &id);
        let good = fs::read(&sig).unwrap();
        fs::write(
            &sig,
            b"-----BEGIN SSH SIGNATURE-----\nAAAA\n-----END SSH SIGNATURE-----\n",
        )
        .unwrap();
        let counts = fast(&fx);
        assert_eq!((counts.citable, counts.unchecked), (0, 1), "{counts:?}");
        let full = fx.counts(false);
        assert_eq!((full.citable, full.forged), (0, 1), "{full:?}");
        fs::write(&sig, good).unwrap();

        let record = crate::seal::record::record_path(&fx.wiki, &id);
        let mut bytes = fs::read(&record).unwrap();
        bytes.push(b'\n');
        fs::write(&record, bytes).unwrap();
        let counts = fast(&fx);
        assert_eq!(counts.citable, 0, "{counts:?}");
    }

    #[test]
    fn a_changed_evidence_file_is_rehashed_in_process_without_a_spawn() {
        let fx = Fx::new("evidence");
        fx.seal("alpha", "s-first-claim", "Body.", &["file:run.log"], None);
        fx.age();
        fx.full();
        assert_eq!(fast(&fx).citable, 1);

        // Same bytes, newer mtime: re-hashed in process, still current.
        let log = fx.root.join("run.log");
        let same = fs::read(&log).unwrap();
        fs::write(&log, &same).unwrap();
        let before = spawn_count();
        let counts = fast(&fx);
        assert_eq!(spawn_count(), before);
        assert_eq!((counts.citable, counts.unchecked), (1, 0), "{counts:?}");
        assert!(counts.diag.hashed_bytes > 0, "{counts:?}");

        // Different bytes: never a stale citable.
        fx.age();
        fx.full();
        assert_eq!(fast(&fx).citable, 1);
        fs::write(&log, b"changed and longer than before").unwrap();
        let before = spawn_count();
        let counts = fast(&fx);
        assert_eq!(spawn_count(), before);
        assert_eq!(counts.citable, 0, "{counts:?}");
        let full = fx.counts(false);
        assert_eq!(full.citable, 0, "{full:?}");
    }

    #[test]
    fn evidence_stored_just_before_the_check_recovers_on_the_fast_path() {
        // Nothing is aged: the file evidence and the capture are younger than
        // the racy window when the post-write check stamps them.
        let fx = Fx::new("racy-evidence");
        fx.seal(
            "alpha",
            "s-first-claim",
            "Body.",
            &["file:run.log", "capture:run.log"],
            None,
        );
        let env = fx.env();
        verify::verify_overlay(&env, &verify::Overlay::default(), &fx.only_id()).unwrap();
        let before = spawn_count();
        let counts = fast(&fx);
        assert_eq!(spawn_count(), before);
        assert_eq!((counts.citable, counts.unchecked), (1, 0), "{counts:?}");
    }

    #[test]
    fn a_relative_cache_root_disables_the_cache() {
        let rel = Path::new("cfg").join("seal");
        assert!(verify::cache_dir_beside(Some(&rel)).is_none());
        let abs = std::env::temp_dir().join("cfg").join("seal");
        assert_eq!(
            verify::cache_dir_beside(Some(&abs)),
            Some(std::env::temp_dir().join("cfg").join("cache").join("seals"))
        );
        assert!(verify::cache_dir_beside(None).is_none());
    }

    #[test]
    fn an_edited_span_is_drifted_on_the_fast_path_not_a_stale_citable() {
        let fx = Fx::new("page");
        fx.seal("alpha", "s-first-claim", "Body.", &["file:run.log"], None);
        fx.age();
        fx.full();
        fx.write_page("alpha", "s-first-claim", "Body, quietly edited.");
        let counts = fast(&fx);
        assert_eq!(
            (counts.citable, counts.drifted, counts.unchecked),
            (0, 1, 0),
            "{counts:?}"
        );
        assert!(counts.diag.hashed_bytes > 0);
    }

    #[test]
    fn a_page_written_within_the_racy_window_is_rehashed() {
        let fx = Fx::new("racy");
        fx.seal("alpha", "s-first-claim", "Body.", &["file:run.log"], None);
        fx.age();
        // Rewritten now, so its mtime is within the window when cached.
        fx.write_page_unaged("alpha", "s-first-claim", "Body.");
        fx.full();
        let counts = fast(&fx);
        assert_eq!((counts.citable, counts.unchecked), (1, 0), "{counts:?}");
        assert!(counts.diag.hashed_bytes > 0, "{counts:?}");
    }

    #[test]
    fn changed_pages_over_the_budget_are_unchecked_and_bounded() {
        let fx = Fx::new("budget");
        let big = "word ".repeat(60_000);
        fx.seal("alpha", "s-first-claim", &big, &["file:run.log"], None);
        fx.seal("beta", "s-second-claim", &big, &["file:run.log"], None);
        fx.age();
        fx.full();
        assert_eq!(fast(&fx).citable, 2);
        let edited = format!("{big}more");
        fx.write_page("alpha", "s-first-claim", &edited);
        fx.write_page("beta", "s-second-claim", &edited);
        let counts = fast(&fx);
        assert!(counts.diag.hashed_bytes <= HASH_BUDGET, "{counts:?}");
        assert!(counts.diag.over_budget > 0, "{counts:?}");
        assert_eq!(counts.unchecked, 1, "{counts:?}");
        assert_eq!(counts.drifted, 1, "{counts:?}");
        assert_eq!(counts.citable, 0);
    }

    #[test]
    fn a_corrupt_cache_is_treated_as_empty_and_repaired_by_a_full_run() {
        let fx = Fx::new("corrupt");
        fx.seal("alpha", "s-first-claim", "Body.", &["file:run.log"], None);
        fx.age();
        fx.full();
        fs::write(fx.cache_file(), "{ not json").unwrap();
        assert_eq!(fast(&fx).unchecked, 1);
        fx.full();
        assert_eq!(fast(&fx).citable, 1);
    }

    #[test]
    fn a_superseded_drifted_record_is_left_out_of_the_counts() {
        let fx = Fx::new("superseded");
        let old = fx.seal("alpha", "s-first-claim", "Body.", &["file:run.log"], None);
        fx.age();
        fx.write_page("alpha", "s-first-claim", "Body, revised.");
        fx.reseal(
            "alpha",
            "s-first-claim",
            "Body, revised.",
            &["file:run.log"],
            &old,
        );
        fx.age();
        let counts = fx.counts(false);
        assert_eq!(
            (counts.citable, counts.drifted),
            (1, 0),
            "the replaced record is history: {counts:?}"
        );
        assert_eq!(fast(&fx).citable, 1);
    }

    #[test]
    fn a_trust_file_changed_after_the_environment_loaded_is_never_a_citable_entry() {
        let fx = Fx::new("trust-race");
        let Some(signer) = fx.signer() else { return };
        fx.seal(
            "alpha",
            "s-first-claim",
            "Body.",
            &["file:run.log"],
            Some(&signer),
        );
        fx.age();
        let env = fx.env();
        // A trust change lands after the verdict's trust was read.
        let allowed = fx.trust_dir.join(trust::ALLOWED_FILE);
        let mut text = fs::read_to_string(&allowed).unwrap();
        text.push_str("# a revocation lands here\n");
        fs::write(&allowed, text).unwrap();
        let reports = verify::verify_and_cache(&env).unwrap();
        assert_eq!(reports.len(), 1);
        let counts = fast(&fx);
        assert_eq!(
            (counts.citable, counts.unchecked),
            (0, 1),
            "stale trust must not be served as citable: {counts:?}"
        );
        assert!(
            load(&fx.cache_file()).signatures.is_empty(),
            "signature results of a run whose trust moved are dropped"
        );
    }

    #[test]
    fn stamps_are_taken_from_the_clock_before_the_read_they_vouch_for() {
        let fx = Fx::new("clock");
        fx.seal("alpha", "s-first-claim", "Body.", &["file:run.log"], None);
        fx.age();
        let env = fx.env();
        let base = now_ns();
        TEST_CLOCK.with(|c| c.set(Some(base)));
        let result = verify::verify_and_cache(&env);
        TEST_CLOCK.with(|c| c.set(None));
        result.unwrap();
        let cache = load(&fx.cache_file());
        assert!(!cache.evidence.is_empty());
        // Exactly one clock read per stamp, taken when it is stamped, and
        // none again at write time (which would be later than the read).
        let mut ticks: Vec<u128> = cache
            .pages
            .values()
            .map(|p| p.stamp.checked_at)
            .chain(cache.evidence.values().map(|e| e.stamp.checked_at))
            .collect();
        ticks.sort_unstable();
        let expected: Vec<u128> = (base..).take(ticks.len()).collect();
        assert_eq!(ticks, expected);
    }

    /// A repository at the fixture root with `main` and two commits; returns
    /// the two commit ids, or `None` without git.
    fn repo_at(fx: &Fx) -> Option<(String, String)> {
        let git_out = |args: &[&str]| {
            let empty = fx.root.join(".git-empty-config");
            let out = std::process::Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
                .args(["-c", "commit.gpgsign=false", "-c", "core.hooksPath=none"])
                .args(args)
                .current_dir(&fx.root)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", &empty)
                .output()
                .ok()?;
            out.status
                .success()
                .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
        };
        fs::write(fx.root.join(".git-empty-config"), "").ok()?;
        git_out(&["init", "-q", "-b", "main"])?;
        git_out(&["add", "run.log"])?;
        git_out(&["commit", "-q", "-m", "first"])?;
        let first = git_out(&["rev-parse", "HEAD"])?;
        git_out(&["commit", "-q", "--allow-empty", "-m", "second"])?;
        let second = git_out(&["rev-parse", "HEAD"])?;
        Some((first, second))
    }

    fn git_in(fx: &Fx, args: &[&str]) {
        assert!(fixture::git(&fx.root, args), "git {args:?} failed");
    }

    #[test]
    fn commit_evidence_is_citable_on_the_fast_path_right_after_a_commit() {
        let fx = Fx::new("commit-fast");
        let Some((_, head)) = repo_at(&fx) else {
            return;
        };
        // Nothing is aged: the refs were written moments ago.
        fx.seal(
            "alpha",
            "s-first-claim",
            "Body.",
            &[&format!("commit:{head}")],
            None,
        );
        let full = fx.full();
        assert_eq!(full.len(), 1);
        assert!(full[0].is_citable(), "{:?}", full[0]);
        let before = spawn_count();
        let counts = fast(&fx);
        assert_eq!(spawn_count(), before, "the fast path started a process");
        assert_eq!((counts.citable, counts.unchecked), (1, 0), "{counts:?}");

        // Unrelated ref traffic leaves it citable.
        git_in(&fx, &["branch", "other"]);
        git_in(&fx, &["tag", "v1"]);
        git_in(&fx, &["update-ref", "refs/remotes/origin/feature", "HEAD"]);
        git_in(&fx, &["update-ref", "refs/stash", "HEAD"]);
        let counts = fast(&fx);
        assert_eq!((counts.citable, counts.unchecked), (1, 0), "{counts:?}");
        assert_eq!(spawn_count(), before);

        // Rewinding the allowed ref makes it unchecked, with no spawn.
        git_in(&fx, &["reset", "-q", "--hard", "HEAD~1"]);
        let counts = fast(&fx);
        assert_eq!((counts.citable, counts.unchecked), (0, 1), "{counts:?}");
        assert_eq!(spawn_count(), before);
        // A full run sees the commit is no longer reachable.
        let full = fx.counts(false);
        assert_eq!(full.citable, 0, "{full:?}");
    }

    #[test]
    fn blob_evidence_is_cached_on_the_fast_path_and_dies_with_its_file() {
        let fx = Fx::new("blob-fast");
        if repo_at(&fx).is_none() {
            return;
        }
        fx.seal("alpha", "s-first-claim", "Body.", &["blob:run.log"], None);
        fx.age();
        let full = fx.full();
        assert!(full[0].is_citable(), "{:?}", full[0]);
        let before = spawn_count();
        let counts = fast(&fx);
        assert_eq!(spawn_count(), before);
        assert_eq!((counts.citable, counts.unchecked), (1, 0), "{counts:?}");

        fs::write(fx.root.join("run.log"), b"edited after the commit").unwrap();
        let counts = fast(&fx);
        assert_eq!(counts.citable, 0, "{counts:?}");
        assert_eq!(spawn_count(), before);
    }

    #[test]
    fn commit_evidence_fingerprint_follows_the_repository_refs() {
        let Some(repo) = fixture::git_repo("refs") else {
            return;
        };
        let (first, _) = refs_fingerprint(&repo.0).expect("a plain repository");
        assert_eq!(refs_fingerprint(&repo.0).unwrap().0, first);
        fixture::git(&repo.0, &["commit", "--allow-empty", "-m", "second"]);
        let (second, _) = refs_fingerprint(&repo.0).unwrap();
        assert_ne!(first, second);
        assert!(refs_fingerprint(&repo.0.join("no-such-dir")).is_none());
    }

    #[test]
    fn the_refs_fingerprint_ignores_refs_the_allowed_ref_does_not_depend_on() {
        let Some(repo) = fixture::git_repo("refs-narrow") else {
            return;
        };
        let (before, newest) = refs_fingerprint(&repo.0).unwrap();
        assert_eq!(newest, 0, "a content fingerprint has no racy window");
        fixture::git(&repo.0, &["branch", "other"]);
        fixture::git(&repo.0, &["tag", "v1"]);
        fixture::git(&repo.0, &["update-ref", "refs/remotes/origin/x", "HEAD"]);
        assert_eq!(refs_fingerprint(&repo.0).unwrap().0, before);
        // Same size, different content: only a content fingerprint sees it.
        let main = repo.0.join(".git/refs/heads").join(
            ["main", "master"]
                .into_iter()
                .find(|b| repo.0.join(".git/refs/heads").join(b).is_file())
                .unwrap(),
        );
        let text = fs::read_to_string(&main).unwrap();
        let flipped: String = text
            .chars()
            .map(|c| if c == '0' { '1' } else { '0' })
            .take(1)
            .chain(text.chars().skip(1))
            .collect();
        fs::write(&main, flipped).unwrap();
        assert_ne!(refs_fingerprint(&repo.0).unwrap().0, before);
    }

    #[test]
    fn a_reftable_update_changes_the_refs_fingerprint() {
        let Some(repo) = fixture::git_repo("reftable") else {
            return;
        };
        let dir = repo.0.join(".git").join("reftable");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("tables.list"), "0x1-0x1-aaaa.ref\n").unwrap();
        let (before, _) = refs_fingerprint(&repo.0).unwrap();
        fs::write(dir.join("tables.list"), "0x1-0x2-bbbb.ref\n").unwrap();
        let (after, _) = refs_fingerprint(&repo.0).unwrap();
        assert_ne!(before, after);
    }

    #[test]
    fn now_is_a_valid_fixture_stamp() {
        assert!(crate::seal::policy::parse_stamp(NOW).is_some());
    }
}

#[cfg(test)]
pub(crate) mod fixture {
    //! A temp workspace with a wiki, evidence files, a trust directory and a
    //! pinned cache directory, shared by the cache and `state` tests.

    use super::*;
    use crate::seal::evidence::{self, AttestOptions, Ctx};
    use crate::seal::record;
    use crate::seal::sshsig;
    use crate::seal::verify::Env;
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};

    pub const STAMP: &str = "2026-01-01 10:00 +02:00";
    pub const NOW: &str = "2026-11-21 10:00 +02:00";
    pub const PRINCIPAL: &str = "agent.one@local";
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    pub struct Fx {
        pub root: PathBuf,
        pub wiki: PathBuf,
        pub trust_dir: PathBuf,
        pub cache_dir: PathBuf,
        pub bin: Option<PathBuf>,
    }

    /// A signing key and its `allowed_signers` registration.
    pub struct Signer(pub PathBuf);

    impl Fx {
        pub fn new(label: &str) -> Fx {
            let n = COUNTER.fetch_add(1, Ordering::SeqCst);
            let root =
                std::env::temp_dir().join(format!("loam-sealcache-{label}-{}-{n}", now_ns()));
            let _ = fs::remove_dir_all(&root);
            let wiki = root.join("wiki");
            let trust_dir = root.join("config").join("seal");
            let cache_dir = root.join("config").join("cache").join("seals");
            fs::create_dir_all(&wiki).unwrap();
            fs::create_dir_all(&trust_dir).unwrap();
            fs::write(wiki.join("index.md"), "# Index\n").unwrap();
            fs::write(root.join("run.log"), b"all tests pass").unwrap();
            // Seals stay fresh for the life of the test suite.
            fs::write(
                trust_dir.join(trust::POLICY_FILE),
                trust::Policy {
                    max_review: "36500d".to_owned(),
                    ..trust::Policy::default()
                }
                .to_json()
                .to_json(),
            )
            .unwrap();
            Fx {
                root,
                wiki,
                trust_dir,
                cache_dir,
                bin: sshsig::keygen_for_tests(),
            }
        }

        pub fn cache_file(&self) -> PathBuf {
            cache_file(&self.cache_dir, &self.wiki)
        }

        /// A registered signing key, or `None` without a capable ssh-keygen.
        pub fn signer(&self) -> Option<Signer> {
            let bin = self.bin.as_ref()?;
            let key = self.root.join("key");
            let out = Command::new(bin)
                .args(["-q", "-t", "ed25519", "-N", "", "-C", "k", "-f"])
                .arg(&key)
                .output()
                .unwrap();
            assert!(out.status.success(), "ssh-keygen failed");
            let public = fs::read_to_string(key.with_extension("pub")).unwrap();
            let mut parts = public.split_whitespace();
            let line = format!("{} {}", parts.next().unwrap(), parts.next().unwrap());
            fs::write(
                self.trust_dir.join(trust::ALLOWED_FILE),
                format!("{PRINCIPAL} namespaces=\"loam-seal\",valid-after=\"20200101\" {line}\n"),
            )
            .unwrap();
            Some(Signer(key))
        }

        fn page_text(anchor: &str, body: &str) -> String {
            format!("# Alpha\n\n<!-- loam:seal {anchor} -->\n{body}\n<!-- /loam:seal -->\n")
        }

        pub fn write_page_unaged(&self, page: &str, anchor: &str, body: &str) {
            fs::write(
                self.wiki.join(format!("{page}.md")),
                Self::page_text(anchor, body),
            )
            .unwrap();
        }

        /// Writes the page; its mtime is the present, so call [`Fx::age`].
        pub fn write_page(&self, page: &str, anchor: &str, body: &str) {
            self.write_page_unaged(page, anchor, body);
        }

        pub fn seal(
            &self,
            page: &str,
            anchor: &str,
            body: &str,
            evidence_args: &[&str],
            signer: Option<&Signer>,
        ) -> String {
            self.write_record(page, anchor, body, evidence_args, signer, None, None)
        }

        /// A seal whose `review_by` is long past, so it is stale.
        pub fn seal_stale(
            &self,
            page: &str,
            anchor: &str,
            body: &str,
            evidence_args: &[&str],
        ) -> String {
            self.write_record(
                page,
                anchor,
                body,
                evidence_args,
                None,
                None,
                Some("2026-01-02 10:00 +02:00"),
            )
        }

        pub fn reseal(
            &self,
            page: &str,
            anchor: &str,
            body: &str,
            evidence_args: &[&str],
            parent: &str,
        ) -> String {
            self.write_record(page, anchor, body, evidence_args, None, Some(parent), None)
        }

        #[allow(clippy::too_many_arguments)]
        fn write_record(
            &self,
            page: &str,
            anchor: &str,
            body: &str,
            evidence_args: &[&str],
            signer: Option<&Signer>,
            parent: Option<&str>,
            review_by: Option<&str>,
        ) -> String {
            self.write_page(page, anchor, body);
            let text = Self::page_text(anchor, body);
            let scan = crate::markers::scan(&text);
            let marker = &scan.markers[0];
            let content = &text.as_bytes()[marker.content.clone()];
            let canonical =
                String::from_utf8(crate::seal::canon::canonicalize(content).unwrap()).unwrap();
            let ctx = Ctx {
                repo_root: self.root.clone(),
                wiki: self.wiki.clone(),
                retrieved_at: STAMP.to_owned(),
                capture_max_bytes: 262_144,
            };
            let items: Vec<_> = evidence_args
                .iter()
                .map(|arg| evidence::parse(arg).unwrap())
                .collect();
            let resolved = evidence::resolve_all(&items, &ctx, &AttestOptions::default()).unwrap();
            evidence::store_captures(&self.wiki, &resolved).unwrap();
            let (id, bytes) = record::build(&record::NewRecord {
                kind: record::RecordKind::Seal,
                page: page.to_owned(),
                anchor: anchor.to_owned(),
                subject_sha256: crate::seal::canon::digest(content).unwrap(),
                locator: record::soft_locator(&["Alpha".to_owned()], &canonical),
                content: canonical,
                requested_level: Some("evidence".to_owned()),
                claim: Some("a claim".to_owned()),
                procedure_uri: "loam://seal/procedure/v1".to_owned(),
                principal: PRINCIPAL.to_owned(),
                acted_on_behalf_of: None,
                generator: record::Generator {
                    name: "loam".to_owned(),
                    version: "1.0.0".to_owned(),
                    harness: "test".to_owned(),
                },
                time_verified: STAMP.to_owned(),
                review_by: review_by.map(str::to_owned),
                evidence: resolved.iter().map(|r| r.item.to_value()).collect(),
                redacted: evidence::redacted_values(&[]),
                parent: parent.map(|id| record::Parent {
                    id: id.to_owned(),
                    verdict: "drifted".to_owned(),
                }),
                status: None,
            })
            .unwrap();
            record::write_record(&self.wiki, &bytes).unwrap();
            if let (Some(signer), Some(bin)) = (signer, &self.bin) {
                let armored = sshsig::sign(bin, &signer.0, &bytes).unwrap();
                record::write_sig(&self.wiki, &id, armored.as_bytes()).unwrap();
            }
            id
        }

        pub fn only_id(&self) -> String {
            let loaded = record::load_all(&self.wiki).unwrap();
            assert_eq!(loaded.len(), 1);
            loaded[0].id.clone()
        }

        /// Backdates every file so no stamp is racy.
        pub fn age(&self) {
            fn walk(dir: &Path, when: SystemTime) {
                for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        walk(&path, when);
                    } else if let Ok(file) = fs::OpenOptions::new().write(true).open(&path) {
                        let _ = file.set_modified(when);
                    }
                }
            }
            walk(
                &self.root,
                SystemTime::now() - std::time::Duration::from_secs(3600),
            );
        }

        pub fn env(&self) -> Env {
            let mut env = Env::load(
                &self.wiki,
                Some(self.trust_dir.clone()),
                crate::seal::policy::parse_stamp(NOW).unwrap(),
            )
            .unwrap();
            env.repo_root = self.root.clone();
            env.keygen = self
                .bin
                .clone()
                .unwrap_or_else(|| self.root.join("no-such-keygen"));
            env.cache_dir = Some(self.cache_dir.clone());
            env
        }

        pub fn full(&self) -> Vec<Report> {
            verify::verify_and_cache(&self.env()).unwrap()
        }

        pub fn counts(&self, fast: bool) -> SealCounts {
            seal_counts_with(
                &self.wiki,
                fast,
                Some(self.trust_dir.clone()),
                Some(self.cache_dir.clone()),
                crate::seal::policy::parse_stamp(NOW).unwrap(),
            )
        }

        /// Path, length and mtime of every file under `dirs`.
        pub fn snapshot(&self, dirs: &[&Path]) -> Vec<(PathBuf, u64, Option<SystemTime>)> {
            fn walk(dir: &Path, out: &mut Vec<(PathBuf, u64, Option<SystemTime>)>) {
                for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        walk(&path, out);
                    } else if let Ok(meta) = fs::metadata(&path) {
                        out.push((path, meta.len(), meta.modified().ok()));
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
    }

    impl Drop for Fx {
        fn drop(&mut self) {
            for _ in 0..10 {
                if fs::remove_dir_all(&self.root).is_ok() {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    }

    pub struct Repo(pub PathBuf);
    impl Drop for Repo {
        fn drop(&mut self) {
            for _ in 0..10 {
                if fs::remove_dir_all(&self.0).is_ok() {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    }

    /// Runs git hermetically (no user or system config, fixed identity).
    pub fn git(dir: &Path, args: &[&str]) -> bool {
        let empty = dir.join(".git-empty-config");
        let _ = fs::write(&empty, "");
        Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args(["-c", "commit.gpgsign=false", "-c", "core.hooksPath=none"])
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", &empty)
            .output()
            .is_ok_and(|out| out.status.success())
    }

    /// A one-commit repository, or `None` without git.
    pub fn git_repo(label: &str) -> Option<Repo> {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("loam-sealrepo-{label}-{}-{n}", now_ns()));
        fs::create_dir_all(&dir).ok()?;
        let repo = Repo(dir);
        let ok = git(&repo.0, &["init", "-q"])
            && git(&repo.0, &["commit", "--allow-empty", "-q", "-m", "first"]);
        ok.then_some(repo)
    }
}
