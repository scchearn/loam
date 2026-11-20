use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const NOW: &str = "2026-07-20 12:00 +02:00";

fn temporary_workspace(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be after epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("loam-lint-{label}-{nonce}"));
    fs::create_dir_all(&path).expect("temporary workspace should be created");
    path
}

fn write(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("parent directory should be created");
    }
    fs::write(path, content).expect("file should be written");
}

fn loam(args: &[&str]) -> std::process::Output {
    let binary = std::env::var("CARGO_BIN_EXE_loam").expect("cargo should provide the loam binary");
    // Default lint loads the seal trust store, so keep it off the real config.
    let config = std::env::temp_dir().join(format!("loam-lint-config-{}", std::process::id()));
    Command::new(binary)
        .args(args)
        .env("LOAM_CONFIG_DIR", config)
        .env("LOAM_WORKSPACES", "off")
        .output()
        .expect("loam should run")
}

/// One defect per domain: a seal opener with tokens (seals), a duplicate heading and broken wikilink (markdown),
/// a legacy `overview.md` (memory), and a goal missing from the index (work).
fn defective_workspace(label: &str) -> PathBuf {
    let workspace = temporary_workspace(label);
    write(
        &workspace.join("wiki/index.md"),
        "# Index\n\n## Overview\n\nSee [[ghost-page]].\n\n## Overview\n",
    );
    write(
        &workspace.join("wiki/overview.md"),
        "# Overview\n\nLegacy.\n",
    );
    write(
        &workspace.join("wiki/sealed.md"),
        "# Sealed\n\n<!-- loam:seal s-aa-bb level=canonical -->\nbody\n<!-- /loam:seal -->\n",
    );
    write(
        &workspace.join("goals/sample-goal.md"),
        "---\ntitle: Sample Goal\nslug: sample-goal\nstatus: active\ncreated_at: 2026-07-19 09:00 +02:00\nupdated_at: 2026-07-19 09:00 +02:00\nreviewed_at: null\nnext_review_at: 2026-08-19 09:00 +02:00\n---\n\n# Sample Goal\n\n## Intent\n\nText.\n\n## Validation contract\n\nText.\n\n## Linked work\n\n- none\n\n## Current state\n\nText.\n\n## Reviews\n\n- none\n",
    );
    write(
        &workspace.join("goals/INDEX.md"),
        "# Goals Index\n\n| Status | Goal | Path |\n|---|---|---|\n",
    );
    workspace
}

fn domains(stdout: &str) -> Vec<String> {
    field_values(stdout, "domain")
}

fn field_values(stdout: &str, name: &str) -> Vec<String> {
    let marker = format!("\"{name}\":\"");
    stdout
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| {
            let start = line
                .find(&marker)
                .unwrap_or_else(|| panic!("finding should carry {name}: {line}"))
                + marker.len();
            let rest = &line[start..];
            rest[..rest.find('"').expect("value should be terminated")].to_owned()
        })
        .collect()
}

#[test]
fn default_run_covers_every_domain() {
    let workspace = defective_workspace("default-all");
    let output = loam(&["lint", workspace.to_str().unwrap(), "--now", NOW]);
    let stdout = String::from_utf8(output.stdout).expect("findings should be UTF-8");
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");

    assert_eq!(output.status.code(), Some(2), "stdout: {stdout}");
    let mut seen = domains(&stdout);
    seen.sort();
    seen.dedup();
    assert_eq!(
        seen,
        vec!["guidance", "markdown", "memory", "seals", "work"],
        "every domain should report: {stdout}"
    );
}

#[test]
fn only_selects_exactly_one_domain() {
    let workspace = defective_workspace("only");
    let root = workspace.to_str().unwrap().to_owned();

    let mut results = Vec::new();
    for domain in ["guidance", "markdown", "memory", "seals", "work"] {
        let output = loam(&["lint", "--only", domain, &root, "--now", NOW]);
        let stdout = String::from_utf8(output.stdout).expect("findings should be UTF-8");
        results.push((domain, output.status.code(), stdout));
    }
    let rejected_pair = loam(&["lint", "--only", "markdown,memory", &root]);
    let rejected_unknown = loam(&["lint", "--only", "goals", &root]);
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");

    for (domain, code, stdout) in &results {
        assert_eq!(*code, Some(2), "{domain} should report findings: {stdout}");
        let mut seen = domains(stdout);
        seen.dedup();
        assert_eq!(seen, vec![*domain], "{domain} filter leaked: {stdout}");
    }
    assert_eq!(
        rejected_pair.status.code(),
        Some(1),
        "--only takes exactly one domain"
    );
    assert_eq!(
        rejected_unknown.status.code(),
        Some(1),
        "unknown domain should be rejected"
    );
}

#[test]
fn old_command_spellings_are_gone() {
    let workspace = temporary_workspace("spellings");
    let root = workspace.to_str().unwrap().to_owned();

    let removed = [
        vec!["markdown", "lint", &root],
        vec!["lint", "markdown", &root],
        vec!["lint", "memory", &root],
    ];
    let mut survivors = Vec::new();
    for args in removed {
        let output = loam(&args);
        if output.status.code() != Some(1) {
            survivors.push(format!("{args:?} exited {:?}", output.status.code()));
        }
    }
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");

    assert!(
        survivors.is_empty(),
        "old spellings still work: {survivors:?}"
    );
}

#[test]
fn every_finding_uses_the_unified_envelope() {
    let workspace = defective_workspace("envelope");
    let output = loam(&["lint", workspace.to_str().unwrap(), "--now", NOW]);
    let stdout = String::from_utf8(output.stdout).expect("findings should be UTF-8");
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");

    for line in stdout.lines().filter(|line| !line.is_empty()) {
        for field in [
            "\"schema_version\":\"1\"",
            "\"domain\":\"",
            "\"rule\":\"",
            "\"rule_names\":[",
            "\"severity\":\"",
            "\"file\":\"",
            "\"line\":",
            "\"column\":",
            "\"end_line\":",
            "\"end_column\":",
            "\"description\":\"",
            "\"detail\":\"",
            "\"context\":\"",
            "\"evidence\":{",
            "\"target\":",
            "\"candidates\":[",
        ] {
            assert!(line.contains(field), "missing {field} in: {line}");
        }
    }

    // Markdown findings keep their range and candidate detail.
    let markdown = stdout
        .lines()
        .find(|line| line.contains("\"domain\":\"markdown\""))
        .expect("a markdown finding is expected");
    assert!(
        !markdown.contains("\"end_line\":0"),
        "markdown ranges should survive: {markdown}"
    );
}

#[test]
fn combined_output_is_byte_deterministic() {
    let workspace = defective_workspace("determinism");
    let root = workspace.to_str().unwrap().to_owned();

    let first = loam(&["lint", &root, "--now", NOW]);
    let second = loam(&["lint", &root, "--now", NOW]);
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");

    assert_eq!(first.status.code(), second.status.code());
    assert_eq!(
        String::from_utf8_lossy(&first.stdout),
        String::from_utf8_lossy(&second.stdout),
        "combined output must be byte-identical"
    );
}

#[test]
fn clean_workspace_exits_zero_and_reports_nothing() {
    let workspace = temporary_workspace("clean");
    write(
        &workspace.join("wiki/index.md"),
        "# Index\n\n## Overview\n\nHub.\n\n- [[alpha-note]]\n",
    );
    write(
        &workspace.join("wiki/alpha-note.md"),
        "# Alpha Note\n\nBody.\n",
    );
    // The wiki holds no page-type directories, so the current map is the bare
    // marker pair.
    write(
        &workspace.join("AGENTS.md"),
        "# Guide\n\n## Memory\n\nProse.\n\n<!-- loam:memory-map · generated from wiki/index.md · do not edit by hand -->\n<!-- /loam:memory-map -->\n",
    );

    let output = loam(&["lint", workspace.to_str().unwrap(), "--now", NOW]);
    let stdout = String::from_utf8(output.stdout).expect("findings should be UTF-8");
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");

    assert_eq!(stdout, "", "clean workspace should be silent");
    assert_eq!(output.status.code(), Some(0));
}

#[test]
fn now_requires_the_documented_timestamp_shape() {
    let workspace = temporary_workspace("now-shape");
    let root = workspace.to_str().unwrap().to_owned();

    let rejected = [
        "2026-07-20",
        "2026-07-20 12:00",
        "2026-07-20 12 +02:00",
        "2026-07-20 12:00 +0200",
        "2026-07-20 25:00 +02:00",
        "2026-07-20 12:00 02:00",
    ];
    let mut failures = Vec::new();
    for value in rejected {
        let output = loam(&["lint", &root, "--now", value]);
        if output.status.code() != Some(1) {
            failures.push(format!("{value:?} exited {:?}", output.status.code()));
        }
    }
    let accepted = loam(&["lint", &root, "--now", "2026-07-20 12:00 +02:00"]);
    let negative_offset = loam(&["lint", &root, "--now", "2026-07-20 12:00 -05:30"]);
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");

    assert!(
        failures.is_empty(),
        "accepted bad --now values: {failures:?}"
    );
    assert_eq!(accepted.status.code(), Some(0));
    assert_eq!(negative_offset.status.code(), Some(0));
}

mod seals {
    use super::{domains, field_values};
    use loam::seal::{canon, evidence, record};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output};

    const STAMP: &str = "2026-11-20 14:30 +02:00";
    const NOW: &str = "2026-11-21 10:00 +02:00";
    const PAGE: &str =
        "# Alpha\n\n<!-- loam:seal s-rung-order -->\nBundled rung first.\n<!-- /loam:seal -->\n";

    struct Workspace(PathBuf);

    impl Workspace {
        fn new(label: &str) -> Workspace {
            let dir = std::env::temp_dir()
                .join(format!("loam-lint-seals-{label}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(dir.join("wiki")).unwrap();
            fs::create_dir_all(dir.join("config")).unwrap();
            fs::write(dir.join("run.log"), b"all tests pass").unwrap();
            Workspace(dir)
        }

        fn wiki(&self) -> PathBuf {
            self.0.join("wiki")
        }

        fn page(&self) -> PathBuf {
            self.wiki().join("alpha.md")
        }

        /// An unsigned evidence-level seal over `alpha.md`; returns its id.
        fn seal(&self) -> String {
            self.seal_over("capture:run.log")
        }

        fn seal_over(&self, evidence_item: &str) -> String {
            self.seal_page(evidence_item, PAGE, None)
        }

        /// Seals the span in `page` (written to `alpha.md`), optionally as a
        /// re-seal naming `parent`.
        fn seal_page(&self, evidence_item: &str, page: &str, parent: Option<&str>) -> String {
            fs::write(self.page(), page).unwrap();
            let scan = loam::markers::scan(page);
            let content = &page.as_bytes()[scan.markers[0].content.clone()];
            let ctx = evidence::Ctx {
                repo_root: self.0.clone(),
                wiki: self.wiki(),
                retrieved_at: STAMP.to_owned(),
                capture_max_bytes: 262_144,
            };
            let items = [evidence::parse(evidence_item).unwrap()];
            let resolved =
                evidence::resolve_all(&items, &ctx, &evidence::AttestOptions::default()).unwrap();
            evidence::store_captures(&self.wiki(), &resolved).unwrap();
            let (id, bytes) = record::build(&record::NewRecord {
                kind: record::RecordKind::Seal,
                page: "alpha".to_owned(),
                anchor: "s-rung-order".to_owned(),
                subject_sha256: canon::digest(content).unwrap(),
                locator: record::soft_locator(&["Alpha".to_owned()], "Bundled rung first."),
                content: "Bundled rung first.\n".to_owned(),
                requested_level: Some("evidence".to_owned()),
                claim: Some("Rung order".to_owned()),
                procedure_uri: "loam://seal/procedure/v1".to_owned(),
                principal: "agent.one@local".to_owned(),
                acted_on_behalf_of: None,
                generator: record::Generator {
                    name: "loam".to_owned(),
                    version: "1.0.0".to_owned(),
                    harness: "test".to_owned(),
                },
                time_verified: STAMP.to_owned(),
                review_by: None,
                evidence: resolved.iter().map(|r| r.item.to_value()).collect(),
                redacted: Vec::new(),
                parent: parent.map(|id| record::Parent {
                    id: id.to_owned(),
                    verdict: "drifted".to_owned(),
                }),
                status: None,
            })
            .unwrap();
            record::write_record(&self.wiki(), &bytes).unwrap();
            id
        }

        fn lint(&self, extra: &[&str]) -> Output {
            Command::new(env!("CARGO_BIN_EXE_loam"))
                .arg("lint")
                .args(extra)
                .arg(&self.0)
                .args(["--now", NOW])
                .env("LOAM_WORKSPACES", "off")
                .env("LOAM_CONFIG_DIR", self.0.join("config"))
                .output()
                .expect("loam should run")
        }

        /// Every file under `wiki/.seals` with its bytes, for before/after.
        fn seals_snapshot(&self) -> Vec<(PathBuf, Vec<u8>)> {
            fn walk(dir: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
                let Ok(entries) = fs::read_dir(dir) else {
                    return;
                };
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        walk(&path, out);
                    } else {
                        out.push((path.clone(), fs::read(&path).unwrap()));
                    }
                }
            }
            let mut out = Vec::new();
            walk(&self.wiki().join(".seals"), &mut out);
            out.sort();
            out
        }
    }

    impl Drop for Workspace {
        fn drop(&mut self) {
            for _ in 0..10 {
                if fs::remove_dir_all(&self.0).is_ok() {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    }

    fn stdout(output: &Output) -> String {
        String::from_utf8(output.stdout.clone()).unwrap()
    }

    #[test]
    fn a_citable_seal_lints_clean() {
        let ws = Workspace::new("clean");
        ws.seal();
        let output = ws.lint(&["--only", "seals"]);
        assert_eq!(output.status.code(), Some(0), "{output:?}");
        assert_eq!(stdout(&output), "");
    }

    #[test]
    fn a_drifted_seal_yields_one_sel004_warning_and_exit_2() {
        let ws = Workspace::new("drift");
        ws.seal();
        fs::write(ws.page(), PAGE.replace("Bundled", "System")).unwrap();
        let before = ws.seals_snapshot();
        let output = ws.lint(&["--only", "seals"]);
        assert_eq!(output.status.code(), Some(2), "{output:?}");
        let text = stdout(&output);
        assert_eq!(field_values(&text, "rule"), vec!["SEL004"], "{text}");
        assert_eq!(field_values(&text, "severity"), vec!["warning"]);
        assert_eq!(domains(&text), vec!["seals"]);
        assert_eq!(ws.seals_snapshot(), before, "lint must not write records");
    }

    #[test]
    fn a_forged_record_yields_an_sel003_error() {
        let ws = Workspace::new("forged");
        let id = ws.seal();
        // Larger than the verifier's signature cap: forged without ssh-keygen.
        record::write_sig(&ws.wiki(), &id, &vec![b'x'; 70 * 1024]).unwrap();
        let before = ws.seals_snapshot();
        let output = ws.lint(&["--only", "seals"]);
        assert_eq!(output.status.code(), Some(2), "{output:?}");
        let text = stdout(&output);
        let rules = field_values(&text, "rule");
        assert!(rules.contains(&"SEL003".to_owned()), "{text}");
        let severities = field_values(&text, "severity");
        assert!(severities.contains(&"error".to_owned()), "{text}");
        assert_eq!(ws.seals_snapshot(), before);
    }

    #[test]
    fn a_re_seal_with_a_parent_clears_the_drift() {
        let ws = Workspace::new("reseal");
        let old = ws.seal();
        let edited = PAGE.replace("Bundled", "System");
        fs::write(ws.page(), &edited).unwrap();
        let drifted = ws.lint(&["--only", "seals"]);
        assert_eq!(drifted.status.code(), Some(2), "{drifted:?}");
        ws.seal_page("capture:run.log", &edited, Some(&old));
        let output = ws.lint(&["--only", "seals"]);
        assert_eq!(
            output.status.code(),
            Some(0),
            "{output:?} {}",
            stdout(&output)
        );
        assert_eq!(stdout(&output), "");
    }

    #[test]
    fn a_marker_without_a_record_yields_sel002() {
        let ws = Workspace::new("unpaired");
        fs::write(ws.page(), PAGE).unwrap();
        let output = ws.lint(&["--only", "seals"]);
        assert_eq!(output.status.code(), Some(2), "{output:?}");
        let text = stdout(&output);
        assert_eq!(field_values(&text, "rule"), vec!["SEL002"], "{text}");
        assert_eq!(field_values(&text, "domain"), vec!["seals"]);
    }

    #[test]
    fn the_same_anchor_on_two_pages_flags_only_the_unrecorded_marker() {
        let ws = Workspace::new("twopages");
        ws.seal();
        fs::write(ws.wiki().join("beta.md"), PAGE.replace("Alpha", "Beta")).unwrap();
        let output = ws.lint(&["--only", "seals"]);
        let text = stdout(&output);
        assert_eq!(field_values(&text, "rule"), vec!["SEL002"], "{text}");
        assert_eq!(field_values(&text, "file"), vec!["wiki/beta.md"], "{text}");
    }

    #[test]
    fn a_record_whose_span_was_removed_yields_sel002_and_sel004() {
        let ws = Workspace::new("removed");
        ws.seal();
        fs::write(ws.page(), "# Alpha\n\nNo seal here.\n").unwrap();
        let output = ws.lint(&["--only", "seals"]);
        let text = stdout(&output);
        let rules = field_values(&text, "rule");
        assert!(rules.contains(&"SEL002".to_owned()), "{text}");
        assert!(rules.contains(&"SEL004".to_owned()), "{text}");
    }

    #[test]
    fn drifted_evidence_yields_sel005() {
        let ws = Workspace::new("evidence");
        ws.seal_over("file:run.log");
        fs::write(ws.0.join("run.log"), b"tests now fail").unwrap();
        let output = ws.lint(&["--only", "seals"]);
        let text = stdout(&output);
        assert!(
            field_values(&text, "rule").contains(&"SEL005".to_owned()),
            "{text}"
        );
    }

    #[test]
    fn only_seals_excludes_other_domains_and_default_includes_seals() {
        let ws = Workspace::new("domains");
        ws.seal();
        fs::write(ws.page(), PAGE.replace("Bundled", "System")).unwrap();
        // A markdown defect that must not appear under `--only seals`.
        fs::write(
            ws.wiki().join("index.md"),
            "# Index\n\n## Overview\n\nSee [[ghost-page]].\n\n## Overview\n",
        )
        .unwrap();
        let only = ws.lint(&["--only", "seals"]);
        assert!(domains(&stdout(&only)).iter().all(|d| d == "seals"));
        let all = ws.lint(&[]);
        let all_domains = domains(&stdout(&all));
        assert!(all_domains.contains(&"seals".to_owned()));
        assert!(all_domains.contains(&"markdown".to_owned()));
    }
}
