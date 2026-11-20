//! `loam seal attest`: the one command that writes a seal (spec D3, D4, D8).
//!
//! The order of work is what makes a refusal leave no trace and lets no code
//! path delete a record:
//!
//! 1. validate the request and build the would-be page bytes in memory;
//! 2. hash the span, resolve all evidence (every failure listed together);
//! 3. build and sign the record, then verify the whole round trip in memory
//!    against an [`Overlay`] (page, record, signature, captures);
//! 4. only then write: page markers, captures, record, signature.
//!
//! The only page edit is the two marker lines and the block-id token they
//! replace, and the page's line endings are preserved byte for byte.

use crate::markdown;
use crate::markers::{self, Kind as MarkerKind};
use crate::seal::canon;
use crate::seal::evidence::{self, AttestOptions, Ctx};
use crate::seal::policy::{self, Level};
use crate::seal::record::{self, Generator, NewRecord, Parent, RecordKind};
use crate::seal::sshsig::{self, Capability};
use crate::seal::trust;
use crate::seal::verify::{self, Env, Overlay, Report};
use chrono::{DateTime, FixedOffset};
use std::fs;
use std::path::{Component, Path, PathBuf};

pub const PROCEDURE_URI: &str = "loam://seal/procedure/v1";
const SETUP_HINT: &str = "run `npx @scchearn/loam setup` to create this instance's signing key";

/// What a verb needs from outside the wiki, made explicit so tests do not
/// depend on process-wide environment.
#[derive(Debug, Clone)]
pub struct Context {
    pub wiki: PathBuf,
    /// Blob, file and commit evidence resolves against this.
    pub repo_root: PathBuf,
    /// `<configRoot>/seal`.
    pub trust_dir: PathBuf,
    pub keygen: PathBuf,
    pub now: DateTime<FixedOffset>,
    /// The enrolled org for the signer principal; `None` is `local`.
    pub org: Option<String>,
    /// `LOAM_HARNESS`, recorded in the generator.
    pub harness: Option<String>,
}

/// A parsed `loam seal attest` request.
#[derive(Debug, Clone, Default)]
pub struct Request {
    /// Wiki-relative page name: forward slashes, no `.md`.
    pub page: String,
    /// The anchor without `#` or `^`.
    pub anchor: String,
    pub level: String,
    pub claim: String,
    pub evidence: Vec<String>,
    pub review_by: Option<String>,
    pub parent: Option<String>,
    pub origin: Option<String>,
    pub redacted: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttestError {
    /// Bad arguments or an unusable target (exit 2). Nothing was written.
    Usage(String),
    /// The attestation was refused (exit 1), one line per reason. Nothing
    /// was written unless a line says otherwise.
    Refused(Vec<String>),
}

fn refuse(message: impl Into<String>) -> AttestError {
    AttestError::Refused(vec![message.into()])
}

#[derive(Debug, Clone)]
pub struct Attested {
    pub id: String,
    pub report: Report,
    /// Whether the page was edited (markers inserted).
    pub inserted_markers: bool,
}

// ------------------------------------------------------------ the page

struct Line {
    start: usize,
    end: usize,
    next: usize,
}

fn split_lines(source: &str) -> Vec<Line> {
    let bytes = source.as_bytes();
    let mut lines = Vec::new();
    let mut offset = 0;
    while offset < source.len() {
        let (end, next) = match source[offset..].find(['\n', '\r']) {
            Some(index) => {
                let end = offset + index;
                let next = if bytes[end] == b'\r' && bytes.get(end + 1) == Some(&b'\n') {
                    end + 2
                } else {
                    end + 1
                };
                (end, next)
            }
            None => (source.len(), source.len()),
        };
        lines.push(Line {
            start: offset,
            end,
            next,
        });
        offset = next;
    }
    lines
}

fn is_heading(text: &str) -> bool {
    let indent = text.len() - text.trim_start_matches(' ').len();
    if indent > 3 {
        return false;
    }
    let body = &text[indent..];
    let hashes = body.chars().take_while(|c| *c == '#').count();
    (1..=6).contains(&hashes)
        && body[hashes..]
            .chars()
            .next()
            .is_none_or(char::is_whitespace)
}

/// A CommonMark thematic break: up to 3 spaces of indent, then 3 or more of
/// the same `*`, `-` or `_`, with only spaces or tabs between them.
fn is_thematic_break(text: &str) -> bool {
    let indent = text.chars().take_while(|c| *c == ' ').count();
    if indent > 3 {
        return false;
    }
    let body = text[indent..].trim_end();
    let Some(mark) = body.chars().next().filter(|c| matches!(c, '*' | '-' | '_')) else {
        return false;
    };
    body.chars().all(|c| c == mark || matches!(c, ' ' | '\t'))
        && body.chars().filter(|c| *c == mark).count() >= 3
}

/// A CommonMark list item start: `-`, `*`, `+` or `1.`/`1)` then a space.
fn is_list_item(text: &str) -> bool {
    let body = text.trim_start_matches([' ', '\t']);
    let rest = match body.chars().next() {
        Some('-' | '*' | '+') => &body[1..],
        Some(c) if c.is_ascii_digit() => {
            let digits = body.chars().take_while(char::is_ascii_digit).count();
            match body[digits..].chars().next() {
                Some('.' | ')') if digits <= 9 => &body[digits + 1..],
                _ => return false,
            }
        }
        _ => return false,
    };
    rest.is_empty() || rest.starts_with([' ', '\t'])
}

/// Whether the line ends in an Obsidian block-id token (`^some-id`).
fn ends_with_block_id(text: &str) -> bool {
    let word = text
        .trim_end()
        .rsplit(char::is_whitespace)
        .next()
        .unwrap_or("");
    word.len() > 1
        && word.starts_with('^')
        && word[1..]
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// A line made only of `=` or `-` (setext underline or rule) ends a block.
fn is_setext_underline(text: &str) -> bool {
    let t = text.trim();
    !t.is_empty() && (t.bytes().all(|b| b == b'=') || t.bytes().all(|b| b == b'-'))
}

fn is_marker_comment(text: &str) -> bool {
    let trimmed = text.trim();
    trimmed.starts_with("<!--") && trimmed.ends_with("-->") && trimmed.contains("loam:")
}

/// The page with seal markers around the block carrying `^<anchor>`, or
/// `None` when the page already holds a well-formed seal span for it.
pub fn insert_markers(source: &str, anchor: &str) -> Result<Option<String>, String> {
    let before = markers::scan(source);
    if before
        .markers
        .iter()
        .any(|m| m.kind == MarkerKind::Seal && m.id == anchor)
    {
        return Ok(None);
    }
    if let Some(kind) = before.anchors.get(anchor) {
        return Err(format!(
            "anchor ^{anchor} is already used by a {kind:?} marker and is not a usable seal span"
        ));
    }

    let bom = if source.starts_with('\u{FEFF}') {
        '\u{FEFF}'.len_utf8()
    } else {
        0
    };
    let excluded: Vec<_> = markdown::excluded_ranges(&source[bom..])
        .into_iter()
        .map(|range| range.start + bom..range.end + bom)
        .collect();
    let in_code = |line: &Line| {
        excluded
            .iter()
            .any(|range| range.start < line.end.max(line.start + 1) && line.start < range.end)
    };

    let token = format!("^{anchor}");
    let lines = split_lines(source);
    let mut found = None;
    let mut matches = 0;
    for (index, line) in lines.iter().enumerate() {
        let text = &source[line.start..line.end];
        let Some(rest) = text.trim_end().strip_suffix(token.as_str()) else {
            continue;
        };
        if in_code(line) {
            continue;
        }
        if rest.trim().is_empty() {
            return Err(format!(
                "block id ^{anchor} stands alone on its line; put it at the end of the block's last line"
            ));
        }
        if !rest.ends_with(char::is_whitespace) {
            continue;
        }
        matches += 1;
        found = Some((index, rest.trim_end().len()));
    }
    let Some((last, kept)) = found else {
        return Err(format!(
            "no block ending in ` ^{anchor}` and no seal span for it on the page"
        ));
    };
    if matches > 1 {
        return Err(format!(
            "block id ^{anchor} ends {matches} blocks on the page; it must be unique"
        ));
    }
    let last_content = &source[lines[last].start.max(bom)..lines[last].start + kept];
    if last_content.trim().is_empty() {
        return Err(format!(
            "block id ^{anchor} stands alone on its line; put it at the end of the block's last line"
        ));
    }

    // Walk back to the first line of the block: a blank line, a heading, a
    // marker comment or code ends it.
    let list_refusal = || {
        format!(
            "block id ^{anchor} is on or beside a list item; a seal cannot scope a single item automatically. Place the `<!-- loam:seal {anchor} -->` markers by hand"
        )
    };
    let mut first = last;
    if !is_thematic_break(last_content) && is_list_item(last_content) {
        return Err(list_refusal());
    }
    if !is_heading(last_content) && !is_thematic_break(last_content) {
        while first > 0 {
            let previous = &lines[first - 1];
            let text = &source[previous.start..previous.end];
            if text.trim().is_empty()
                || is_heading(text)
                || is_marker_comment(text)
                || is_thematic_break(text)
                || is_setext_underline(text)
                || in_code(previous)
            {
                break;
            }
            if is_list_item(text) {
                return Err(list_refusal());
            }
            if ends_with_block_id(text) {
                return Err(format!(
                    "the line above ^{anchor} carries another block id; separate the blocks with a blank line or place the markers by hand"
                ));
            }
            first -= 1;
        }
    }

    let first_start = lines[first].start.max(bom);
    let first_text = &source[first_start..lines[first].end];
    let indent: String = first_text
        .chars()
        .take_while(|c| matches!(c, ' ' | '\t'))
        .collect();
    let last_terminator = &source[lines[last].end..lines[last].next];
    let eol = if !last_terminator.is_empty() {
        last_terminator
    } else {
        lines
            .iter()
            .map(|l| &source[l.end..l.next])
            .find(|t| !t.is_empty())
            .unwrap_or("\n")
    };

    let mut out = String::with_capacity(source.len() + 64);
    out.push_str(&source[..first_start]);
    out.push_str(&format!("{indent}<!-- loam:seal {anchor} -->{eol}"));
    let last_from = lines[last].start.max(first_start);
    out.push_str(&source[first_start..last_from]);
    out.push_str(&source[last_from..lines[last].start + kept]);
    out.push_str(if last_terminator.is_empty() {
        eol
    } else {
        last_terminator
    });
    out.push_str(&format!("{indent}<!-- /loam:seal -->"));
    out.push_str(last_terminator);
    out.push_str(&source[lines[last].next..]);

    let after = markers::scan(&out);
    let sealed = after
        .markers
        .iter()
        .any(|m| m.kind == MarkerKind::Seal && m.id == anchor);
    if !sealed || after.problems.len() != before.problems.len() {
        return Err(format!(
            "the block ending in ^{anchor} cannot be wrapped as a valid seal span (anchors are `s-` plus 2 to 6 lowercase words; spans may not contain other markers)"
        ));
    }
    Ok(Some(out))
}

fn page_file(wiki: &Path, page: &str) -> Result<PathBuf, String> {
    let relative = Path::new(page);
    let clean = !page.is_empty()
        && relative.components().all(|c| match c {
            Component::Normal(part) => !part.to_string_lossy().starts_with('.'),
            _ => false,
        });
    if !clean {
        return Err(format!("`{page}` is not a wiki page name"));
    }
    Ok(wiki.join(format!("{page}.md")))
}

/// Resolves a requested page name to the exact on-disk name and path. On
/// case-insensitive filesystems a wrong-case name would otherwise read fine
/// yet be recorded verbatim, so the name must match a listed page: exactly, or
/// by a unique case-insensitive match.
fn resolve_page(wiki: &Path, page: &str) -> Result<(String, PathBuf), String> {
    page_file(wiki, page)?;
    let files = verify::markdown_files(wiki)?;
    if let Some((name, path)) = files.iter().find(|(name, _)| name == page) {
        return Ok((name.clone(), path.clone()));
    }
    let lower = page.to_lowercase();
    let mut folded = files
        .iter()
        .filter(|(name, _)| name.to_lowercase() == lower);
    match (folded.next(), folded.next()) {
        (Some((name, path)), None) => Ok((name.clone(), path.clone())),
        (Some(_), Some(_)) => Err(format!(
            "page `{page}` matches several pages that differ only in case; use the exact name"
        )),
        _ => Err(format!("cannot read page {page}: no such page")),
    }
}

fn write_page(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temp = path.with_file_name(format!(".{name}.loam-attest"));
    let result = (|| -> std::io::Result<()> {
        fs::write(&temp, bytes)?;
        if let Ok(meta) = fs::metadata(path) {
            let _ = fs::set_permissions(&temp, meta.permissions());
        }
        fs::rename(&temp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result.map_err(|e| format!("cannot write {}: {e}", path.display()))
}

// ------------------------------------------------------------- attest

fn stamp(now: &DateTime<FixedOffset>) -> String {
    now.format("%Y-%m-%d %H:%M %:z").to_string()
}

fn is_record_id(value: &str) -> bool {
    value.len() == record::ID_LEN
        && value
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Seals `request.page#^request.anchor`. See the module docs for the order of
/// work; nothing under `wiki/` changes unless this returns `Ok`.
pub fn attest(ctx: &Context, request: &Request) -> Result<Attested, AttestError> {
    let usage = |message: String| AttestError::Usage(message);

    // 1. Arguments.
    let level = Level::parse(&request.level).ok_or_else(|| {
        usage(format!(
            "--level `{}` must be evidence, audited or canonical",
            request.level
        ))
    })?;
    if request.claim.trim().is_empty() {
        return Err(usage("--claim must not be empty".to_owned()));
    }
    if request.evidence.is_empty() {
        return Err(usage(
            "at least one --evidence <kind:ref> is required".to_owned(),
        ));
    }
    let mut items = Vec::new();
    for arg in &request.evidence {
        items.push(evidence::parse(arg).map_err(|f| usage(f.to_string()))?);
    }
    let now_stamp = stamp(&ctx.now);
    if let Some(review_by) = &request.review_by {
        let due = record::valid_timestamp(review_by)
            .then(|| policy::parse_stamp(review_by))
            .flatten()
            .ok_or_else(|| {
                usage(format!(
                    "--review-by `{review_by}` must be `YYYY-MM-DD HH:MM +HH:MM`"
                ))
            })?;
        if due <= ctx.now {
            return Err(usage(format!(
                "--review-by `{review_by}` is not in the future"
            )));
        }
    }
    if let Some(parent) = &request.parent {
        if !is_record_id(parent) {
            return Err(usage(format!(
                "--parent `{parent}` is not a {}-hex seal id",
                record::ID_LEN
            )));
        }
    }
    if request.redacted.iter().any(|label| label.trim().is_empty()) {
        return Err(usage("--redacted needs a non-empty label".to_owned()));
    }

    // 2. The page, as it will be.
    let (page, path) = resolve_page(&ctx.wiki, &request.page).map_err(&usage)?;
    let original = fs::read(&path).map_err(|e| usage(format!("cannot read page {}: {e}", page)))?;
    let source = std::str::from_utf8(&original)
        .map_err(|e| usage(format!("page {} is not valid UTF-8 ({e})", page)))?;
    let rewritten = insert_markers(source, &request.anchor).map_err(&usage)?;
    let page_text = rewritten.as_deref().unwrap_or(source);
    let page_bytes: Vec<u8> = page_text.as_bytes().to_vec();
    let scan = markers::scan(page_text);
    let marker = scan
        .markers
        .iter()
        .find(|m| m.kind == MarkerKind::Seal && m.id == request.anchor)
        .ok_or_else(|| usage(format!("no seal span ^{} on the page", request.anchor)))?;
    let span = &page_bytes[marker.content.clone()];
    let subject_sha256 = canon::digest(span).map_err(|e| usage(format!("span: {e}")))?;
    let canonical =
        String::from_utf8(canon::canonicalize(span).map_err(|e| usage(format!("span: {e}")))?)
            .map_err(|_| usage("span is not valid UTF-8".to_owned()))?;
    if canonical.is_empty() {
        return Err(usage(format!("span ^{} is empty", request.anchor)));
    }
    let locator = record::soft_locator(
        &record::heading_path(page_text, marker.opener.start),
        &canonical,
    );

    // Signing identity.
    let key = ctx.trust_dir.join(trust::KEY_FILE);
    let instance = fs::read_to_string(ctx.trust_dir.join(trust::INSTANCE_FILE))
        .ok()
        .map(|text| text.trim().to_owned())
        .filter(|id| !id.is_empty());
    let (true, Some(instance)) = (key.is_file(), instance) else {
        return Err(refuse(format!(
            "no signing identity under {}: {SETUP_HINT}",
            ctx.trust_dir.display()
        )));
    };
    if sshsig::capability_of(&ctx.keygen) != Capability::Available {
        return Err(refuse(format!(
            "{} cannot sign (needs OpenSSH 9.1+ ssh-keygen with -Y support)",
            ctx.keygen.display()
        )));
    }
    let principal = format!("agent.{instance}@{}", ctx.org.as_deref().unwrap_or("local"));

    let mut env = Env::load(&ctx.wiki, Some(ctx.trust_dir.clone()), ctx.now)
        .map_err(|e| refuse(format!("cannot load trust: {e}")))?;
    env.repo_root = ctx.repo_root.clone();
    env.keygen = ctx.keygen.clone();

    // 3. Evidence: every failure, listed together.
    let evidence_ctx = Ctx {
        repo_root: ctx.repo_root.clone(),
        wiki: ctx.wiki.clone(),
        retrieved_at: now_stamp.clone(),
        capture_max_bytes: env.trust.policy.capture_max_bytes,
    };
    let options = AttestOptions {
        origin: request.origin.as_deref(),
    };
    let mut resolved = evidence::resolve_all(&items, &evidence_ctx, &options)
        .map_err(|fails| AttestError::Refused(fails.iter().map(ToString::to_string).collect()))?;

    // Parent: its verdict now, and unchanged captures keep their provenance.
    let parent = match &request.parent {
        None => None,
        Some(id) => {
            let reports = verify::verify_all(&env).map_err(|e| refuse(e.to_string()))?;
            let report = reports
                .iter()
                .find(|report| &report.id == id)
                .ok_or_else(|| usage(format!("--parent {id} is not a seal record in this wiki")))?;
            if report.anchor.as_deref() != Some(request.anchor.as_str())
                || report.page.as_deref() != Some(page.as_str())
            {
                return Err(usage(format!(
                    "--parent {id} does not seal this span; a parent must seal {page}#^{}",
                    request.anchor
                )));
            }
            let loaded = record::load_all(&ctx.wiki).map_err(|e| refuse(e.to_string()))?;
            let parent_items = loaded
                .iter()
                .find(|l| &l.id == id)
                .and_then(|l| l.view.as_ref().ok())
                .map(|view| evidence::items_from_values(&view.evidence).unwrap_or_default())
                .unwrap_or_default();
            for entry in &mut resolved {
                let carried = parent_items.iter().find(|old| {
                    old.kind == entry.item.kind
                        && old.reference == entry.item.reference
                        && old.sha256 == entry.item.sha256
                });
                if let (Some(old), evidence::Kind::Capture) = (carried, entry.item.kind) {
                    if request.origin.is_none() {
                        entry.item.origin.clone_from(&old.origin);
                    }
                    entry.item.retrieved_at.clone_from(&old.retrieved_at);
                }
            }
            Some(Parent {
                id: id.clone(),
                verdict: report.outcome.verdict.as_str().to_owned(),
            })
        }
    };

    // 4. The record, signed, and the whole round trip verified in memory.
    let (id, bytes) = record::build(&NewRecord {
        kind: RecordKind::Seal,
        page: page.clone(),
        anchor: request.anchor.clone(),
        subject_sha256,
        locator,
        content: canonical,
        requested_level: Some(level.as_str().to_owned()),
        claim: Some(request.claim.clone()),
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
        time_verified: now_stamp,
        review_by: request.review_by.clone(),
        evidence: resolved.iter().map(|r| r.item.to_value()).collect(),
        redacted: evidence::redacted_values(&request.redacted),
        parent,
        status: None,
    })
    .map_err(|e| refuse(format!("cannot build the record: {e}")))?;
    if record::record_path(&ctx.wiki, &id).exists() {
        return Err(refuse(format!(
            "seal {id} already exists (an identical claim was recorded in the same minute); nothing written"
        )));
    }
    let signature = sshsig::sign(&ctx.keygen, &key, &bytes)
        .map_err(|e| refuse(format!("signing failed: {e}")))?;

    let overlay = Overlay {
        page: rewritten
            .as_ref()
            .map(|_| (page.clone(), page_bytes.clone())),
        record: Some((id.clone(), bytes.clone())),
        signature: Some(signature.clone()),
        captures: resolved
            .iter()
            .filter(|r| r.capture.is_some())
            .filter_map(|r| r.item.sha256.clone())
            .collect(),
    };
    let rehearsal = verify::verify_overlay(&env, &overlay, &id)
        .map_err(|e| refuse(format!("round-trip verification failed: {e}")))?;
    if !rehearsal.is_citable() {
        let codes = &rehearsal.outcome.codes;
        let mut lines = vec![format!(
            "the seal would not be citable ({}); nothing written",
            rehearsal.outcome.verdict.as_str()
        )];
        lines.extend(codes.failure.iter().map(|c| format!("  {c}")));
        return Err(AttestError::Refused(lines));
    }

    // 5. Writes. The page first (restored if the record cannot be created),
    // then captures, the record and its signature. Nothing is ever deleted.
    if fs::read(&path).ok().as_deref() != Some(original.as_slice()) {
        return Err(refuse(format!(
            "page {} changed while attesting; nothing written",
            page
        )));
    }
    let edited = rewritten.is_some();
    if edited {
        write_page(&path, &page_bytes).map_err(refuse)?;
    }
    let restore = |why: String| {
        if edited {
            if let Err(e) = write_page(&path, &original) {
                return refuse(format!("{why}; and the page could not be restored: {e}"));
            }
        }
        refuse(format!("{why}; page restored"))
    };
    // Captures are content-addressed, not records: if a later write fails,
    // newly stored capture files stay behind harmlessly. Two concurrent
    // attests on one page can both pass the changed-while-attesting check
    // above and overwrite each other's markers; that window is accepted.
    if let Err(e) = evidence::store_captures(&ctx.wiki, &resolved) {
        return Err(restore(e));
    }
    if let Err(e) = record::write_record(&ctx.wiki, &bytes) {
        return Err(restore(format!("cannot write the record: {e}")));
    }
    if let Err(e) = record::write_sig(&ctx.wiki, &id, signature.as_bytes()) {
        return Err(refuse(format!(
            "seal {id} was written without its signature: {e}"
        )));
    }

    let report = verify::verify_overlay(&env, &Overlay::default(), &id)
        .map_err(|e| refuse(format!("seal {id} written but not re-verified: {e}")))?;
    if !report.is_citable() {
        return Err(refuse(format!(
            "seal {id} written but is {} on re-verification",
            report.outcome.verdict.as_str()
        )));
    }
    Ok(Attested {
        id,
        report,
        inserted_markers: edited,
    })
}

// ---------------------------------------------------------------- CLI

fn usage_text() -> i32 {
    eprintln!(
        "Usage: loam seal attest <page>#^<anchor> --level <evidence|audited|canonical> --claim <text> --evidence <kind:ref>... [--review-by <stamp>] [--parent <id>] [--origin <text>] [--redacted <label>]..."
    );
    crate::seal::EXIT_USAGE
}

struct Parsed {
    selector: verify::Selector,
    request: Request,
}

fn parse_args(args: impl Iterator<Item = String>) -> Result<Parsed, String> {
    let mut selector = None;
    let mut request = Request::default();
    let mut level = None;
    let mut claim = None;
    let mut args = args;
    while let Some(arg) = args.next() {
        let mut value = |flag: &str| args.next().ok_or_else(|| format!("{flag} needs a value"));
        match arg.as_str() {
            "--level" => level = Some(value("--level")?),
            "--claim" => claim = Some(value("--claim")?),
            "--evidence" => request.evidence.push(value("--evidence")?),
            "--review-by" => request.review_by = Some(value("--review-by")?),
            "--parent" => request.parent = Some(value("--parent")?),
            "--origin" => request.origin = Some(value("--origin")?),
            "--redacted" => request.redacted.push(value("--redacted")?),
            flag if flag.starts_with("--") => return Err(format!("unknown option {flag}")),
            _ if selector.is_none() => selector = Some(verify::Selector::parse(&arg)?),
            _ => return Err(format!("unexpected argument `{arg}`")),
        }
    }
    let selector = selector.ok_or("a <page>#^<anchor> target is required")?;
    request.level = level.ok_or("--level is required")?;
    request.claim = claim.ok_or("--claim is required")?;
    Ok(Parsed { selector, request })
}

pub fn run(args: impl Iterator<Item = String>) -> i32 {
    use crate::seal::{EXIT_OK, EXIT_REFUSED, EXIT_USAGE};
    let fail = |message: String| {
        eprintln!("loam seal attest: {message}");
        EXIT_USAGE
    };
    let parsed = match parse_args(args) {
        Ok(parsed) => parsed,
        Err(message) => {
            eprintln!("loam seal attest: {message}");
            return usage_text();
        }
    };
    let Ok(cwd) = std::env::current_dir() else {
        return fail("cannot read the current directory".to_owned());
    };
    let Some(wiki) = crate::seal::find_wiki(&cwd) else {
        return fail(format!("no wiki/ directory at or above {}", cwd.display()));
    };
    let selector = parsed.selector.resolve(&wiki, &cwd);
    let (Some(page), Some(anchor)) = (selector.page, selector.anchor) else {
        return fail("the target must be `<page>#^<anchor>`".to_owned());
    };
    let trust_dir = match trust::seal_dir() {
        Ok(dir) => dir,
        Err(message) => {
            eprintln!("loam seal attest: {message}; {SETUP_HINT}");
            return EXIT_REFUSED;
        }
    };
    // Always the wall clock: the signed `time_verified` must not be settable.
    let now = {
        let local = chrono::Local::now();
        local.with_timezone(local.offset())
    };
    let repo_root = evidence::repo_root(&wiki)
        .or_else(|| wiki.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| wiki.clone());
    let ctx = Context {
        wiki,
        repo_root,
        trust_dir,
        keygen: sshsig::keygen_binary(),
        now,
        org: trust::Enrolled::from_environment().org,
        harness: std::env::var("LOAM_HARNESS").ok(),
    };
    let request = Request {
        page,
        anchor,
        ..parsed.request
    };
    match attest(&ctx, &request) {
        Ok(done) => {
            println!("{}", done.report.to_value().to_json());
            EXIT_OK
        }
        Err(AttestError::Usage(message)) => fail(message),
        Err(AttestError::Refused(lines)) => {
            for line in lines {
                eprintln!("loam seal attest: {line}");
            }
            EXIT_REFUSED
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inserted(source: &str) -> String {
        insert_markers(source, "s-trust-order")
            .unwrap()
            .expect("markers inserted")
    }

    #[test]
    fn wraps_the_whole_paragraph_and_drops_only_the_token() {
        assert_eq!(
            inserted("# T\n\nfirst\nsecond ^s-trust-order\n\nnext\n"),
            "# T\n\n<!-- loam:seal s-trust-order -->\nfirst\nsecond\n<!-- /loam:seal -->\n\nnext\n"
        );
    }

    #[test]
    fn a_heading_or_blank_line_ends_the_block() {
        assert_eq!(
            inserted("## H\nbody ^s-trust-order\n"),
            "## H\n<!-- loam:seal s-trust-order -->\nbody\n<!-- /loam:seal -->\n"
        );
        assert_eq!(
            inserted("## H ^s-trust-order\nbody\n"),
            "<!-- loam:seal s-trust-order -->\n## H\n<!-- /loam:seal -->\nbody\n"
        );
    }

    #[test]
    fn keeps_crlf_indentation_and_a_missing_final_newline() {
        assert_eq!(
            inserted("a\r\n\r\n  item ^s-trust-order\r\n"),
            "a\r\n\r\n  <!-- loam:seal s-trust-order -->\r\n  item\r\n  <!-- /loam:seal -->\r\n"
        );
        assert_eq!(
            inserted("a\n\nlast ^s-trust-order"),
            "a\n\n<!-- loam:seal s-trust-order -->\nlast\n<!-- /loam:seal -->"
        );
        assert_eq!(
            inserted("a\r\n\r\nlast ^s-trust-order"),
            "a\r\n\r\n<!-- loam:seal s-trust-order -->\r\nlast\r\n<!-- /loam:seal -->"
        );
    }

    #[test]
    fn a_bom_stays_first_and_a_first_line_block_works() {
        assert_eq!(
            inserted("\u{FEFF}only ^s-trust-order\n"),
            "\u{FEFF}<!-- loam:seal s-trust-order -->\nonly\n<!-- /loam:seal -->\n"
        );
        assert_eq!(
            inserted("\u{FEFF}one\ntwo ^s-trust-order\n"),
            "\u{FEFF}<!-- loam:seal s-trust-order -->\none\ntwo\n<!-- /loam:seal -->\n"
        );
    }

    #[test]
    fn refuses_list_items_and_neighbouring_block_ids() {
        let err = |source: &str| insert_markers(source, "s-trust-order").unwrap_err();
        assert!(err("- a\n- b ^s-trust-order\n").contains("list item"));
        assert!(err("1. only ^s-trust-order\n").contains("list item"));
        assert!(err("- a\n  more ^s-trust-order\n").contains("list item"));
        assert!(err("one ^s-other\ntwo ^s-trust-order\n").contains("another block id"));
        // A blank line separates the blocks, so neither rule fires.
        assert!(insert_markers("one ^s-other\n\ntwo ^s-trust-order\n", "s-trust-order").is_ok());
        assert!(!is_list_item("-dash") && !is_list_item("2024 was") && is_list_item("  * x"));
    }

    #[test]
    fn existing_markers_need_no_edit() {
        let page = "<!-- loam:seal s-trust-order -->\nx\n<!-- /loam:seal -->\n";
        assert_eq!(insert_markers(page, "s-trust-order"), Ok(None));
    }

    #[test]
    fn refuses_what_it_cannot_place_unambiguously() {
        let err = |source: &str, anchor: &str| insert_markers(source, anchor).unwrap_err();
        assert!(err("no id here\n", "s-trust-order").contains("no block"));
        assert!(
            err("a ^s-trust-order\n\nb ^s-trust-order\n", "s-trust-order")
                .contains("must be unique")
        );
        assert!(err("a\n\n^s-trust-order\n", "s-trust-order").contains("stands alone"));
        // Inside a code fence the token is just text.
        assert!(err("```\ncode ^s-trust-order\n```\n", "s-trust-order").contains("no block"));
        // Not a valid seal anchor: the rescan rejects the wrapped span.
        assert!(err("x ^s-bad\n", "s-bad").contains("cannot be wrapped"));
        assert!(err("x ^not-seal\n", "not-seal").contains("cannot be wrapped"));
        // A class anchor is not reusable as a seal.
        assert!(err(
            "<!-- loam:class c-trust-order -->\nx\n<!-- /loam:class -->\n",
            "c-trust-order"
        )
        .contains("already used"));
    }

    #[test]
    fn a_seal_may_sit_inside_a_class_span_but_never_breaks_the_scan() {
        let page =
            "<!-- loam:class c-inner-note project -->\nx ^s-trust-order\n<!-- /loam:class -->\n";
        let text = inserted(page);
        let scan = markers::scan(&text);
        assert!(scan.problems.is_empty(), "{text} {:?}", scan.problems);
        assert!(scan
            .markers
            .iter()
            .any(|m| m.kind == MarkerKind::Seal && m.id == "s-trust-order"));
    }

    #[test]
    fn page_names_resolve_to_the_on_disk_case() {
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                for _ in 0..10 {
                    if fs::remove_dir_all(&self.0).is_ok() || !self.0.exists() {
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            }
        }
        let dir = std::env::temp_dir().join(format!("loam-attest-resolve-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let _cleanup = Cleanup(dir.clone());
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("alpha.md"), "x").unwrap();
        let (name, _) = resolve_page(&dir, "Alpha").unwrap();
        assert_eq!(name, "alpha");
        assert!(resolve_page(&dir, "beta").is_err());
        fs::write(dir.join("Gamma.md"), "x").unwrap();
        assert_eq!(resolve_page(&dir, "gamma").unwrap().0, "Gamma");
    }

    #[test]
    fn thematic_breaks_bound_a_block() {
        for rule in ["***", "___", "* * *", "- - -", "---", "  ***"] {
            let source = format!("A\n{rule}\npara ^s-after-rule\n");
            let out = insert_markers(&source, "s-after-rule").unwrap().unwrap();
            assert!(
                out.starts_with(&format!("A\n{rule}\n<!-- loam:seal s-after-rule -->\npara")),
                "{rule}: {out}"
            );
        }
        assert!(
            is_thematic_break("_ _ _") && !is_thematic_break("**") && !is_thematic_break("*-*")
        );
        assert!(!is_thematic_break("    ***") && !is_thematic_break("*** x"));
    }

    #[test]
    fn setext_underlines_bound_a_block() {
        let source = "Title\n=====\nbody text ^s-one-two\n";
        let out = insert_markers(source, "s-one-two").unwrap().unwrap();
        assert!(out.starts_with("Title\n=====\n<!-- loam:seal"), "{out}");
    }

    #[test]
    fn page_names_cannot_escape_the_wiki() {
        let wiki = Path::new("wiki");
        assert!(page_file(wiki, "a/b").is_ok());
        for bad in ["", "../x", "/abs", "a/../b", ".seals/x", "a/.hidden"] {
            assert!(page_file(wiki, bad).is_err(), "{bad}");
        }
    }
}
