//! Evidence items for seals (spec D5): parse `--evidence <kind:ref>`, resolve
//! at attest time (refusing on any failure) and re-check at verify time.
//!
//! Kinds: `blob` (git blob on the allowed ref), `file` (sha256), `commit`
//! (reachable from the allowed ref), `capture` (copy stored by digest under
//! `wiki/.seals/evidence/sha256/`), `seal` (another record) and `note` (free
//! text). There is deliberately no `cmd` kind, and nothing found in memory is
//! ever executed: git is the only subprocess, run with fixed arguments.
//!
//! Attest-time resolution is pure: [`resolve_all`] reads but never writes, so
//! a refused attest leaves no trace. Captures are written afterwards by
//! [`store_captures`].

use crate::json::Value;
use crate::seal::record;
use crate::sha256;
use std::fmt;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Largest cited record `seal:<id>` evidence will read.
const MAX_RECORD_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Blob,
    File,
    Commit,
    Capture,
    Seal,
    Note,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Blob => "blob",
            Kind::File => "file",
            Kind::Commit => "commit",
            Kind::Capture => "capture",
            Kind::Seal => "seal",
            Kind::Note => "note",
        }
    }

    fn parse(value: &str) -> Option<Kind> {
        Some(match value {
            "blob" => Kind::Blob,
            "file" => Kind::File,
            "commit" => Kind::Commit,
            "capture" => Kind::Capture,
            "seal" => Kind::Seal,
            "note" => Kind::Note,
            _ => return None,
        })
    }
}

/// When an item should be re-checked beyond "its content changed".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recheck {
    OnChange,
    /// `every:<n><h|d|w>`; the string is the duration part, e.g. `30d`.
    Every(String),
}

impl Recheck {
    pub fn as_string(&self) -> String {
        match self {
            Recheck::OnChange => "on-change".to_owned(),
            Recheck::Every(duration) => format!("every:{duration}"),
        }
    }

    fn parse(value: &str) -> Result<Recheck, String> {
        if value == "on-change" {
            return Ok(Recheck::OnChange);
        }
        let duration = value
            .strip_prefix("every:")
            .ok_or_else(|| format!("recheck {value:?} is not on-change or every:<n>h|d|w"))?;
        if parse_duration(duration).is_none() {
            return Err(format!(
                "recheck duration {duration:?} is not <n>h, <n>d or <n>w with n >= 1"
            ));
        }
        Ok(Recheck::Every(duration.to_owned()))
    }

    /// The recheck interval, for the freshness check to compare against
    /// `time_verified`. `None` for `on-change`.
    pub fn interval(&self) -> Option<chrono::Duration> {
        match self {
            Recheck::OnChange => None,
            Recheck::Every(duration) => parse_duration(duration),
        }
    }
}

fn parse_duration(value: &str) -> Option<chrono::Duration> {
    let unit = value.chars().last()?;
    let digits = &value[..value.len() - unit.len_utf8()];
    if digits.is_empty() || digits.len() > 4 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let count: i64 = digits.parse().ok()?;
    if count < 1 {
        return None;
    }
    match unit {
        'h' => Some(chrono::Duration::hours(count)),
        'd' => Some(chrono::Duration::days(count)),
        'w' => Some(chrono::Duration::weeks(count)),
        _ => None,
    }
}

/// How much an item counts toward the `evidence` level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Weight {
    Primary,
    Derived,
    Zero,
}

/// Stable per-item result codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Code {
    Current,
    Drifted,
    Unresolvable,
    Orphaned,
}

impl Code {
    pub fn as_str(self) -> &'static str {
        match self {
            Code::Current => "seal.evidence.current",
            Code::Drifted => "seal.evidence.drifted",
            Code::Unresolvable => "seal.evidence.unresolvable",
            Code::Orphaned => "seal.evidence.orphaned",
        }
    }
}

/// One evidence item: either as typed (`parse`) or as recorded
/// (`from_value`); `resolve_all` fills the derived fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub kind: Kind,
    /// Repository-relative forward-slash path (blob, file), full oid
    /// (commit), record id (seal), text (note), or the sha256 digest
    /// (capture; the source path survives in `origin`).
    pub reference: String,
    /// Blob and commit: the allowed ref the item was checked against.
    pub git_ref: Option<String>,
    pub git_blob: Option<String>,
    pub sha256: Option<String>,
    pub origin: Option<String>,
    pub retrieved_at: Option<String>,
    pub recheck: Recheck,
}

impl Item {
    fn new(kind: Kind, reference: String) -> Item {
        Item {
            kind,
            reference,
            git_ref: None,
            git_blob: None,
            sha256: None,
            origin: None,
            retrieved_at: None,
            recheck: Recheck::OnChange,
        }
    }

    /// `kind:ref`, for messages.
    pub fn label(&self) -> String {
        match (self.kind, &self.git_ref) {
            (Kind::Commit, Some(r)) => format!("commit:{}@{r}", self.reference),
            _ => format!("{}:{}", self.kind.as_str(), self.reference),
        }
    }

    /// The JSON object stored in the record's `evidence[]`.
    pub fn to_value(&self) -> Value {
        let text = |s: &str| Value::String(s.to_owned());
        let mut entries: Vec<(String, Value)> = vec![("kind".to_owned(), text(self.kind.as_str()))];
        let mut push = |key: &str, value: Value| entries.push((key.to_owned(), value));
        match self.kind {
            Kind::Blob => {
                push("path", text(&self.reference));
                push("gitBlob", text(self.git_blob.as_deref().unwrap_or("")));
                push("sha256", text(self.sha256.as_deref().unwrap_or("")));
                push("ref", text(self.git_ref.as_deref().unwrap_or("")));
            }
            Kind::File => {
                push("path", text(&self.reference));
                push("sha256", text(self.sha256.as_deref().unwrap_or("")));
            }
            Kind::Commit => {
                push("oid", text(&self.reference));
                push("ref", text(self.git_ref.as_deref().unwrap_or("")));
            }
            Kind::Capture => {
                push("sha256", text(self.sha256.as_deref().unwrap_or("")));
                push("origin", text(self.origin.as_deref().unwrap_or("")));
                push(
                    "retrieved_at",
                    text(self.retrieved_at.as_deref().unwrap_or("")),
                );
            }
            Kind::Seal => push("id", text(&self.reference)),
            Kind::Note => push("text", text(&self.reference)),
        }
        push("recheck", text(&self.recheck.as_string()));
        Value::Object(entries)
    }

    /// Parses a recorded evidence object, validating every field a check
    /// depends on so verification never trusts an unchecked string.
    pub fn from_value(value: &Value) -> Result<Item, String> {
        let field = |key: &str| value.get(key).and_then(Value::as_str);
        let required = |key: &str| {
            field(key)
                .filter(|v| !v.is_empty())
                .map(str::to_owned)
                .ok_or_else(|| format!("evidence item is missing {key}"))
        };
        let kind_name = field("kind").ok_or("evidence item is missing kind")?;
        let kind =
            Kind::parse(kind_name).ok_or_else(|| format!("unknown evidence kind {kind_name:?}"))?;
        let recheck = match field("recheck") {
            Some(text) => Recheck::parse(text)?,
            None => Recheck::OnChange,
        };
        let hex64 = |key: &str| -> Result<String, String> {
            let digest = required(key)?;
            if is_hex(&digest, 64, 64) {
                Ok(digest)
            } else {
                Err(format!("evidence {key} {digest:?} is not 64 lowercase hex"))
            }
        };
        let mut item = match kind {
            Kind::Blob => {
                let mut item = Item::new(kind, recorded_path(&required("path")?)?);
                let blob = required("gitBlob")?;
                if !is_hex(&blob, 40, 64) {
                    return Err(format!("evidence gitBlob {blob:?} is not a git object id"));
                }
                item.git_blob = Some(blob);
                item.sha256 = Some(hex64("sha256")?);
                item.git_ref = Some(valid_ref(&required("ref")?)?);
                item
            }
            Kind::File => {
                let mut item = Item::new(kind, recorded_path(&required("path")?)?);
                item.sha256 = Some(hex64("sha256")?);
                item
            }
            Kind::Commit => {
                let oid = required("oid")?;
                if !is_hex(&oid, 40, 64) {
                    return Err(format!("evidence oid {oid:?} is not a full commit id"));
                }
                let mut item = Item::new(kind, oid);
                item.git_ref = Some(valid_ref(&required("ref")?)?);
                item
            }
            Kind::Capture => {
                let digest = hex64("sha256")?;
                let mut item = Item::new(kind, digest.clone());
                item.sha256 = Some(digest);
                item.origin = Some(required("origin")?);
                item.retrieved_at = Some(required("retrieved_at")?);
                item
            }
            Kind::Seal => {
                let id = required("id")?;
                if !is_hex(&id, record::ID_LEN, record::ID_LEN) {
                    return Err(format!("evidence seal id {id:?} is not a record id"));
                }
                Item::new(kind, id)
            }
            Kind::Note => Item::new(kind, required("text")?),
        };
        item.recheck = recheck;
        Ok(item)
    }
}

fn is_hex(value: &str, min: usize, max: usize) -> bool {
    (min..=max).contains(&value.len())
        && value
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn valid_ref(value: &str) -> Result<String, String> {
    let bad_char = |c: char| {
        c.is_control() || c.is_whitespace() || matches!(c, '~' | '^' | ':' | '?' | '*' | '[' | '\\')
    };
    if value.is_empty()
        || value.starts_with('-')
        || value.contains("..")
        || value.chars().any(bad_char)
    {
        return Err(format!("{value:?} is not a usable ref name"));
    }
    Ok(value.to_owned())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailKind {
    Invalid,
    Missing,
    ShaMismatch,
    Unreachable,
    OverCap,
    NoAllowedRef,
    Git,
}

impl FailKind {
    pub fn name(self) -> &'static str {
        match self {
            FailKind::Invalid => "invalid",
            FailKind::Missing => "missing",
            FailKind::ShaMismatch => "sha_mismatch",
            FailKind::Unreachable => "unreachable",
            FailKind::OverCap => "over_cap",
            FailKind::NoAllowedRef => "no_allowed_ref",
            FailKind::Git => "git_error",
        }
    }
}

/// An evidence item that could not be resolved at attest time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub item: String,
    pub kind: FailKind,
    pub detail: String,
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}: {}", self.item, self.kind.name(), self.detail)
    }
}

impl std::error::Error for Failure {}

fn fail(item: &str, kind: FailKind, detail: impl Into<String>) -> Failure {
    Failure {
        item: item.to_owned(),
        kind,
        detail: detail.into(),
    }
}

/// Parses one `--evidence` argument: `kind:ref`, `commit:<oid>[@<ref>]`, each
/// optionally suffixed `;every:<n>h|d|w` to request a periodic recheck.
pub fn parse(arg: &str) -> Result<Item, Failure> {
    let bad = |detail: String| fail(arg, FailKind::Invalid, detail);
    let (kind_text, rest) = arg
        .split_once(':')
        .ok_or_else(|| bad("expected <kind>:<ref>".to_owned()))?;
    let kind = Kind::parse(kind_text).ok_or_else(|| {
        bad(format!(
            "unknown evidence kind {kind_text:?} (blob, file, commit, capture, seal, note)"
        ))
    })?;

    let mut reference = rest;
    let mut recheck = Recheck::OnChange;
    if let Some((head, tail)) = rest.rsplit_once(';') {
        if tail.starts_with("every:") {
            recheck = Recheck::parse(tail).map_err(&bad)?;
            reference = head;
        }
    }
    if reference.trim().is_empty() {
        return Err(bad("empty reference".to_owned()));
    }

    let mut item = match kind {
        Kind::Commit => {
            let (oid, git_ref) = match reference.split_once('@') {
                Some((oid, git_ref)) => (oid, Some(valid_ref(git_ref).map_err(&bad)?)),
                None => (reference, None),
            };
            let oid = oid.to_ascii_lowercase();
            if !is_hex(&oid, 7, 64) {
                return Err(bad(format!("{oid:?} is not a commit id (7 to 64 hex)")));
            }
            let mut item = Item::new(kind, oid);
            item.git_ref = git_ref;
            item
        }
        Kind::Seal => {
            if !is_hex(reference, record::ID_LEN, record::ID_LEN) {
                return Err(bad(format!(
                    "{reference:?} is not a {}-hex seal id",
                    record::ID_LEN
                )));
            }
            Item::new(kind, reference.to_owned())
        }
        _ => Item::new(kind, reference.to_owned()),
    };
    item.recheck = recheck;
    Ok(item)
}

/// Repeatable `--redacted <label>` flags as the record's `redacted[]`.
pub fn redacted_values(labels: &[String]) -> Vec<Value> {
    labels.iter().map(|l| Value::String(l.clone())).collect()
}

/// Where and how to resolve.
#[derive(Debug, Clone)]
pub struct Ctx {
    /// Blob paths resolve against this, not the wiki root.
    pub repo_root: PathBuf,
    pub wiki: PathBuf,
    /// `YYYY-MM-DD HH:MM ±HH:MM`, recorded as a capture's `retrieved_at`.
    pub retrieved_at: String,
    /// Policy `capture_max_bytes`.
    pub capture_max_bytes: u64,
}

#[derive(Debug, Clone, Default)]
pub struct AttestOptions<'a> {
    /// `--origin`; applies to every capture. Defaults to the source path.
    pub origin: Option<&'a str>,
}

/// An item resolved for attest.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub item: Item,
    pub weight: Weight,
    /// Local input path, for the T23 mtime/size cache.
    pub local_path: Option<PathBuf>,
    /// Capture bytes still to be stored with [`store_captures`].
    pub capture: Option<Vec<u8>>,
}

/// The re-check of one recorded item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub code: Code,
    pub weight: Weight,
    pub detail: String,
    pub local_path: Option<PathBuf>,
}

// ---------------------------------------------------------------- git

struct GitRun {
    code: Option<i32>,
    stdout: String,
}

fn git_command(repo: &Path, args: &[&str]) -> Command {
    #[cfg(test)]
    crate::seal::note_spawn();
    let mut command = Command::new("git");
    command
        .args(args)
        .current_dir(repo)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    command
}

fn git_run(repo: &Path, args: &[&str]) -> Option<GitRun> {
    let output = git_command(repo, args).output().ok()?;
    Some(GitRun {
        code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).trim().to_owned(),
    })
}

fn git_ok(repo: &Path, args: &[&str]) -> Option<String> {
    git_run(repo, args)
        .filter(|run| run.code == Some(0))
        .map(|run| run.stdout)
        .filter(|out| !out.is_empty())
}

/// The repository's top level for `start`, if inside one.
pub fn repo_root(start: &Path) -> Option<PathBuf> {
    git_ok(start, &["rev-parse", "--show-toplevel"]).map(PathBuf::from)
}

fn commit_of(repo: &Path, spec: &str) -> Option<String> {
    git_ok(
        repo,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{spec}^{{commit}}"),
        ],
    )
}

/// The ref evidence must be reachable from: the origin/HEAD target, else
/// local `main`, else `master`.
pub fn allowed_ref(repo: &Path) -> Option<String> {
    if let Some(target) = git_ok(
        repo,
        &[
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ],
    ) {
        if valid_ref(&target).is_ok() && commit_of(repo, &target).is_some() {
            return Some(target);
        }
    }
    ["main", "master"]
        .into_iter()
        .find(|name| commit_of(repo, &format!("refs/heads/{name}")).is_some())
        .map(str::to_owned)
}

fn short_name(git_ref: &str) -> &str {
    let name = git_ref.strip_prefix("refs/heads/").unwrap_or(git_ref);
    name.strip_prefix("origin/").unwrap_or(name)
}

// -------------------------------------------------------------- paths

/// Cleans a relative path to repository-relative, forward-slash form.
/// Rejects absolute paths, drive prefixes and any `..`.
fn clean_rel(raw: &str) -> Result<String, String> {
    let norm = raw.replace('\\', "/");
    if norm.starts_with('/') {
        return Err(format!("{raw:?} must be relative to the repository"));
    }
    let mut parts: Vec<&str> = Vec::new();
    for (index, part) in norm.split('/').enumerate() {
        match part {
            "" | "." => {}
            ".." => return Err(format!("{raw:?} must stay inside the repository")),
            p if index == 0 && p.len() == 2 && p.as_bytes()[1] == b':' => {
                return Err(format!("{raw:?} must be relative to the repository"));
            }
            p => parts.push(p),
        }
    }
    if parts.is_empty() {
        return Err("empty path".to_owned());
    }
    Ok(parts.join("/"))
}

/// A recorded path must already be in the form `clean_rel` produces.
fn recorded_path(raw: &str) -> Result<String, String> {
    match clean_rel(raw) {
        Ok(clean) if clean == raw => Ok(clean),
        Ok(_) => Err(format!("evidence path {raw:?} is not normalized")),
        Err(why) => Err(format!("evidence path: {why}")),
    }
}

fn canonical(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// A path given at attest time: repository-relative, forward slashes, no
/// escape. Absolute paths must lie inside the repository (as spelled or
/// resolved).
fn repo_rel(ctx: &Ctx, raw: &str) -> Result<String, String> {
    let path = Path::new(raw);
    if path.is_absolute() {
        let roots = [ctx.repo_root.clone(), canonical(&ctx.repo_root)];
        let spellings = [path.to_path_buf(), canonical(path)];
        for spelling in &spellings {
            for root in &roots {
                if let Ok(rest) = spelling.strip_prefix(root) {
                    return clean_rel(&rest.to_string_lossy());
                }
            }
        }
        return Err(format!("{raw:?} is outside the repository"));
    }
    clean_rel(raw)
}

fn fold(part: &std::ffi::OsStr) -> String {
    let text = part.to_string_lossy();
    if cfg!(any(windows, target_os = "macos")) {
        text.to_lowercase()
    } else {
        text.into_owned()
    }
}

fn starts_with_folded(path: &Path, prefix: &Path) -> bool {
    let a: Vec<String> = path.components().map(|c| fold(c.as_os_str())).collect();
    let b: Vec<String> = prefix.components().map(|c| fold(c.as_os_str())).collect();
    a.len() >= b.len() && a[..b.len()] == b[..]
}

/// Whether a repository-relative path lies in the wiki, by spelling or after
/// resolving symlinks, comparing case-insensitively where the file system is.
fn in_wiki(ctx: &Ctx, rel: &str) -> bool {
    let wikis = [ctx.wiki.clone(), canonical(&ctx.wiki)];
    let lexical = [ctx.repo_root.join(rel), canonical(&ctx.repo_root).join(rel)];
    lexical
        .iter()
        .flat_map(|path| [path.clone(), canonical(path)])
        .any(|path| wikis.iter().any(|wiki| starts_with_folded(&path, wiki)))
}

fn local(ctx: &Ctx, raw: &str) -> PathBuf {
    let path = Path::new(raw);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        ctx.repo_root.join(path)
    }
}

pub fn weight_of(item: &Item, ctx: &Ctx) -> Weight {
    match item.kind {
        Kind::Note => Weight::Zero,
        Kind::Seal => Weight::Derived,
        Kind::Commit | Kind::Capture => Weight::Primary,
        Kind::Blob | Kind::File => match clean_rel(&item.reference) {
            Ok(rel) if !in_wiki(ctx, &rel) => Weight::Primary,
            _ => Weight::Zero,
        },
    }
}

pub fn local_input(item: &Item, ctx: &Ctx) -> Option<PathBuf> {
    match item.kind {
        Kind::Blob | Kind::File => recorded_path(&item.reference)
            .ok()
            .map(|rel| ctx.repo_root.join(rel)),
        Kind::Capture => item
            .sha256
            .as_deref()
            .map(|digest| record::evidence_path(&ctx.wiki, digest)),
        Kind::Seal => Some(record::record_path(&ctx.wiki, &item.reference)),
        Kind::Commit | Kind::Note => None,
    }
}

// ------------------------------------------------------------- attest

/// Resolves every item, or returns every failure. Writes nothing.
pub fn resolve_all(
    items: &[Item],
    ctx: &Ctx,
    options: &AttestOptions<'_>,
) -> Result<Vec<Resolved>, Vec<Failure>> {
    let mut resolved = Vec::new();
    let mut failures = Vec::new();
    for item in items {
        match resolve_one(item, ctx, options) {
            Ok(r) => resolved.push(r),
            Err(f) => failures.push(f),
        }
    }
    if failures.is_empty() {
        Ok(resolved)
    } else {
        Err(failures)
    }
}

fn resolve_one(item: &Item, ctx: &Ctx, options: &AttestOptions<'_>) -> Result<Resolved, Failure> {
    let label = item.label();
    let mut out = item.clone();
    let mut capture = None;
    match item.kind {
        Kind::Blob => resolve_blob(&mut out, ctx, &label)?,
        Kind::File => {
            let rel = repo_rel(ctx, &item.reference)
                .map_err(|why| fail(&label, FailKind::Invalid, why))?;
            let path = ctx.repo_root.join(&rel);
            if !path.is_file() {
                return Err(fail(&label, FailKind::Missing, "file not found"));
            }
            let digest = sha256::file_hex(&path);
            if digest.is_empty() {
                return Err(fail(&label, FailKind::Invalid, "file is unreadable"));
            }
            out.reference = rel;
            out.sha256 = Some(digest);
        }
        Kind::Commit => resolve_commit(&mut out, ctx, &label)?,
        Kind::Capture => {
            capture = Some(resolve_capture(&mut out, ctx, options, &label)?);
        }
        Kind::Seal => {
            if let Err(why) = check_seal(&item.reference, ctx) {
                return Err(fail(&label, FailKind::Missing, why));
            }
        }
        Kind::Note => {
            if item.reference.trim().is_empty() {
                return Err(fail(&label, FailKind::Invalid, "empty note"));
            }
        }
    }
    Ok(Resolved {
        weight: weight_of(&out, ctx),
        local_path: local_input(&out, ctx),
        item: out,
        capture,
    })
}

const NO_ALLOWED_REF: &str = "no origin/HEAD, main or master ref to check against";

fn resolve_blob(item: &mut Item, ctx: &Ctx, label: &str) -> Result<(), Failure> {
    let rel = repo_rel(ctx, &item.reference).map_err(|why| fail(label, FailKind::Invalid, why))?;
    if !ctx.repo_root.join(&rel).is_file() {
        return Err(fail(
            label,
            FailKind::Missing,
            "file not found in the worktree",
        ));
    }
    let Some(git_ref) = allowed_ref(&ctx.repo_root) else {
        return Err(fail(label, FailKind::NoAllowedRef, NO_ALLOWED_REF));
    };
    let worktree = git_ok(&ctx.repo_root, &["hash-object", "--", &rel])
        .ok_or_else(|| fail(label, FailKind::Git, "git hash-object failed"))?;
    let committed = git_ok(
        &ctx.repo_root,
        &["rev-parse", "--verify", &format!("{git_ref}:{rel}")],
    )
    .ok_or_else(|| {
        fail(
            label,
            FailKind::Missing,
            format!("not present at {git_ref}"),
        )
    })?;
    if worktree != committed {
        return Err(fail(
            label,
            FailKind::ShaMismatch,
            format!("worktree blob {worktree} differs from {committed} at {git_ref}"),
        ));
    }
    let content = git_command(&ctx.repo_root, &["cat-file", "blob", &committed])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .ok_or_else(|| fail(label, FailKind::Git, "git cat-file failed"))?;
    item.reference = rel;
    item.sha256 = Some(sha256::hex(&content.stdout));
    item.git_blob = Some(committed);
    item.git_ref = Some(git_ref);
    Ok(())
}

fn resolve_commit(item: &mut Item, ctx: &Ctx, label: &str) -> Result<(), Failure> {
    let Some(allowed) = allowed_ref(&ctx.repo_root) else {
        return Err(fail(label, FailKind::NoAllowedRef, NO_ALLOWED_REF));
    };
    let git_ref = match item.git_ref.take() {
        Some(requested) if requested != allowed => {
            return Err(fail(
                label,
                FailKind::Unreachable,
                format!("{requested} is not the allowed ref ({allowed})"),
            ));
        }
        Some(requested) => requested,
        None => allowed,
    };
    let full = commit_of(&ctx.repo_root, &item.reference)
        .ok_or_else(|| fail(label, FailKind::Missing, "no such commit"))?;
    let run = git_run(
        &ctx.repo_root,
        &["merge-base", "--is-ancestor", &full, &git_ref],
    )
    .ok_or_else(|| fail(label, FailKind::Git, "git is unavailable"))?;
    match run.code {
        Some(0) => {}
        Some(1) => {
            return Err(fail(
                label,
                FailKind::Unreachable,
                format!("not reachable from {git_ref} (branch-only commit)"),
            ));
        }
        _ => return Err(fail(label, FailKind::Git, "git merge-base failed")),
    }
    item.reference = full;
    item.git_ref = Some(git_ref);
    Ok(())
}

fn resolve_capture(
    item: &mut Item,
    ctx: &Ctx,
    options: &AttestOptions<'_>,
    label: &str,
) -> Result<Vec<u8>, Failure> {
    let path = local(ctx, &item.reference);
    let size = fs::metadata(&path)
        .ok()
        .filter(|m| m.is_file())
        .map(|m| m.len())
        .ok_or_else(|| fail(label, FailKind::Missing, "capture source not found"))?;
    let cap = ctx.capture_max_bytes;
    if size > cap {
        return Err(fail(
            label,
            FailKind::OverCap,
            format!("{size} bytes exceeds capture_max_bytes {cap}"),
        ));
    }
    let mut bytes = Vec::new();
    fs::File::open(&path)
        .and_then(|file| file.take(cap.saturating_add(1)).read_to_end(&mut bytes))
        .map_err(|e| fail(label, FailKind::Invalid, format!("cannot read source: {e}")))?;
    if bytes.len() as u64 > cap {
        return Err(fail(
            label,
            FailKind::OverCap,
            format!("grew past capture_max_bytes {cap} while reading"),
        ));
    }
    let origin = options.origin.map(str::to_owned).unwrap_or_else(|| {
        repo_rel(ctx, &item.reference).unwrap_or_else(|_| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "capture".to_owned())
        })
    });
    let digest = sha256::hex(&bytes);
    item.reference = digest.clone();
    item.sha256 = Some(digest);
    item.origin = Some(origin);
    item.retrieved_at = Some(ctx.retrieved_at.clone());
    Ok(bytes)
}

fn check_seal(id: &str, ctx: &Ctx) -> Result<(), String> {
    let path = record::record_path(&ctx.wiki, id);
    let size = fs::metadata(&path)
        .map_err(|_| format!("no record {id}"))?
        .len();
    if size > MAX_RECORD_BYTES {
        return Err(format!("record {id} is too large"));
    }
    let bytes = fs::read(&path).map_err(|e| format!("cannot read record {id}: {e}"))?;
    record::well_formed(id, &bytes)
        .map(|_| ())
        .map_err(|e| format!("record {id}: {e}"))
}

/// Writes resolved captures under `wiki/.seals/evidence/sha256/`. Call only
/// after every item resolved. Re-storing identical bytes is a no-op.
pub fn store_captures(wiki: &Path, resolved: &[Resolved]) -> Result<(), String> {
    for entry in resolved {
        let (Some(bytes), Some(digest)) = (&entry.capture, entry.item.sha256.as_deref()) else {
            continue;
        };
        match record::write_evidence(wiki, digest, bytes) {
            Ok(()) => {}
            Err(record::StoreError::Exists(_)) => {
                let stored = fs::read(record::evidence_path(wiki, digest)).ok();
                if stored.as_deref() != Some(bytes.as_slice()) {
                    return Err(format!("stored capture {digest} does not match its digest"));
                }
            }
            Err(e) => return Err(format!("cannot store capture {digest}: {e}")),
        }
    }
    Ok(())
}

// ------------------------------------------------------------- verify

/// Re-resolves a recorded item against the current state.
pub fn check(item: &Item, ctx: &Ctx) -> Check {
    let (code, detail) = check_code(item, ctx);
    Check {
        code,
        weight: weight_of(item, ctx),
        detail,
        local_path: local_input(item, ctx),
    }
}

pub fn check_code(item: &Item, ctx: &Ctx) -> (Code, String) {
    let current = || (Code::Current, String::new());
    let unresolvable = |why: &str| (Code::Unresolvable, why.to_owned());
    match item.kind {
        Kind::Note => current(),
        Kind::Seal => match check_seal(&item.reference, ctx) {
            Ok(()) => current(),
            Err(why) => (Code::Unresolvable, why),
        },
        Kind::File => {
            let Ok(rel) = recorded_path(&item.reference) else {
                return unresolvable("path is not repository-relative");
            };
            let path = ctx.repo_root.join(rel);
            if !path.is_file() {
                return unresolvable("file not found");
            }
            let digest = sha256::file_hex(&path);
            if digest.is_empty() {
                unresolvable("file is unreadable")
            } else if Some(&digest) == item.sha256.as_ref() {
                current()
            } else {
                (Code::Drifted, "sha256 changed".to_owned())
            }
        }
        Kind::Blob => {
            let Ok(rel) = recorded_path(&item.reference) else {
                return unresolvable("path is not repository-relative");
            };
            if !ctx.repo_root.join(&rel).is_file() {
                return unresolvable("file not found in the worktree");
            }
            match git_ok(&ctx.repo_root, &["hash-object", "--", &rel]) {
                None => unresolvable("git hash-object failed"),
                Some(now) if Some(&now) == item.git_blob.as_ref() => current(),
                Some(_) => (Code::Drifted, "git blob changed".to_owned()),
            }
        }
        Kind::Commit => {
            let Some(recorded) = item.git_ref.as_deref() else {
                return unresolvable("no ref recorded");
            };
            let Some(git_ref) = allowed_ref(&ctx.repo_root) else {
                return unresolvable(NO_ALLOWED_REF);
            };
            if short_name(recorded) != short_name(&git_ref) {
                return unresolvable(&format!(
                    "recorded ref {recorded} is not the allowed ref ({git_ref})"
                ));
            }
            let git_ref = git_ref.as_str();
            let reachable = commit_of(&ctx.repo_root, &item.reference).is_some()
                && git_run(
                    &ctx.repo_root,
                    &["merge-base", "--is-ancestor", &item.reference, git_ref],
                )
                .is_some_and(|run| run.code == Some(0));
            if reachable {
                current()
            } else {
                (Code::Orphaned, format!("not reachable from {git_ref}"))
            }
        }
        Kind::Capture => {
            let Some(digest) = item.sha256.as_deref() else {
                return unresolvable("no digest recorded");
            };
            let stored = record::evidence_path(&ctx.wiki, digest);
            if !stored.is_file() {
                return unresolvable("capture is not in the evidence store");
            }
            let now = sha256::file_hex(&stored);
            if now.is_empty() {
                unresolvable("capture is unreadable")
            } else if now == digest {
                current()
            } else {
                (
                    Code::Drifted,
                    "stored capture no longer matches its digest".to_owned(),
                )
            }
        }
    }
}

/// Parses a record's `evidence[]`; the first malformed item is an error.
pub fn items_from_values(values: &[Value]) -> Result<Vec<Item>, String> {
    values.iter().map(Item::from_value).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seal::record;
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);
    const STAMP: &str = "2026-11-20 14:30 +02:00";

    struct Tmp(PathBuf);
    impl Tmp {
        fn new(label: &str) -> Tmp {
            let n = COUNTER.fetch_add(1, Ordering::SeqCst);
            let dir = std::env::temp_dir()
                .join(format!("loam-evidence-{label}-{}-{n}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(dir.join("wiki")).unwrap();
            Tmp(dir)
        }
        fn repo(&self) -> &Path {
            &self.0
        }
        fn wiki(&self) -> PathBuf {
            self.0.join("wiki")
        }
        fn ctx(&self, cap: u64) -> Ctx {
            Ctx {
                repo_root: self.0.clone(),
                wiki: self.wiki(),
                retrieved_at: STAMP.to_owned(),
                capture_max_bytes: cap,
            }
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            for _ in 0..10 {
                if fs::remove_dir_all(&self.0).is_ok() {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    }
    fn git(dir: &Path, args: &[&str]) -> Option<String> {
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
            .ok()?;
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        Some(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    }

    /// A repo on `main` holding `src/a.txt`. Fails loudly without git.
    fn repo(label: &str) -> Tmp {
        git(Path::new("."), &["--version"]).expect("git must be installed to run these tests");
        let tmp = Tmp::new(label);
        git(tmp.repo(), &["init", "-q"]).unwrap();
        git(tmp.repo(), &["symbolic-ref", "HEAD", "refs/heads/main"]).unwrap();
        fs::create_dir_all(tmp.repo().join("src")).unwrap();
        fs::write(tmp.repo().join("src/a.txt"), "alpha\n").unwrap();
        git(tmp.repo(), &["add", "src/a.txt"]).unwrap();
        git(tmp.repo(), &["commit", "-q", "-m", "first"]).unwrap();
        tmp
    }

    fn parsed(arg: &str) -> Item {
        parse(arg).unwrap_or_else(|f| panic!("{f}"))
    }

    fn resolve(tmp: &Tmp, args: &[&str]) -> Result<Vec<Resolved>, Vec<Failure>> {
        let items: Vec<Item> = args.iter().map(|a| parsed(a)).collect();
        resolve_all(&items, &tmp.ctx(1024), &AttestOptions::default())
    }

    fn only_failure(result: Result<Vec<Resolved>, Vec<Failure>>) -> Failure {
        let mut failures = result.expect_err("expected a failure");
        assert_eq!(failures.len(), 1, "{failures:?}");
        failures.remove(0)
    }

    #[test]
    fn parse_kinds_and_recheck() {
        let blob = parsed("blob:src/a.txt");
        assert_eq!(blob.kind, Kind::Blob);
        assert_eq!(blob.reference, "src/a.txt");
        assert_eq!(blob.recheck, Recheck::OnChange);

        let commit = parsed("commit:abc1234@origin/main;every:30d");
        assert_eq!(commit.kind, Kind::Commit);
        assert_eq!(commit.reference, "abc1234");
        assert_eq!(commit.git_ref.as_deref(), Some("origin/main"));
        assert_eq!(commit.recheck, Recheck::Every("30d".to_owned()));
        assert_eq!(commit.recheck.interval(), Some(chrono::Duration::days(30)));

        assert_eq!(parsed("file:raw/x.pdf").kind, Kind::File);
        assert_eq!(parsed("capture:/tmp/log.txt").kind, Kind::Capture);
        assert_eq!(parsed("seal:0123456789abcdef0123").kind, Kind::Seal);
        let note = parsed("note:checked with a human; every:day");
        assert_eq!(note.kind, Kind::Note);
        assert_eq!(note.reference, "checked with a human; every:day");
    }

    #[test]
    fn parse_rejects_bad_input() {
        for bad in [
            "",
            "blob",
            "blob:",
            "cmd:echo hi",
            "bogus:x",
            "commit:nothex",
            "commit:abc1234@",
            "commit:abc1234@-evil",
            "seal:SHORT",
            "note:   ",
            "blob:a;every:0d",
        ] {
            let failure = parse(bad).err().unwrap_or_else(|| panic!("{bad:?} parsed"));
            assert_eq!(failure.kind, FailKind::Invalid, "{bad:?}");
        }
        assert_eq!(
            Recheck::Every("12h".to_owned()).interval(),
            Some(chrono::Duration::hours(12))
        );
        assert_eq!(
            Recheck::Every("2w".to_owned()).interval(),
            Some(chrono::Duration::weeks(2))
        );
        assert_eq!(Recheck::OnChange.interval(), None);
    }

    #[test]
    fn blob_resolves_and_round_trips_through_json() {
        let tmp = repo("blob");
        let resolved = resolve(&tmp, &["blob:src/a.txt"]).unwrap();
        let item = &resolved[0].item;
        assert_eq!(item.git_ref.as_deref(), Some("main"));
        let expect = git(tmp.repo(), &["rev-parse", "main:src/a.txt"]).unwrap();
        assert_eq!(item.git_blob.as_deref(), Some(expect.as_str()));
        assert_eq!(
            item.sha256.as_deref(),
            Some(crate::sha256::hex(b"alpha\n").as_str())
        );
        assert_eq!(resolved[0].weight, Weight::Primary);
        assert_eq!(resolved[0].local_path, Some(tmp.repo().join("src/a.txt")));

        let value = item.to_value();
        assert_eq!(value.get("kind").and_then(Value::as_str), Some("blob"));
        let back = Item::from_value(&crate::json::parse(&value.to_json()).unwrap()).unwrap();
        assert_eq!(&back, item);
        let check = check(&back, &tmp.ctx(1024));
        assert_eq!(check.code, Code::Current);
        assert_eq!(check.code.as_str(), "seal.evidence.current");
    }

    #[test]
    fn blob_failures_are_named() {
        let tmp = repo("blobfail");
        let missing = only_failure(resolve(&tmp, &["blob:src/nope.txt"]));
        assert_eq!(missing.kind, FailKind::Missing);
        assert!(missing.to_string().contains("blob:src/nope.txt"));

        fs::write(tmp.repo().join("src/a.txt"), "edited\n").unwrap();
        let mismatch = only_failure(resolve(&tmp, &["blob:src/a.txt"]));
        assert_eq!(mismatch.kind, FailKind::ShaMismatch);

        fs::write(tmp.repo().join("untracked.txt"), "x").unwrap();
        let untracked = only_failure(resolve(&tmp, &["blob:untracked.txt"]));
        assert_eq!(untracked.kind, FailKind::Missing);

        for escape in ["blob:../outside.txt", "blob:/etc/passwd"] {
            let f = only_failure(resolve(&tmp, &[escape]));
            assert_eq!(f.kind, FailKind::Invalid, "{escape}");
        }
    }

    #[test]
    fn edited_blob_re_resolves_as_drifted_and_deleted_as_unresolvable() {
        let tmp = repo("drift");
        let item = resolve(&tmp, &["blob:src/a.txt"]).unwrap().remove(0).item;
        fs::write(tmp.repo().join("src/a.txt"), "beta\n").unwrap();
        let drifted = check(&item, &tmp.ctx(1024));
        assert_eq!(drifted.code, Code::Drifted);
        assert_eq!(drifted.code.as_str(), "seal.evidence.drifted");
        fs::remove_file(tmp.repo().join("src/a.txt")).unwrap();
        assert_eq!(check(&item, &tmp.ctx(1024)).code, Code::Unresolvable);
    }

    #[test]
    fn file_evidence_resolves_drifts_and_goes_missing() {
        let tmp = repo("file");
        fs::write(tmp.repo().join("raw.txt"), "raw bytes").unwrap();
        let resolved = resolve(&tmp, &["file:raw.txt"]).unwrap();
        let item = resolved[0].item.clone();
        assert_eq!(
            item.sha256.as_deref(),
            Some(crate::sha256::hex(b"raw bytes").as_str())
        );
        assert_eq!(check(&item, &tmp.ctx(1024)).code, Code::Current);
        fs::write(tmp.repo().join("raw.txt"), "other").unwrap();
        assert_eq!(check(&item, &tmp.ctx(1024)).code, Code::Drifted);
        fs::remove_file(tmp.repo().join("raw.txt")).unwrap();
        assert_eq!(check(&item, &tmp.ctx(1024)).code, Code::Unresolvable);
        let f = only_failure(resolve(&tmp, &["file:raw.txt"]));
        assert_eq!(f.kind, FailKind::Missing);
    }

    #[test]
    fn commit_on_allowed_ref_resolves_and_branch_only_is_refused() {
        let tmp = repo("commit");
        let head = git(tmp.repo(), &["rev-parse", "HEAD"]).unwrap();
        let resolved = resolve(&tmp, &[&format!("commit:{}", &head[..10])]).unwrap();
        assert_eq!(resolved[0].item.reference, head);
        assert_eq!(resolved[0].item.git_ref.as_deref(), Some("main"));
        assert_eq!(check(&resolved[0].item, &tmp.ctx(1024)).code, Code::Current);
        let explicit = resolve(&tmp, &[&format!("commit:{head}@main")]).unwrap();
        assert_eq!(explicit[0].item.reference, head);

        git(tmp.repo(), &["checkout", "-q", "-b", "feature"]).unwrap();
        fs::write(tmp.repo().join("b.txt"), "b").unwrap();
        git(tmp.repo(), &["add", "b.txt"]).unwrap();
        git(tmp.repo(), &["commit", "-q", "-m", "branch only"]).unwrap();
        let branch_only = git(tmp.repo(), &["rev-parse", "HEAD"]).unwrap();
        git(tmp.repo(), &["checkout", "-q", "main"]).unwrap();
        let f = only_failure(resolve(&tmp, &[&format!("commit:{branch_only}")]));
        assert_eq!(f.kind, FailKind::Unreachable);

        let wrong_ref = only_failure(resolve(&tmp, &[&format!("commit:{head}@feature")]));
        assert_eq!(wrong_ref.kind, FailKind::Unreachable);

        let unknown = only_failure(resolve(&tmp, &["commit:deadbeefdeadbeef"]));
        assert_eq!(unknown.kind, FailKind::Missing);
    }

    #[test]
    fn squash_orphaned_commit_is_orphaned_never_current() {
        let tmp = repo("orphan");
        fs::write(tmp.repo().join("b.txt"), "b").unwrap();
        git(tmp.repo(), &["add", "b.txt"]).unwrap();
        git(tmp.repo(), &["commit", "-q", "-m", "second"]).unwrap();
        let second = git(tmp.repo(), &["rev-parse", "HEAD"]).unwrap();
        let item = resolve(&tmp, &[&format!("commit:{second}")])
            .unwrap()
            .remove(0)
            .item;
        assert_eq!(check(&item, &tmp.ctx(1024)).code, Code::Current);
        git(tmp.repo(), &["reset", "-q", "--hard", "HEAD~1"]).unwrap();
        let orphaned = check(&item, &tmp.ctx(1024));
        assert_eq!(orphaned.code, Code::Orphaned);
        assert_eq!(orphaned.code.as_str(), "seal.evidence.orphaned");
    }

    #[test]
    fn allowed_ref_prefers_origin_head_then_main_then_master() {
        let tmp = repo("allowed");
        assert_eq!(allowed_ref(tmp.repo()).as_deref(), Some("main"));
        git(tmp.repo(), &["branch", "-m", "main", "master"]).unwrap();
        assert_eq!(allowed_ref(tmp.repo()).as_deref(), Some("master"));
        let head = git(tmp.repo(), &["rev-parse", "HEAD"]).unwrap();
        git(
            tmp.repo(),
            &["update-ref", "refs/remotes/origin/trunk", &head],
        )
        .unwrap();
        git(
            tmp.repo(),
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/trunk",
            ],
        )
        .unwrap();
        assert_eq!(allowed_ref(tmp.repo()).as_deref(), Some("origin/trunk"));
        git(tmp.repo(), &["checkout", "-q", "-b", "other"]).unwrap();
        git(tmp.repo(), &["branch", "-D", "master"]).unwrap();
        git(
            tmp.repo(),
            &["symbolic-ref", "--delete", "refs/remotes/origin/HEAD"],
        )
        .unwrap();
        git(
            tmp.repo(),
            &["update-ref", "-d", "refs/remotes/origin/trunk"],
        )
        .unwrap();
        assert_eq!(allowed_ref(tmp.repo()), None);
        let f = only_failure(resolve(&tmp, &["blob:src/a.txt"]));
        assert_eq!(f.kind, FailKind::NoAllowedRef);
    }

    #[test]
    fn capture_under_cap_is_stored_with_origin_and_time() {
        let tmp = Tmp::new("capture");
        fs::write(tmp.repo().join("log.txt"), b"test run output\r\n").unwrap();
        let items = [parsed("capture:log.txt")];
        let opts = AttestOptions {
            origin: Some("ci run 42"),
        };
        let resolved = resolve_all(&items, &tmp.ctx(1024), &opts).unwrap();
        let item = &resolved[0].item;
        let digest = crate::sha256::hex(b"test run output\r\n");
        assert_eq!(item.sha256.as_deref(), Some(digest.as_str()));
        assert_eq!(item.origin.as_deref(), Some("ci run 42"));
        assert_eq!(item.retrieved_at.as_deref(), Some(STAMP));
        assert_eq!(resolved[0].weight, Weight::Primary);
        assert!(!record::evidence_path(&tmp.wiki(), &digest).exists());

        store_captures(&tmp.wiki(), &resolved).unwrap();
        store_captures(&tmp.wiki(), &resolved).unwrap();
        assert_eq!(
            fs::read(record::evidence_path(&tmp.wiki(), &digest)).unwrap(),
            b"test run output\r\n"
        );
        assert_eq!(check(item, &tmp.ctx(1024)).code, Code::Current);

        let defaulted = resolve_all(&items, &tmp.ctx(1024), &AttestOptions::default()).unwrap();
        assert_eq!(defaulted[0].item.origin.as_deref(), Some("log.txt"));
    }

    #[test]
    fn capture_over_cap_is_refused_and_nothing_is_written() {
        let tmp = Tmp::new("capcap");
        fs::write(tmp.repo().join("big.log"), vec![b'x'; 11]).unwrap();
        let items = [parsed("capture:big.log")];
        let err = resolve_all(&items, &tmp.ctx(10), &AttestOptions::default()).unwrap_err();
        assert_eq!(err.len(), 1);
        assert_eq!(err[0].kind, FailKind::OverCap);
        assert!(!record::seals_dir(&tmp.wiki()).exists());
        fs::write(tmp.repo().join("ok.log"), vec![b'x'; 10]).unwrap();
        let ok = [parsed("capture:ok.log")];
        assert!(resolve_all(&ok, &tmp.ctx(10), &AttestOptions::default()).is_ok());
    }

    #[test]
    fn capture_verify_detects_tamper_and_missing_store() {
        let tmp = Tmp::new("captamper");
        fs::write(tmp.repo().join("log.txt"), b"original").unwrap();
        let resolved = resolve_all(
            &[parsed("capture:log.txt")],
            &tmp.ctx(1024),
            &AttestOptions::default(),
        )
        .unwrap();
        let item = resolved[0].item.clone();
        assert_eq!(check(&item, &tmp.ctx(1024)).code, Code::Unresolvable);
        store_captures(&tmp.wiki(), &resolved).unwrap();
        let stored = record::evidence_path(&tmp.wiki(), item.sha256.as_deref().unwrap());
        let check_ok = check(&item, &tmp.ctx(1024));
        assert_eq!(check_ok.local_path, Some(stored.clone()));
        fs::write(&stored, b"tampered").unwrap();
        assert_eq!(check(&item, &tmp.ctx(1024)).code, Code::Drifted);
    }

    #[test]
    fn redacted_labels_become_string_values() {
        let values = redacted_values(&["token".to_owned(), "email".to_owned()]);
        assert_eq!(values.len(), 2);
        assert_eq!(values[0].as_str(), Some("token"));
    }

    fn write_seal(tmp: &Tmp) -> String {
        let r = record::NewRecord {
            kind: record::RecordKind::Seal,
            page: "p".to_owned(),
            anchor: "s-one-two".to_owned(),
            subject_sha256: "ab".repeat(32),
            locator: record::Locator {
                heading_path: vec![],
                prefix: String::new(),
                suffix: String::new(),
            },
            content: "x\n".to_owned(),
            requested_level: Some("evidence".to_owned()),
            claim: Some("c".to_owned()),
            procedure_uri: "loam://seal/attest".to_owned(),
            principal: "agent".to_owned(),
            acted_on_behalf_of: None,
            generator: record::Generator {
                name: "loam".to_owned(),
                version: "1".to_owned(),
                harness: "test".to_owned(),
            },
            time_verified: STAMP.to_owned(),
            review_by: None,
            evidence: vec![],
            redacted: vec![],
            parent: None,
            status: None,
        };
        let (_, bytes) = record::build(&r).unwrap();
        record::write_record(&tmp.wiki(), &bytes).unwrap()
    }

    #[test]
    fn seal_evidence_requires_a_well_formed_record() {
        let tmp = Tmp::new("seal");
        let id = write_seal(&tmp);
        let resolved = resolve(&tmp, &[&format!("seal:{id}")]).unwrap();
        assert_eq!(resolved[0].weight, Weight::Derived);
        assert_eq!(resolved[0].item.reference, id);
        assert_eq!(check(&resolved[0].item, &tmp.ctx(1)).code, Code::Current);
        let f = only_failure(resolve(&tmp, &["seal:00000000000000000000"]));
        assert_eq!(f.kind, FailKind::Missing);
        fs::remove_file(record::record_path(&tmp.wiki(), &id)).unwrap();
        assert_eq!(
            check(&resolved[0].item, &tmp.ctx(1)).code,
            Code::Unresolvable
        );
    }

    #[test]
    fn note_is_zero_weight_and_always_current() {
        let tmp = Tmp::new("note");
        let resolved = resolve(&tmp, &["note:read the vendor docs"]).unwrap();
        assert_eq!(resolved[0].weight, Weight::Zero);
        assert_eq!(resolved[0].local_path, None);
        let c = check(&resolved[0].item, &tmp.ctx(1));
        assert_eq!(c.code, Code::Current);
        assert_eq!(c.weight, Weight::Zero);
    }

    #[test]
    fn unsealed_wiki_pages_weigh_zero() {
        let tmp = repo("wikiweight");
        fs::write(tmp.wiki().join("page.md"), "# Page\n").unwrap();
        let resolved = resolve(&tmp, &["file:wiki/page.md", "file:src/a.txt"]).unwrap();
        assert_eq!(resolved[0].weight, Weight::Zero);
        assert_eq!(resolved[1].weight, Weight::Primary);
        assert_eq!(check(&resolved[0].item, &tmp.ctx(1)).weight, Weight::Zero);
    }

    #[test]
    fn wiki_pages_cannot_be_laundered_through_odd_paths() {
        let tmp = repo("wikipaths");
        fs::write(tmp.wiki().join("page.md"), "# Page\n").unwrap();
        let abs = tmp.wiki().join("page.md");
        let abs = abs.to_string_lossy().into_owned();
        let dotted = "file:src/../wiki/page.md";
        let noisy = "file:./wiki//page.md";
        let back = "file:wiki\\page.md";
        let abs_arg = format!("file:{abs}");
        for arg in [dotted, noisy, back, abs_arg.as_str()] {
            if arg == dotted {
                let f = only_failure(resolve(&tmp, &[arg]));
                assert_eq!(f.kind, FailKind::Invalid, "{arg}");
                continue;
            }
            let r = resolve(&tmp, &[arg]).unwrap();
            assert_eq!(r[0].item.reference, "wiki/page.md", "{arg}");
            assert_eq!(r[0].weight, Weight::Zero, "{arg}");
        }
        if cfg!(any(windows, target_os = "macos")) {
            let r = resolve(&tmp, &["file:Wiki/PAGE.md"]).unwrap();
            assert_eq!(r[0].weight, Weight::Zero);
        }
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_wiki_pages_weigh_zero() {
        let tmp = repo("wikilink");
        fs::write(tmp.wiki().join("page.md"), "# Page\n").unwrap();
        std::os::unix::fs::symlink(tmp.wiki(), tmp.repo().join("alias")).unwrap();
        let r = resolve(&tmp, &["file:alias/page.md"]).unwrap();
        assert_eq!(r[0].weight, Weight::Zero);
        let spelled = Ctx {
            repo_root: fs::canonicalize(tmp.repo()).unwrap(),
            ..tmp.ctx(1024)
        };
        let items = [parsed("file:wiki/page.md")];
        let r = resolve_all(&items, &spelled, &AttestOptions::default()).unwrap();
        assert_eq!(r[0].weight, Weight::Zero);
    }

    #[test]
    fn file_paths_are_stored_portably_and_checked_strictly() {
        let tmp = repo("fileportable");
        fs::create_dir_all(tmp.repo().join("raw")).unwrap();
        fs::write(tmp.repo().join("raw/x.pdf"), "pdf").unwrap();
        let backslash = resolve(&tmp, &["file:raw\\x.pdf"]).unwrap();
        assert_eq!(backslash[0].item.reference, "raw/x.pdf");
        let abs = tmp.repo().join("raw/x.pdf").to_string_lossy().into_owned();
        let absolute = resolve(&tmp, &[&format!("file:{abs}")]).unwrap();
        assert_eq!(absolute[0].item.reference, "raw/x.pdf");
        let value = absolute[0].item.to_value();
        assert_eq!(value.get("path").and_then(Value::as_str), Some("raw/x.pdf"));

        let outside = only_failure(resolve(&tmp, &["file:../x.pdf"]));
        assert_eq!(outside.kind, FailKind::Invalid);
        let elsewhere = std::env::temp_dir().join("loam-evidence-elsewhere.txt");
        fs::write(&elsewhere, "x").unwrap();
        let f = only_failure(resolve(&tmp, &[&format!("file:{}", elsewhere.display())]));
        let _ = fs::remove_file(&elsewhere);
        assert_eq!(f.kind, FailKind::Invalid);

        for bad in ["raw\\x.pdf", "../x.pdf", "/etc/passwd", "C:/x.pdf"] {
            let json = format!(
                "{{\"kind\":\"file\",\"path\":{},\"sha256\":\"{}\"}}",
                Value::String(bad.to_owned()).to_json(),
                "ab".repeat(32)
            );
            assert!(
                Item::from_value(&crate::json::parse(&json).unwrap()).is_err(),
                "{bad}"
            );
            let mut item = absolute[0].item.clone();
            item.reference = bad.to_owned();
            assert_eq!(
                check(&item, &tmp.ctx(1024)).code,
                Code::Unresolvable,
                "{bad}"
            );
        }
    }

    #[test]
    fn commit_ref_must_be_exactly_the_allowed_ref_at_attest() {
        let tmp = repo("commitref");
        let first = git(tmp.repo(), &["rev-parse", "HEAD"]).unwrap();
        git(
            tmp.repo(),
            &["update-ref", "refs/remotes/origin/main", &first],
        )
        .unwrap();
        git(
            tmp.repo(),
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/main",
            ],
        )
        .unwrap();
        fs::write(tmp.repo().join("local.txt"), "x").unwrap();
        git(tmp.repo(), &["add", "local.txt"]).unwrap();
        git(tmp.repo(), &["commit", "-q", "-m", "unpushed"]).unwrap();
        let local_only = git(tmp.repo(), &["rev-parse", "HEAD"]).unwrap();

        let by_default = only_failure(resolve(&tmp, &[&format!("commit:{local_only}")]));
        assert_eq!(by_default.kind, FailKind::Unreachable);
        let by_short = only_failure(resolve(&tmp, &[&format!("commit:{local_only}@main")]));
        assert_eq!(by_short.kind, FailKind::Unreachable);
        let exact = resolve(&tmp, &[&format!("commit:{first}@origin/main")]).unwrap();
        assert_eq!(exact[0].item.git_ref.as_deref(), Some("origin/main"));
        let short_for_pushed = only_failure(resolve(&tmp, &[&format!("commit:{first}@main")]));
        assert_eq!(short_for_pushed.kind, FailKind::Unreachable);
    }

    #[test]
    fn commit_check_applies_the_readers_allowed_ref() {
        let tmp = repo("commitverify");
        git(tmp.repo(), &["checkout", "-q", "-b", "feature"]).unwrap();
        fs::write(tmp.repo().join("b.txt"), "b").unwrap();
        git(tmp.repo(), &["add", "b.txt"]).unwrap();
        git(tmp.repo(), &["commit", "-q", "-m", "branch only"]).unwrap();
        let branch_only = git(tmp.repo(), &["rev-parse", "HEAD"]).unwrap();
        git(tmp.repo(), &["checkout", "-q", "main"]).unwrap();

        let mut item = Item::new(Kind::Commit, branch_only.clone());
        item.git_ref = Some("feature".to_owned());
        let hand_made = check(&item, &tmp.ctx(1024));
        assert_eq!(hand_made.code, Code::Unresolvable);
        assert!(hand_made.detail.contains("not the allowed ref"));

        item.git_ref = Some("main".to_owned());
        assert_eq!(check(&item, &tmp.ctx(1024)).code, Code::Orphaned);
    }

    #[test]
    fn resolved_items_equal_their_json_round_trip() {
        let tmp = repo("roundtrip");
        fs::write(tmp.repo().join("log.txt"), "out").unwrap();
        fs::write(tmp.repo().join("raw.txt"), "raw").unwrap();
        let head = git(tmp.repo(), &["rev-parse", "HEAD"]).unwrap();
        let resolved = resolve(
            &tmp,
            &[
                "capture:log.txt",
                "file:raw.txt",
                "blob:src/a.txt",
                &format!("commit:{head}"),
                "note:n",
            ],
        )
        .unwrap();
        for entry in &resolved {
            let back =
                Item::from_value(&crate::json::parse(&entry.item.to_value().to_json()).unwrap())
                    .unwrap();
            assert_eq!(back, entry.item, "{}", entry.item.label());
        }
        assert_eq!(resolved[0].item.origin.as_deref(), Some("log.txt"));
    }

    #[test]
    fn capture_origin_never_leaks_an_outside_path() {
        let tmp = repo("capturepath");
        let outside = std::env::temp_dir().join("loam-evidence-outside.log");
        fs::write(&outside, "x").unwrap();
        let items = [parse(&format!("capture:{}", outside.display())).unwrap()];
        let r = resolve_all(&items, &tmp.ctx(1024), &AttestOptions::default());
        let _ = fs::remove_file(&outside);
        assert_eq!(
            r.unwrap()[0].item.origin.as_deref(),
            Some("loam-evidence-outside.log")
        );
    }

    #[test]
    fn every_failure_is_reported_not_just_the_first() {
        let tmp = repo("many");
        let failures = resolve(
            &tmp,
            &["blob:nope", "file:nope", "seal:00000000000000000000"],
        )
        .err()
        .unwrap();
        assert_eq!(failures.len(), 3);
    }

    #[test]
    fn item_from_value_rejects_malformed_records() {
        for bad in [
            "{}",
            "{\"kind\":\"cmd\",\"text\":\"x\"}",
            "{\"kind\":\"blob\",\"path\":\"a\"}",
            "{\"kind\":\"file\",\"path\":\"a\",\"sha256\":\"zz\"}",
            "{\"kind\":\"commit\",\"oid\":\"abc\",\"ref\":\"main\"}",
            "{\"kind\":\"note\",\"text\":\"x\",\"recheck\":\"every:bad\"}",
        ] {
            let value = crate::json::parse(bad).unwrap();
            assert!(Item::from_value(&value).is_err(), "{bad}");
        }
    }
}
