// The session-start hook gives `loam state --fast` a five-second hard timeout.
// This is the CI-enforced budget on a synthetic fixture whose size is recorded
// here rather than committed as thousands of files.
//
// Fixture size: 2000 source files across 20 directories, 2000 wiki code pages,
// 50 checkpoints, plus 500 sealed spans (about 1.5 KB each, about 750 KB in
// all) over 120 wiki pages with one record apiece. Real-workspace sub-0.5s
// runs stay manual benchmarks.
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

const SOURCE_FILES: usize = 2000;
const CODE_PAGES: usize = 2000;
const CHECKPOINTS: usize = 50;
const BUDGET_SECONDS: u64 = 5;
const SEAL_PAGES: usize = 120;
const SEALS: usize = 500;
const SEAL_SPAN_BYTES: usize = 1500;
const HASH_CAP: u64 = 512 * 1024;

/// Writes 500 sealed spans across 120 pages under `wiki/decisions/`, one
/// well-formed record per span, each carrying a `blob:` evidence item over
/// `src/evidence.txt`. Returns the total bytes of sealed span text.
fn seed_seals(root: &Path) -> usize {
    use loam::seal::{canon, evidence, record};

    let wiki = root.join("wiki");
    fs::create_dir_all(wiki.join("decisions")).expect("decisions");
    fs::create_dir_all(root.join("src")).expect("src");
    let evidence_bytes = b"seal evidence fixture\n";
    fs::write(root.join("src/evidence.txt"), evidence_bytes).expect("evidence file");
    let git_blob = git_blob_id(root);

    let mut total = 0;
    let mut made = 0;
    for page in 0..SEAL_PAGES {
        // 20 pages of five spans and 100 of four make exactly 500.
        let spans = if page < SEALS - 4 * SEAL_PAGES { 5 } else { 4 };
        let mut text = format!("# Decision {page}\n\n");
        let mut bodies = Vec::new();
        for n in 0..spans {
            let anchor = format!("s-fx-p{page}-n{n}");
            let mut body = String::new();
            while body.len() < SEAL_SPAN_BYTES {
                body.push_str(&format!(
                    "Finding {page} {n} holds under review {}.\n",
                    body.len()
                ));
            }
            text.push_str(&format!(
                "<!-- loam:seal {anchor} -->\n{body}<!-- /loam:seal -->\n\n"
            ));
            total += body.len();
            bodies.push((anchor, body));
        }
        fs::write(wiki.join(format!("decisions/d-{page}.md")), &text).expect("page");

        let scan = loam::markers::scan(&text);
        assert_eq!(
            scan.markers.len(),
            spans,
            "page {page} markers: {:?}",
            scan.problems
        );
        for (marker, (anchor, _)) in scan.markers.iter().zip(&bodies) {
            let content = &text.as_bytes()[marker.content.clone()];
            let canonical = String::from_utf8(canon::canonicalize(content).expect("canon"))
                .expect("canonical text is UTF-8");
            let mut item = evidence::parse("blob:src/evidence.txt").expect("blob item");
            item.git_blob = Some(git_blob.clone());
            item.sha256 = Some(loam::sha256::hex(evidence_bytes));
            item.git_ref = Some("refs/heads/main".to_owned());
            let (_, bytes) = record::build(&record::NewRecord {
                kind: record::RecordKind::Seal,
                page: format!("decisions/d-{page}"),
                anchor: anchor.clone(),
                subject_sha256: canon::digest(content).expect("digest"),
                locator: record::soft_locator(&[format!("Decision {page}")], &canonical),
                content: canonical,
                requested_level: Some("evidence".to_owned()),
                claim: Some("a claim".to_owned()),
                procedure_uri: "loam://seal/procedure/v1".to_owned(),
                principal: "agent.one@local".to_owned(),
                acted_on_behalf_of: None,
                generator: record::Generator {
                    name: "loam".to_owned(),
                    version: "1.0.0".to_owned(),
                    harness: "test".to_owned(),
                },
                time_verified: "2026-11-20 14:30 +02:00".to_owned(),
                review_by: None,
                evidence: vec![item.to_value()],
                redacted: Vec::new(),
                parent: None,
                status: None,
            })
            .expect("record builds");
            record::write_record(&wiki, &bytes).expect("record written");
            made += 1;
        }
    }
    assert_eq!(made, SEALS);
    total
}

/// The git object id of `src/evidence.txt`, from real git (hash-object reads
/// the file and writes nothing, in or out of a repository).
fn git_blob_id(root: &Path) -> String {
    let empty = root.join("git-empty-config");
    fs::write(&empty, "").expect("empty git config");
    let out = Command::new("git")
        .args(["hash-object", "--", "src/evidence.txt"])
        .current_dir(root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", &empty)
        .output()
        .expect("git should run");
    assert!(out.status.success(), "git hash-object failed: {out:?}");
    String::from_utf8(out.stdout)
        .expect("utf8")
        .trim()
        .to_owned()
}

/// Backdates every file under `dir` so no cache stamp is racy.
fn age(dir: &Path) {
    let when = SystemTime::now() - std::time::Duration::from_secs(3600);
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            age(&path);
        } else if let Ok(file) = fs::OpenOptions::new().write(true).open(&path) {
            let _ = file.set_modified(when);
        }
    }
}

/// Pre-warms the signature and evidence entries for every record by running a
/// full verification into the pinned cache, then drops the page entries so the
/// only source of cold-run `unchecked` is the hash budget.
fn prewarm_cache(root: &Path, config: &Path) {
    use loam::seal::verify;

    let wiki = root.join("wiki");
    let trust_dir = config.join("seal");
    let cache_dir = config.join("cache").join("seals");
    let now = chrono::Utc::now().with_timezone(&chrono::FixedOffset::east_opt(0).expect("utc"));
    let mut env = verify::Env::load(&wiki, Some(trust_dir), now).expect("env loads");
    env.cache_dir = Some(cache_dir.clone());
    let reports = verify::verify_and_cache(&env).expect("full verification");
    assert_eq!(reports.len(), SEALS);
    let file = loam::seal_cache::cache_file(&cache_dir, &wiki);
    let mut cache = loam::seal_cache::load(&file);
    assert!(
        !cache.evidence.is_empty(),
        "evidence entries were not cached"
    );
    // The records are unsigned, so there is no signature to verify or cache;
    // the fast path settles them without `ssh-keygen`. The signature-cache
    // hit path is outside this budget fixture (signing 500 records needs
    // ssh-keygen and is platform-sensitive); the seals_counts tests cover it.
    cache.pages.clear();
    loam::seal_cache::save(&file, &cache).expect("cache rewritten");
}

fn counter(object: &loam::json::Value, key: &str) -> u64 {
    match object.get(key) {
        Some(loam::json::Value::Number(n)) => n.parse().expect("number"),
        other => panic!("{key} is not a number: {other:?}"),
    }
}

#[test]
fn fast_state_stays_within_the_five_second_hook_budget() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("loam-state-budget-{nonce}"));
    // Removes the fixture even when an assertion panics.
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            for _ in 0..5 {
                if fs::remove_dir_all(&self.0).is_ok() || !self.0.exists() {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
    }
    let _cleanup = Cleanup(root.clone());
    let wiki = root.join("wiki");
    fs::create_dir_all(wiki.join("checkpoints")).expect("checkpoints");
    fs::create_dir_all(wiki.join("code")).expect("code pages");
    fs::write(wiki.join("SCHEMA.md"), "# Schema\n").expect("schema");
    fs::write(wiki.join("index.md"), "# Index\n").expect("index");
    // A ready collection short-circuits the optional qmd CLI probe, matching a
    // real indexed workspace; the probe is a host-tool boundary, not our cost.
    fs::write(
        wiki.join(".wiki-metadata.json"),
        "{\"retrieval\":{\"status\":\"ready\",\"collection_name\":\"budget-fixture\"}}\n",
    )
    .expect("metadata");

    for directory in 0..20 {
        fs::create_dir_all(root.join(format!("src/module_{directory}"))).expect("module");
    }
    for index in 0..SOURCE_FILES {
        fs::write(
            root.join(format!("src/module_{}/file_{index}.rs", index % 20)),
            format!("fn item_{index}() {{ let value = {index}; }}\n"),
        )
        .expect("source");
    }
    for index in 0..CODE_PAGES {
        fs::write(
            wiki.join(format!("code/page-{index}.md")),
            format!(
                "---\nsource_path: src/module_{}/file_{index}.rs\ningested_at: \"1\"\n---\n\n# page\n",
                index % 20
            ),
        )
        .expect("code page");
    }
    for index in 0..CHECKPOINTS {
        fs::write(
            wiki.join(format!("checkpoints/checkpoint-2026-07-{:02}-1000.md", index % 28 + 1)),
            "# Checkpoint\n\n- Captured: 2026-07-01 10:00 +02:00\n- Scope: fixture\n\n## Workstreams\n\n### Budget fixture\n",
        )
        .expect("checkpoint");
    }

    // The hcom probe's worst case, deliberately: no launcher env marker to
    // short-circuit on, so the fixture pays the full ladder — directory
    // resolution plus one health-check spawn — inside the same budget.
    //
    // "Worst case" holds on unix only. The fake is a `#!/bin/sh` script named
    // `hcom`, and the Rust ladder matches `hcom.exe` on Windows (see
    // `hcom_readiness`), so the windows leg pays the stat walk and stops there.
    // Measuring the spawn there too would mean committing a real `.exe` — a
    // `.cmd` shim does not match the `.exe`-only rung — which is not worth a
    // binary in the tree for a rung whose cost is bounded by
    // `HCOM_HEALTH_TIMEOUT` anyway.
    let bin = root.join("bin");
    fs::create_dir_all(&bin).expect("bin directory");
    let fake = bin.join("hcom");
    fs::write(&fake, "#!/bin/sh\necho 'hcom 0.7.25'\n").expect("fake hcom");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).expect("fake hcom mode");
    }

    // 500 seals over 120 pages, a pinned config dir (trust, cache) under the
    // fixture root, and a cache pre-warmed for signatures and evidence only.
    let sealed_bytes = seed_seals(&root);
    assert!(
        sealed_bytes as u64 > HASH_CAP,
        "sealed spans must exceed the hash cap, got {sealed_bytes}"
    );
    let config = root.join("config");
    fs::create_dir_all(config.join("seal")).expect("trust dir");
    fs::write(
        config.join("seal").join(loam::seal::trust::POLICY_FILE),
        loam::seal::trust::Policy {
            max_review: "36500d".to_owned(),
            ..loam::seal::trust::Policy::default()
        }
        .to_json()
        .to_json(),
    )
    .expect("policy");
    age(&root.join("wiki/decisions"));
    age(&root.join("wiki/.seals"));
    age(&root.join("src/evidence.txt"));
    prewarm_cache(&root, &config);

    // `git` and `ssh-keygen` stand-ins that record every call. The `git` stub
    // hands off to the real git so the timed run still pays for the probe;
    // the `ssh-keygen` stub fails. The fast path must never spawn the latter.
    // Unix only: a Windows stub would need a committed `.exe`, the same
    // caveat as the hcom stub above.
    #[cfg(unix)]
    let real_git = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|dir| dir.join("git"))
        .find(|candidate| candidate.is_file())
        .expect("a real git on PATH");
    let stubs = root.join("stubs");
    #[cfg(unix)]
    let calls = root.join("stub-calls.log");
    fs::create_dir_all(&stubs).expect("stubs directory");
    #[cfg(unix)]
    for name in ["git", "ssh-keygen"] {
        use std::os::unix::fs::PermissionsExt;
        let stub = stubs.join(name);
        let tail = if name == "git" {
            format!("exec '{}' \"$@\"\n", real_git.display())
        } else {
            "exit 1\n".to_string()
        };
        fs::write(
            &stub,
            format!(
                "#!/bin/sh\necho \"{name} $*\" >> '{}'\n{tail}",
                calls.display()
            ),
        )
        .expect("stub");
        fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).expect("stub mode");
    }

    let path_with = |directory: &std::path::Path| {
        let mut entries = vec![directory.to_path_buf()];
        if let Some(existing) = std::env::var_os("PATH") {
            entries.extend(std::env::split_paths(&existing));
        }
        std::env::join_paths(entries).expect("PATH should join")
    };

    let binary = std::env::var("CARGO_BIN_EXE_loam").expect("cargo should provide the loam binary");
    let state = |binary: &str| {
        let mut command = Command::new(binary);
        command
            .args(["state", "--fast", root.to_str().unwrap()])
            .env("HCOM_TOOL", "")
            .env("HCOM_INSTALL_DIR", "")
            // Prepended, not replaced: the fixture must still pay for the git
            // probe the real hook path pays for.
            .env("PATH", path_with(&bin))
            .env("LOAM_CONFIG_DIR", &config)
            .env("LOAM_WORKSPACES", "off");
        #[cfg(unix)]
        {
            let mut entries = vec![stubs.clone()];
            entries.extend(std::env::split_paths(&path_with(&bin)));
            command
                .env(
                    "PATH",
                    std::env::join_paths(entries).expect("PATH should join"),
                )
                .env("LOAM_SSH_KEYGEN", stubs.join("ssh-keygen"));
        }
        command
    };

    // Cold run: no page digest is cached, so the 512 KB hash cap must engage.
    let cold = state(&binary).output().expect("cold run should run");
    assert!(
        cold.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&cold.stderr)
    );
    let cold_stdout = String::from_utf8(cold.stdout).expect("state output should be UTF-8");
    assert!(cold_stdout.contains("\"seals\":"), "{cold_stdout}");
    let cold_json = loam::json::parse(&cold_stdout).expect("state output should be JSON");
    let diag = cold_json
        .get("seals")
        .and_then(|seals| seals.get("diag"))
        .expect("seals.diag");
    let hashed = counter(diag, "hashed_bytes");
    let over = counter(diag, "over_budget");
    assert!(
        hashed <= HASH_CAP,
        "cold run hashed {hashed} bytes, over the cap"
    );
    assert!(hashed > 0, "cold run hashed nothing: {cold_stdout}");
    assert!(over > 0, "the hash cap never engaged: {cold_stdout}");

    // One warmup so the page cache, not cold I/O, is what the budget measures.
    state(&binary).output().expect("warmup should run");

    let started = Instant::now();
    let output = state(&binary).output().expect("state should run");
    let elapsed = started.elapsed();
    // `state` itself runs one `git status --porcelain` per run (the working
    // tree probe, three runs here); the seal path adds no other git call and
    // never reaches `ssh-keygen`.
    #[cfg(unix)]
    {
        let spawned = fs::read_to_string(&calls).unwrap_or_default();
        let unexpected: Vec<_> = spawned
            .lines()
            .filter(|line| !(line.starts_with("git -C ") && line.ends_with(" status --porcelain")))
            .collect();
        assert!(
            unexpected.is_empty(),
            "the seal path spawned a subprocess: {unexpected:?}"
        );
        assert_eq!(spawned.lines().count(), 3, "{spawned}");
    }

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("state output should be UTF-8");
    // Fast mode omits the date and codegraph probes and reports a null drift count.
    assert!(stdout.contains("\"drift_count\":null"), "{stdout}");
    assert!(!stdout.contains("date_drift_pending"), "{stdout}");
    assert!(!stdout.contains("code_ingest_pending"), "{stdout}");
    // The measured run really did walk the whole hcom ladder, spawn included —
    // a probe that silently stopped detecting would make this budget meaningless.
    // Unix only, for the reason the fixture comment above gives.
    #[cfg(unix)]
    assert!(stdout.contains("\"hcom_ready\":true"), "{stdout}");
    assert!(
        elapsed.as_secs() < BUDGET_SECONDS,
        "state --fast took {elapsed:?} on {SOURCE_FILES} sources / {CODE_PAGES} code pages / \
         {CHECKPOINTS} checkpoints, over the {BUDGET_SECONDS}s hook budget"
    );
}
