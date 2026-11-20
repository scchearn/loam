//! `seals` lint findings SEL002–SEL008 (spec D9), derived from the verify
//! engine. Lint is read-only: it never writes a record, a signature or a
//! hash, and a finding never blocks or reverts a memory write.
//!
//! SEL001 (a seal opener carrying tokens) comes from the marker scan inside
//! the markdown pass, not from here.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use chrono::{DateTime, FixedOffset};

use crate::lint::{Finding, Severity};
use crate::markers::{self, Kind};
use crate::seal::trust;
use crate::seal::verdict::code;
use crate::seal::verify::{self, Env, Report};

const RULE_UNPAIRED: (&str, &str, &str) = (
    "SEL002",
    "seal-unpaired",
    "Seal marker and seal record do not pair up",
);
const RULE_INVALID: (&str, &str, &str) = (
    "SEL003",
    "seal-record-invalid",
    "Seal record is malformed, forged or from a revoked signer",
);
const RULE_DRIFTED: (&str, &str, &str) = (
    "SEL004",
    "seal-drifted",
    "Sealed span no longer matches its record",
);
const RULE_EVIDENCE: (&str, &str, &str) = (
    "SEL005",
    "seal-evidence-stale",
    "Seal evidence drifted or no longer resolves",
);
const RULE_DUE: (&str, &str, &str) = ("SEL006", "seal-review-due", "Seal review or recheck is due");
const RULE_CITES: (&str, &str, &str) = (
    "SEL007",
    "seal-cites-unsound",
    "Seal cites a retracted, drifted or unknown seal",
);
// SEL008 shares its code and name with the rewriting-tool skip in
// `markers`; the advisory text covers the verifier-side notes.
const RULE_ADVISORY: (&str, &str, &str) = (
    markers::SEL008_CODE,
    markers::SEL008_NAME,
    "Seal advisory: level capped or verifier note",
);

/// A well-formed seal span found on a page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpanRef {
    /// Wiki-relative page name: forward slashes, no `.md`.
    pub page: String,
    pub anchor: String,
    pub line: usize,
}

fn rule_for(lint_code: &str) -> Option<((&'static str, &'static str, &'static str), Severity)> {
    Some(match lint_code {
        "SEL003" => (RULE_INVALID, Severity::Error),
        "SEL004" => (RULE_DRIFTED, Severity::Warning),
        "SEL005" => (RULE_EVIDENCE, Severity::Warning),
        "SEL006" => (RULE_DUE, Severity::Warning),
        "SEL007" => (RULE_CITES, Severity::Warning),
        "SEL008" => (RULE_ADVISORY, Severity::Info),
        _ => return None,
    })
}

fn file_for(report: &Report) -> String {
    match report.current_page.as_ref().or(report.page.as_ref()) {
        Some(page) => format!("wiki/{page}.md"),
        None => format!("wiki/.seals/records/{}.json", report.id),
    }
}

fn finding(
    rule: (&'static str, &'static str, &'static str),
    severity: Severity,
    file: &str,
    line: usize,
    detail: String,
) -> Finding {
    let mut finding = Finding::file(rule.0, rule.1, severity, file, rule.2);
    if line > 0 {
        finding.line = line;
        finding.column = 1;
        finding.end_line = line;
        finding.end_column = 1;
    }
    finding.detail = detail;
    finding
}

/// One finding per rule per seal, with every contributing verify code in the
/// detail. Success codes never map to a lint rule.
pub fn report_findings(report: &Report, out: &mut Vec<Finding>) {
    if report.superseded_by.is_some() {
        return;
    }
    let file = file_for(report);
    let codes = &report.outcome.codes;
    let mut by_rule: Vec<(&'static str, Vec<&'static str>)> = Vec::new();
    for verify_code in codes.failure.iter().chain(&codes.informational) {
        let Some(lint_code) = code::lint_code(verify_code) else {
            continue;
        };
        match by_rule.iter_mut().find(|(rule, _)| *rule == lint_code) {
            Some((_, list)) => list.push(verify_code),
            None => by_rule.push((lint_code, vec![verify_code])),
        }
    }
    for (lint_code, list) in by_rule {
        let Some((rule, severity)) = rule_for(lint_code) else {
            continue;
        };
        let mut found = finding(rule, severity, &file, 0, list.join(", "))
            .with_evidence("seal", &report.id)
            .with_evidence("verdict", report.outcome.verdict.as_str());
        if let Some(anchor) = &report.anchor {
            found = found.with_evidence("anchor", anchor);
        }
        out.push(found);
    }
    // A record whose anchor no longer resolves to a well-formed span is also
    // a record without an anchor (SEL002), whatever else it reports.
    if codes.failure.contains(&code::SPAN_MISSING) {
        let mut found = finding(
            RULE_UNPAIRED,
            Severity::Warning,
            &file,
            0,
            "record without anchor".to_owned(),
        )
        .with_evidence("seal", &report.id);
        if let Some(anchor) = &report.anchor {
            found = found.with_evidence("anchor", anchor);
        }
        if let Some(location) = &report.likely_location {
            found = found.with_evidence("likely_location", location);
        }
        out.push(found);
    }
}

/// SEL002 for a seal marker whose anchor no seal record names. (The reverse,
/// a record without an anchor, comes from [`report_findings`].)
pub fn unpaired_markers(spans: &[SpanRef], reports: &[Report], out: &mut Vec<Finding>) {
    // Anchors are unique per page only, so pair on (page, anchor). A carried
    // or moved span pairs through the page it currently sits on.
    let recorded: BTreeSet<(&str, &str)> = reports
        .iter()
        .filter_map(|report| {
            let page = report.current_page.as_deref().or(report.page.as_deref())?;
            Some((page, report.anchor.as_deref()?))
        })
        .collect();
    for span in spans {
        if recorded.contains(&(span.page.as_str(), span.anchor.as_str())) {
            continue;
        }
        out.push(
            finding(
                RULE_UNPAIRED,
                Severity::Warning,
                &format!("wiki/{}.md", span.page),
                span.line,
                "marker without record".to_owned(),
            )
            .with_evidence("anchor", &span.anchor),
        );
    }
}

fn collect_spans(wiki: &Path) -> Result<Vec<SpanRef>, String> {
    let mut spans = Vec::new();
    for (page, path) in verify::markdown_files(wiki)? {
        let Ok(bytes) = fs::read(&path) else { continue };
        let Ok(source) = std::str::from_utf8(&bytes) else {
            continue;
        };
        for marker in markers::scan(source)
            .markers
            .iter()
            .filter(|marker| marker.kind == Kind::Seal)
        {
            let line = source[..marker.opener.start].matches('\n').count() + 1;
            spans.push(SpanRef {
                page: page.clone(),
                anchor: marker.id.clone(),
                line,
            });
        }
    }
    Ok(spans)
}

/// Appends SEL002–SEL008 for the workspace's wiki. A workspace with no
/// `wiki/` has nothing to check. Reads only.
pub fn lint_findings(
    workspace: &Path,
    now: DateTime<FixedOffset>,
    out: &mut Vec<Finding>,
) -> Result<(), String> {
    let wiki = workspace.join("wiki");
    if !wiki.is_dir() {
        return Ok(());
    }
    let env = Env::load(&wiki, trust::seal_dir().ok(), now)?;
    let reports = verify::verify_all(&env)?;
    for report in &reports {
        report_findings(report, out);
    }
    unpaired_markers(&collect_spans(&wiki)?, &reports, out);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seal::policy::Level;
    use crate::seal::verdict::{Codes, Outcome, Verdict};

    fn report(verdict: Verdict, failure: &[&'static str], info: &[&'static str]) -> Report {
        Report {
            id: "abc123".to_owned(),
            subject: Some("alpha#^s-one".to_owned()),
            page: Some("alpha".to_owned()),
            anchor: Some("s-one".to_owned()),
            current_page: Some("alpha".to_owned()),
            outcome: Outcome {
                verdict,
                effective_level: Some(Level::Evidence),
                requested_level: Level::Evidence,
                codes: Codes {
                    success: vec![code::SIGNATURE_VALID],
                    informational: info.to_vec(),
                    failure: failure.to_vec(),
                },
            },
            base_level: None,
            signer: None,
            time_verified: None,
            likely_location: None,
            superseded_by: None,
        }
    }

    fn findings_of(report: &Report) -> Vec<Finding> {
        let mut out = Vec::new();
        report_findings(report, &mut out);
        out
    }

    /// (rule, severity) pairs, sorted for stable comparison.
    fn rules(findings: &[Finding]) -> Vec<(&'static str, &'static str)> {
        let mut rules: Vec<_> = findings
            .iter()
            .map(|f| {
                let severity = match f.severity {
                    Severity::Error => "error",
                    Severity::Warning => "warning",
                    Severity::Info => "info",
                };
                (f.rule, severity)
            })
            .collect();
        rules.sort();
        rules
    }

    #[test]
    fn every_rule_sel002_through_sel008_has_its_severity_and_domain() {
        let span = SpanRef {
            page: "alpha".to_owned(),
            anchor: "s-orphan".to_owned(),
            line: 3,
        };
        let mut marker_only = Vec::new();
        unpaired_markers(&[span], &[], &mut marker_only);

        let cases: Vec<(&str, Vec<Finding>, (&str, &str))> = vec![
            (
                "SEL002 marker without record",
                marker_only,
                ("SEL002", "warning"),
            ),
            (
                "SEL002 record without anchor",
                findings_of(&report(Verdict::Drifted, &[code::SPAN_MISSING], &[]))
                    .into_iter()
                    .filter(|f| f.rule == "SEL002")
                    .collect(),
                ("SEL002", "warning"),
            ),
            (
                "SEL003",
                findings_of(&report(Verdict::Forged, &[code::SIGNATURE_INVALID], &[])),
                ("SEL003", "error"),
            ),
            (
                "SEL004",
                findings_of(&report(Verdict::Drifted, &[code::BINDING_MISMATCH], &[]))
                    .into_iter()
                    .filter(|f| f.rule == "SEL004")
                    .collect(),
                ("SEL004", "warning"),
            ),
            (
                "SEL005",
                findings_of(&report(Verdict::Stale, &[code::EVIDENCE_DRIFTED], &[])),
                ("SEL005", "warning"),
            ),
            (
                "SEL006",
                findings_of(&report(Verdict::Stale, &[code::REVIEW_BY_PASSED], &[])),
                ("SEL006", "warning"),
            ),
            (
                "SEL007",
                findings_of(&report(Verdict::Stale, &[code::CITED_NOT_CITABLE], &[])),
                ("SEL007", "warning"),
            ),
            (
                "SEL008",
                findings_of(&report(
                    Verdict::Citable,
                    &[],
                    &[code::LEVEL_ABOVE_ATTAINABLE],
                )),
                ("SEL008", "info"),
            ),
        ];
        for (label, found, expected) in cases {
            assert_eq!(rules(&found), vec![expected], "{label}");
            for f in &found {
                assert_eq!(f.domain(), "seals", "{label}");
            }
        }
    }

    #[test]
    fn paired_markers_and_citable_seals_report_nothing() {
        let span = SpanRef {
            page: "alpha".to_owned(),
            anchor: "s-one".to_owned(),
            line: 3,
        };
        let citable = report(
            Verdict::Citable,
            &[],
            &[code::DISPUTED, code::REVIEW_BY_CLAMPED],
        );
        let mut out = Vec::new();
        unpaired_markers(&[span], std::slice::from_ref(&citable), &mut out);
        report_findings(&citable, &mut out);
        assert!(out.is_empty(), "{:?}", rules(&out));
    }

    #[test]
    fn the_same_anchor_on_two_pages_pairs_per_page() {
        let span = |page: &str| SpanRef {
            page: page.to_owned(),
            anchor: "s-one".to_owned(),
            line: 3,
        };
        let recorded = report(Verdict::Citable, &[], &[]);
        let mut out = Vec::new();
        unpaired_markers(&[span("alpha"), span("beta")], &[recorded], &mut out);
        assert_eq!(out.len(), 1, "{:?}", rules(&out));
        assert_eq!(out[0].file, "wiki/beta.md");
        assert_eq!(rules(&out), vec![("SEL002", "warning")]);
    }

    #[test]
    fn unattended_key_is_an_sel008_info_carrying_its_code() {
        let found = findings_of(&report(Verdict::Citable, &[], &[code::UNATTENDED_KEY]));
        assert_eq!(rules(&found), vec![("SEL008", "info")]);
        assert!(found[0].detail.contains("seal.canonical.unattended_key"));
    }

    #[test]
    fn one_finding_per_rule_per_seal_collects_every_code() {
        let found = findings_of(&report(
            Verdict::Stale,
            &[code::REVIEW_BY_PASSED, code::RECHECK_DUE],
            &[],
        ));
        assert_eq!(rules(&found), vec![("SEL006", "warning")]);
        assert!(found[0].detail.contains("review_by_passed"));
        assert!(found[0].detail.contains("recheck_due"));
    }

    #[test]
    fn span_missing_reports_sel004_and_sel002() {
        let found = findings_of(&report(Verdict::Drifted, &[code::SPAN_MISSING], &[]));
        assert_eq!(
            rules(&found),
            vec![("SEL002", "warning"), ("SEL004", "warning")]
        );
    }
}
