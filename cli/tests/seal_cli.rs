use std::path::PathBuf;
use std::process::{Command, Output};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/seal")
        .join(name)
}

fn loam(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_loam"))
        .args(args)
        .env("LOAM_WORKSPACES", "off")
        .output()
        .expect("loam should run")
}

fn hash(name: &str, anchor: &str) -> Output {
    loam(&["seal", "hash", fixture(name).to_str().unwrap(), anchor])
}

fn digest(name: &str, anchor: &str) -> String {
    let output = hash(name, anchor);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

#[test]
fn hash_prints_the_canon_digest_of_a_span_found_by_anchor() {
    let value = digest("lf.md", "s-trust-rung-order");
    assert_eq!(value.len(), 64);
    assert!(value.bytes().all(|b| b.is_ascii_hexdigit()));
    let expected = loam::sha256::hex(
        b"TLS tries the bundled rung first.\n\nThen it falls back to the system store.\n",
    );
    assert_eq!(value, expected);
}

#[test]
fn anchor_may_carry_a_caret_prefix() {
    assert_eq!(
        digest("lf.md", "^s-trust-rung-order"),
        digest("lf.md", "s-trust-rung-order")
    );
}

#[test]
fn crlf_trailing_space_and_blank_lines_hash_like_the_lf_page() {
    assert_eq!(
        digest("crlf.md", "s-trust-rung-order"),
        digest("lf.md", "s-trust-rung-order")
    );
}

#[test]
fn reflowed_paragraph_hashes_differently() {
    assert_ne!(
        digest("reflowed.md", "s-trust-rung-order"),
        digest("lf.md", "s-trust-rung-order")
    );
}

#[test]
fn unknown_anchor_exits_2() {
    assert_eq!(hash("lf.md", "s-nope").status.code(), Some(2));
}

#[test]
fn invalid_utf8_exits_2() {
    assert_eq!(
        hash("invalid-utf8.md", "s-bad-bytes").status.code(),
        Some(2)
    );
}

#[test]
fn missing_file_exits_2() {
    let output = loam(&["seal", "hash", "does-not-exist.md", "s-x"]);
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn usage_errors_exit_2() {
    assert_eq!(loam(&["seal"]).status.code(), Some(2));
    assert_eq!(
        loam(&["seal", "hash", "only-one-arg"]).status.code(),
        Some(2)
    );
    assert_eq!(loam(&["seal", "verify", "--bogus"]).status.code(), Some(2));
    assert_eq!(loam(&["seal", "status", "extra"]).status.code(), Some(2));
    assert_eq!(loam(&["seal", "status", "--now"]).status.code(), Some(2));
}

// ---------------------------------------------------------------- verify

mod verify {
    use super::*;
    use loam::json::{self, Value};
    use loam::seal::{canon, evidence, record};
    use std::fs;
    use std::path::Path;

    const STAMP: &str = "2026-11-20 14:30 +02:00";
    const NOW: &str = "2026-11-21 10:00 +02:00";

    struct Workspace(PathBuf);

    impl Workspace {
        fn new(label: &str) -> Workspace {
            let dir =
                std::env::temp_dir().join(format!("loam-seal-cli-{label}-{}", std::process::id()));
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

        /// An unsigned evidence-level seal over `alpha.md`, backed by a
        /// stored capture. Returns the record id.
        fn seal(&self) -> String {
            let text =
                "# Alpha\n\n<!-- loam:seal s-rung-order -->\nBundled rung first.\n<!-- /loam:seal -->\n";
            fs::write(self.page(), text).unwrap();
            let scan = loam::markers::scan(text);
            let content = &text.as_bytes()[scan.markers[0].content.clone()];
            let ctx = evidence::Ctx {
                repo_root: self.0.clone(),
                wiki: self.wiki(),
                retrieved_at: STAMP.to_owned(),
                capture_max_bytes: 262_144,
            };
            let items = [evidence::parse("capture:run.log").unwrap()];
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
                parent: None,
                status: None,
            })
            .unwrap();
            record::write_record(&self.wiki(), &bytes).unwrap();
            id
        }

        fn run(&self, args: &[&str]) -> Output {
            self.run_in(&self.0, args)
        }

        fn run_in(&self, dir: &Path, args: &[&str]) -> Output {
            Command::new(env!("CARGO_BIN_EXE_loam"))
                .args(args)
                .current_dir(dir)
                .env("LOAM_WORKSPACES", "off")
                .env("LOAM_CONFIG_DIR", self.0.join("config"))
                .output()
                .expect("loam should run")
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

    fn first_json(output: &Output) -> Value {
        json::parse(stdout(output).lines().next().expect("one JSON line")).unwrap()
    }

    #[test]
    fn verify_json_prints_the_d8_object_and_exits_0_for_a_citable_seal() {
        let ws = Workspace::new("citable");
        let id = ws.seal();
        let output = ws.run(&["seal", "verify", "--json", "--now", NOW]);
        assert_eq!(output.status.code(), Some(0), "{output:?}");
        let value = first_json(&output);
        assert_eq!(value.get("seal").and_then(Value::as_str), Some(id.as_str()));
        assert_eq!(
            value.get("verdict").and_then(Value::as_str),
            Some("citable")
        );
        assert_eq!(
            value.get("effective_level").and_then(Value::as_str),
            Some("evidence")
        );
        assert_eq!(
            value.get("cite_as").and_then(Value::as_str),
            Some(format!("seal:{id}").as_str())
        );
        assert!(value.get("codes").and_then(|c| c.get("success")).is_some());
    }

    #[test]
    fn verify_human_output_lists_verdict_level_and_cite_as() {
        let ws = Workspace::new("human");
        let id = ws.seal();
        let output = ws.run(&["seal", "verify", "alpha#^s-rung-order", "--now", NOW]);
        assert_eq!(output.status.code(), Some(0), "{output:?}");
        let text = stdout(&output);
        assert!(text.contains(&id) && text.contains("citable") && text.contains("evidence"));
        assert!(text.contains(&format!("seal:{id}")), "{text}");
    }

    #[test]
    fn verify_exits_1_when_a_selected_seal_is_not_citable() {
        let ws = Workspace::new("drift");
        ws.seal();
        let edited = fs::read_to_string(ws.page())
            .unwrap()
            .replace("Bundled", "System");
        fs::write(ws.page(), edited).unwrap();
        let output = ws.run(&["seal", "verify", "--json", "--now", NOW]);
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        let value = first_json(&output);
        assert_eq!(
            value.get("verdict").and_then(Value::as_str),
            Some("drifted")
        );
        assert!(value.get("cite_as").is_some_and(Value::is_null));
    }

    #[test]
    fn verify_writes_nothing_and_works_from_a_subdirectory() {
        let ws = Workspace::new("readonly");
        ws.seal();
        let sub = ws.wiki().join("deep");
        fs::create_dir_all(&sub).unwrap();
        let before = fs::read(ws.page()).unwrap();
        let output = ws.run_in(&sub, &["seal", "verify", "--now", NOW]);
        assert_eq!(output.status.code(), Some(0), "{output:?}");
        assert_eq!(fs::read(ws.page()).unwrap(), before);
        assert!(!ws.0.join("config").join("seal").exists());
    }

    #[test]
    fn verify_with_unmatched_selector_or_no_wiki_exits_2() {
        let ws = Workspace::new("select");
        ws.seal();
        assert_eq!(
            ws.run(&["seal", "verify", "nowhere#^s-x", "--now", NOW])
                .status
                .code(),
            Some(2)
        );
        assert_eq!(
            ws.run(&["seal", "verify", "alpha#^", "--now", NOW])
                .status
                .code(),
            Some(2)
        );
        assert_eq!(
            ws.run(&["seal", "verify", "--now", "tomorrow"])
                .status
                .code(),
            Some(2)
        );
        let bare = std::env::temp_dir().join(format!("loam-seal-cli-bare-{}", std::process::id()));
        fs::create_dir_all(&bare).unwrap();
        let output = ws.run_in(&bare, &["seal", "verify", "--now", NOW]);
        let _ = fs::remove_dir_all(&bare);
        // No wiki/ here or above (the temp dir has none): usage-class failure.
        assert_eq!(output.status.code(), Some(2), "{output:?}");
    }

    #[test]
    fn verify_accepts_wiki_prefixed_absolute_and_cwd_relative_page_paths() {
        let ws = Workspace::new("paths");
        ws.seal();
        let sub = ws.wiki().join("deep");
        fs::create_dir_all(&sub).unwrap();
        let absolute = ws.page().to_string_lossy().into_owned();
        let cases: [(&Path, String); 4] = [
            (&ws.0, "wiki/alpha.md#^s-rung-order".to_owned()),
            (&ws.0, "wiki/alpha#^s-rung-order".to_owned()),
            (&sub, "../alpha.md#^s-rung-order".to_owned()),
            (&sub, format!("{absolute}#^s-rung-order")),
        ];
        for (dir, selector) in cases {
            let output = ws.run_in(dir, &["seal", "verify", &selector, "--now", NOW]);
            assert_eq!(output.status.code(), Some(0), "{selector}: {output:?}");
        }
        let output = ws.run(&["seal", "verify", "wiki/beta.md#^s-rung-order", "--now", NOW]);
        assert_eq!(output.status.code(), Some(2), "{output:?}");
    }

    #[test]
    fn verify_with_no_seals_exits_0() {
        let ws = Workspace::new("empty");
        let output = ws.run(&["seal", "verify", "--now", NOW]);
        assert_eq!(output.status.code(), Some(0), "{output:?}");
    }

    #[test]
    fn status_counts_verdicts_in_text_and_json() {
        let ws = Workspace::new("status");
        ws.seal();
        let output = ws.run(&["seal", "status", "--json", "--now", NOW]);
        assert_eq!(output.status.code(), Some(0), "{output:?}");
        let value = first_json(&output);
        let count = |key: &str| match value.get(key) {
            Some(Value::Number(n)) => n.clone(),
            other => panic!("{key}: {other:?}"),
        };
        assert_eq!(count("citable"), "1");
        for key in [
            "drifted",
            "stale",
            "unverifiable",
            "untrusted",
            "forged",
            "retracted",
            "disputed",
        ] {
            assert_eq!(count(key), "0", "{key}");
        }

        let edited = fs::read_to_string(ws.page())
            .unwrap()
            .replace("Bundled", "System");
        fs::write(ws.page(), edited).unwrap();
        let output = ws.run(&["seal", "status", "--now", NOW]);
        assert_eq!(
            output.status.code(),
            Some(0),
            "status reports, it never gates"
        );
        let text = stdout(&output);
        assert!(
            text.contains("citable: 0") && text.contains("drifted: 1"),
            "{text}"
        );
    }
}

// ---------------------------------------------------------------- attest

mod attest {
    use super::*;
    use loam::json::{self, Value};
    use loam::seal::sshsig::{self, Capability};
    use std::fs;
    use std::path::Path;

    const PAGE: &str =
        "# Alpha\n\nBundled rung first.\nThen the system store. ^s-rung-order\n\nTail.\n";

    /// The first capable `ssh-keygen`: `LOAM_SSH_KEYGEN`, PATH, then Git for
    /// Windows' bundled one (the Windows in-box OpenSSH lacks verify-time).
    fn capable_keygen() -> Option<PathBuf> {
        static FOUND: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
        FOUND
            .get_or_init(|| {
                let mut candidates = Vec::new();
                if let Ok(value) = std::env::var("LOAM_SSH_KEYGEN") {
                    if !value.trim().is_empty() {
                        candidates.push(PathBuf::from(value.trim()));
                    }
                }
                candidates.push(PathBuf::from("ssh-keygen"));
                if cfg!(windows) {
                    candidates.push(PathBuf::from(
                        r"C:\Program Files\Git\usr\bin\ssh-keygen.exe",
                    ));
                }
                candidates
                    .into_iter()
                    .find(|bin| sshsig::capability_of(bin) == Capability::Available)
            })
            .clone()
    }

    struct Workspace {
        dir: PathBuf,
        keygen: PathBuf,
    }

    impl Workspace {
        /// `None` when this machine has no ssh-keygen able to sign (the
        /// CI legs that must have one set `LOAM_REQUIRE_SSHSIG=1`).
        fn new(label: &str) -> Option<Workspace> {
            let Some(keygen) = capable_keygen() else {
                assert!(
                    std::env::var("LOAM_REQUIRE_SSHSIG").map_or(true, |v| v != "1"),
                    "LOAM_REQUIRE_SSHSIG=1 but no capable ssh-keygen was found"
                );
                return None;
            };
            let dir = std::env::temp_dir()
                .join(format!("loam-seal-attest-{label}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(dir.join("wiki")).unwrap();
            fs::create_dir_all(dir.join("config")).unwrap();
            fs::write(dir.join("run.log"), b"all tests pass").unwrap();
            fs::write(dir.join("wiki").join("alpha.md"), PAGE).unwrap();
            Some(Workspace { dir, keygen })
        }

        fn page(&self) -> PathBuf {
            self.dir.join("wiki").join("alpha.md")
        }

        fn run(&self, args: &[&str]) -> Output {
            Command::new(env!("CARGO_BIN_EXE_loam"))
                .args(args)
                .current_dir(&self.dir)
                .env("LOAM_WORKSPACES", "off")
                .env("LOAM_CONFIG_DIR", self.dir.join("config"))
                .env("LOAM_HARNESS", "cli-test")
                .env("LOAM_SSH_KEYGEN", &self.keygen)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env(
                    "GIT_CONFIG_GLOBAL",
                    if cfg!(windows) { "NUL" } else { "/dev/null" },
                )
                .env_remove("LOAM_FEDERATION_ORG")
                .output()
                .expect("loam should run")
        }

        fn trust_init(&self) {
            let output = self.run(&["seal", "trust", "init"]);
            assert_eq!(output.status.code(), Some(0), "{output:?}");
        }

        fn attest(&self, extra: &[&str]) -> Output {
            let mut args = vec![
                "seal",
                "attest",
                "alpha#^s-rung-order",
                "--level",
                "evidence",
                "--claim",
                "Rung order",
            ];
            args.extend_from_slice(extra);
            self.run(&args)
        }
    }

    impl Drop for Workspace {
        fn drop(&mut self) {
            for _ in 0..10 {
                if fs::remove_dir_all(&self.dir).is_ok() {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    }

    fn stdout(output: &Output) -> String {
        String::from_utf8(output.stdout.clone()).unwrap()
    }

    fn stderr(output: &Output) -> String {
        String::from_utf8(output.stderr.clone()).unwrap()
    }

    fn seals_exist(ws: &Workspace) -> bool {
        Path::new(&ws.dir.join("wiki").join(".seals")).exists()
    }

    #[test]
    fn attest_inserts_markers_writes_the_record_and_verifies_citable() {
        let Some(ws) = Workspace::new("ok") else {
            return;
        };
        ws.trust_init();
        let output = ws.attest(&["--evidence", "capture:run.log"]);
        assert_eq!(output.status.code(), Some(0), "{output:?}");
        let value = json::parse(stdout(&output).lines().next().unwrap()).unwrap();
        let id = value
            .get("seal")
            .and_then(Value::as_str)
            .unwrap()
            .to_owned();
        assert_eq!(
            value.get("verdict").and_then(Value::as_str),
            Some("citable")
        );

        let records = ws.dir.join("wiki/.seals/records");
        let bytes = fs::read(records.join(format!("{id}.json"))).unwrap();
        assert_eq!(id, loam::sha256::hex(&bytes)[..20]);
        assert!(records.join(format!("{id}.json.sig")).is_file());
        assert!(String::from_utf8_lossy(&bytes).contains("\"harness\":\"cli-test\""));
        assert_eq!(
            fs::read_to_string(ws.page()).unwrap(),
            "# Alpha\n\n<!-- loam:seal s-rung-order -->\nBundled rung first.\nThen the system store.\n<!-- /loam:seal -->\n\nTail.\n"
        );

        let verify = ws.run(&["seal", "verify", "alpha#^s-rung-order"]);
        assert_eq!(verify.status.code(), Some(0), "{verify:?}");
        assert!(stdout(&verify).contains("citable"));
    }

    #[test]
    fn attest_resolves_page_paths_and_chains_a_parent() {
        let Some(ws) = Workspace::new("parent") else {
            return;
        };
        ws.trust_init();
        let first = ws.run(&[
            "seal",
            "attest",
            "wiki/alpha.md#^s-rung-order",
            "--level",
            "evidence",
            "--claim",
            "Rung order",
            "--evidence",
            "capture:run.log",
            "--origin",
            "ci job 7",
        ]);
        assert_eq!(first.status.code(), Some(0), "{first:?}");
        let id = json::parse(stdout(&first).lines().next().unwrap())
            .unwrap()
            .get("seal")
            .and_then(Value::as_str)
            .unwrap()
            .to_owned();
        let second = ws.attest(&["--evidence", "capture:run.log", "--parent", &id]);
        assert_eq!(second.status.code(), Some(0), "{second:?}");
        let next = json::parse(stdout(&second).lines().next().unwrap())
            .unwrap()
            .get("seal")
            .and_then(Value::as_str)
            .unwrap()
            .to_owned();
        assert_ne!(id, next);
        let text =
            fs::read_to_string(ws.dir.join(format!("wiki/.seals/records/{next}.json"))).unwrap();
        assert!(text.contains(&format!(
            "\"parent\":{{\"id\":\"{id}\",\"verdict\":\"citable\"}}"
        )));
        assert!(text.contains("ci job 7"), "carried capture origin: {text}");
    }

    #[test]
    fn attest_rejects_now() {
        let Some(ws) = Workspace::new("now") else {
            return;
        };
        ws.trust_init();
        let output = ws.attest(&[
            "--evidence",
            "capture:run.log",
            "--now",
            "2020-01-01 00:00 +00:00",
        ]);
        assert_eq!(output.status.code(), Some(2), "{output:?}");
        assert!(stderr(&output).contains("unknown option --now"));
        assert_eq!(fs::read_to_string(ws.page()).unwrap(), PAGE);
        assert!(!seals_exist(&ws));
    }

    #[test]
    fn a_refused_attest_exits_1_and_leaves_no_trace() {
        let Some(ws) = Workspace::new("refused") else {
            return;
        };
        ws.trust_init();
        let output = ws.attest(&[
            "--evidence",
            "blob:nope.txt",
            "--evidence",
            "capture:run.log",
        ]);
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert!(stderr(&output).contains("blob:nope.txt"), "{output:?}");
        assert_eq!(fs::read_to_string(ws.page()).unwrap(), PAGE);
        assert!(!seals_exist(&ws));
    }

    #[test]
    fn missing_identity_exits_1_with_the_setup_hint() {
        let Some(ws) = Workspace::new("identity") else {
            return;
        };
        let output = ws.attest(&["--evidence", "capture:run.log"]);
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert!(stderr(&output).contains("npx @scchearn/loam setup"));
        assert_eq!(fs::read_to_string(ws.page()).unwrap(), PAGE);
        assert!(!seals_exist(&ws));
    }

    #[test]
    fn bad_arguments_and_targets_exit_2() {
        let Some(ws) = Workspace::new("usage") else {
            return;
        };
        ws.trust_init();
        let cases: [&[&str]; 6] = [
            &["seal", "attest"],
            &["seal", "attest", "alpha#^s-rung-order"],
            &[
                "seal", "attest", "alpha", "--level", "evidence", "--claim", "c",
            ],
            &[
                "seal",
                "attest",
                "alpha#^s-rung-order",
                "--level",
                "gold",
                "--claim",
                "c",
                "--evidence",
                "note:x",
            ],
            &[
                "seal",
                "attest",
                "alpha#^s-no-such-anchor",
                "--level",
                "evidence",
                "--claim",
                "c",
                "--evidence",
                "capture:run.log",
            ],
            &[
                "seal",
                "attest",
                "alpha#^s-rung-order",
                "--level",
                "evidence",
                "--claim",
                "c",
                "--evidence",
                "nonsense",
                "--bogus",
            ],
        ];
        for args in cases {
            let output = ws.run(args);
            assert_eq!(output.status.code(), Some(2), "{args:?}: {output:?}");
        }
        assert_eq!(fs::read_to_string(ws.page()).unwrap(), PAGE);
        assert!(!seals_exist(&ws));
    }

    #[test]
    fn mark_writes_status_records_and_retract_takes_effect() {
        let Some(ws) = Workspace::new("mark") else {
            return;
        };
        ws.trust_init();
        let attested = ws.attest(&["--evidence", "capture:run.log"]);
        assert_eq!(attested.status.code(), Some(0), "{attested:?}");
        let id = json::parse(stdout(&attested).lines().next().unwrap())
            .unwrap()
            .get("seal")
            .and_then(Value::as_str)
            .unwrap()
            .to_owned();
        let original = fs::read(ws.dir.join(format!("wiki/.seals/records/{id}.json"))).unwrap();

        let dispute = ws.run(&["seal", "mark", &id, "dispute", "--reason", "looks off"]);
        assert_eq!(dispute.status.code(), Some(0), "{dispute:?}");
        let value = json::parse(stdout(&dispute).lines().next().unwrap()).unwrap();
        assert_eq!(value.get("type").and_then(Value::as_str), Some("dispute"));
        assert_eq!(value.get("downgraded"), Some(&Value::Bool(false)));

        let retract = ws.run(&["seal", "mark", &id, "retract", "--reason", "wrong"]);
        assert_eq!(retract.status.code(), Some(0), "{retract:?}");
        let value = json::parse(stdout(&retract).lines().next().unwrap()).unwrap();
        assert_eq!(value.get("type").and_then(Value::as_str), Some("retract"));
        assert_eq!(
            value
                .get("seal")
                .and_then(|seal| seal.get("verdict"))
                .and_then(Value::as_str),
            Some("retracted")
        );
        assert_eq!(
            fs::read(ws.dir.join(format!("wiki/.seals/records/{id}.json"))).unwrap(),
            original
        );
    }

    #[test]
    fn mark_usage_errors_exit_2() {
        let Some(ws) = Workspace::new("mark-usage") else {
            return;
        };
        ws.trust_init();
        let cases: [&[&str]; 5] = [
            &["seal", "mark"],
            &["seal", "mark", "0123456789abcdef0123", "dispute"],
            &[
                "seal",
                "mark",
                "0123456789abcdef0123",
                "bogus",
                "--reason",
                "r",
            ],
            &[
                "seal",
                "mark",
                "0123456789abcdef0123",
                "retract",
                "--reason",
                "r",
            ],
            &["seal", "mark", "x", "retract", "--reason", "r", "--bogus"],
        ];
        for args in cases {
            let output = ws.run(args);
            assert_eq!(output.status.code(), Some(2), "{args:?}: {output:?}");
        }
        assert!(!seals_exist(&ws));
    }
}
