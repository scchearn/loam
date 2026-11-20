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
    "cr.md",
    "trailing-ws.md",
    "boundary-blank.md",
    "non-ascii.md",
    "fenced-lookalike.md",
    "reflowed.md",
];

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

fn floors() -> Vec<Floor> {
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
fn raw_output_is_the_exact_canonical_bytes() {
    let expected: &[u8] =
        b"TLS tries the bundled rung first.\n\nThen it falls back to the system store.\n";
    for floor in floors() {
        for name in [
            "lf.md",
            "crlf.md",
            "cr.md",
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
            "cr.md",
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
