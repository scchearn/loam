//! Fence-aware marker scanner: the one parser for loam's marker system.
//!
//! Three marker kinds share one per-page anchor namespace (`^s-…`, `^c-…`,
//! `^q-…`):
//!
//! - seal and class spans: whole-line HTML comments
//!   `<!-- loam:<kind> <id>[ <token>…] -->` … `<!-- /loam:<kind> -->`;
//! - question records: GFM task lines `- [ ] Q: … ^q-<slug>`.
//!
//! Markers inside code blocks (fenced or indented) and front matter are
//! ignored, as are other `loam:*` comment kinds such as `loam:memory-map`.
//! All byte ranges index the original source, CRLF intact. The scanner is
//! read-only and never rewrites a page.

use crate::markdown;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::ops::Range;

/// The kind of a marker, which fixes the anchor prefix.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Seal,
    Class,
    Question,
}

impl Kind {
    pub fn prefix(self) -> &'static str {
        match self {
            Kind::Seal => "s-",
            Kind::Class => "c-",
            Kind::Question => "q-",
        }
    }
}

/// Diagnostic codes the scanner can raise.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProblemCode {
    /// Unbalanced span, bad id, wrong closer or bad class token.
    Mrk001,
    /// Illegal nesting.
    Mrk002,
    /// Duplicate anchor on one page.
    Mrk003,
    /// Seal opener carrying tokens after the id.
    Sel001,
}

impl ProblemCode {
    pub fn as_str(self) -> &'static str {
        match self {
            ProblemCode::Mrk001 => "MRK001",
            ProblemCode::Mrk002 => "MRK002",
            ProblemCode::Mrk003 => "MRK003",
            ProblemCode::Sel001 => "SEL001",
        }
    }
}

/// A well-formed marker.
///
/// For spans, `opener` and `closer` are the marker lines (indentation
/// included, terminator excluded) and `content` runs from the byte after the
/// opener line's terminator to the start of the closer line (raw bytes, CRLF
/// intact). For questions, `opener` is the record line (terminator excluded)
/// and `content` and `closer` are empty ranges at its end.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Marker {
    pub kind: Kind,
    pub id: String,
    /// The single reserved class token (`local-only`, `project`,
    /// `hub-promotable`); `None` for seals and questions.
    pub class: Option<String>,
    pub opener: Range<usize>,
    pub content: Range<usize>,
    pub closer: Range<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    pub code: ProblemCode,
    pub message: String,
    pub range: Range<usize>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Scan {
    /// Well-formed markers only, in document order. Malformed, illegally
    /// nested and duplicate-id spans never appear here.
    pub markers: Vec<Marker>,
    /// Block-id registry: every grammatically valid anchor on the page, with
    /// the kind that declared it. A duplicated id registers once.
    pub anchors: BTreeMap<String, Kind>,
    pub problems: Vec<Problem>,
}

/// SEL008 (domain `seals`, severity info): a rewriting tool skipped a sealed
/// span. Shared by every fixer so the rule has one definition.
pub const SEL008_CODE: &str = "SEL008";
pub const SEL008_NAME: &str = "sealed-span-skipped";
pub const SEL008_DESCRIPTION: &str = "rewriting tool skipped a sealed span";

/// Byte ranges (opener line start through closer line end) and anchors of the
/// well-formed seal spans in `source`. Malformed spans are never included, so
/// rewriting tools only ever protect spans the scanner accepts.
pub fn sealed_spans(source: &str) -> Vec<(Range<usize>, String)> {
    scan(source)
        .markers
        .into_iter()
        .filter(|marker| marker.kind == Kind::Seal)
        .map(|marker| (marker.opener.start..marker.closer.end, marker.id))
        .collect()
}

const CLASS_TOKENS: [&str; 3] = ["local-only", "project", "hub-promotable"];

/// Scan `source` for markers.
pub fn scan(source: &str) -> Scan {
    // A leading BOM would hide a first-line marker or fence: scan the stripped
    // text and shift every offset back so ranges index the original source.
    let bom = if source.starts_with('\u{FEFF}') {
        '\u{FEFF}'.len_utf8()
    } else {
        0
    };
    let body = &source[bom..];
    let excluded = markdown::excluded_ranges(body)
        .into_iter()
        .map(|range| range.start + bom..range.end + bom)
        .collect::<Vec<_>>();
    let mut scanner = Scanner::default();
    let mut offset = bom;
    // Lines arrive in increasing offset order, so one cursor through the
    // sorted excluded ranges suffices.
    let mut cursor = 0usize;
    while offset < source.len() {
        // `\n`, `\r\n` and a lone `\r` all terminate a line.
        let (line_end, next) = match source[offset..].find(['\n', '\r']) {
            Some(index) => {
                let end = offset + index;
                let bytes = source.as_bytes();
                let next = if bytes[end] == b'\r' && bytes.get(end + 1) == Some(&b'\n') {
                    end + 2
                } else {
                    end + 1
                };
                (end, next)
            }
            None => (source.len(), source.len()),
        };
        let line = &source[offset..line_end];
        while cursor < excluded.len() && excluded[cursor].end <= offset {
            cursor += 1;
        }
        let in_code = excluded[cursor..]
            .iter()
            .take_while(|range| range.start < line_end)
            .any(|range| range.start < line_end && offset < range.end);
        if !line.trim().is_empty() && !in_code {
            scanner.line(line, offset..line_end, next);
        }
        offset = next;
    }
    scanner.finish()
}

struct Open {
    kind: Kind,
    id: String,
    class: Option<String>,
    opener: Range<usize>,
    content_start: usize,
    /// Set when the span is malformed or illegally nested.
    excluded: bool,
}

#[derive(Default)]
struct Scanner {
    stack: Vec<Open>,
    done: Vec<Marker>,
    anchors: BTreeMap<String, Kind>,
    seen: HashMap<String, usize>,
    duplicated: HashSet<String>,
    problems: Vec<Problem>,
}

enum Comment<'a> {
    Open { kind: Kind, rest: Vec<&'a str> },
    Close { kind: Kind, rest: Vec<&'a str> },
}

fn parse_comment(line: &str) -> Option<Comment<'_>> {
    let trimmed = line.trim();
    let inner = trimmed.strip_prefix("<!--")?.strip_suffix("-->")?.trim();
    let mut words = inner.split_whitespace();
    let head = words.next()?;
    let (closing, name) = match head.strip_prefix('/') {
        Some(name) => (true, name),
        None => (false, head),
    };
    let kind = match name.strip_prefix("loam:")? {
        "seal" => Kind::Seal,
        "class" => Kind::Class,
        _ => return None,
    };
    let rest = words.collect();
    Some(if closing {
        Comment::Close { kind, rest }
    } else {
        Comment::Open { kind, rest }
    })
}

/// Slug length in kebab words: 2-6 (spec D1). A `-2` collision suffix counts
/// as one of the words.
const SLUG_WORDS: std::ops::RangeInclusive<usize> = 2..=6;

/// `<prefix>-<slug>` with the slug kebab words of `[a-z0-9]`.
fn valid_id(kind: Kind, id: &str) -> bool {
    let Some(slug) = id.strip_prefix(kind.prefix()) else {
        return false;
    };
    let words = slug.split('-').collect::<Vec<_>>();
    SLUG_WORDS.contains(&words.len())
        && words.iter().all(|word| {
            !word.is_empty()
                && word
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        })
}

fn is_question_record(line: &str) -> bool {
    let trimmed = line.trim_start();
    let Some(rest) = trimmed
        .strip_prefix("- [")
        .or_else(|| trimmed.strip_prefix("* ["))
    else {
        return false;
    };
    let mut chars = rest.chars();
    chars.next().is_some() && chars.as_str().starts_with("] Q: ")
}

impl Scanner {
    fn problem(&mut self, code: ProblemCode, message: impl Into<String>, range: Range<usize>) {
        self.problems.push(Problem {
            code,
            message: message.into(),
            range,
        });
    }

    /// Register an anchor occurrence, reporting MRK003 on every repeat.
    fn register(&mut self, kind: Kind, id: &str, range: &Range<usize>) {
        self.anchors.entry(id.to_owned()).or_insert(kind);
        let count = self.seen.entry(id.to_owned()).or_insert(0);
        *count += 1;
        if *count > 1 {
            self.duplicated.insert(id.to_owned());
            self.problem(
                ProblemCode::Mrk003,
                format!("duplicate anchor `{id}` on this page"),
                range.clone(),
            );
        }
    }

    fn line(&mut self, line: &str, range: Range<usize>, next: usize) {
        match parse_comment(line) {
            Some(Comment::Open { kind, rest }) => self.opener(kind, &rest, range, next),
            Some(Comment::Close { kind, rest }) => self.closer(kind, &rest, range),
            None => {
                if is_question_record(line) {
                    self.question(line, range);
                }
            }
        }
    }

    fn opener(&mut self, kind: Kind, rest: &[&str], range: Range<usize>, next: usize) {
        let name = if kind == Kind::Seal { "seal" } else { "class" };
        let mut excluded = false;
        let id = rest.first().copied().unwrap_or("");
        let tokens = rest.get(1..).unwrap_or(&[]);
        if valid_id(kind, id) {
            self.register(kind, id, &range);
        } else {
            excluded = true;
            self.problem(
                ProblemCode::Mrk001,
                format!(
                    "`loam:{name}` opener needs an id like `{}<word>[-<word>…]` (max 6 words), found `{id}`",
                    kind.prefix()
                ),
                range.clone(),
            );
        }

        let mut class = None;
        match kind {
            Kind::Seal if !tokens.is_empty() => {
                excluded = true;
                self.problem(
                    ProblemCode::Sel001,
                    "seal openers carry only an id; level, hash and scope live in records",
                    range.clone(),
                );
            }
            Kind::Class => match tokens {
                [token] if CLASS_TOKENS.contains(token) => class = Some((*token).to_owned()),
                _ => {
                    excluded = true;
                    self.problem(
                        ProblemCode::Mrk001,
                        "`loam:class` opener needs exactly one of `local-only`, `project`, `hub-promotable`",
                        range.clone(),
                    );
                }
            },
            _ => {}
        }

        // Nesting: nothing may sit inside a seal, and a class may not nest.
        let conflict = self
            .stack
            .iter()
            .rposition(|open| open.kind == Kind::Seal)
            .map(|index| (index, "span marker inside a seal"))
            .or_else(|| {
                if kind == Kind::Class {
                    self.stack
                        .iter()
                        .rposition(|open| open.kind == Kind::Class)
                        .map(|index| (index, "class span inside a class span"))
                } else {
                    None
                }
            });
        if let Some((index, message)) = conflict {
            self.stack[index].excluded = true;
            excluded = true;
            self.problem(ProblemCode::Mrk002, message, range.clone());
        }

        self.stack.push(Open {
            kind,
            id: id.to_owned(),
            class,
            opener: range,
            content_start: next,
            excluded,
        });
    }

    fn closer(&mut self, kind: Kind, rest: &[&str], range: Range<usize>) {
        let name = if kind == Kind::Seal { "seal" } else { "class" };
        let Some(top) = self.stack.last_mut() else {
            self.problem(
                ProblemCode::Mrk001,
                format!("stray `/loam:{name}` closer with no opener"),
                range,
            );
            return;
        };
        if top.kind != kind {
            top.excluded = true;
            let open = if top.kind == Kind::Seal {
                "seal"
            } else {
                "class"
            };
            self.problem(
                ProblemCode::Mrk001,
                format!("`/loam:{name}` closer does not match the open `loam:{open}` span"),
                range,
            );
            return;
        }
        let mut open = self.stack.pop().expect("stack top checked above");
        if !rest.is_empty() {
            open.excluded = true;
            self.problem(
                ProblemCode::Mrk001,
                format!("`/loam:{name}` closer takes no tokens"),
                range.clone(),
            );
        }
        if !open.excluded {
            self.done.push(Marker {
                kind: open.kind,
                id: open.id,
                class: open.class,
                content: open.content_start..range.start.max(open.content_start),
                opener: open.opener,
                closer: range,
            });
        }
    }

    fn question(&mut self, line: &str, range: Range<usize>) {
        let trimmed = line.trim_end();
        let Some((_, token)) = trimmed.rsplit_once(char::is_whitespace) else {
            return;
        };
        let Some(id) = token.strip_prefix('^') else {
            return;
        };
        if id.starts_with("s-") || id.starts_with("c-") {
            self.problem(
                ProblemCode::Mrk001,
                format!("question record anchor must use the `q-` prefix, found `{id}`"),
                range,
            );
            return;
        }
        if !id.starts_with("q-") {
            // A generic block id, not part of the marker system.
            return;
        }
        if !valid_id(Kind::Question, id) {
            self.problem(
                ProblemCode::Mrk001,
                format!("question anchor needs 2-6 kebab words of [a-z0-9], found `{id}`"),
                range,
            );
            return;
        }
        if let Some(index) = self.stack.iter().rposition(|open| open.kind == Kind::Seal) {
            self.stack[index].excluded = true;
            self.problem(
                ProblemCode::Mrk002,
                "question record inside a seal",
                range.clone(),
            );
        }
        self.register(Kind::Question, id, &range);
        let end = range.end;
        self.done.push(Marker {
            kind: Kind::Question,
            id: id.to_owned(),
            class: None,
            opener: range,
            content: end..end,
            closer: end..end,
        });
    }

    fn finish(mut self) -> Scan {
        while let Some(open) = self.stack.pop() {
            let name = if open.kind == Kind::Seal {
                "seal"
            } else {
                "class"
            };
            self.problem(
                ProblemCode::Mrk001,
                format!("`loam:{name}` opener is never closed"),
                open.opener,
            );
        }
        let duplicated = self.duplicated;
        let mut markers = self
            .done
            .into_iter()
            .filter(|marker| !duplicated.contains(&marker.id))
            .collect::<Vec<_>>();
        markers.sort_by_key(|marker| marker.opener.start);
        let mut problems = self.problems;
        problems.sort_by_key(|problem| problem.range.start);
        Scan {
            markers,
            anchors: self.anchors,
            problems,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const M1: &str = "---\ntitle: T\n---\n\n# Page\n\n\
<!-- loam:class c-payroll-notes local-only -->\n\
Payroll notes.\n\
\n\
<!-- loam:seal s-tls-rung-order -->\n\
TLS tries the bundled rung first.\n\
<!-- /loam:seal -->\n\
<!-- /loam:class -->\n\
\n\
- [ ] Q: Who signs off hosting? ^q-hosting-signoff\n";

    fn codes(scan: &Scan) -> Vec<&'static str> {
        scan.problems.iter().map(|p| p.code.as_str()).collect()
    }

    fn fenced(source: &str) -> String {
        format!("```md\n{source}```\n")
    }

    #[test]
    fn recognises_every_kind_with_ids_and_ranges() {
        let result = scan(M1);
        assert!(result.problems.is_empty(), "{:?}", result.problems);
        assert_eq!(result.markers.len(), 3);

        let class = &result.markers[0];
        assert_eq!(class.kind, Kind::Class);
        assert_eq!(class.id, "c-payroll-notes");
        assert_eq!(class.class.as_deref(), Some("local-only"));
        assert_eq!(
            &M1[class.opener.clone()],
            "<!-- loam:class c-payroll-notes local-only -->"
        );
        assert_eq!(&M1[class.closer.clone()], "<!-- /loam:class -->");

        let seal = &result.markers[1];
        assert_eq!(seal.kind, Kind::Seal);
        assert_eq!(seal.id, "s-tls-rung-order");
        assert_eq!(seal.class, None);
        assert_eq!(
            &M1[seal.content.clone()],
            "TLS tries the bundled rung first.\n"
        );
        assert_eq!(
            &M1[seal.opener.clone()],
            "<!-- loam:seal s-tls-rung-order -->"
        );
        assert_eq!(&M1[seal.closer.clone()], "<!-- /loam:seal -->");

        let question = &result.markers[2];
        assert_eq!(question.kind, Kind::Question);
        assert_eq!(question.id, "q-hosting-signoff");
        assert_eq!(
            &M1[question.opener.clone()],
            "- [ ] Q: Who signs off hosting? ^q-hosting-signoff"
        );

        assert_eq!(result.anchors.len(), 3);
        assert_eq!(result.anchors["s-tls-rung-order"], Kind::Seal);
        assert_eq!(result.anchors["c-payroll-notes"], Kind::Class);
        assert_eq!(result.anchors["q-hosting-signoff"], Kind::Question);
    }

    #[test]
    fn question_records_accept_every_state_marker() {
        for state in [" ", "x", ">", "-"] {
            let source = format!("* [{state}] Q: why? ^q-some-reason\n");
            let result = scan(&source);
            assert_eq!(result.markers.len(), 1, "state {state:?}");
        }
        // No anchor, or a generic block id, is not a marker and not a problem.
        let result = scan("- [ ] Q: why?\n- [ ] Q: why? ^plain\n- [ ] note ^q-some-reason\n");
        assert!(result.markers.is_empty());
        assert!(result.problems.is_empty());
    }

    #[test]
    fn ignores_markers_in_fenced_and_indented_code_and_front_matter() {
        let fenced_page = fenced(M1);
        let scan_fenced = scan(&fenced_page);
        assert!(scan_fenced.markers.is_empty());
        assert!(scan_fenced.anchors.is_empty());
        assert!(scan_fenced.problems.is_empty());

        let tilde = format!("~~~\n{M1}~~~\n");
        assert!(scan(&tilde).markers.is_empty());

        let indented =
            "Intro\n\n    <!-- loam:seal s-tls-rung-order -->\n    body\n    <!-- /loam:seal -->\n";
        let scan_indented = scan(indented);
        assert!(scan_indented.markers.is_empty());
        assert!(scan_indented.problems.is_empty());

        let in_list = "- item\n\n  ```\n  <!-- loam:seal s-tls-rung-order -->\n  ```\n";
        assert!(scan(in_list).problems.is_empty());

        let front = "---\nnote: |\n  <!-- loam:seal s-front-matter -->\n---\n\nBody\n";
        let scan_front = scan(front);
        assert!(scan_front.markers.is_empty());
        assert!(scan_front.problems.is_empty());

        let inline = "Use `<!-- loam:seal s-tls-rung-order -->` to open a seal.\n";
        let scan_inline = scan(inline);
        assert!(scan_inline.markers.is_empty());
        assert!(scan_inline.problems.is_empty());

        let question_in_code = "```\n- [ ] Q: why? ^q-some-reason\n```\n";
        assert!(scan(question_in_code).markers.is_empty());
    }

    #[test]
    fn ignores_other_loam_comment_kinds() {
        let source = "<!-- loam:memory-map · generated from wiki/index.md · do not edit by hand -->\nmap\n<!-- /loam:memory-map -->\n<!-- loam:other x -->\n<!-- loam:sealed s-aa-bb -->\n<!-- plain comment -->\n";
        let result = scan(source);
        assert!(result.markers.is_empty());
        assert!(result.problems.is_empty());
    }

    #[test]
    fn unbalanced_and_wrong_closer_report_mrk001() {
        // Unclosed opener at EOF.
        let scan_unclosed = scan("<!-- loam:seal s-aa-bb -->\nbody\n");
        assert_eq!(codes(&scan_unclosed), ["MRK001"]);
        assert!(scan_unclosed.markers.is_empty());

        // Stray closer.
        let scan_stray = scan("body\n<!-- /loam:seal -->\n");
        assert_eq!(codes(&scan_stray), ["MRK001"]);

        // Wrong-kind closer: reported once, span never returned.
        let wrong = "<!-- loam:seal s-aa-bb -->\nbody\n<!-- /loam:class -->\n<!-- /loam:seal -->\n";
        let scan_wrong = scan(wrong);
        assert_eq!(codes(&scan_wrong), ["MRK001"]);
        assert!(scan_wrong.markers.is_empty());

        // Bad id, kind/prefix mismatch and missing id.
        for opener in [
            "<!-- loam:seal Bad_Id -->",
            "<!-- loam:seal c-aa-bb -->",
            "<!-- loam:seal s-Only -->",
            "<!-- loam:seal s-a-b-c-d-e-f-g -->",
            "<!-- loam:seal -->",
        ] {
            let source = format!("{opener}\nbody\n<!-- /loam:seal -->\n");
            let result = scan(&source);
            assert_eq!(codes(&result), ["MRK001"], "{opener}");
            assert!(result.markers.is_empty(), "{opener}");
        }

        // Class token rules.
        for opener in [
            "<!-- loam:class c-aa-bb -->",
            "<!-- loam:class c-aa-bb secret -->",
            "<!-- loam:class c-aa-bb project local-only -->",
        ] {
            let source = format!("{opener}\nbody\n<!-- /loam:class -->\n");
            let result = scan(&source);
            assert_eq!(codes(&result), ["MRK001"], "{opener}");
            assert!(result.markers.is_empty(), "{opener}");
        }

        // Question anchors with the wrong prefix or a bad slug.
        assert_eq!(codes(&scan("- [ ] Q: why? ^s-aa-bb\n")), ["MRK001"]);
        assert_eq!(codes(&scan("- [ ] Q: why? ^q-Bad\n")), ["MRK001"]);
    }

    #[test]
    fn memory_map_closer_never_reports_mrk001() {
        let page = "<!-- /loam:memory-map -->\n";
        assert!(scan(page).problems.is_empty());
    }

    #[test]
    fn illegal_nesting_reports_mrk002() {
        let seal_in_seal = "<!-- loam:seal s-aa-bb -->\n<!-- loam:seal s-cc-dd -->\nx\n<!-- /loam:seal -->\n<!-- /loam:seal -->\n";
        let scan_a = scan(seal_in_seal);
        assert_eq!(codes(&scan_a), ["MRK002"]);
        assert!(scan_a.markers.is_empty());
        assert_eq!(scan_a.anchors.len(), 2);

        let class_in_seal = "<!-- loam:seal s-aa-bb -->\n<!-- loam:class c-aa-bb project -->\nx\n<!-- /loam:class -->\n<!-- /loam:seal -->\n";
        let scan_b = scan(class_in_seal);
        assert_eq!(codes(&scan_b), ["MRK002"]);
        assert!(scan_b.markers.is_empty());

        let class_in_class = "<!-- loam:class c-aa-bb project -->\n<!-- loam:class c-cc-dd project -->\nx\n<!-- /loam:class -->\n<!-- /loam:class -->\n";
        let scan_c = scan(class_in_class);
        assert_eq!(codes(&scan_c), ["MRK002"]);
        assert!(scan_c.markers.is_empty());

        // A question record inside a seal is illegal too; the seal is dropped.
        let question_in_seal =
            "<!-- loam:seal s-aa-bb -->\n- [ ] Q: why? ^q-some-reason\n<!-- /loam:seal -->\n";
        let scan_d = scan(question_in_seal);
        assert_eq!(codes(&scan_d), ["MRK002"]);
        assert!(scan_d.markers.iter().all(|m| m.kind != Kind::Seal));
        assert_eq!(scan_d.anchors.len(), 2);

        // A class may contain seals: that is legal (M1 page).
        assert!(scan(M1).problems.is_empty());
    }

    #[test]
    fn duplicate_anchor_across_kinds_reports_mrk003() {
        // The prefix fixes the kind, so a duplicate can only occur within one
        // kind; the same slug under different prefixes is distinct.
        let source = "<!-- loam:seal s-aa-bb -->\none\n<!-- /loam:seal -->\n<!-- loam:seal s-aa-bb -->\ntwo\n<!-- /loam:seal -->\n- [ ] Q: why? ^q-xx-yy\n- [ ] Q: again? ^q-xx-yy\n";
        let result = scan(source);
        assert_eq!(codes(&result), ["MRK003", "MRK003"]);
        // Reported at the later occurrence.
        let later = source.find("<!-- loam:seal s-aa-bb -->\ntwo").unwrap();
        assert_eq!(result.problems[0].range.start, later);
        // No span or record sharing a duplicated id is returned, but the ids
        // register once so links still resolve.
        assert!(result.markers.is_empty());
        assert_eq!(result.anchors.len(), 2);

        // The same slug under different prefixes is not a duplicate.
        let distinct =
            "<!-- loam:seal s-aa-bb -->\nx\n<!-- /loam:seal -->\n- [ ] Q: why? ^q-aa-bb\n";
        let distinct_scan = scan(distinct);
        assert!(distinct_scan.problems.is_empty());
        assert_eq!(distinct_scan.markers.len(), 2);

        // A question reusing a span's id across kinds has the wrong prefix for
        // its kind, so it reports MRK001 rather than MRK003.
        let crossed =
            "<!-- loam:seal s-aa-bb -->\nx\n<!-- /loam:seal -->\n- [ ] Q: why? ^s-aa-bb\n";
        assert_eq!(codes(&scan(crossed)), ["MRK001"]);
    }

    #[test]
    fn seal_opener_with_tokens_reports_sel001() {
        let source = "<!-- loam:seal s-aa-bb level=canonical -->\nbody\n<!-- /loam:seal -->\n";
        let result = scan(source);
        // Paired with its closer: exactly one problem, no stray MRK001.
        assert_eq!(codes(&result), ["SEL001"]);
        assert!(result.markers.is_empty());
        assert_eq!(result.anchors["s-aa-bb"], Kind::Seal);
    }

    #[test]
    fn malformed_spans_are_never_returned_as_seals() {
        let cases = [
            "<!-- loam:seal s-aa-bb level=canonical -->\nx\n<!-- /loam:seal -->\n",
            "<!-- loam:seal s-aa-bb -->\nx\n",
            "<!-- loam:seal Bad -->\nx\n<!-- /loam:seal -->\n",
            "<!-- loam:seal s-aa-bb -->\nx\n<!-- /loam:class -->\n<!-- /loam:seal -->\n",
            "<!-- loam:seal s-aa-bb -->\n<!-- loam:seal s-cc-dd -->\nx\n<!-- /loam:seal -->\n<!-- /loam:seal -->\n",
            "<!-- loam:seal s-aa-bb -->\nx\n<!-- /loam:seal extra -->\n",
            "<!-- loam:seal s-aa-bb -->\nx\n<!-- /loam:seal -->\n<!-- loam:seal s-aa-bb -->\ny\n<!-- /loam:seal -->\n",
        ];
        for case in cases {
            let result = scan(case);
            assert!(!result.problems.is_empty(), "{case}");
            assert!(
                result.markers.iter().all(|m| m.kind != Kind::Seal),
                "{case}: {:?}",
                result.markers
            );
        }
    }

    #[test]
    fn crlf_ranges_exclude_marker_lines() {
        let source = M1.replace('\n', "\r\n");
        let result = scan(&source);
        assert!(result.problems.is_empty(), "{:?}", result.problems);
        assert_eq!(result.markers.len(), 3);

        let seal = &result.markers[1];
        assert_eq!(
            &source[seal.opener.clone()],
            "<!-- loam:seal s-tls-rung-order -->"
        );
        assert_eq!(&source[seal.closer.clone()], "<!-- /loam:seal -->");
        // Raw bytes, CRLF intact, strictly between the marker lines.
        assert_eq!(
            &source[seal.content.clone()],
            "TLS tries the bundled rung first.\r\n"
        );
        let question = &result.markers[2];
        assert_eq!(
            &source[question.opener.clone()],
            "- [ ] Q: Who signs off hosting? ^q-hosting-signoff"
        );
    }

    #[test]
    fn empty_and_adjacent_spans_have_empty_content() {
        let source = "<!-- loam:seal s-aa-bb -->\n<!-- /loam:seal -->";
        let result = scan(source);
        assert!(result.problems.is_empty());
        assert_eq!(result.markers.len(), 1);
        assert!(result.markers[0].content.is_empty());
        assert_eq!(
            &source[result.markers[0].closer.clone()],
            "<!-- /loam:seal -->"
        );
    }

    #[test]
    fn slug_word_count_boundary() {
        let ok = |id: &str| {
            let source = format!("<!-- loam:seal {id} -->\nx\n<!-- /loam:seal -->\n");
            scan(&source).problems.is_empty()
        };
        assert!(!ok("s-x"), "one word is rejected");
        assert!(ok("s-x-y"), "two words are accepted");
        assert!(ok("s-a-b-c-d-e-f"), "six words are accepted");
        assert!(!ok("s-a-b-c-d-e-f-g"), "seven words are rejected");
        assert!(!ok("s-"), "an empty slug is rejected");
    }

    #[test]
    fn leading_bom_does_not_hide_first_line_marker() {
        let source = "\u{FEFF}<!-- loam:seal s-aa-bb -->\nx\n<!-- /loam:seal -->\n";
        let result = scan(source);
        assert!(result.problems.is_empty(), "{:?}", result.problems);
        assert_eq!(result.markers.len(), 1);
        assert_eq!(
            &source[result.markers[0].opener.clone()],
            "<!-- loam:seal s-aa-bb -->"
        );
    }

    #[test]
    fn bom_shifts_front_matter_and_fence_exclusions() {
        let front = "\u{FEFF}---\nnote: |\n  <!-- loam:seal s-front-matter -->\n---\n\nBody\n";
        let result = scan(front);
        assert!(result.markers.is_empty());
        assert!(result.problems.is_empty(), "{:?}", result.problems);

        let fence = format!("\u{FEFF}{}", fenced("<!-- loam:seal s-aa-bb -->\nx\n"));
        let result = scan(&fence);
        assert!(result.markers.is_empty());
        assert!(result.problems.is_empty(), "{:?}", result.problems);

        // Ranges still index the original, BOM-prefixed source.
        let page = format!(
            "{}---\ntitle: T
---
{M1}",
            "\u{FEFF}"
        );
        let result = scan(&page);
        assert!(result.problems.is_empty(), "{:?}", result.problems);
        let seal = &result.markers[1];
        assert_eq!(
            &page[seal.opener.clone()],
            "<!-- loam:seal s-tls-rung-order -->"
        );
    }

    #[test]
    fn lone_cr_terminates_lines() {
        let source = "<!-- loam:seal s-aa-bb -->\rbody\r<!-- /loam:seal -->\r";
        let result = scan(source);
        assert!(result.problems.is_empty(), "{:?}", result.problems);
        assert_eq!(result.markers.len(), 1);
        let seal = &result.markers[0];
        assert_eq!(&source[seal.content.clone()], "body\r");
        assert_eq!(&source[seal.closer.clone()], "<!-- /loam:seal -->");
    }

    #[test]
    fn malformed_first_occurrence_still_registers_the_id() {
        let source = "<!-- loam:seal s-aa-bb level=x -->\na\n<!-- /loam:seal -->\n<!-- loam:seal s-aa-bb -->\nb\n<!-- /loam:seal -->\n";
        let result = scan(source);
        assert_eq!(codes(&result), ["SEL001", "MRK003"]);
        assert!(result.markers.is_empty());
        assert_eq!(result.anchors.len(), 1);
    }
}
