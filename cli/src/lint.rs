use std::path::Path;

use crate::guidance;
use crate::markdown;
use crate::markers;
use crate::memory;

const DOMAINS: [&str; 5] = ["guidance", "markdown", "memory", "seals", "work"];

pub fn run(mut args: impl Iterator<Item = String>) -> i32 {
    let mut workspace = None;
    let mut only = None;
    let mut now = None;
    let mut fix = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--fix" => fix = true,
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

    // SEL008 from the fixer follows the fixer's domain selection (guidance),
    // not the seals domain, so the skip stays visible under `--only guidance`.
    if fix && selected("guidance") {
        match guidance::fix(workspace) {
            Ok(guidance::FixOutcome::SealSkipped(anchor)) => findings.push(
                Finding::file(
                    markers::SEL008_CODE,
                    markers::SEL008_NAME,
                    Severity::Info,
                    "AGENTS.md",
                    markers::SEL008_DESCRIPTION,
                )
                .with_evidence("anchor", &anchor),
            ),
            Ok(_) => {}
            Err(error) => {
                eprintln!("loam lint: {error}");
                return 1;
            }
        }
    }
    if selected("guidance") {
        guidance::findings(workspace, &mut findings);
    }
    if selected("markdown") || selected("seals") {
        match markdown::lint_workspace(workspace) {
            Ok(diagnostics) => findings.extend(
                diagnostics
                    .into_iter()
                    .map(Finding::from_markdown)
                    .filter(|finding| selected(finding.domain())),
            ),
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
    // Info findings are advisory: only a warning or error fails the run.
    if findings
        .iter()
        .any(|finding| !matches!(finding.severity, Severity::Info))
    {
        2
    } else {
        0
    }
}

fn usage() {
    eprintln!(
        "Usage: loam lint [--only guidance|markdown|memory|seals|work] [--fix] <workspace-root> [--now 'YYYY-MM-DD HH:MM ±HH:MM']\n\n  --only runs exactly one domain; the default runs all five.\n  --fix regenerates the `loam:memory-map` region in AGENTS.md; it is the\n  only writing option and touches nothing outside the fenced markers.\n  --now overrides the clock for date-relative rules; it exists for\n  deterministic tests and replay, not for routine use."
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

    /// A finding anchored to one source line.
    pub fn line(
        rule: &'static str,
        rule_name: &'static str,
        severity: Severity,
        file: &str,
        line: usize,
        context: &str,
        description: &'static str,
    ) -> Self {
        let mut finding = Self::file(rule, rule_name, severity, file, description);
        finding.line = line;
        finding.column = 1;
        finding.end_line = line;
        finding.end_column = context.chars().count() + 1;
        finding.context = context.to_owned();
        finding
    }

    fn from_markdown(diagnostic: markdown::Diagnostic) -> Self {
        Self {
            rule: diagnostic.rule,
            rule_name: diagnostic.rule_name,
            severity: diagnostic.severity,
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
            "GDN" => "guidance",
            "MEM" => "memory",
            "SEL" => "seals",
            "MRK" => "markdown",
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

    fn temp_workspace(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "loam-lintfix-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join("wiki/topics")).unwrap();
        std::fs::write(dir.join("wiki/index.md"), "# Index\n\n## Overview\n").unwrap();
        std::fs::write(dir.join("wiki/topics/auth.md"), "# Auth\n").unwrap();
        dir
    }

    #[test]
    fn fix_skips_sealed() {
        let dir = temp_workspace("sealed");
        let agents = format!(
            "<!-- loam:seal s-map-copy -->\n{}\nTopics (1): stale\n{}\n<!-- /loam:seal -->\n",
            guidance::MAP_OPEN,
            guidance::MAP_CLOSE
        );
        std::fs::write(dir.join("AGENTS.md"), &agents).unwrap();
        let wiki_before = std::fs::read(dir.join("wiki/topics/auth.md")).unwrap();

        let outcome = guidance::fix(&dir).unwrap();
        assert_eq!(
            outcome,
            guidance::FixOutcome::SealSkipped("s-map-copy".to_owned())
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("AGENTS.md")).unwrap(),
            agents
        );
        assert_eq!(
            std::fs::read(dir.join("wiki/topics/auth.md")).unwrap(),
            wiki_before
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fix_rewrites_an_unsealed_region_and_never_writes_under_wiki() {
        let dir = temp_workspace("unsealed");
        let agents = format!(
            "<!-- loam:seal s-other-note -->\nkept\n<!-- /loam:seal -->\n\n{}\nTopics (1): stale\n{}\n",
            guidance::MAP_OPEN,
            guidance::MAP_CLOSE
        );
        std::fs::write(dir.join("AGENTS.md"), &agents).unwrap();
        let snapshot = |root: &std::path::Path| {
            let mut out = Vec::new();
            for entry in ["index.md", "topics/auth.md"] {
                out.push(std::fs::read(root.join("wiki").join(entry)).unwrap());
            }
            out
        };
        let wiki_before = snapshot(&dir);

        assert_eq!(guidance::fix(&dir).unwrap(), guidance::FixOutcome::Written);
        let after = std::fs::read_to_string(dir.join("AGENTS.md")).unwrap();
        assert!(after.starts_with("<!-- loam:seal s-other-note -->\nkept\n<!-- /loam:seal -->\n"));
        assert!(after.contains("Topics (1): auth"));
        assert_eq!(snapshot(&dir), wiki_before);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
