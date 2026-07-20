use std::path::Path;

use crate::markdown;
use crate::memory;

const DOMAINS: [&str; 3] = ["markdown", "memory", "work"];

pub fn run(mut args: impl Iterator<Item = String>) -> i32 {
    let mut workspace = None;
    let mut only = None;
    let mut now = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--only" => match args.next() {
                Some(value) if DOMAINS.contains(&value.as_str()) && only.is_none() => {
                    only = Some(value)
                }
                _ => {
                    usage();
                    return 1;
                }
            },
            "--now" => match args.next() {
                Some(value) => now = Some(value),
                None => {
                    usage();
                    return 1;
                }
            },
            value if value.starts_with('-') => {
                usage();
                return 1;
            }
            value if workspace.is_none() => workspace = Some(value.to_owned()),
            _ => {
                usage();
                return 1;
            }
        }
    }

    let Some(workspace) = workspace else {
        usage();
        return 1;
    };
    let workspace = Path::new(&workspace);
    if !workspace.is_dir() {
        eprintln!("loam lint: workspace not found: {}", workspace.display());
        return 1;
    }

    let today = match now {
        Some(value) => match memory::timestamp_days(&value) {
            Some(days) => days,
            None => {
                eprintln!("loam lint: --now must be 'YYYY-MM-DD HH:MM ±HH:MM'");
                return 1;
            }
        },
        None => memory::today_utc(),
    };

    let selected = |domain: &str| only.as_deref().map(|value| value == domain).unwrap_or(true);
    let mut findings = Vec::new();

    if selected("markdown") {
        match markdown::lint_workspace(workspace) {
            Ok(diagnostics) => findings.extend(diagnostics.into_iter().map(Finding::from_markdown)),
            Err(error) => {
                eprintln!("loam lint: {error}");
                return 1;
            }
        }
    }
    if selected("memory") {
        memory::wiki_findings(workspace, &mut findings);
    }
    if selected("work") {
        memory::work_findings(workspace, today, &mut findings);
    }

    findings.sort_by(Finding::cmp);
    for finding in &findings {
        println!("{}", finding.to_json());
    }
    if findings.is_empty() {
        0
    } else {
        2
    }
}

fn usage() {
    eprintln!(
        "Usage: loam lint [--only markdown|memory|work] <workspace-root> [--now 'YYYY-MM-DD HH:MM ±HH:MM']\n\n  --only runs exactly one domain; the default runs all three.\n  --now overrides the clock for date-relative rules; it exists for\n  deterministic tests and replay, not for routine use."
    );
}

#[derive(Clone, Copy)]
pub enum Severity {
    Error,
    Warning,
    Info,
}

impl Severity {
    fn as_str(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Info => "info",
        }
    }
}

pub struct Finding {
    pub rule: &'static str,
    pub rule_name: &'static str,
    pub severity: Severity,
    pub file: String,
    pub line: usize,
    pub column: usize,
    pub end_line: usize,
    pub end_column: usize,
    pub description: String,
    pub detail: String,
    pub context: String,
    pub evidence: Vec<(String, String)>,
    pub target: Option<String>,
    pub candidates: Vec<String>,
}

impl Finding {
    /// A file-level finding with neutral range, context, and candidate values.
    pub fn file(
        rule: &'static str,
        rule_name: &'static str,
        severity: Severity,
        file: &str,
        description: &'static str,
    ) -> Self {
        Self {
            rule,
            rule_name,
            severity,
            file: file.to_owned(),
            line: 0,
            column: 0,
            end_line: 0,
            end_column: 0,
            description: description.to_owned(),
            detail: String::new(),
            context: String::new(),
            evidence: Vec::new(),
            target: None,
            candidates: Vec::new(),
        }
    }

    fn from_markdown(diagnostic: markdown::Diagnostic) -> Self {
        Self {
            rule: diagnostic.rule,
            rule_name: diagnostic.rule_name,
            severity: Severity::Error,
            file: diagnostic.file,
            line: diagnostic.line,
            column: diagnostic.column,
            end_line: diagnostic.end_line,
            end_column: diagnostic.end_column,
            description: diagnostic.description.to_owned(),
            detail: diagnostic.detail,
            context: diagnostic.context,
            evidence: Vec::new(),
            target: diagnostic.target,
            candidates: diagnostic.candidates,
        }
    }

    pub fn with_target(mut self, target: &str) -> Self {
        self.target = Some(target.to_owned());
        self
    }

    pub fn with_evidence(mut self, key: &str, value: &str) -> Self {
        self.evidence.push((key.to_owned(), value.to_owned()));
        self
    }

    /// Rule prefixes are the single source of truth for which domain a finding
    /// belongs to, so no call site has to repeat it.
    fn domain(&self) -> &'static str {
        match &self.rule[..3] {
            "MEM" => "memory",
            "WRK" => "work",
            _ => "markdown",
        }
    }

    fn cmp(left: &Self, right: &Self) -> std::cmp::Ordering {
        left.file
            .cmp(&right.file)
            .then(left.line.cmp(&right.line))
            .then(left.column.cmp(&right.column))
            .then(left.domain().cmp(right.domain()))
            .then(left.rule.cmp(right.rule))
            .then(left.target.cmp(&right.target))
            .then(left.candidates.cmp(&right.candidates))
    }

    fn to_json(&self) -> String {
        let evidence = self
            .evidence
            .iter()
            .map(|(key, value)| format!("\"{}\":\"{}\"", escape(key), escape(value)))
            .collect::<Vec<_>>()
            .join(",");
        let candidates = self
            .candidates
            .iter()
            .map(|value| format!("\"{}\"", escape(value)))
            .collect::<Vec<_>>()
            .join(",");
        let target = match &self.target {
            Some(value) => format!("\"{}\"", escape(value)),
            None => "null".to_owned(),
        };
        format!(
            "{{\"schema_version\":\"1\",\"domain\":\"{}\",\"rule\":\"{}\",\"rule_names\":[\"{}\",\"{}\"],\"severity\":\"{}\",\"file\":\"{}\",\"line\":{},\"column\":{},\"end_line\":{},\"end_column\":{},\"description\":\"{}\",\"detail\":\"{}\",\"context\":\"{}\",\"evidence\":{{{}}},\"target\":{},\"candidates\":[{}]}}",
            self.domain(),
            self.rule,
            self.rule,
            self.rule_name,
            self.severity.as_str(),
            escape(&self.file),
            self.line,
            self.column,
            self.end_line,
            self.end_column,
            escape(&self.description),
            escape(&self.detail),
            escape(&self.context),
            evidence,
            target,
            candidates
        )
    }
}

fn escape(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            value if (value as u32) < 0x20 => {
                output.push_str(&format!("\\u{:04x}", value as u32));
            }
            value => output.push(value),
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding(candidates: &[&str]) -> Finding {
        let mut finding = Finding::file(
            "LMD002",
            "ambiguous-document",
            Severity::Error,
            "wiki/index.md",
            "Wikilink resolves to multiple documents",
        );
        finding.candidates = candidates.iter().map(|value| (*value).to_owned()).collect();
        finding
    }

    #[test]
    fn ordering_breaks_candidate_ties_deterministically() {
        let left = finding(&["wiki/a.md"]);
        let right = finding(&["wiki/b.md"]);

        assert_eq!(Finding::cmp(&left, &right), std::cmp::Ordering::Less);
        assert_eq!(Finding::cmp(&right, &left), std::cmp::Ordering::Greater);
        assert_eq!(
            Finding::cmp(&left, &finding(&["wiki/a.md"])),
            std::cmp::Ordering::Equal
        );
    }
}
