//! Conformance of the floor canon scripts (`seal-canon.sh` / `seal-canon.ps1`)
//! against the runtime: for every fixture, the script's raw bytes and digest
//! mode must equal `loam seal hash`. The floor is the trust fallback, so a
//! divergence is fixed in the script, never in the runtime canon.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const ANCHOR: &str = "s-floor-span";

const FIXTURES: &[&str] = &[
    "lf.md",
    "crlf.md",
    "trailing-ws.md",
    "boundary-blank.md",
    "non-ascii.md",
    "fenced-lookalike.md",
    "reflowed.md",
];

/// Lone-CR page: the runtime accepts it; the floor deliberately refuses.
const REFUSED_FIXTURE: &str = "cr.md";

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/seal-canon")
        .join(name)
}

fn script(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../skills/loam-using/scripts")
        .join(name)
}

/// One way of invoking a floor script.
struct Floor {
    label: &'static str,
    program: String,
    prefix: Vec<String>,
}

impl Floor {
    fn run(&self, digest: bool, file: &Path, anchor: &str) -> Output {
        let mut args = self.prefix.clone();
        if digest {
            args.push(
                if self.label == "sh" {
                    "--digest"
                } else {
                    "-Digest"
                }
                .to_owned(),
            );
        }
        args.push(file.to_str().unwrap().to_owned());
        args.push(anchor.to_owned());
        self.exec(&args)
    }

    fn exec(&self, args: &[String]) -> Output {
        Command::new(&self.program)
            .args(args)
            .stdin(Stdio::null())
            .output()
            .unwrap_or_else(|error| panic!("{} should run: {error}", self.label))
    }
}

fn on_path(program: &str) -> bool {
    Command::new(program)
        .arg("-NoProfile")
        .arg("-Command")
        .arg("exit 0")
        .stdin(Stdio::null())
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn ps1_prefix() -> Vec<String> {
    vec![
        "-NoProfile".into(),
        "-NonInteractive".into(),
        "-ExecutionPolicy".into(),
        "Bypass".into(),
        "-File".into(),
        script("seal-canon.ps1").to_str().unwrap().to_owned(),
    ]
}

/// Probed once per process: each probe is a PowerShell cold start on Windows.
fn floors() -> &'static [Floor] {
    static FLOORS: std::sync::OnceLock<Vec<Floor>> = std::sync::OnceLock::new();
    FLOORS.get_or_init(probe_floors)
}

fn probe_floors() -> Vec<Floor> {
    let mut floors = Vec::new();
    if cfg!(unix) {
        floors.push(Floor {
            label: "sh",
            program: "sh".into(),
            prefix: vec![script("seal-canon.sh").to_str().unwrap().to_owned()],
        });
    }
    if cfg!(windows) {
        // In-box Windows PowerShell 5.1 is the contract there: it must exist.
        assert!(on_path("powershell.exe"), "powershell.exe is required");
        floors.push(Floor {
            label: "ps1",
            program: "powershell.exe".into(),
            prefix: ps1_prefix(),
        });
    } else if on_path("pwsh") {
        // Opportunistic: pwsh is optional and never required.
        floors.push(Floor {
            label: "ps1",
            program: "pwsh".into(),
            prefix: ps1_prefix(),
        });
    }
    assert!(!floors.is_empty(), "no floor script runner available");
    floors
}

fn loam_hash(file: &Path) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_loam"))
        .args(["seal", "hash", file.to_str().unwrap(), ANCHOR])
        .env("LOAM_WORKSPACES", "off")
        .output()
        .expect("loam should run");
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn stdout(output: &Output) -> Vec<u8> {
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    output.stdout.clone()
}

#[test]
fn raw_and_digest_modes_equal_the_runtime_for_every_fixture() {
    for floor in floors() {
        for name in FIXTURES {
            let file = fixture(name);
            let expected = loam_hash(&file);

            let raw = stdout(&floor.run(false, &file, ANCHOR));
            assert_eq!(
                loam::sha256::hex(&raw),
                expected,
                "{} raw bytes for {name}",
                floor.label
            );

            let digest = String::from_utf8(stdout(&floor.run(true, &file, ANCHOR))).unwrap();
            assert_eq!(
                digest.trim(),
                expected,
                "{} digest mode for {name}",
                floor.label
            );
            assert_eq!(
                digest.trim_end_matches(['\r', '\n']).len(),
                64,
                "{} digest is bare lowercase hex for {name}",
                floor.label
            );
        }
    }
}

#[test]
fn lone_cr_pages_are_refused_by_the_floor() {
    for floor in floors() {
        for digest in [false, true] {
            let output = floor.run(digest, &fixture(REFUSED_FIXTURE), ANCHOR);
            assert_eq!(output.status.code(), Some(2), "{} {output:?}", floor.label);
            assert!(output.stdout.is_empty(), "{} {output:?}", floor.label);
        }
    }
}

#[test]
fn raw_output_is_the_exact_canonical_bytes() {
    let expected: &[u8] =
        b"TLS tries the bundled rung first.\n\nThen it falls back to the system store.\n";
    for floor in floors() {
        for name in [
            "lf.md",
            "crlf.md",
            "trailing-ws.md",
            "boundary-blank.md",
            "fenced-lookalike.md",
        ] {
            let raw = stdout(&floor.run(false, &fixture(name), ANCHOR));
            assert_eq!(raw, expected, "{} {name}", floor.label);
        }
    }
}

#[test]
fn non_ascii_survives_as_utf8_without_bom_or_normalization() {
    for floor in floors() {
        let raw = stdout(&floor.run(false, &fixture("non-ascii.md"), ANCHOR));
        assert!(!raw.starts_with(&[0xEF, 0xBB, 0xBF]), "{}", floor.label);
        let text = String::from_utf8(raw).unwrap();
        assert!(text.contains("e\u{301}."), "{}", floor.label);
        assert!(text.contains("\u{e9} is not"), "{}", floor.label);
    }
}

#[test]
fn encoding_noise_is_absorbed_but_reflow_and_content_are_not() {
    for floor in floors() {
        let digest = |name: &str| {
            String::from_utf8(stdout(&floor.run(true, &fixture(name), ANCHOR)))
                .unwrap()
                .trim()
                .to_owned()
        };
        let base = digest("lf.md");
        for same in [
            "crlf.md",
            "trailing-ws.md",
            "boundary-blank.md",
            "fenced-lookalike.md",
        ] {
            assert_eq!(digest(same), base, "{} {same}", floor.label);
        }
        assert_ne!(digest("reflowed.md"), base, "{}", floor.label);
        assert_ne!(digest("non-ascii.md"), base, "{}", floor.label);
    }
}

#[test]
fn caret_and_hash_prefixed_anchors_are_accepted() {
    for floor in floors() {
        let file = fixture("lf.md");
        let plain = stdout(&floor.run(true, &file, ANCHOR));
        assert_eq!(
            stdout(&floor.run(true, &file, &format!("^{ANCHOR}"))),
            plain
        );
        assert_eq!(
            stdout(&floor.run(true, &file, &format!("#^{ANCHOR}"))),
            plain
        );
    }
}

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> TempDir {
        let dir =
            std::env::temp_dir().join(format!("loam-seal-floor-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        for _ in 0..5 {
            if std::fs::remove_dir_all(&self.0).is_ok() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
}

#[test]
fn failures_exit_2_with_nothing_on_stdout() {
    let dir = TempDir::new("errors");
    let bad = dir.0.join("bad.md");
    std::fs::write(
        &bad,
        b"<!-- loam:seal s-floor-span -->\nbad \xff byte\n<!-- /loam:seal -->\n",
    )
    .unwrap();
    // Valid markers, invalid UTF-8 outside the span: the runtime refuses the whole file.
    let outside = dir.0.join("outside.md");
    std::fs::write(
        &outside,
        b"stray \xc3\n\n<!-- loam:seal s-floor-span -->\nok\n<!-- /loam:seal -->\n",
    )
    .unwrap();
    let nested = dir.0.join("nested.md");
    std::fs::write(
        &nested,
        "<!-- loam:seal s-floor-span -->\na\n<!-- loam:seal s-inner-span -->\nb\n<!-- /loam:seal -->\n<!-- /loam:seal -->\n",
    )
    .unwrap();
    let unclosed = dir.0.join("unclosed.md");
    std::fs::write(&unclosed, "<!-- loam:seal s-floor-span -->\na\n").unwrap();
    let duplicate = dir.0.join("duplicate.md");
    std::fs::write(
        &duplicate,
        "<!-- loam:seal s-floor-span -->\na\n<!-- /loam:seal -->\n<!-- loam:seal s-floor-span -->\nb\n<!-- /loam:seal -->\n",
    )
    .unwrap();
    let tokens = dir.0.join("tokens.md");
    std::fs::write(
        &tokens,
        "<!-- loam:seal s-floor-span extra -->\na\n<!-- /loam:seal -->\n",
    )
    .unwrap();

    for floor in floors() {
        for digest in [false, true] {
            let cases = [
                (fixture("lf.md"), "s-nope"),
                // The runtime matches ids exactly; a case-insensitive map would not.
                (fixture("lf.md"), "S-Floor-Span"),
                (fixture("lf.md"), "^#s-floor-span"),
                (dir.0.join("missing.md"), ANCHOR),
                (bad.clone(), ANCHOR),
                (outside.clone(), ANCHOR),
                (nested.clone(), ANCHOR),
                (unclosed.clone(), ANCHOR),
                (duplicate.clone(), ANCHOR),
                (tokens.clone(), ANCHOR),
            ];
            for (file, anchor) in cases {
                let output = floor.run(digest, &file, anchor);
                assert_eq!(
                    output.status.code(),
                    Some(2),
                    "{} digest={digest} {} {anchor}: {output:?}",
                    floor.label,
                    file.display()
                );
                assert!(output.stdout.is_empty(), "{} {output:?}", floor.label);
            }
        }
    }
}

#[test]
fn sh_floor_reads_files_named_like_awk_assignments_or_options() {
    let sh = floors().iter().find(|floor| floor.label == "sh");
    let Some(sh) = sh else { return };
    let dir = TempDir::new("odd-names");
    let page = "<!-- loam:seal s-floor-span -->\nbody\n<!-- /loam:seal -->\n";
    for name in ["a=b.md", "-dash.md"] {
        std::fs::write(dir.0.join(name), page).unwrap();
        let output = Command::new(&sh.program)
            .arg(script("seal-canon.sh"))
            .args(["--digest", name, ANCHOR])
            .current_dir(&dir.0)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(0), "{name}: {output:?}");
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            loam_hash(&dir.0.join(name)),
            "{name}"
        );
    }
}

#[test]
fn missing_arguments_exit_2() {
    for floor in floors() {
        let output = floor.exec(&floor.prefix.clone());
        assert_eq!(output.status.code(), Some(2), "{} {output:?}", floor.label);
    }
}

#[test]
fn runtime_refuses_the_same_malformed_spans() {
    // The scripts must not find spans the runtime scanner rejects.
    let dir = TempDir::new("parity");
    let cases = [
        (
            "nested.md",
            "<!-- loam:seal s-floor-span -->\na\n<!-- loam:seal s-inner-span -->\nb\n<!-- /loam:seal -->\n<!-- /loam:seal -->\n",
        ),
        ("unclosed.md", "<!-- loam:seal s-floor-span -->\na\n"),
        (
            "duplicate.md",
            "<!-- loam:seal s-floor-span -->\na\n<!-- /loam:seal -->\n<!-- loam:seal s-floor-span -->\nb\n<!-- /loam:seal -->\n",
        ),
        (
            "indented.md",
            "intro\n\n    <!-- loam:seal s-floor-span -->\n    a\n    <!-- /loam:seal -->\n",
        ),
    ];
    for (name, body) in cases {
        let file = dir.0.join(name);
        std::fs::write(&file, body).unwrap();
        let runtime = Command::new(env!("CARGO_BIN_EXE_loam"))
            .args(["seal", "hash", file.to_str().unwrap(), ANCHOR])
            .env("LOAM_WORKSPACES", "off")
            .output()
            .unwrap();
        assert_eq!(runtime.status.code(), Some(2), "runtime {name}");
        for floor in floors() {
            let output = floor.run(false, &file, ANCHOR);
            assert_eq!(output.status.code(), Some(2), "{} {name}", floor.label);
        }
    }
}

fn runtime_hash(file: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_loam"))
        .args(["seal", "hash", file.to_str().unwrap(), ANCHOR])
        .env("LOAM_WORKSPACES", "off")
        .output()
        .unwrap()
}

#[test]
fn floor_never_certifies_a_span_the_runtime_would_not() {
    // Each page is one the floor's line scanner could misread. Whatever the
    // runtime says, the floor must either print the runtime's digest or
    // refuse with exit 2; it may never print a different digest, and it
    // must refuse whenever the runtime does.
    let open = "<!-- loam:seal s-floor-span -->";
    let close = "<!-- /loam:seal -->";
    let cases: Vec<(&str, String)> = vec![
        // A decoy copy in a list-item fence, the real span a list continuation.
        (
            "list-fence-decoy",
            format!(
                "- ```\n  {open}\n  original\n  {close}\n  ```\n\n- item\n\n    {open}\n    tampered\n    {close}\n"
            ),
        ),
        (
            "list-continuation",
            format!("- item\n\n    {open}\n    real\n    {close}\n"),
        ),
        (
            "list-fence-hides-real",
            format!("- item\n  ```\n  {open}\n  fake\n  {close}\n  ```\n"),
        ),
        (
            "quote-fence",
            format!("> ```\n{open}\nx\n{close}\n"),
        ),
        (
            "question-inside",
            format!("{open}\nx\n- [ ] Q: who? ^q-who-signs\n{close}\n"),
        ),
        (
            "heading-then-indented",
            format!("# H\n    {open}\n    x\n    {close}\n"),
        ),
        (
            "fence-then-indented",
            format!("```\ncode\n```\n    {open}\n    x\n    {close}\n"),
        ),
        (
            "comment-then-indented",
            format!("<!-- note -->\n    {open}\n    x\n    {close}\n"),
        ),
        (
            "lone-cr-x1",
            format!("- item\r\r    {open}\r    x\r    {close}\r"),
        ),
        ("lone-cr-trailing", format!("{open}\nx\n{close}\n\r")),
        ("lone-cr-in-body", format!("{open}\na\rb\n{close}\n")),
        ("lone-cr-eof-no-lf", format!("{open}\nx\n{close}\r")),
        ("form-feed-closer", format!("{open}\nx\n{close}\x0c\n")),
        ("nbsp-closer", format!("{open}\nx\n{close}\u{a0}\n")),
        ("nul-in-span", format!("{open}\na\0b\n{close}\n")),
        (
            "front-matter-blank-line",
            format!("---\n\n{open}\nx\n{close}\n---\n"),
        ),
        (
            "front-matter",
            format!("---\ntitle: t\n---\n\n{open}\nx\n{close}\n"),
        ),
        ("plain", format!("intro\n\n{open}\nx\n{close}\n")),
        // HTML blocks hide a fence line from the runtime's code view.
        (
            "pre-swap",
            format!(
                "<pre>\n```\n</pre>\n\n{open}\nALTERED\n{close}\n\n```\n{open}\nORIGINAL\n{close}\n```\n"
            ),
        ),
        (
            "compact-pre-swap",
            "<pre>\n```\n</pre>\n\n<!--loam:seal s-floor-span-->\nALTERED\n<!--/loam:seal-->\n\n```\n<!--loam:seal s-floor-span-->\nORIGINAL\n<!--/loam:seal-->\n```\n".to_string(),
        ),
        (
            "half-compact-pre-swap",
            "<pre>\n```\n</pre>\n\n<!-- loam:seal s-floor-span-->\nALTERED\n<!-- /loam:seal-->\n\n```\n<!-- loam:seal s-floor-span-->\nORIGINAL\n<!-- /loam:seal-->\n```\n".to_string(),
        ),
        (
            "div-fence-refused",
            format!("<div>\n```\n</div>\n\n```\n{open}\nx\n{close}\n```\n"),
        ),
        (
            "multiline-comment-fence",
            format!("<!--\n```\n-->\n\n{open}\nALTERED\n{close}\n\n```\n{open}\nORIGINAL\n{close}\n```\n"),
        ),
        (
            "footnote-fence",
            format!("[^n]: ```\n\n{open}\nALTERED\n{close}\n\n```\n{open}\nORIGINAL\n{close}\n```\n"),
        ),
        (
            "fenced-same-id-decoy",
            format!("```\n{open}\nORIGINAL\n{close}\n```\n\n{open}\nALTERED\n{close}\n"),
        ),
        (
            "duplicate-decoy",
            format!("{open}\nBODY\n{close}\n---\n{open}\nC\n{close}\n{close}\n"),
        ),
        (
            "paragraph-continuation-comment",
            format!("intro\n{open}\n    {close}\nORIGINAL\n{close}\n"),
        ),
        (
            "indented-opener-after-paragraph",
            format!("intro\n    {open}\n    {close}\nORIGINAL\n{close}\n"),
        ),
        (
            "equals-after-blank",
            format!("{open}\nx\n\n===\n    {close}\ny\n{close}\n"),
        ),
        (
            "dashes-after-blank",
            format!("{open}\nx\n\n--\n    {close}\ny\n{close}\n"),
        ),
        (
            "setext-after-paragraph",
            format!("{open}\nx\n==\n    {close}\ny\n{close}\n"),
        ),
        (
            "suffixed-id-neighbour",
            format!("{open}\nx\n{close}\n\n<!-- loam:seal s-floor-span-2 -->\ny\n{close}\n"),
        ),
        (
            "list-adjacent-fences",
            format!("<!-- x -->\nBODY\n{close}\n---\n{open}\nC\n{close}\n{close}\n<!-- x -->\n- item\n```\n~~~\n{open}\n{open}\n---\n"),
        ),
    ];
    let dir = TempDir::new("never-certifies");
    check_never_certifies(&dir, &cases, usize::MAX, usize::MAX);
    // Ordinary pages still verify.
    for name in ["plain", "front-matter", "suffixed-id-neighbour"] {
        let file = dir.0.join(format!("{name}.md"));
        for floor in floors() {
            assert_eq!(
                floor.run(true, &file, ANCHOR).status.code(),
                Some(0),
                "{} {name}",
                floor.label
            );
        }
    }
}

#[test]
fn floor_never_certifies_generated_pages() {
    // Deterministic seeded pages built from structure snippets: whatever the
    // floor prints with status 0 must be the runtime's digest.
    let open = "<!-- loam:seal s-floor-span -->";
    let close = "<!-- /loam:seal -->";
    let snippets: Vec<String> = vec![
        "```".into(),
        "~~~".into(),
        "<pre>".into(),
        "</pre>".into(),
        "<div>".into(),
        "<!--".into(),
        "-->".into(),
        "<!-- x -->".into(),
        "- item".into(),
        "> quote".into(),
        "[^n]: ```".into(),
        "    indented".into(),
        "\tindented".into(),
        "- outer\n  - nested".into(),
        "1. ordered".into(),
        "> ```".into(),
        "text\r".into(),
        "```\r".into(),
        "---".into(),
        "==".into(),
        "===".into(),
        "--".into(),
        "-".into(),
        "intro".into(),
        format!("    {open}"),
        format!("    {close}"),
        "<!--loam:seal s-floor-span-->".into(),
        "<!-- loam:seal s-floor-span-->".into(),
        "<!--/loam:seal-->".into(),
        "text".into(),
        "".into(),
        open.into(),
        open.into(),
        close.into(),
        close.into(),
        "BODY".into(),
    ];
    let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = move || {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (state >> 33) as usize
    };
    let terminators = ["\n", "\n", "\n", "\r\n", "\r"];
    let random_pages: Vec<(String, String)> = (0..600)
        .map(|index| {
            let count = 4 + next() % 12;
            let mut body = String::new();
            for _ in 0..count {
                body.push_str(&snippets[next() % snippets.len()]);
                body.push_str(terminators[next() % terminators.len()]);
            }
            (format!("gen-{index}"), body)
        })
        .collect();
    // Plain intro / opener / random content / closer pages mostly reach the
    // accept path, so digest parity (not just refusal) is exercised.
    let content = [
        "plain words",
        "caf\u{e9} \u{301}",
        "nbsp\u{a0}inside",
        "tab\tinside",
        "trailing  ",
        "trailing\t",
        "",
        "   ",
        "\u{4e16}\u{754c}",
        "emoji \u{1f600}",
        "a  b",
    ];
    let accept_terms = ["\n", "\r\n", "\n", "\r\n"];
    let mut accept_pages: Vec<(String, String)> = Vec::new();
    for index in 0..300 {
        let mut body = String::from("intro");
        body.push_str(accept_terms[next() % accept_terms.len()]);
        body.push_str(accept_terms[next() % accept_terms.len()]);
        body.push_str(open);
        body.push_str(accept_terms[next() % accept_terms.len()]);
        for _ in 0..(1 + next() % 5) {
            body.push_str(content[next() % content.len()]);
            // Occasionally a lone CR, which the floor must refuse.
            let term = if next() % 25 == 0 {
                "\r"
            } else {
                accept_terms[next() % accept_terms.len()]
            };
            body.push_str(term);
        }
        body.push_str(close);
        body.push_str(accept_terms[next() % accept_terms.len()]);
        accept_pages.push((format!("acc-{index}"), body));
    }
    // Interleave so the ps1 floor's first 60 pages cover both families.
    let mut accept_iter = accept_pages.into_iter();
    let mut pages: Vec<(String, String)> = Vec::new();
    for (index, page) in random_pages.into_iter().enumerate() {
        pages.push(page);
        if index % 2 == 0 {
            pages.extend(accept_iter.next());
        }
    }
    let cases: Vec<(&str, String)> = pages
        .iter()
        .map(|(name, body)| (name.as_str(), body.clone()))
        .collect();
    let dir = TempDir::new("never-certifies-gen");
    check_never_certifies(&dir, &cases, usize::MAX, 60);
}

/// Each ps1 case is a PowerShell cold start, so a ps1 floor only sees the
/// first `ps1_limit` cases; the sh floor sees up to `sh_limit`.
fn check_never_certifies(
    dir: &TempDir,
    cases: &[(&str, String)],
    sh_limit: usize,
    ps1_limit: usize,
) {
    for (index, (name, body)) in cases.iter().enumerate() {
        let file = dir.0.join(format!("{name}.md"));
        std::fs::write(&file, body).unwrap();
        let runtime = runtime_hash(&file);
        let runtime_ok = runtime.status.code() == Some(0);
        for floor in floors() {
            let limit = if floor.label.contains("ps1") {
                ps1_limit
            } else {
                sh_limit
            };
            if index >= limit {
                continue;
            }
            let output = floor.run(true, &file, ANCHOR);
            match output.status.code() {
                Some(0) => {
                    assert!(
                        runtime_ok,
                        "{} certified {name} but the runtime refuses",
                        floor.label
                    );
                    assert_eq!(
                        String::from_utf8_lossy(&output.stdout).trim(),
                        String::from_utf8_lossy(&runtime.stdout).trim(),
                        "{} digest differs from the runtime for {name}",
                        floor.label
                    );
                }
                Some(2) => assert!(
                    output.stdout.is_empty(),
                    "{} {name}: {output:?}",
                    floor.label
                ),
                other => panic!("{} {name}: unexpected status {other:?}", floor.label),
            }
            if !runtime_ok {
                assert_eq!(
                    output.status.code(),
                    Some(2),
                    "{} must refuse {name}, which the runtime refuses",
                    floor.label
                );
            }
        }
    }
}
