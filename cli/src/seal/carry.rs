//! `loam seal carry`: the link-retarget prover (spec D4, S11).
//!
//! Obsidian rewrites wikilinks when a page is renamed (`alwaysUpdateLinks`),
//! which changes the bytes of a sealed span without changing what it says.
//! `carry` proves that the only difference between the as-sealed text and the
//! live span is wikilink targets that git recorded as renames, and then
//! writes a signed `kind: update`, `type: link-retarget` record that moves
//! the seal's binding to the live bytes. Anything else, and any memory git
//! does not track, is refused with a re-seal hint: a proof from git is only
//! as good as the history it reads.
//!
//! Carry never re-attests evidence and never raises a level: the update
//! carries no evidence of its own, and the seal is re-judged from its
//! original record, so the result is at most the level it had before the
//! rename.

use crate::markers::{self, Kind as MarkerKind};
use crate::seal::attest::{
    ambient_context, is_record_id, refuse, signing_identity, stamp, AttestError, Context,
    PROCEDURE_URI,
};
use crate::seal::canon;
use crate::seal::policy::Level;
use crate::seal::record::{
    self, Generator, NewRecord, RecordKind, StatusInfo, UPDATE_TYPE_LINK_RETARGET,
};
use crate::seal::sshsig;
use crate::seal::status;
use crate::seal::verdict::Verdict;
use crate::seal::verify::{self, Env, Overlay, Report};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// A parsed `loam seal carry` request.
#[derive(Debug, Clone)]
pub struct Request {
    /// The seal record id to carry.
    pub id: String,
}

#[derive(Debug, Clone)]
pub struct Carried {
    /// Id of the update record written.
    pub id: String,
    /// The `(old, new)` link targets that were retargeted.
    pub retargets: Vec<(String, String)>,
    /// The seal as it now verifies.
    pub report: Report,
}

// ------------------------------------------------------------ tokenising

#[derive(Debug, Clone, PartialEq, Eq)]
enum Segment {
    Text(String),
    /// `[[target<tail>]]`: `tail` is everything after the target (a
    /// `#fragment`, a `|alias`, a table-escaped `\|alias`).
    Link {
        target: String,
        tail: String,
    },
}

/// Splits `text` into literal text and wikilinks. A `[[` with no `]]` on the
/// same line is literal text.
fn tokenise(text: &str) -> Vec<Segment> {
    let mut out = Vec::new();
    let mut literal = String::new();
    let mut rest = text;
    while let Some(open) = rest.find("[[") {
        let after = &rest[open + 2..];
        let line_end = after.find('\n').unwrap_or(after.len());
        let close = after[..line_end].find("]]");
        let inner = close.map(|c| &after[..c]).filter(|i| !i.contains("[["));
        let (Some(inner), Some(close)) = (inner, close) else {
            literal.push_str(&rest[..open + 2]);
            rest = after;
            continue;
        };
        literal.push_str(&rest[..open]);
        if !literal.is_empty() {
            out.push(Segment::Text(std::mem::take(&mut literal)));
        }
        out.push(split_link(inner));
        rest = &after[close + 2..];
    }
    literal.push_str(rest);
    if !literal.is_empty() {
        out.push(Segment::Text(literal));
    }
    out
}

fn split_link(inner: &str) -> Segment {
    let mut end = inner.len();
    if let Some(hash) = inner.find('#') {
        end = hash;
    }
    if let Some(bar) = inner.find('|') {
        // A table cell escapes the bar: `[[page\|alias]]`.
        let bar = if inner[..bar].ends_with('\\') {
            bar - 1
        } else {
            bar
        };
        end = end.min(bar);
    }
    Segment::Link {
        target: inner[..end].to_owned(),
        tail: inner[end..].to_owned(),
    }
}

// ------------------------------------------------------------- the proof

/// One rename git recorded: repo-relative paths, forward slashes.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Rename {
    old: String,
    new: String,
}

fn bare(path: &str) -> &str {
    path.strip_suffix(".md").unwrap_or(path)
}

/// A link target names `path` when it is the path itself, a path suffix on a
/// directory boundary (`topics/name` for `wiki/topics/name.md`), or the bare
/// file name.
fn names(target: &str, path: &str) -> bool {
    let (target, path) = (bare(target), bare(path));
    !target.is_empty() && (path == target || path.ends_with(&format!("/{target}")))
}

/// Every `(old, new)` path pair a link could have followed: each rename on
/// its own, and chains (`a`→`b`, then `b`→`c` gives `a`→`c`). `renames` is
/// oldest first.
fn closure(renames: &[Rename]) -> Vec<Rename> {
    let mut pairs: Vec<Rename> = Vec::new();
    for rename in renames {
        let extended: Vec<Rename> = pairs
            .iter()
            .filter(|p| p.new == rename.old)
            .map(|p| Rename {
                old: p.old.clone(),
                new: rename.new.clone(),
            })
            .collect();
        pairs.push(rename.clone());
        pairs.extend(extended);
    }
    pairs
}

/// `Ok` with the retargeted `(old, new)` link targets when `live` differs
/// from `sealed` only by wikilink targets that `renames` explain; otherwise
/// the reason it does not.
fn prove(sealed: &str, live: &str, renames: &[Rename]) -> Result<Vec<(String, String)>, String> {
    let changes = link_changes(sealed, live)?;
    let pairs = closure(renames);
    for (from, to) in &changes {
        let explained = pairs
            .iter()
            .any(|rename| names(from, &rename.old) && names(to, &rename.new));
        if !explained {
            return Err(format!(
                "[[{from}]] became [[{to}]] but git records no rename from one to the other since the seal was committed"
            ));
        }
    }
    Ok(changes)
}

/// The offline half of the proof, enforced by carry and by verify: `live`
/// differs from `sealed` only in wikilink targets (identical text, aliases,
/// fragments and segment structure). Returns the distinct `(old, new)` pairs.
pub(crate) fn link_changes(sealed: &str, live: &str) -> Result<Vec<(String, String)>, String> {
    let (old, new) = (tokenise(sealed), tokenise(live));
    if old.len() != new.len() {
        return Err("the span's text changed, not only its links".to_owned());
    }
    let mut retargets: Vec<(String, String)> = Vec::new();
    for (a, b) in old.iter().zip(&new) {
        match (a, b) {
            (Segment::Text(x), Segment::Text(y)) if x == y => {}
            (Segment::Text(_), Segment::Text(_)) => {
                return Err("the span's text changed, not only its links".to_owned())
            }
            (
                Segment::Link {
                    target: from,
                    tail: from_tail,
                },
                Segment::Link {
                    target: to,
                    tail: to_tail,
                },
            ) => {
                if from_tail != to_tail {
                    return Err(format!(
                        "the alias or fragment of [[{from}{from_tail}]] changed"
                    ));
                }
                if from == to {
                    continue;
                }
                if !retargets.iter().any(|(f, t)| f == from && t == to) {
                    retargets.push((from.clone(), to.clone()));
                }
            }
            _ => return Err("the span's text changed, not only its links".to_owned()),
        }
    }
    Ok(retargets)
}

// ------------------------------------------------------------------- git

struct GitOut {
    ok: bool,
    stdout: String,
}

fn git(repo: &Path, args: &[&str]) -> Result<GitOut, String> {
    let output = Command::new("git")
        .args([
            "-c",
            "core.quotepath=false",
            "-c",
            "log.showSignature=false",
        ])
        .args(args)
        .current_dir(repo)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    Ok(GitOut {
        ok: output.status.success(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
    })
}

/// `path` relative to `root` with forward slashes, resolving symlinks and
/// Windows verbatim prefixes so both sides compare in one form.
fn relative_to(root: &Path, path: &Path) -> Option<String> {
    let canon = |p: &Path| fs::canonicalize(p).map(crate::enrollment::dos_compatible);
    let (root, path) = (canon(root).ok()?, canon(path).ok()?);
    let rel = path.strip_prefix(&root).ok()?;
    Some(rel.to_string_lossy().replace('\\', "/"))
}

fn parse_renames(log: &str) -> Vec<Rename> {
    // `-z`: NUL-separated `status NUL path [NUL path]`; renames carry two
    // paths, everything else one. Tolerate stray newlines between commits.
    let mut renames: Vec<Rename> = Vec::new();
    let mut fields = log.split('\0').map(|f| f.trim_start_matches('\n'));
    while let Some(status) = fields.next() {
        if status.is_empty() {
            continue;
        }
        let paths = if status.starts_with('R') || status.starts_with('C') {
            2
        } else {
            1
        };
        let first = fields.next();
        let second = (paths == 2).then(|| fields.next()).flatten();
        if let (true, Some(old), Some(new)) = (status.starts_with('R'), first, second) {
            renames.push(Rename {
                old: old.to_owned(),
                new: new.to_owned(),
            });
        }
    }
    // `git log` lists newest first.
    renames.reverse();
    renames
}

fn untracked(id: &str, why: &str) -> AttestError {
    refuse(format!(
        "untracked memory: re-seal with loam seal attest … --parent {id} ({why})"
    ))
}

// ----------------------------------------------------------------- carry

/// Carries the seal `request.id` over a link retarget. Nothing is written
/// unless the proof holds and the carried seal verifies citable in memory.
pub fn carry(ctx: &Context, request: &Request) -> Result<Carried, AttestError> {
    let usage = AttestError::Usage;
    if !is_record_id(&request.id) {
        return Err(usage(format!(
            "`{}` is not a {}-hex seal id",
            request.id,
            record::ID_LEN
        )));
    }

    let mut env = Env::load(&ctx.wiki, Some(ctx.trust_dir.clone()), ctx.now)
        .map_err(|e| refuse(format!("cannot load trust: {e}")))?;
    env.repo_root = ctx.repo_root.clone();
    env.keygen = ctx.keygen.clone();

    let reports = verify::verify_all(&env).map_err(|e| refuse(e.to_string()))?;
    let target = reports
        .iter()
        .find(|report| report.id == request.id)
        .ok_or_else(|| usage(format!("{} is not a seal record in this wiki", request.id)))?;
    let loaded = record::load_all(&ctx.wiki).map_err(|e| refuse(e.to_string()))?;
    let view = loaded
        .iter()
        .find(|l| l.id == request.id)
        .and_then(|l| l.view.as_ref().ok())
        .ok_or_else(|| usage(format!("seal {} is not a well-formed record", request.id)))?;
    if target.outcome.verdict == Verdict::Retracted {
        return Err(refuse(format!(
            "seal {} is retracted; a retraction is not undone by carrying",
            request.id
        )));
    }
    let (page, _) = view
        .subject_name
        .split_once("#^")
        .unwrap_or((view.subject_name.as_str(), ""));
    let anchor = view.anchor.clone();

    // The live span.
    let Some(current) = target.current_page.as_deref() else {
        return Err(refuse(format!(
            "no seal span ^{anchor} is on any page; nothing to carry (re-seal with loam seal attest … --parent {})",
            request.id
        )));
    };
    let page_path = ctx.wiki.join(format!("{current}.md"));
    let page_bytes =
        fs::read(&page_path).map_err(|e| refuse(format!("cannot read page {current}: {e}")))?;
    let page_text = std::str::from_utf8(&page_bytes)
        .map_err(|e| refuse(format!("page {current} is not valid UTF-8 ({e})")))?;
    let scan = markers::scan(page_text);
    let marker = scan
        .markers
        .iter()
        .find(|m| m.kind == MarkerKind::Seal && m.id == anchor)
        .ok_or_else(|| refuse(format!("page {current} has no seal span ^{anchor}")))?;
    let span = &page_bytes[marker.content.clone()];
    let live_digest = canon::digest(span).map_err(|e| refuse(format!("span: {e}")))?;
    let live =
        String::from_utf8(canon::canonicalize(span).map_err(|e| refuse(format!("span: {e}")))?)
            .map_err(|_| refuse("span is not valid UTF-8"))?;

    // What the seal is bound to now: the newest verified update, else the seal.
    // Every verified update is a proven retarget of the original text, so the
    // proof always starts from the as-sealed content; renames since the
    // record was committed cover chained carries.
    let updates = verify::updates_for(&env, &request.id).map_err(refuse)?;
    let bound_content = view.content.clone().ok_or_else(|| {
        refuse(format!(
            "seal {} stores no content, so a retarget cannot be proven; re-seal with loam seal attest {page}#^{anchor} … --parent {}",
            request.id, request.id
        ))
    })?;
    if live_digest == view.subject_sha256 || updates.iter().any(|u| u.digest == live_digest) {
        return Err(refuse(format!(
            "seal {} still matches its span; nothing to carry",
            request.id
        )));
    }

    // Git must know both the page and the record.
    let repo = &ctx.repo_root;
    let page_rel = relative_to(repo, &page_path)
        .ok_or_else(|| untracked(&request.id, "the page is outside the repository"))?;
    let record_rel = relative_to(repo, &record::record_path(&ctx.wiki, &request.id))
        .ok_or_else(|| untracked(&request.id, "the record is outside the repository"))?;
    let tracked = git(repo, &["ls-files", "--error-unmatch", "--", &page_rel])
        .map_err(|e| refuse(format!("{e}; carry needs git")))?;
    if !tracked.ok {
        return Err(untracked(&request.id, "git does not track the page"));
    }
    let added = git(
        repo,
        &["log", "--diff-filter=A", "--format=%H", "--", &record_rel],
    )
    .map_err(refuse)?;
    let base = added
        .stdout
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty())
        .filter(|_| added.ok)
        .ok_or_else(|| untracked(&request.id, "the seal record is not committed"))?
        .to_owned();
    let range = format!("{base}..HEAD");
    let log = git(
        repo,
        &[
            "log",
            "--diff-filter=R",
            "--name-status",
            "-z",
            "-M",
            "--format=",
            &range,
        ],
    )
    .map_err(refuse)?;
    if !log.ok {
        return Err(refuse(format!("git could not list renames in {range}")));
    }
    let renames = parse_renames(&log.stdout);

    let retargets = prove(&bound_content, &live, &renames).map_err(|why| {
        refuse(format!(
            "not a pure link retarget: {why}; re-seal with loam seal attest {current}#^{anchor} … --parent {}",
            request.id
        ))
    })?;
    if retargets.is_empty() {
        return Err(refuse(format!(
            "the span changed in a way carry cannot explain; re-seal with loam seal attest {current}#^{anchor} … --parent {}",
            request.id
        )));
    }

    // Authority: the sealer, or a ceiling that reaches the seal's level.
    let (key, principal) = signing_identity(ctx)?;
    let requested = view
        .requested_level
        .as_deref()
        .and_then(Level::parse)
        .unwrap_or(Level::Evidence);
    let authority = status::authority_level(&env.trust.policy, requested, &view.principal);
    if !status::may_act(&env.trust.policy, &principal, &view.principal, authority) {
        return Err(refuse(format!(
            "{principal} may not carry seal {}: only its sealer or a principal whose ceiling reaches {} can",
            request.id,
            authority.as_str()
        )));
    }

    let reason = format!(
        "link-retarget: {}",
        retargets
            .iter()
            .map(|(from, to)| format!("[[{from}]] -> [[{to}]]"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let (id, bytes) = record::build(&NewRecord {
        kind: RecordKind::Update,
        page: current.to_owned(),
        anchor: anchor.clone(),
        subject_sha256: live_digest,
        locator: record::soft_locator(&record::heading_path(page_text, marker.opener.start), &live),
        content: live,
        requested_level: view.requested_level.clone(),
        claim: None,
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
        time_verified: stamp(&ctx.now),
        review_by: None,
        evidence: Vec::new(),
        redacted: Vec::new(),
        parent: None,
        status: Some(StatusInfo {
            type_: UPDATE_TYPE_LINK_RETARGET.to_owned(),
            updates: request.id.clone(),
            reason,
        }),
    })
    .map_err(|e| refuse(format!("cannot build the record: {e}")))?;
    if record::record_path(&ctx.wiki, &id).exists() {
        return Err(refuse(format!(
            "update record {id} already exists (the same carry was recorded in the same minute); nothing written"
        )));
    }
    let signature = sshsig::sign(&ctx.keygen, &key, &bytes)
        .map_err(|e| refuse(format!("signing failed: {e}")))?;

    // Rehearse in memory: the carried seal must be citable, or nothing is stored.
    let overlay = Overlay {
        record: Some((id.clone(), bytes.clone())),
        signature: Some(signature.clone()),
        ..Overlay::default()
    };
    let rehearsal = verify::verify_overlay(&env, &overlay, &request.id)
        .map_err(|e| refuse(format!("round-trip verification failed: {e}")))?;
    if !rehearsal.is_citable() {
        let mut lines = vec![format!(
            "the carried seal would not be citable ({}); nothing written",
            rehearsal.outcome.verdict.as_str()
        )];
        lines.extend(
            rehearsal
                .outcome
                .codes
                .failure
                .iter()
                .map(|c| format!("  {c}")),
        );
        return Err(AttestError::Refused(lines));
    }

    record::write_record(&ctx.wiki, &bytes)
        .map_err(|e| refuse(format!("cannot write the record: {e}")))?;
    if let Err(e) = record::write_sig(&ctx.wiki, &id, signature.as_bytes()) {
        return Err(refuse(format!(
            "update record {id} was written without its signature and is ignored until signed: {e}"
        )));
    }
    let report = verify::verify_overlay(&env, &Overlay::default(), &request.id)
        .map_err(|e| refuse(format!("update {id} written but not re-verified: {e}")))?;
    Ok(Carried {
        id,
        retargets,
        report,
    })
}

// ------------------------------------------------------------------- CLI

fn usage_text() -> i32 {
    eprintln!("Usage: loam seal carry <id>");
    crate::seal::EXIT_USAGE
}

pub fn run(args: impl Iterator<Item = String>) -> i32 {
    use crate::seal::{EXIT_OK, EXIT_REFUSED, EXIT_USAGE};
    let args: Vec<String> = args.collect();
    let [id] = args.as_slice() else {
        return usage_text();
    };
    if id.starts_with("--") {
        eprintln!("loam seal carry: unknown option {id}");
        return usage_text();
    }
    let Ok(cwd) = std::env::current_dir() else {
        eprintln!("loam seal carry: cannot read the current directory");
        return EXIT_USAGE;
    };
    let Some(wiki) = crate::seal::find_wiki(&cwd) else {
        eprintln!(
            "loam seal carry: no wiki/ directory at or above {}",
            PathBuf::from(&cwd).display()
        );
        return EXIT_USAGE;
    };
    let ctx = match ambient_context(wiki) {
        Ok(ctx) => ctx,
        Err(message) => {
            eprintln!("loam seal carry: {message}");
            return EXIT_REFUSED;
        }
    };
    match carry(&ctx, &Request { id: id.clone() }) {
        Ok(done) => {
            use crate::json::Value;
            let text = |s: &str| Value::String(s.to_owned());
            let retargets = Value::Array(
                done.retargets
                    .iter()
                    .map(|(from, to)| {
                        Value::Object(vec![
                            ("from".to_owned(), text(from)),
                            ("to".to_owned(), text(to)),
                        ])
                    })
                    .collect(),
            );
            let out = Value::Object(vec![
                ("update_record".to_owned(), text(&done.id)),
                ("type".to_owned(), text(UPDATE_TYPE_LINK_RETARGET)),
                ("retargets".to_owned(), retargets),
                ("seal".to_owned(), done.report.to_value()),
            ]);
            println!("{}", out.to_json());
            EXIT_OK
        }
        Err(AttestError::Usage(message)) => {
            eprintln!("loam seal carry: {message}");
            EXIT_USAGE
        }
        Err(AttestError::Refused(lines)) => {
            for line in lines {
                eprintln!("loam seal carry: {line}");
            }
            EXIT_REFUSED
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rename(old: &str, new: &str) -> Rename {
        Rename {
            old: old.to_owned(),
            new: new.to_owned(),
        }
    }

    #[test]
    fn tokenises_links_with_fragments_aliases_and_table_escapes() {
        let got = tokenise("a [[x#h|al]] b [[p/q\\|al]] [[#only]] ![[pic.png]] [[open");
        assert_eq!(
            got,
            vec![
                Segment::Text("a ".into()),
                Segment::Link {
                    target: "x".into(),
                    tail: "#h|al".into()
                },
                Segment::Text(" b ".into()),
                Segment::Link {
                    target: "p/q".into(),
                    tail: "\\|al".into()
                },
                Segment::Text(" ".into()),
                Segment::Link {
                    target: String::new(),
                    tail: "#only".into()
                },
                Segment::Text(" !".into()),
                Segment::Link {
                    target: "pic.png".into(),
                    tail: String::new()
                },
                Segment::Text(" [[open".into()),
            ]
        );
        // A link cannot span lines.
        assert_eq!(tokenise("[[a\nb]]"), vec![Segment::Text("[[a\nb]]".into())]);
    }

    #[test]
    fn proves_only_renamed_targets() {
        let renames = [rename("wiki/topics/old.md", "wiki/topics/new.md")];
        let got = prove(
            "See [[old]] and [[topics/old|al]].\n",
            "See [[new]] and [[topics/new|al]].\n",
            &renames,
        )
        .unwrap();
        assert_eq!(
            got,
            vec![
                ("old".to_owned(), "new".to_owned()),
                ("topics/old".to_owned(), "topics/new".to_owned())
            ]
        );
        // Text, alias, fragment, an unexplained target, and structure all fail.
        for live in [
            "See [[new]] and [[topics/new|al]]!\n",
            "See [[new]] and [[topics/new|other]].\n",
            "See [[new#h]] and [[topics/new|al]].\n",
            "See [[else]] and [[topics/new|al]].\n",
            "See new and [[topics/new|al]].\n",
        ] {
            assert!(
                prove("See [[old]] and [[topics/old|al]].\n", live, &renames).is_err(),
                "{live}"
            );
        }
        // A rename is not a licence to swap which page a link names.
        assert!(prove(
            "[[old]]\n",
            "[[topics/new]]\n",
            &[rename("wiki/other/old.md", "wiki/other/new.md")]
        )
        .is_err());
        // Identical text carries nothing.
        assert_eq!(prove("[[a]]\n", "[[a]]\n", &renames).unwrap(), vec![]);
    }

    #[test]
    fn follows_rename_chains() {
        let renames = [
            rename("wiki/a.md", "wiki/b.md"),
            rename("wiki/b.md", "wiki/c.md"),
        ];
        assert!(prove("[[a]]\n", "[[c]]\n", &renames).is_ok());
        assert!(prove("[[a]]\n", "[[b]]\n", &renames).is_ok());
        assert!(prove("[[c]]\n", "[[a]]\n", &renames).is_err());
    }

    #[test]
    fn parses_name_status_log() {
        let log = "R100\0wiki/a.md\0wiki/b.md\0\nR087\0wiki/b.md\0wiki/c.md\0M\0wiki/x.md\0";
        assert_eq!(
            parse_renames(log),
            vec![
                rename("wiki/b.md", "wiki/c.md"),
                rename("wiki/a.md", "wiki/b.md")
            ]
        );
    }
}
