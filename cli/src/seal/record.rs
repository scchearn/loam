//! Seal records: in-toto Statement v1 documents stored under `wiki/.seals/`.
//!
//! A record is immutable. Its exact serialised bytes are the record: the id is
//! the first 20 hex characters of `sha256(bytes)`, the optional SSHSIG covers
//! those bytes, and a parsed record is only ever a read-only view. Nothing in
//! this module re-serialises a parsed record, and no code path edits or
//! deletes an existing record: writes are create-only.
//!
//! Store layout (spec D4):
//!
//! - `records/<id>.json`: the exact Statement bytes.
//! - `records/<id>.json.sig`: armored SSHSIG over those bytes.
//! - `evidence/sha256/<hex>`: captured evidence, named by its digest.
//! - `.gitattributes`: `** text eol=lf` plus `evidence/** -text` (captures are
//!   named by the digest of their raw bytes, so git must never normalise
//!   them), written only when the memory tree is git-tracked and only when
//!   absent.

use crate::json::{self, Value};
use crate::seal::canon::CANON_ID;
use crate::sha256;
use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub const STATEMENT_TYPE: &str = "https://in-toto.io/Statement/v1";
pub const PREDICATE_TYPE: &str = "https://github.com/scchearn/loam/seal/v1";
const PREDICATE_PREFIX: &str = "https://github.com/scchearn/loam/seal/v";
pub const PREDICATE_MAJOR: &str = "1";
pub const PURPOSE: &str = "loam.seal.attest";
/// Length of a record id in hex characters.
pub const ID_LEN: usize = 20;
const LOCATOR_EDGE: usize = 32;
const GITATTRIBUTES: &str = "** text eol=lf\nevidence/** -text\n";
const LEVELS: [&str; 3] = ["evidence", "audited", "canonical"];
/// Largest record file `load_all` will read; anything bigger is reported as
/// unreadable rather than pulled into memory.
const MAX_RECORD_BYTES: u64 = 4 * 1024 * 1024;
const STATUS_TYPES: [&str; 4] = ["dispute", "demote", "supersede", "retract"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordKind {
    Seal,
    Update,
    Status,
}

impl RecordKind {
    pub fn as_str(self) -> &'static str {
        match self {
            RecordKind::Seal => "seal",
            RecordKind::Update => "update",
            RecordKind::Status => "status",
        }
    }

    fn parse(value: &str) -> Option<RecordKind> {
        match value {
            "seal" => Some(RecordKind::Seal),
            "update" => Some(RecordKind::Update),
            "status" => Some(RecordKind::Status),
            _ => None,
        }
    }
}

/// Soft locator: used only to find a span whose markers were stripped, never
/// to validate one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Locator {
    pub heading_path: Vec<String>,
    pub prefix: String,
    pub suffix: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Generator {
    pub name: String,
    pub version: String,
    pub harness: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parent {
    pub id: String,
    pub verdict: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusInfo {
    /// `dispute | demote | supersede | retract`.
    pub type_: String,
    /// Id of the record this status record applies to.
    pub updates: String,
    pub reason: String,
}

/// Everything needed to build one record.
#[derive(Debug, Clone)]
pub struct NewRecord {
    pub kind: RecordKind,
    /// Page name (no extension); the subject name is `<page>#^<anchor>`.
    pub page: String,
    pub anchor: String,
    /// `loam.canon/v1` sha256 of the span (64 lowercase hex).
    pub subject_sha256: String,
    pub locator: Locator,
    /// As-sealed canonical content. Omitted from status records.
    pub content: String,
    pub requested_level: Option<String>,
    pub claim: Option<String>,
    pub procedure_uri: String,
    pub principal: String,
    pub acted_on_behalf_of: Option<String>,
    pub generator: Generator,
    pub time_verified: String,
    pub review_by: Option<String>,
    pub evidence: Vec<Value>,
    pub redacted: Vec<Value>,
    pub parent: Option<Parent>,
    pub status: Option<StatusInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordError {
    Malformed(String),
    MissingField(String),
    UnknownPredicate(String),
    UnknownCanon(String),
    IdMismatch { expected: String, actual: String },
    BadTimestamp { field: String, value: String },
}

impl fmt::Display for RecordError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RecordError::Malformed(why) => write!(f, "malformed record: {why}"),
            RecordError::MissingField(name) => write!(f, "missing required field {name}"),
            RecordError::UnknownPredicate(value) => {
                write!(f, "unknown predicateType {value:?}")
            }
            RecordError::UnknownCanon(value) => write!(f, "unknown canon {value:?}"),
            RecordError::IdMismatch { expected, actual } => write!(
                f,
                "record id {expected} does not match its bytes (sha256 prefix {actual})"
            ),
            RecordError::BadTimestamp { field, value } => write!(
                f,
                "{field} {value:?} is not YYYY-MM-DD HH:MM +HH:MM with an explicit offset"
            ),
        }
    }
}

impl std::error::Error for RecordError {}

#[derive(Debug)]
pub enum StoreError {
    /// A record, signature or evidence file with that id already exists.
    Exists(String),
    InvalidId(String),
    /// The bytes are not a well-formed record (malformed input, not I/O).
    Invalid(RecordError),
    Io(String),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::Exists(id) => write!(f, "{id} already exists; records are append-only"),
            StoreError::InvalidId(id) => write!(f, "invalid id or digest {id:?}"),
            StoreError::Invalid(e) => write!(f, "refusing to store record: {e}"),
            StoreError::Io(why) => write!(f, "{why}"),
        }
    }
}

impl std::error::Error for StoreError {}

/// Read-only parsed view of a well-formed record.
#[derive(Debug, Clone)]
pub struct View {
    pub kind: RecordKind,
    pub anchor: String,
    pub subject_name: String,
    pub subject_sha256: String,
    pub content: Option<String>,
    pub requested_level: Option<String>,
    pub claim: Option<String>,
    pub principal: String,
    pub time_verified: String,
    pub review_by: Option<String>,
    pub evidence: Vec<Value>,
    pub redacted: Vec<Value>,
    pub parent: Option<Parent>,
    pub status: Option<StatusInfo>,
    /// The whole document, for fields this module does not model.
    pub document: Value,
}

/// One record file as found on disk.
#[derive(Debug)]
pub struct Loaded {
    /// The id taken from the file name.
    pub id: String,
    pub bytes: Vec<u8>,
    pub view: Result<View, RecordError>,
}

fn string(value: &str) -> Value {
    Value::String(value.to_owned())
}

fn object(entries: Vec<(&str, Value)>) -> Value {
    Value::Object(
        entries
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v))
            .collect(),
    )
}

/// Serialises `record` once and returns `(id, bytes)`. The bytes are checked
/// with [`well_formed`] before being returned, so a builder bug or bad input
/// can never produce a record the loader would reject.
pub fn build(record: &NewRecord) -> Result<(String, Vec<u8>), RecordError> {
    let mut locator = vec![(
        "heading_path",
        Value::Array(
            record
                .locator
                .heading_path
                .iter()
                .map(|h| string(h))
                .collect(),
        ),
    )];
    locator.push(("prefix", string(&record.locator.prefix)));
    locator.push(("suffix", string(&record.locator.suffix)));

    let mut verifier = vec![("principal", string(&record.principal))];
    if let Some(behalf) = &record.acted_on_behalf_of {
        verifier.push(("acted_on_behalf_of", string(behalf)));
    }

    let mut predicate: Vec<(&str, Value)> = vec![
        ("kind", string(record.kind.as_str())),
        ("purpose", string(PURPOSE)),
        ("canon", string(CANON_ID)),
        ("anchor", string(&record.anchor)),
        ("locator", object(locator)),
    ];
    if record.kind != RecordKind::Status {
        predicate.push(("content", string(&record.content)));
    }
    if let Some(level) = &record.requested_level {
        predicate.push(("requested_level", string(level)));
    }
    if let Some(claim) = &record.claim {
        predicate.push(("claim", string(claim)));
    }
    predicate.push((
        "procedure",
        object(vec![("uri", string(&record.procedure_uri))]),
    ));
    predicate.push(("verifier", object(verifier)));
    predicate.push((
        "generator",
        object(vec![
            ("name", string(&record.generator.name)),
            ("version", string(&record.generator.version)),
            ("harness", string(&record.generator.harness)),
        ]),
    ));
    predicate.push(("time_verified", string(&record.time_verified)));
    if let Some(review_by) = &record.review_by {
        predicate.push(("review_by", string(review_by)));
    }
    predicate.push(("evidence", Value::Array(record.evidence.clone())));
    predicate.push(("redacted", Value::Array(record.redacted.clone())));
    if let Some(parent) = &record.parent {
        predicate.push((
            "parent",
            object(vec![
                ("id", string(&parent.id)),
                ("verdict", string(&parent.verdict)),
            ]),
        ));
    }
    if let Some(status) = &record.status {
        predicate.push(("type", string(&status.type_)));
        predicate.push(("updates", string(&status.updates)));
        predicate.push(("reason", string(&status.reason)));
    }

    let statement = object(vec![
        ("_type", string(STATEMENT_TYPE)),
        (
            "subject",
            Value::Array(vec![object(vec![
                (
                    "name",
                    string(&format!("{}#^{}", record.page, record.anchor)),
                ),
                (
                    "digest",
                    object(vec![("sha256", string(&record.subject_sha256))]),
                ),
            ])]),
        ),
        ("predicateType", string(PREDICATE_TYPE)),
        ("predicate", object(predicate)),
    ]);
    let bytes = statement.to_json().into_bytes();
    let id = id_of(&bytes);
    well_formed(&id, &bytes)?;
    Ok((id, bytes))
}

/// First 20 hex characters of `sha256(bytes)`.
pub fn id_of(bytes: &[u8]) -> String {
    sha256::hex(bytes)[..ID_LEN].to_owned()
}

fn is_lower_hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn is_id(value: &str) -> bool {
    is_lower_hex(value, ID_LEN)
}

fn text<'a>(value: &'a Value, path: &str, key: &str) -> Result<&'a str, RecordError> {
    let name = format!("{path}{key}");
    match value.get(key) {
        None | Some(Value::Null) => Err(RecordError::MissingField(name)),
        Some(Value::String(s)) => Ok(s),
        Some(_) => Err(RecordError::Malformed(format!("{name} must be a string"))),
    }
}

fn optional_text(value: &Value, path: &str, key: &str) -> Result<Option<String>, RecordError> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(RecordError::Malformed(format!(
            "{path}{key} must be a string"
        ))),
    }
}

fn nonempty<'a>(value: &'a str, name: &str) -> Result<&'a str, RecordError> {
    if value.trim().is_empty() {
        Err(RecordError::MissingField(name.to_owned()))
    } else {
        Ok(value)
    }
}

fn array(value: &Value, key: &str) -> Result<Vec<Value>, RecordError> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => Ok(items.clone()),
        Some(_) => Err(RecordError::Malformed(format!(
            "predicate.{key} must be an array"
        ))),
    }
}

/// Rejects any object that repeats a key. `Value::get` is first-wins while
/// most other JSON tooling is last-wins, so an ambiguous signed record could be
/// read two ways.
fn reject_duplicate_keys(value: &Value, path: &str) -> Result<(), RecordError> {
    match value {
        Value::Object(entries) => {
            for (i, (key, child)) in entries.iter().enumerate() {
                if entries[..i].iter().any(|(earlier, _)| earlier == key) {
                    return Err(RecordError::Malformed(format!(
                        "duplicate key {key:?} in {}",
                        if path.is_empty() { "<root>" } else { path }
                    )));
                }
                reject_duplicate_keys(child, &format!("{path}/{key}"))?;
            }
            Ok(())
        }
        Value::Array(items) => items
            .iter()
            .enumerate()
            .try_for_each(|(i, child)| reject_duplicate_keys(child, &format!("{path}/{i}"))),
        _ => Ok(()),
    }
}

/// Validates `bytes` as the record stored under `id` and returns a parsed
/// view. Unknown fields are ignored.
pub fn well_formed(id: &str, bytes: &[u8]) -> Result<View, RecordError> {
    let actual = id_of(bytes);
    if actual != id {
        return Err(RecordError::IdMismatch {
            expected: id.to_owned(),
            actual,
        });
    }
    let source = std::str::from_utf8(bytes)
        .map_err(|e| RecordError::Malformed(format!("not valid UTF-8 ({e})")))?;
    let document = json::parse(source).map_err(RecordError::Malformed)?;
    if !matches!(document, Value::Object(_)) {
        return Err(RecordError::Malformed(
            "top level is not an object".to_owned(),
        ));
    }
    reject_duplicate_keys(&document, "")?;
    let statement_type = text(&document, "", "_type")?;
    if statement_type != STATEMENT_TYPE {
        return Err(RecordError::Malformed(format!(
            "_type {statement_type:?} is not {STATEMENT_TYPE}"
        )));
    }
    let predicate_type = text(&document, "", "predicateType")?;
    let major = predicate_type
        .strip_prefix(PREDICATE_PREFIX)
        .and_then(|rest| rest.split('.').next());
    if major != Some(PREDICATE_MAJOR) {
        return Err(RecordError::UnknownPredicate(predicate_type.to_owned()));
    }

    // Exactly one subject: other in-toto tooling treats every subject as
    // attested, so extra entries would be read two ways.
    let subjects = document
        .get("subject")
        .and_then(Value::as_array)
        .ok_or_else(|| RecordError::MissingField("subject".to_owned()))?;
    if subjects.len() != 1 {
        return Err(RecordError::Malformed(format!(
            "subject must have exactly one entry, found {}",
            subjects.len()
        )));
    }
    let subject = &subjects[0];
    let subject_name =
        nonempty(text(subject, "subject[0].", "name")?, "subject[0].name")?.to_owned();
    let digest = subject
        .get("digest")
        .ok_or_else(|| RecordError::MissingField("subject[0].digest".to_owned()))?;
    let subject_sha256 = text(digest, "subject[0].digest.", "sha256")?.to_owned();
    if !is_lower_hex(&subject_sha256, 64) {
        return Err(RecordError::Malformed(
            "subject[0].digest.sha256 is not 64 lowercase hex".to_owned(),
        ));
    }

    let p = document
        .get("predicate")
        .ok_or_else(|| RecordError::MissingField("predicate".to_owned()))?;
    if !matches!(p, Value::Object(_)) {
        return Err(RecordError::Malformed(
            "predicate is not an object".to_owned(),
        ));
    }
    let kind_text = text(p, "predicate.", "kind")?;
    let kind = RecordKind::parse(kind_text)
        .ok_or_else(|| RecordError::Malformed(format!("unknown kind {kind_text:?}")))?;
    let purpose = text(p, "predicate.", "purpose")?;
    if purpose != PURPOSE {
        return Err(RecordError::Malformed(format!(
            "purpose {purpose:?} is not {PURPOSE}"
        )));
    }
    let canon = text(p, "predicate.", "canon")?;
    if canon != CANON_ID {
        return Err(RecordError::UnknownCanon(canon.to_owned()));
    }
    let anchor = nonempty(text(p, "predicate.", "anchor")?, "predicate.anchor")?.to_owned();
    if subject_name.rsplit_once("#^").map(|(_, a)| a) != Some(anchor.as_str()) {
        return Err(RecordError::Malformed(format!(
            "subject[0].name {subject_name:?} does not end in #^{anchor}"
        )));
    }
    // The soft locator is what recovers a span whose markers were stripped.
    match p.get("locator") {
        None | Some(Value::Null) => {
            return Err(RecordError::MissingField("predicate.locator".to_owned()))
        }
        Some(Value::Object(_)) => {}
        Some(_) => {
            return Err(RecordError::Malformed(
                "predicate.locator must be an object".to_owned(),
            ))
        }
    }

    // Provenance fields the writer always emits; required on read so a record
    // cannot silently drop them.
    let procedure = p
        .get("procedure")
        .ok_or_else(|| RecordError::MissingField("predicate.procedure".to_owned()))?;
    nonempty(
        text(procedure, "predicate.procedure.", "uri")?,
        "predicate.procedure.uri",
    )?;
    let generator = p
        .get("generator")
        .ok_or_else(|| RecordError::MissingField("predicate.generator".to_owned()))?;
    for key in ["name", "version", "harness"] {
        text(generator, "predicate.generator.", key)?;
    }

    let verifier = p
        .get("verifier")
        .ok_or_else(|| RecordError::MissingField("predicate.verifier".to_owned()))?;
    let principal = nonempty(
        text(verifier, "predicate.verifier.", "principal")?,
        "predicate.verifier.principal",
    )?
    .to_owned();

    let time_verified = text(p, "predicate.", "time_verified")?.to_owned();
    if !valid_timestamp(&time_verified) {
        return Err(RecordError::BadTimestamp {
            field: "time_verified".to_owned(),
            value: time_verified,
        });
    }
    let review_by = optional_text(p, "predicate.", "review_by")?;
    if let Some(value) = &review_by {
        if !valid_timestamp(value) {
            return Err(RecordError::BadTimestamp {
                field: "review_by".to_owned(),
                value: value.clone(),
            });
        }
    }

    let content = optional_text(p, "predicate.", "content")?;
    let requested_level = optional_text(p, "predicate.", "requested_level")?;
    let claim = optional_text(p, "predicate.", "claim")?;
    if let Some(level) = &requested_level {
        if !LEVELS.contains(&level.as_str()) {
            return Err(RecordError::Malformed(format!(
                "requested_level {level:?} is not one of evidence, audited, canonical"
            )));
        }
    }
    match kind {
        RecordKind::Seal => {
            if content.is_none() {
                return Err(RecordError::MissingField("predicate.content".to_owned()));
            }
            if requested_level.is_none() {
                return Err(RecordError::MissingField(
                    "predicate.requested_level".to_owned(),
                ));
            }
            if claim.is_none() {
                return Err(RecordError::MissingField("predicate.claim".to_owned()));
            }
        }
        RecordKind::Update => {
            if content.is_none() {
                return Err(RecordError::MissingField("predicate.content".to_owned()));
            }
        }
        RecordKind::Status => {}
    }

    let parent = match p.get("parent") {
        None | Some(Value::Null) => None,
        Some(parent) => {
            let id = text(parent, "predicate.parent.", "id")?.to_owned();
            if !is_id(&id) {
                return Err(RecordError::Malformed(
                    "predicate.parent.id is not a record id".to_owned(),
                ));
            }
            Some(Parent {
                id,
                verdict: text(parent, "predicate.parent.", "verdict")?.to_owned(),
            })
        }
    };

    let status = match kind {
        RecordKind::Status => {
            let type_ = text(p, "predicate.", "type")?.to_owned();
            if !STATUS_TYPES.contains(&type_.as_str()) {
                return Err(RecordError::Malformed(format!(
                    "unknown status type {type_:?}"
                )));
            }
            let updates = text(p, "predicate.", "updates")?.to_owned();
            if !is_id(&updates) {
                return Err(RecordError::Malformed(
                    "predicate.updates is not a record id".to_owned(),
                ));
            }
            let reason = nonempty(text(p, "predicate.", "reason")?, "predicate.reason")?.to_owned();
            Some(StatusInfo {
                type_,
                updates,
                reason,
            })
        }
        _ => None,
    };

    Ok(View {
        kind,
        anchor,
        subject_name,
        subject_sha256,
        content,
        requested_level,
        claim,
        principal,
        time_verified,
        review_by,
        evidence: array(p, "evidence")?,
        redacted: array(p, "redacted")?,
        parent,
        status,
        document,
    })
}

/// `YYYY-MM-DD HH:MM ±HH:MM`, with a real calendar date and an explicit
/// canonical offset. Offset-less and `±HHMM` stamps are rejected.
pub fn valid_timestamp(value: &str) -> bool {
    let b = value.as_bytes();
    if b.len() != 23 || !value.is_ascii() {
        return false;
    }
    let digits = |range: std::ops::Range<usize>| b[range].iter().all(u8::is_ascii_digit);
    let num = |range: std::ops::Range<usize>| value[range].parse::<u32>().unwrap_or(u32::MAX);
    if !(digits(0..4)
        && b[4] == b'-'
        && digits(5..7)
        && b[7] == b'-'
        && digits(8..10)
        && b[10] == b' '
        && digits(11..13)
        && b[13] == b':'
        && digits(14..16)
        && b[16] == b' '
        && matches!(b[17], b'+' | b'-')
        && digits(18..20)
        && b[20] == b':'
        && digits(21..23))
    {
        return false;
    }
    let (year, month, day) = (num(0..4), num(5..7), num(8..10));
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    day >= 1
        && day <= days
        && num(11..13) < 24
        && num(14..16) < 60
        && num(18..20) < 24
        && num(21..23) < 60
}

/// Removes a CommonMark closing `#` sequence, which only counts when preceded
/// by whitespace (or is the whole title).
fn strip_closing_hashes(title: &str) -> &str {
    let without = title.trim_end_matches('#');
    if without.len() == title.len() {
        return title;
    }
    if without.is_empty() {
        return "";
    }
    if without.ends_with([' ', '\t']) {
        without.trim_end()
    } else {
        title
    }
}

/// Heading text trail (outermost first) in force at byte `offset` of `source`.
/// ATX headings only; fenced code blocks are skipped.
pub fn heading_path(source: &str, offset: usize) -> Vec<String> {
    let mut stack: Vec<(usize, String)> = Vec::new();
    let mut fence: Option<(char, usize)> = None;
    let mut position = 0;
    for line in source.split_inclusive('\n') {
        if position >= offset {
            break;
        }
        position += line.len();
        let trimmed = line.trim_end_matches(['\n', '\r']);
        let indent = trimmed.len() - trimmed.trim_start_matches(' ').len();
        if indent > 3 {
            continue;
        }
        let body = &trimmed[indent..];
        let marker = body.chars().next().filter(|c| matches!(c, '`' | '~'));
        if let Some(c) = marker {
            let run = body.chars().take_while(|x| *x == c).count();
            if run >= 3 {
                match fence {
                    None => {
                        fence = Some((c, run));
                        continue;
                    }
                    Some((open, len))
                        if open == c && run >= len && body[run..].trim().is_empty() =>
                    {
                        fence = None;
                        continue;
                    }
                    _ => {}
                }
            }
        }
        if fence.is_some() {
            continue;
        }
        let level = body.chars().take_while(|c| *c == '#').count();
        if (1..=6).contains(&level) {
            let rest = &body[level..];
            if rest.is_empty() || rest.starts_with(' ') || rest.starts_with('\t') {
                let title = strip_closing_hashes(rest.trim()).to_owned();
                while stack.last().is_some_and(|(l, _)| *l >= level) {
                    stack.pop();
                }
                stack.push((level, title));
            }
        }
    }
    stack.into_iter().map(|(_, title)| title).collect()
}

/// Builds the soft locator: heading path plus the first and last 32
/// characters of the canonical content.
pub fn soft_locator(heading_path: &[String], canonical_content: &str) -> Locator {
    let trimmed = canonical_content.trim();
    let chars: Vec<char> = trimmed.chars().collect();
    let prefix: String = chars.iter().take(LOCATOR_EDGE).collect();
    let skip = chars.len().saturating_sub(LOCATOR_EDGE);
    let suffix: String = chars.iter().skip(skip).collect();
    Locator {
        heading_path: heading_path.to_vec(),
        prefix,
        suffix,
    }
}

pub fn seals_dir(wiki: &Path) -> PathBuf {
    wiki.join(".seals")
}

pub fn records_dir(wiki: &Path) -> PathBuf {
    seals_dir(wiki).join("records")
}

pub fn record_path(wiki: &Path, id: &str) -> PathBuf {
    records_dir(wiki).join(format!("{id}.json"))
}

pub fn sig_path(wiki: &Path, id: &str) -> PathBuf {
    records_dir(wiki).join(format!("{id}.json.sig"))
}

pub fn evidence_path(wiki: &Path, hex: &str) -> PathBuf {
    seals_dir(wiki).join("evidence").join("sha256").join(hex)
}

fn io_error(context: &Path, error: io::Error) -> StoreError {
    StoreError::Io(format!("{}: {error}", context.display()))
}

static TMP_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Writes `bytes` to `path`, failing with `Exists(name)` when it is there.
///
/// The bytes go to a unique dot-prefixed temp file in the same directory, are
/// synced, and are then published with a hard link, which fails atomically
/// with `AlreadyExists`. A reader therefore never sees a partial file at the
/// final name, and a crash leaves at worst a stray temp file that `load_all`
/// ignores. When hard links are unsupported it degrades to `create_new` on the
/// final name (see `create_new_direct`).
fn create_only(path: &Path, bytes: &[u8], name: &str) -> Result<(), StoreError> {
    create_only_with(path, bytes, name, |from, to| fs::hard_link(from, to))
}

/// Fallback publish for filesystems without hard links: `create_new` on the
/// final name keeps the create-only and append-only guarantees and gives up
/// only atomic visibility. A failed write removes the file this call created
/// (and only that one).
fn create_new_direct(path: &Path, bytes: &[u8], name: &str) -> Result<(), StoreError> {
    let mut file = match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            return Err(StoreError::Exists(name.to_owned()))
        }
        Err(e) => return Err(io_error(path, e)),
    };
    let written = file
        .write_all(bytes)
        .and_then(|()| file.flush())
        .and_then(|()| file.sync_all());
    drop(file);
    if let Err(e) = written {
        let _ = fs::remove_file(path);
        return Err(io_error(path, e));
    }
    Ok(())
}

fn create_only_with(
    path: &Path,
    bytes: &[u8],
    name: &str,
    link: impl Fn(&Path, &Path) -> io::Result<()>,
) -> Result<(), StoreError> {
    let parent = path
        .parent()
        .ok_or_else(|| StoreError::Io(format!("{}: no parent directory", path.display())))?;
    fs::create_dir_all(parent).map_err(|e| io_error(parent, e))?;
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| StoreError::Io(format!("{}: unusable file name", path.display())))?;
    let tmp = parent.join(format!(
        ".{file_name}.{}.{}.tmp",
        std::process::id(),
        TMP_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let result = (|| -> Result<(), StoreError> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .map_err(|e| io_error(&tmp, e))?;
        file.write_all(bytes)
            .and_then(|()| file.flush())
            .and_then(|()| file.sync_all())
            .map_err(|e| io_error(&tmp, e))?;
        drop(file);
        match link(&tmp, path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                Err(StoreError::Exists(name.to_owned()))
            }
            // No hard links here (exFAT/FAT, some SMB/FUSE and cloud-drive
            // mounts). Fall back to create-only on the final name.
            Err(_) => create_new_direct(path, bytes, name),
        }
    })();
    // The temp file is ours in every outcome; the final name is never removed.
    let _ = fs::remove_file(&tmp);
    result
}

fn git_succeeds(dir: &Path, args: &[&str]) -> Option<bool> {
    Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()
        .and_then(|status| status.code().map(|code| (code, status.success())))
        .and_then(|(code, ok)| {
            if code == 0 || code == 1 {
                Some(ok)
            } else {
                None
            }
        })
}

/// True when the memory tree lives in a git work tree and is not ignored, so
/// its `.seals/` content would be committed. Any git failure (no git, not a
/// repository, bare repository) counts as untracked, never as an error.
pub fn is_git_tracked(wiki: &Path) -> bool {
    if !wiki.is_dir() {
        return false;
    }
    if git_succeeds(wiki, &["rev-parse", "--is-inside-work-tree"]) != Some(true) {
        return false;
    }
    git_succeeds(wiki, &["check-ignore", "-q", "--", "./.seals/records/x"]) == Some(false)
}

fn ensure_gitattributes(wiki: &Path) -> Result<(), StoreError> {
    if !is_git_tracked(wiki) {
        return Ok(());
    }
    match create_only(
        &seals_dir(wiki).join(".gitattributes"),
        GITATTRIBUTES.as_bytes(),
        ".gitattributes",
    ) {
        Ok(()) | Err(StoreError::Exists(_)) => Ok(()),
        Err(e) => Err(e),
    }
}

/// Stores `bytes` as `records/<id>.json`, create-only, after verifying they
/// are a well-formed record. Returns the id.
pub fn write_record(wiki: &Path, bytes: &[u8]) -> Result<String, StoreError> {
    let id = id_of(bytes);
    well_formed(&id, bytes).map_err(StoreError::Invalid)?;
    ensure_gitattributes(wiki)?;
    create_only(&record_path(wiki, &id), bytes, &id)?;
    Ok(id)
}

/// Stores an armored SSHSIG next to its record, create-only.
pub fn write_sig(wiki: &Path, id: &str, armored: &[u8]) -> Result<(), StoreError> {
    if !is_id(id) {
        return Err(StoreError::InvalidId(id.to_owned()));
    }
    ensure_gitattributes(wiki)?;
    create_only(&sig_path(wiki, id), armored, &format!("{id}.json.sig"))
}

/// Stores captured evidence under its digest. `hex` must be the sha256 of
/// `bytes`; a mismatch or a non-hex name is refused so the name can never
/// escape the evidence directory.
pub fn write_evidence(wiki: &Path, hex: &str, bytes: &[u8]) -> Result<(), StoreError> {
    if !is_lower_hex(hex, 64) || sha256::hex(bytes) != hex {
        return Err(StoreError::InvalidId(hex.to_owned()));
    }
    ensure_gitattributes(wiki)?;
    create_only(&evidence_path(wiki, hex), bytes, hex)
}

/// Loads every `records/*.json`, sorted by id. A record that fails
/// well-formedness is returned with its error rather than dropped, so callers
/// can report it.
pub fn load_all(wiki: &Path) -> Result<Vec<Loaded>, String> {
    let dir = records_dir(wiki);
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("{}: {e}", dir.display())),
    };
    let mut loaded = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| format!("{}: {e}", dir.display()))?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(id) = name.strip_suffix(".json") else {
            continue;
        };
        let path = entry.path();
        let read = match fs::metadata(&path) {
            Ok(meta) if meta.is_dir() => continue,
            Ok(meta) if meta.len() > MAX_RECORD_BYTES => Err(format!(
                "{} bytes exceeds the {MAX_RECORD_BYTES}-byte record cap",
                meta.len()
            )),
            Ok(_) => fs::read(&path).map_err(|e| e.to_string()),
            Err(e) => Err(e.to_string()),
        };
        let (bytes, view) = match read {
            Ok(bytes) => {
                let view = well_formed(id, &bytes);
                (bytes, view)
            }
            Err(why) => (
                Vec::new(),
                Err(RecordError::Malformed(format!("unreadable: {why}"))),
            ),
        };
        loaded.push(Loaded {
            id: id.to_owned(),
            bytes,
            view,
        });
    }
    loaded.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(loaded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    struct Tmp(PathBuf);
    impl Tmp {
        fn new(label: &str) -> Tmp {
            let dir =
                std::env::temp_dir().join(format!("loam-record-{label}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Tmp(dir)
        }
        fn wiki(&self) -> PathBuf {
            let wiki = self.0.join("wiki");
            fs::create_dir_all(&wiki).unwrap();
            wiki
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            for _ in 0..5 {
                if fs::remove_dir_all(&self.0).is_ok() {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    }

    fn git(dir: &Path, args: &[&str]) -> bool {
        Command::new("git")
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
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn sample() -> NewRecord {
        NewRecord {
            kind: RecordKind::Seal,
            page: "federation-tls-trust".to_owned(),
            anchor: "s-tls-rung-order".to_owned(),
            subject_sha256: "ab".repeat(32),
            locator: Locator {
                heading_path: vec!["Trust rung order".to_owned()],
                prefix: "The connector builds".to_owned(),
                suffix: "platform store.".to_owned(),
            },
            content: "line one\nline \"two\" \u{e9}\n".to_owned(),
            requested_level: Some("audited".to_owned()),
            claim: Some("Trust order is env, bundled, platform".to_owned()),
            procedure_uri: "loam://seal/procedure/v1".to_owned(),
            principal: "agent.abc@local".to_owned(),
            acted_on_behalf_of: Some("sam".to_owned()),
            generator: Generator {
                name: "loam".to_owned(),
                version: "1.0.0".to_owned(),
                harness: "claude".to_owned(),
            },
            time_verified: "2026-11-20 14:30 +02:00".to_owned(),
            review_by: Some("2027-05-19 14:30 +02:00".to_owned()),
            evidence: vec![],
            redacted: vec![],
            parent: None,
            status: None,
        }
    }

    #[test]
    fn built_record_round_trips_byte_for_byte() {
        let tmp = Tmp::new("roundtrip");
        let wiki = tmp.wiki();
        let (id, bytes) = build(&sample()).unwrap();
        assert_eq!(id.len(), 20);
        assert_eq!(id, crate::sha256::hex(&bytes)[..20]);
        assert_eq!(write_record(&wiki, &bytes).unwrap(), id);
        let path = records_dir(&wiki).join(format!("{id}.json"));
        assert_eq!(fs::read(&path).unwrap(), bytes);
        let loaded = load_all(&wiki).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, id);
        assert_eq!(loaded[0].bytes, bytes);
        let view = loaded[0].view.as_ref().unwrap();
        assert_eq!(view.kind, RecordKind::Seal);
        assert_eq!(view.anchor, "s-tls-rung-order");
        assert_eq!(view.principal, "agent.abc@local");
        assert_eq!(view.subject_name, "federation-tls-trust#^s-tls-rung-order");
        assert_eq!(
            view.content.as_deref(),
            Some("line one\nline \"two\" \u{e9}\n")
        );
    }

    #[test]
    fn statement_shape_matches_in_toto_v1() {
        let (_, bytes) = build(&sample()).unwrap();
        let v = crate::json::parse(std::str::from_utf8(&bytes).unwrap()).unwrap();
        assert_eq!(
            v.get("_type").unwrap().as_str(),
            Some("https://in-toto.io/Statement/v1")
        );
        assert_eq!(
            v.get("predicateType").unwrap().as_str(),
            Some("https://github.com/scchearn/loam/seal/v1")
        );
        let subject = v.get("subject").unwrap().at(0).unwrap();
        assert_eq!(
            subject.get("name").unwrap().as_str(),
            Some("federation-tls-trust#^s-tls-rung-order")
        );
        let p = v.get("predicate").unwrap();
        assert_eq!(p.get("purpose").unwrap().as_str(), Some("loam.seal.attest"));
        assert_eq!(p.get("canon").unwrap().as_str(), Some("loam.canon/v1"));
        assert_eq!(p.get("kind").unwrap().as_str(), Some("seal"));
    }

    #[test]
    fn build_is_deterministic() {
        assert_eq!(build(&sample()).unwrap(), build(&sample()).unwrap());
    }

    #[test]
    fn write_is_create_only() {
        let tmp = Tmp::new("createonly");
        let wiki = tmp.wiki();
        let (id, bytes) = build(&sample()).unwrap();
        write_record(&wiki, &bytes).unwrap();
        let err = write_record(&wiki, &bytes).unwrap_err();
        assert!(
            matches!(err, StoreError::Exists(ref e) if *e == id),
            "{err}"
        );
        write_sig(&wiki, &id, b"sig").unwrap();
        assert!(matches!(
            write_sig(&wiki, &id, b"x"),
            Err(StoreError::Exists(_))
        ));
        assert_eq!(fs::read(sig_path(&wiki, &id)).unwrap(), b"sig");
    }

    #[test]
    fn renamed_record_fails_well_formedness() {
        let tmp = Tmp::new("renamed");
        let wiki = tmp.wiki();
        let (id, bytes) = build(&sample()).unwrap();
        write_record(&wiki, &bytes).unwrap();
        let other = "0".repeat(20);
        fs::rename(
            records_dir(&wiki).join(format!("{id}.json")),
            records_dir(&wiki).join(format!("{other}.json")),
        )
        .unwrap();
        let loaded = load_all(&wiki).unwrap();
        assert_eq!(loaded[0].bytes, bytes);
        assert!(matches!(
            loaded[0].view,
            Err(RecordError::IdMismatch { .. })
        ));
    }

    #[test]
    fn edited_record_fails_well_formedness() {
        let tmp = Tmp::new("edited");
        let wiki = tmp.wiki();
        let (id, bytes) = build(&sample()).unwrap();
        write_record(&wiki, &bytes).unwrap();
        let mut edited = bytes.clone();
        edited.push(b'\n');
        fs::write(records_dir(&wiki).join(format!("{id}.json")), &edited).unwrap();
        let loaded = load_all(&wiki).unwrap();
        assert!(matches!(
            loaded[0].view,
            Err(RecordError::IdMismatch { .. })
        ));
    }

    fn with_field(bytes: &[u8], from: &str, to: &str) -> Vec<u8> {
        let text = std::str::from_utf8(bytes).unwrap();
        assert!(text.contains(from), "{from} missing from {text}");
        text.replacen(from, to, 1).into_bytes()
    }

    fn check(bytes: &[u8]) -> Result<View, RecordError> {
        well_formed(&crate::sha256::hex(bytes)[..20], bytes)
    }

    #[test]
    fn unknown_predicate_major_canon_and_statement_are_rejected() {
        let (_, bytes) = build(&sample()).unwrap();
        let v2 = with_field(&bytes, "seal/v1", "seal/v2");
        assert!(matches!(check(&v2), Err(RecordError::UnknownPredicate(_))));
        let canon = with_field(&bytes, "loam.canon/v1", "loam.canon/v2");
        assert!(matches!(check(&canon), Err(RecordError::UnknownCanon(_))));
        let stmt = with_field(&bytes, "Statement/v1", "Statement/v0.1");
        assert!(matches!(check(&stmt), Err(RecordError::Malformed(_))));
        // A minor revision of the same major is accepted.
        let minor = with_field(&bytes, "seal/v1", "seal/v1.2");
        assert!(check(&minor).is_ok());
    }

    #[test]
    fn missing_required_fields_are_rejected() {
        let (_, bytes) = build(&sample()).unwrap();
        for (from, to) in [
            ("\"anchor\"", "\"anchorx\""),
            ("\"time_verified\"", "\"tv\""),
            ("\"principal\"", "\"pr\""),
            ("\"claim\"", "\"clm\""),
            ("\"subject\"", "\"subj\""),
            ("\"requested_level\"", "\"rl\""),
        ] {
            let broken = with_field(&bytes, from, to);
            assert!(
                matches!(check(&broken), Err(RecordError::MissingField(_))),
                "{from}: {:?}",
                check(&broken)
            );
        }
    }

    #[test]
    fn extra_subjects_and_missing_provenance_are_rejected() {
        let (_, bytes) = build(&sample()).unwrap();
        let two = with_field(
            &bytes,
            "\"subject\":[{",
            "\"subject\":[{\"name\":\"a#^b\",\"digest\":{}},{",
        );
        assert!(
            matches!(check(&two), Err(RecordError::Malformed(_))),
            "{:?}",
            check(&two)
        );
        let none = with_field(&bytes, "\"subject\":[", "\"subject\":[],\"x\":[");
        assert!(check(&none).is_err());
        for (from, to) in [
            ("\"procedure\"", "\"proc\""),
            ("\"generator\"", "\"gen\""),
            ("\"harness\"", "\"h\""),
            ("\"uri\"", "\"u\""),
        ] {
            let broken = with_field(&bytes, from, to);
            assert!(
                matches!(check(&broken), Err(RecordError::MissingField(_))),
                "{from}: {:?}",
                check(&broken)
            );
        }
    }

    #[test]
    fn malformed_record_is_a_typed_invalid_error() {
        let tmp = Tmp::new("invalid");
        let err = write_record(&tmp.wiki(), b"{}").unwrap_err();
        assert!(matches!(err, StoreError::Invalid(_)), "{err}");
    }

    #[test]
    fn link_failure_falls_back_to_create_new_and_stays_create_only() {
        let tmp = Tmp::new("nolink");
        let path = tmp.wiki().join("out").join("file");
        let no_link =
            |_: &Path, _: &Path| Err(io::Error::new(io::ErrorKind::Unsupported, "no hard links"));
        create_only_with(&path, b"one", "file", &no_link).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"one");
        let err = create_only_with(&path, b"two", "file", &no_link).unwrap_err();
        assert!(matches!(err, StoreError::Exists(_)), "{err}");
        assert_eq!(fs::read(&path).unwrap(), b"one");
        let strays: Vec<_> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(strays.len(), 1, "temp file left behind");
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let (_, bytes) = build(&sample()).unwrap();
        let extra = with_field(
            &bytes,
            "\"predicate\":{",
            "\"predicate\":{\"future\":{\"x\":1},",
        );
        assert!(check(&extra).is_ok());
        let top = with_field(&bytes, "{\"_type\"", "{\"zzz\":true,\"_type\"");
        assert!(check(&top).is_ok());
    }

    #[test]
    fn timestamps_require_canonical_colon_offset() {
        let (_, bytes) = build(&sample()).unwrap();
        for bad in [
            "2026-11-20 14:30",
            "2026-11-20 14:30 +0200",
            "2026-11-20T14:30:00+02:00",
            "2026-13-20 14:30 +02:00",
            "2026-02-30 14:30 +02:00",
            "2026-11-20 24:30 +02:00",
            "2026-11-20 14:60 +02:00",
            "2026-11-20 14:30 +02:60",
            "2026-11-20 14:30 Z",
            "",
        ] {
            let broken = with_field(&bytes, "2026-11-20 14:30 +02:00", bad);
            assert!(
                matches!(check(&broken), Err(RecordError::BadTimestamp { .. })),
                "{bad:?} -> {:?}",
                check(&broken)
            );
        }
        for good in [
            "2026-11-20 14:30 -05:00",
            "2028-02-29 00:00 +00:00",
            "2026-11-20 14:30 +05:30",
        ] {
            assert!(
                check(&with_field(&bytes, "2026-11-20 14:30 +02:00", good)).is_ok(),
                "{good}"
            );
        }
        let bad_review = with_field(&bytes, "2027-05-19 14:30 +02:00", "2027-05-19 14:30");
        assert!(matches!(
            check(&bad_review),
            Err(RecordError::BadTimestamp { .. })
        ));
    }

    #[test]
    fn build_rejects_invalid_input() {
        let mut r = sample();
        r.time_verified = "2026-11-20 14:30".to_owned();
        assert!(matches!(build(&r), Err(RecordError::BadTimestamp { .. })));
        let mut r = sample();
        r.subject_sha256 = "xyz".to_owned();
        assert!(build(&r).is_err());
        let mut r = sample();
        r.claim = None;
        assert!(matches!(build(&r), Err(RecordError::MissingField(_))));
    }

    #[test]
    fn status_records_need_type_updates_and_reason() {
        let mut r = sample();
        r.kind = RecordKind::Status;
        r.content = String::new();
        r.requested_level = None;
        r.claim = None;
        assert!(matches!(build(&r), Err(RecordError::MissingField(_))));
        r.status = Some(StatusInfo {
            type_: "dispute".to_owned(),
            updates: "0123456789abcdef0123".to_owned(),
            reason: "outdated".to_owned(),
        });
        let (_, bytes) = build(&r).unwrap();
        let view = check(&bytes).unwrap();
        assert_eq!(view.kind, RecordKind::Status);
        assert_eq!(view.status.as_ref().unwrap().type_, "dispute");
        r.status.as_mut().unwrap().type_ = "bogus".to_owned();
        assert!(build(&r).is_err());
        r.status.as_mut().unwrap().type_ = "retract".to_owned();
        r.status.as_mut().unwrap().updates = "../x".to_owned();
        assert!(build(&r).is_err());
    }

    #[test]
    fn parent_evidence_and_redacted_round_trip() {
        let mut r = sample();
        r.parent = Some(Parent {
            id: "0123456789abcdef0123".to_owned(),
            verdict: "drifted".to_owned(),
        });
        r.evidence =
            vec![
                crate::json::parse("{\"kind\":\"file\",\"path\":\"a\",\"sha256\":\"00\"}").unwrap(),
            ];
        r.redacted = vec![crate::json::Value::String("token".to_owned())];
        let (_, bytes) = build(&r).unwrap();
        let view = check(&bytes).unwrap();
        assert_eq!(view.parent.as_ref().unwrap().id, "0123456789abcdef0123");
        assert_eq!(view.evidence.len(), 1);
    }

    #[test]
    fn evidence_and_sig_paths_validate_ids() {
        let tmp = Tmp::new("evidence");
        let wiki = tmp.wiki();
        let hex = crate::sha256::hex(b"capture");
        write_evidence(&wiki, &hex, b"capture").unwrap();
        assert_eq!(fs::read(evidence_path(&wiki, &hex)).unwrap(), b"capture");
        assert!(matches!(
            write_evidence(&wiki, &hex, b"capture"),
            Err(StoreError::Exists(_))
        ));
        assert!(matches!(
            write_evidence(&wiki, "../escape", b"x"),
            Err(StoreError::InvalidId(_))
        ));
        assert!(matches!(
            write_sig(&wiki, "../x", b"x"),
            Err(StoreError::InvalidId(_))
        ));
        assert!(matches!(
            write_evidence(&wiki, &crate::sha256::hex(b"other"), b"mismatch"),
            Err(StoreError::InvalidId(_))
        ));
    }

    #[test]
    fn load_all_on_missing_store_is_empty_and_skips_sigs() {
        let tmp = Tmp::new("empty");
        let wiki = tmp.wiki();
        assert!(load_all(&wiki).unwrap().is_empty());
        let (id, bytes) = build(&sample()).unwrap();
        write_record(&wiki, &bytes).unwrap();
        write_sig(&wiki, &id, b"sig").unwrap();
        fs::write(records_dir(&wiki).join("notes.txt"), "x").unwrap();
        let loaded = load_all(&wiki).unwrap();
        assert_eq!(loaded.len(), 1);
    }

    #[test]
    fn untracked_memory_tree_writes_no_gitattributes_and_no_error() {
        let tmp = Tmp::new("untracked");
        let wiki = tmp.wiki();
        // A TMPDIR inside some repository would make the tree tracked.
        if git_succeeds(&wiki, &["rev-parse", "--is-inside-work-tree"]) == Some(true) {
            return;
        }
        // Not a git repository at all.
        let (_, bytes) = build(&sample()).unwrap();
        write_record(&wiki, &bytes).unwrap();
        assert!(!seals_dir(&wiki).join(".gitattributes").exists());
    }

    #[test]
    fn tracked_wiki_gets_gitattributes_once_and_keeps_existing() {
        if !git(Path::new("."), &["--version"]) {
            return;
        }
        let tmp = Tmp::new("tracked");
        let wiki = tmp.wiki();
        assert!(git(&tmp.0, &["init", "-q"]));
        fs::write(wiki.join("page.md"), "x\n").unwrap();
        assert!(git(&tmp.0, &["add", "wiki/page.md"]));
        let (_, bytes) = build(&sample()).unwrap();
        write_record(&wiki, &bytes).unwrap();
        let attrs = seals_dir(&wiki).join(".gitattributes");
        assert_eq!(
            fs::read_to_string(&attrs).unwrap(),
            "** text eol=lf\nevidence/** -text\n"
        );
        // Never overwritten when a human edited it.
        fs::write(&attrs, "custom\n").unwrap();
        let mut other = sample();
        other.claim = Some("different".to_owned());
        let (_, bytes2) = build(&other).unwrap();
        write_record(&wiki, &bytes2).unwrap();
        assert_eq!(fs::read_to_string(&attrs).unwrap(), "custom\n");
    }

    #[test]
    fn ignored_wiki_is_treated_as_untracked() {
        if !git(Path::new("."), &["--version"]) {
            return;
        }
        let tmp = Tmp::new("ignored");
        let wiki = tmp.wiki();
        assert!(git(&tmp.0, &["init", "-q"]));
        fs::write(tmp.0.join(".gitignore"), "wiki/\n").unwrap();
        let (_, bytes) = build(&sample()).unwrap();
        write_record(&wiki, &bytes).unwrap();
        assert!(!seals_dir(&wiki).join(".gitattributes").exists());
    }

    #[test]
    fn duplicate_keys_are_rejected_anywhere() {
        let (_, bytes) = build(&sample()).unwrap();
        let dup = with_field(
            &bytes,
            "\"principal\":\"agent.abc@local\"",
            "\"principal\":\"agent.abc@local\",\"principal\":\"evil@local\"",
        );
        assert!(
            matches!(check(&dup), Err(RecordError::Malformed(ref m)) if m.contains("duplicate"))
        );
        let top = with_field(&bytes, "{\"_type\"", "{\"predicateType\":\"x\",\"_type\"");
        assert!(matches!(check(&top), Err(RecordError::Malformed(_))));
    }

    #[test]
    fn subject_anchor_and_predicate_anchor_must_agree() {
        let (_, bytes) = build(&sample()).unwrap();
        let broken = with_field(
            &bytes,
            "\"anchor\":\"s-tls-rung-order\"",
            "\"anchor\":\"s-other\"",
        );
        assert!(matches!(check(&broken), Err(RecordError::Malformed(_))));
    }

    #[test]
    fn requested_level_and_locator_are_validated() {
        let mut r = sample();
        r.requested_level = Some("attested".to_owned());
        assert!(matches!(build(&r), Err(RecordError::Malformed(_))));
        for level in ["evidence", "audited", "canonical"] {
            let mut r = sample();
            r.requested_level = Some(level.to_owned());
            assert!(build(&r).is_ok(), "{level}");
        }
        let (_, bytes) = build(&sample()).unwrap();
        let broken = with_field(&bytes, "\"locator\"", "\"locatorx\"");
        assert!(matches!(check(&broken), Err(RecordError::MissingField(_))));
    }

    #[test]
    fn gitattributes_does_not_normalise_evidence() {
        assert!(GITATTRIBUTES.contains("** text eol=lf\n"));
        assert!(GITATTRIBUTES.ends_with("evidence/** -text\n"));
    }

    #[test]
    fn create_only_leaves_no_temp_files_and_load_all_skips_them() {
        let tmp = Tmp::new("atomic");
        let wiki = tmp.wiki();
        let (id, bytes) = build(&sample()).unwrap();
        write_record(&wiki, &bytes).unwrap();
        let _ = write_record(&wiki, &bytes);
        fs::write(records_dir(&wiki).join(".stray.json.1.0.tmp"), "partial").unwrap();
        let names: Vec<String> = fs::read_dir(records_dir(&wiki))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert_eq!(names, vec![".stray.json.1.0.tmp".to_owned()]);
        let loaded = load_all(&wiki).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, id);
    }

    #[test]
    fn unreadable_record_is_reported_not_fatal() {
        let tmp = Tmp::new("unreadable");
        let wiki = tmp.wiki();
        let (_, bytes) = build(&sample()).unwrap();
        write_record(&wiki, &bytes).unwrap();
        // A directory-like or oversized entry must not abort the load.
        let big = records_dir(&wiki).join(format!("{}.json", "1".repeat(20)));
        let file = fs::File::create(&big).unwrap();
        file.set_len(MAX_RECORD_BYTES + 1).unwrap();
        drop(file);
        let loaded = load_all(&wiki).unwrap();
        assert_eq!(loaded.len(), 2);
        let bad = loaded.iter().find(|l| l.id == "1".repeat(20)).unwrap();
        assert!(matches!(&bad.view, Err(RecordError::Malformed(m)) if m.contains("unreadable")));
        assert!(loaded.iter().any(|l| l.view.is_ok()));
    }

    #[test]
    fn heading_closing_hashes_need_leading_space() {
        let page = "# C#
## Intro ##
## Tail #
x
";
        let at = page.find('x').unwrap();
        assert_eq!(
            heading_path(page, at),
            vec!["C#".to_owned(), "Tail".to_owned()]
        );
        assert_eq!(
            heading_path(
                "# Intro ##
x",
                11
            ),
            vec!["Intro".to_owned()]
        );
    }

    #[test]
    fn soft_locator_uses_heading_path_and_32_char_edges() {
        let page = "# Top\n\n## Sub\n```\n# not a heading\n```\n<!-- loam:seal s-a-b -->\nbody\n<!-- /loam:seal -->\n";
        let at = page.find("body").unwrap();
        assert_eq!(
            heading_path(page, at),
            vec!["Top".to_owned(), "Sub".to_owned()]
        );
        let content = "x".repeat(40) + "\n" + &"y".repeat(40) + "\n";
        let loc = soft_locator(&["A".to_owned()], &content);
        assert_eq!(loc.prefix.chars().count(), 32);
        assert_eq!(loc.suffix.chars().count(), 32);
        assert!(loc.prefix.starts_with("xxxx"));
        assert!(loc.suffix.ends_with("yyyy"));
        let short = soft_locator(&[], "tiny\n");
        assert_eq!(short.prefix, "tiny");
        assert_eq!(short.suffix, "tiny");
    }
}
