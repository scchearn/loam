use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::UNIX_EPOCH;

const MAX_BYTES: u64 = 500 * 1024;
const DEFAULT_EXTENSIONS: &[&str] = &[
    "ts", "tsx", "js", "jsx", "mjs", "cjs", "py", "java", "go", "rb", "rs", "c", "cpp", "cc", "h",
    "hpp", "hh", "cs", "php", "swift", "kt", "kts", "scala", "sql", "graphql", "gql", "proto",
    "sh", "svelte", "vue", "astro", "mdx", "razor", "liquid", "njk",
];
const DEFAULT_PATTERNS: &[&str] = &[
    "**/dist/**",
    "**/build/**",
    "**/out/**",
    "**/target/**",
    "**/bin/**",
    "**/obj/**",
    "**/__pycache__/**",
    "**/.next/**",
    "**/.nuxt/**",
    "**/.cache/**",
    "**/node_modules/**",
    "**/vendor/**",
    "**/.venv/**",
    "**/venv/**",
    "**/Pods/**",
    "**/.gradle/**",
    "package-lock.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "Gemfile.lock",
    "go.sum",
    "Cargo.lock",
    "poetry.lock",
    "uv.lock",
    "bun.lockb",
    ".git/**",
    ".github/**",
    ".gitignore",
    ".env*",
    ".eslintrc*",
    ".prettierrc*",
    "tsconfig.json",
    "jsconfig.json",
    "*.config.js",
    "*.config.ts",
    "*.config.mjs",
    "*.config.cjs",
    "webpack.config.*",
    "vite.config.*",
    "rollup.config.*",
    "babel.config.*",
    "jest.config.*",
    "vitest.config.*",
    "Makefile",
    "CMakeLists.txt",
    "Dockerfile",
    "docker-compose*",
    ".DS_Store",
    ".vscode/**",
    ".idea/**",
    "*.swp",
    "*.swo",
    "*~",
    "*.min.js",
    "*.min.css",
    "*.generated.*",
    "*.gen.*",
    "wiki/**",
    ".wiki-metadata.json",
    ".claude-plugin/**",
    ".opencode/**",
    ".claude/**",
];

pub fn run(mut args: impl Iterator<Item = String>) -> i32 {
    match args.next().as_deref() {
        Some("walk") => run_walk(args),
        Some("index") => run_index(args),
        Some("diff") => run_diff(args),
        _ => {
            usage();
            1
        }
    }
}

fn run_walk(mut args: impl Iterator<Item = String>) -> i32 {
    let Some(codebase) = args.next() else {
        usage();
        return 1;
    };

    let mut options = Options::default();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--summary" => options.summary = true,
            "--no-gitignore" => options.no_gitignore = true,
            "--ref" => {
                let Some(value) = args.next() else {
                    eprintln!("Error: --ref requires a commit");
                    return 1;
                };
                options.source_ref = Some(value);
            }
            "--generator-version" => {
                let Some(value) = args.next() else {
                    eprintln!("Error: --generator-version requires a value");
                    return 1;
                };
                options.generator_version = value;
            }
            "--exclusions" => {
                let Some(path) = args.next() else {
                    eprintln!("Error: --exclusions requires a file");
                    return 1;
                };
                options.exclusions = Some(PathBuf::from(path));
            }
            _ => {
                eprintln!("Error: unknown flag: {arg}");
                return 1;
            }
        }
    }

    let codebase = Path::new(&codebase);
    if !codebase.is_dir() {
        eprintln!("Error: codebase root not found: {}", codebase.display());
        return 2;
    }

    match collect(codebase, &options) {
        Ok(result) => {
            if options.summary {
                println!("{}", summary_json(&result));
            } else {
                println!("{}", walk_json(&result.items));
            }
            0
        }
        Err((code, message)) => {
            eprintln!("Error: {message}");
            code
        }
    }
}

fn run_index(mut args: impl Iterator<Item = String>) -> i32 {
    let Some(wiki_root) = args.next() else {
        usage();
        return 1;
    };
    let mut codebase_root = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--codebase-root" => {
                let Some(path) = args.next() else {
                    eprintln!("Error: --codebase-root requires a directory");
                    return 1;
                };
                codebase_root = Some(PathBuf::from(path));
            }
            _ => {
                eprintln!("Error: unknown flag: {arg}");
                return 1;
            }
        }
    }

    let wiki_root = Path::new(&wiki_root);
    if let Err(code) = validate_wiki_root(wiki_root) {
        return code;
    }
    println!(
        "{}",
        index_json(&index_records(wiki_root, codebase_root.as_deref()))
    );
    0
}

fn run_diff(mut args: impl Iterator<Item = String>) -> i32 {
    let Some(codebase_root) = args.next() else {
        usage();
        return 1;
    };
    // The wiki root is optional: it is almost always <codebase-root>/wiki, and
    // requiring it again is friction. A leading flag means it was omitted.
    let mut next = args.next();
    let explicit_wiki_root = match next.as_deref() {
        Some(value) if !value.starts_with('-') => next.take(),
        _ => None,
    };

    let mut options = Options::default();
    let mut pending = next;
    while let Some(arg) = pending.take().or_else(|| args.next()) {
        match arg.as_str() {
            "--no-gitignore" => options.no_gitignore = true,
            "--strict" => {}
            "--ref" => {
                let Some(value) = args.next() else {
                    eprintln!("Error: --ref requires a commit");
                    return 1;
                };
                options.source_ref = Some(value);
            }
            "--generator-version" => {
                let Some(value) = args.next() else {
                    eprintln!("Error: --generator-version requires a value");
                    return 1;
                };
                options.generator_version = value;
            }
            "--exclusions" => {
                let Some(path) = args.next() else {
                    eprintln!("Error: --exclusions requires a file");
                    return 1;
                };
                options.exclusions = Some(PathBuf::from(path));
            }
            _ => {
                eprintln!("Error: unknown flag: {arg}");
                return 1;
            }
        }
    }

    let codebase_root = PathBuf::from(codebase_root);
    if !codebase_root.is_dir() {
        eprintln!(
            "Error: codebase root not found: {}",
            codebase_root.display()
        );
        return 2;
    }

    // An empty index is indistinguishable from "nothing is stale", so a wiki
    // root that cannot be resolved must fail loudly either way.
    let wiki_root = match explicit_wiki_root {
        Some(value) => {
            let path = PathBuf::from(value);
            if let Err(code) = validate_wiki_root(&path) {
                return code;
            }
            path
        }
        None => match crate::state::resolve_wiki_root(&codebase_root) {
            Some(path) => path,
            None => {
                eprintln!(
                    "Error: no wiki root found under {}; pass it explicitly: loam codegraph diff <codebase-root> <wiki-root>",
                    codebase_root.display()
                );
                return 2;
            }
        },
    };

    let walk = match collect(&codebase_root, &options) {
        Ok(walk) => walk,
        Err((code, message)) => {
            eprintln!("Error: {message}");
            return code;
        }
    };
    let index = index_records(&wiki_root, Some(&codebase_root));
    let by_source: HashMap<&str, &IndexEntry> = index
        .iter()
        .map(|entry| (entry.source_path.as_str(), entry))
        .collect();
    let by_content: HashMap<&str, &IndexEntry> = index
        .iter()
        .filter(|entry| !entry.content_id.is_empty())
        .map(|entry| (entry.content_id.as_str(), entry))
        .collect();

    let mut entries = Vec::new();
    for item in &walk.items {
        let Some(record) = by_source.get(item.path.as_str()) else {
            let reuse = by_content.get(item.content_id.as_str()).copied();
            entries.push(diff_record_json(item, "new", None, reuse));
            continue;
        };
        if !is_stale(item, record, &options.generator_version) {
            continue;
        }
        entries.push(diff_record_json(item, "stale", Some(&record.slug), None));
    }
    println!("[{}]", entries.join(","));
    0
}

fn diff_record_json(
    item: &WalkItem,
    reason: &str,
    slug: Option<&str>,
    reuse: Option<&IndexEntry>,
) -> String {
    let mut record = format!(
        "{{\"path\":\"{}\",\"mtime\":\"{}\",\"reason\":\"{}\"",
        json_escape(&item.path),
        item.mtime,
        reason
    );
    if let Some(slug) = slug {
        record.push_str(&format!(",\"slug\":\"{}\"", json_escape(slug)));
    }
    record.push_str(&format!(
        ",\"content_id\":\"{}\",\"blob_oid\":\"{}\",\"source_commit\":\"{}\",\"source_state\":\"{}\",\"generator_version\":\"{}\"",
        json_escape(&item.content_id),
        json_escape(&item.blob_oid),
        json_escape(&item.source_commit),
        json_escape(&item.source_state),
        json_escape(&item.generator_version)
    ));
    if let Some(reuse) = reuse {
        record.push_str(&format!(
            ",\"reuse_slug\":\"{}\",\"reuse_source_path\":\"{}\"",
            json_escape(&reuse.slug),
            json_escape(&reuse.source_path)
        ));
    }
    record.push('}');
    record
}

fn is_stale(item: &WalkItem, record: &IndexEntry, generator_version: &str) -> bool {
    record.content_id.is_empty()
        || item.content_id != record.content_id
        || (!generator_version.is_empty() && generator_version != record.generator_version)
}

/// 0 when the root holds the wiki contract, otherwise exit code 2 with the
/// `did you mean .../wiki` hint that loam-common.sh used to emit.
fn validate_wiki_root(wiki_root: &Path) -> Result<(), i32> {
    const CONTRACT: [&str; 3] = ["SCHEMA.md", "index.md", "log.md"];
    if !wiki_root.is_dir() {
        eprintln!("Error: wiki root not found: {}", wiki_root.display());
        return Err(2);
    }
    if CONTRACT.iter().any(|name| wiki_root.join(name).is_file()) {
        return Ok(());
    }
    if CONTRACT
        .iter()
        .any(|name| wiki_root.join("wiki").join(name).is_file())
    {
        eprintln!(
            "Error: wiki root contract not found: {}; did you mean: {}/wiki",
            wiki_root.display(),
            wiki_root.display()
        );
        return Err(2);
    }
    eprintln!(
        "Error: wiki root contract not found: {}",
        wiki_root.display()
    );
    Err(2)
}

struct IndexEntry {
    source_path: String,
    slug: String,
    ingested_at: String,
    source_size: Option<String>,
    content_hash: String,
    content_id: String,
    blob_oid: String,
    source_commit: String,
    source_state: String,
    generator_version: String,
    mtime: Option<u64>,
}

fn index_records(wiki_root: &Path, codebase_root: Option<&Path>) -> Vec<IndexEntry> {
    let mut seen = HashSet::new();
    let mut entries = Vec::new();
    for directory in [wiki_root.join("code"), wiki_root.join("entities")] {
        let Ok(read_dir) = fs::read_dir(&directory) else {
            continue;
        };
        let mut pages: Vec<PathBuf> = read_dir
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.is_file() && path.extension().and_then(|value| value.to_str()) == Some("md")
            })
            .collect();
        pages.sort();
        for page in pages {
            let Some((source_path, record)) = parse_index_page(&page) else {
                continue;
            };
            if !seen.insert(source_path.clone()) {
                continue;
            }
            let resolved = resolve_source(&source_path, codebase_root);
            let mtime = fs::metadata(&resolved).ok().and_then(|metadata| {
                metadata
                    .modified()
                    .ok()?
                    .duration_since(UNIX_EPOCH)
                    .ok()
                    .map(|duration| duration.as_secs())
            });
            entries.push(IndexEntry {
                source_path,
                slug: page
                    .file_stem()
                    .and_then(|value| value.to_str())
                    .unwrap_or_default()
                    .to_owned(),
                ingested_at: record.ingested_at,
                source_size: record.source_size,
                content_hash: record.content_hash,
                content_id: record.content_id,
                blob_oid: record.blob_oid,
                source_commit: record.source_commit,
                source_state: record.source_state,
                generator_version: record.generator_version,
                mtime,
            });
        }
    }
    entries
}

fn resolve_source(source_path: &str, codebase_root: Option<&Path>) -> PathBuf {
    match codebase_root {
        Some(root) if !Path::new(source_path).is_absolute() => root.join(source_path),
        _ => PathBuf::from(source_path),
    }
}

fn index_json(entries: &[IndexEntry]) -> String {
    let records = entries
        .iter()
        .map(|entry| {
            format!(
                "{{\"source_path\":\"{}\",\"slug\":\"{}\",\"ingested_at\":\"{}\",\"source_size\":\"{}\",\"content_hash\":\"{}\",\"content_id\":\"{}\",\"blob_oid\":\"{}\",\"source_commit\":\"{}\",\"source_state\":\"{}\",\"generator_version\":\"{}\",\"mtime\":\"{}\",\"exists\":{}}}",
                json_escape(&entry.source_path),
                json_escape(&entry.slug),
                json_escape(&entry.ingested_at),
                json_escape(entry.source_size.as_deref().unwrap_or_default()),
                json_escape(&entry.content_hash),
                json_escape(&entry.content_id),
                json_escape(&entry.blob_oid),
                json_escape(&entry.source_commit),
                json_escape(&entry.source_state),
                json_escape(&entry.generator_version),
                entry.mtime.map(|value| value.to_string()).unwrap_or_default(),
                entry.mtime.is_some()
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!("[{records}]")
}

pub fn pending_count(codebase: &Path, wiki_root: &Path) -> Option<usize> {
    pending_count_with(codebase, wiki_root, Options::default())
}

pub(crate) fn pending_count_at_ref(
    codebase: &Path,
    wiki_root: &Path,
    source_ref: &str,
) -> Option<usize> {
    pending_count_with(
        codebase,
        wiki_root,
        Options {
            source_ref: Some(source_ref.to_owned()),
            ..Options::default()
        },
    )
}

fn pending_count_with(codebase: &Path, wiki_root: &Path, options: Options) -> Option<usize> {
    let walk = collect(codebase, &options).ok()?;
    let index = index_records(wiki_root, Some(codebase));
    let by_source: HashMap<&str, &IndexEntry> = index
        .iter()
        .map(|entry| (entry.source_path.as_str(), entry))
        .collect();

    Some(
        walk.items
            .iter()
            .filter(|item| match by_source.get(item.path.as_str()) {
                Some(record) => is_stale(item, record, ""),
                None => true,
            })
            .count(),
    )
}

struct IndexRecord {
    ingested_at: String,
    source_size: Option<String>,
    content_hash: String,
    content_id: String,
    blob_oid: String,
    source_commit: String,
    source_state: String,
    generator_version: String,
}

fn parse_index_page(path: &Path) -> Option<(String, IndexRecord)> {
    let content = fs::read_to_string(path).ok()?;
    let mut in_frontmatter = false;
    let mut source_path = None;
    let mut ingested_at = None;
    let mut source_size = None;
    let mut content_hash = None;
    let mut content_id = None;
    let mut blob_oid = None;
    let mut source_commit = None;
    let mut source_state = None;
    let mut generator_version = None;
    for line in content.lines() {
        if line == "---" {
            if in_frontmatter {
                break;
            }
            in_frontmatter = true;
            continue;
        }
        if !in_frontmatter {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim().replace('"', "");
        match key.trim() {
            "source_path" => source_path = Some(value),
            "ingested_at" => ingested_at = Some(value),
            "source_size" => source_size = Some(value),
            "content_hash" => content_hash = Some(value.to_ascii_lowercase()),
            "content_id" => content_id = Some(value),
            "blob_oid" => blob_oid = Some(value.to_ascii_lowercase()),
            "source_commit" => source_commit = Some(value.to_ascii_lowercase()),
            "source_state" => source_state = Some(value),
            "generator_version" => generator_version = Some(value),
            _ => {}
        }
    }
    Some((
        source_path.filter(|value| !value.is_empty())?,
        IndexRecord {
            ingested_at: ingested_at.filter(|value| !value.is_empty())?,
            source_size,
            content_hash: content_hash.unwrap_or_default(),
            content_id: content_id.unwrap_or_default(),
            blob_oid: blob_oid.unwrap_or_default(),
            source_commit: source_commit.unwrap_or_default(),
            source_state: source_state.unwrap_or_default(),
            generator_version: generator_version.unwrap_or_default(),
        },
    ))
}

fn usage() {
    eprintln!("Usage:");
    eprintln!("  loam codegraph index <wiki-root> [--codebase-root <codebase-root>]");
    eprintln!(
        "  loam codegraph walk  <codebase-root> [--exclusions <file>] [--summary] [--no-gitignore] [--ref <commit>] [--generator-version <opaque>]"
    );
    eprintln!(
        "  loam codegraph diff  <codebase-root> [<wiki-root>] [--exclusions <file>] [--no-gitignore] [--strict] [--ref <commit>] [--generator-version <opaque>]"
    );
}

#[derive(Default)]
struct Options {
    summary: bool,
    no_gitignore: bool,
    exclusions: Option<PathBuf>,
    source_ref: Option<String>,
    generator_version: String,
}

struct Exclusions {
    patterns: Vec<String>,
    extensions: HashSet<String>,
}

struct WalkItem {
    path: String,
    filesystem_path: Option<PathBuf>,
    mtime: u64,
    size: u64,
    content_id: String,
    blob_oid: String,
    source_commit: String,
    source_state: String,
    generator_version: String,
}

struct Candidate {
    path: PathBuf,
    relative: String,
    extension: String,
    mtime: u64,
    size: u64,
}

struct GitRepo {
    root: PathBuf,
    prefix: String,
    object_format: String,
    head_commit: Option<String>,
}

impl GitRepo {
    fn discover(codebase: &Path) -> Option<Self> {
        let output = Command::new("git")
            .args([
                "-C",
                codebase.to_str()?,
                "rev-parse",
                "--show-toplevel",
                "--show-object-format",
                "--show-prefix",
            ])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let text = String::from_utf8(output.stdout).ok()?;
        let mut lines = text.trim_end_matches('\n').split('\n');
        let root = PathBuf::from(lines.next()?);
        let object_format = lines.next()?.trim().to_owned();
        if !matches!(object_format.as_str(), "sha1" | "sha256") {
            return None;
        }
        let prefix = lines
            .next()
            .unwrap_or_default()
            .trim_end_matches('/')
            .to_owned();
        let head_commit = git_text(
            &root,
            &["rev-parse", "--verify", "--end-of-options", "HEAD^{commit}"],
        );
        Some(Self {
            root,
            prefix,
            object_format,
            head_commit,
        })
    }

    fn repo_path(&self, relative: &str) -> String {
        if self.prefix.is_empty() {
            relative.to_owned()
        } else {
            format!("{}/{relative}", self.prefix)
        }
    }

    fn local_path(&self, repo_path: &str) -> Option<String> {
        if self.prefix.is_empty() {
            return Some(repo_path.to_owned());
        }
        repo_path
            .strip_prefix(&self.prefix)
            .and_then(|path| path.strip_prefix('/'))
            .map(str::to_owned)
    }

    fn resolve_commit(&self, source_ref: &str) -> Option<String> {
        git_text(
            &self.root,
            &[
                "rev-parse",
                "--verify",
                "--end-of-options",
                &format!("{source_ref}^{{commit}}"),
            ],
        )
    }

    fn commit_timestamp(&self, commit: &str) -> Option<u64> {
        git_text(&self.root, &["show", "-s", "--format=%ct", commit])?
            .parse()
            .ok()
    }

    fn tree_blobs(&self, commit: &str) -> Option<Vec<(String, String)>> {
        let mut command = Command::new("git");
        command
            .current_dir(&self.root)
            .args(["ls-tree", "-r", "-z", "--full-tree", commit, "--"]);
        if !self.prefix.is_empty() {
            command.arg(&self.prefix);
        }
        let output = command.output().ok()?;
        if !output.status.success() {
            return None;
        }
        let mut blobs = Vec::new();
        for record in output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|record| !record.is_empty())
        {
            let (metadata, path) = record.split_once_byte(b'\t')?;
            let metadata = String::from_utf8_lossy(metadata);
            let mut fields = metadata.split_whitespace();
            let mode = fields.next()?;
            let kind = fields.next()?;
            let oid = fields.next()?;
            if !mode.starts_with("100") || kind != "blob" {
                continue;
            }
            let repo_path = String::from_utf8_lossy(path).replace('\\', "/");
            let Some(local_path) = self.local_path(&repo_path) else {
                continue;
            };
            blobs.push((local_path, oid.to_owned()));
        }
        blobs.sort_by(|left, right| left.0.cmp(&right.0));
        Some(blobs)
    }

    fn hash_paths(&self, paths: &[String]) -> Vec<Option<String>> {
        let mut hashes = vec![None; paths.len()];
        let normal = paths
            .iter()
            .enumerate()
            .filter(|(_, path)| !path.contains('\n') && !path.contains('\r'))
            .collect::<Vec<_>>();
        if !normal.is_empty() {
            let mut child = match Command::new("git")
                .current_dir(&self.root)
                .args(["hash-object", "--stdin-paths"])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
            {
                Ok(child) => child,
                Err(_) => return hashes,
            };
            if let Some(mut stdin) = child.stdin.take() {
                for (_, path) in &normal {
                    let _ = writeln!(stdin, "{path}");
                }
            }
            if let Ok(output) = child.wait_with_output() {
                if output.status.success() {
                    let values = String::from_utf8_lossy(&output.stdout)
                        .lines()
                        .map(str::to_owned)
                        .collect::<Vec<_>>();
                    if values.len() == normal.len() {
                        for ((index, _), value) in normal.iter().zip(values) {
                            hashes[*index] = Some(value);
                        }
                    }
                }
            }
        }
        for (index, path) in paths
            .iter()
            .enumerate()
            .filter(|(_, path)| path.contains('\n') || path.contains('\r'))
        {
            hashes[index] = git_text(
                &self.root,
                &["hash-object", &format!("--path={path}"), "--", path],
            );
        }
        hashes
    }

    fn cat_files(&self, oids: &[String]) -> Option<Vec<Vec<u8>>> {
        let mut child = Command::new("git")
            .current_dir(&self.root)
            .args(["cat-file", "--batch"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        {
            let mut stdin = child.stdin.take()?;
            for oid in oids {
                writeln!(stdin, "{oid}").ok()?;
            }
        }
        let output = child.wait_with_output().ok()?;
        if !output.status.success() {
            return None;
        }
        let mut cursor = 0usize;
        let mut contents = Vec::with_capacity(oids.len());
        for _ in oids {
            let header_end = output.stdout[cursor..]
                .iter()
                .position(|byte| *byte == b'\n')?
                + cursor;
            let header = String::from_utf8_lossy(&output.stdout[cursor..header_end]);
            let size = header.split_whitespace().last()?.parse::<usize>().ok()?;
            let start = header_end + 1;
            let end = start.checked_add(size)?;
            if end >= output.stdout.len() || output.stdout[end] != b'\n' {
                return None;
            }
            contents.push(output.stdout[start..end].to_vec());
            cursor = end + 1;
        }
        Some(contents)
    }
}

trait SplitOnceByte {
    fn split_once_byte(&self, separator: u8) -> Option<(&[u8], &[u8])>;
}

impl SplitOnceByte for [u8] {
    fn split_once_byte(&self, separator: u8) -> Option<(&[u8], &[u8])> {
        let index = self.iter().position(|byte| *byte == separator)?;
        Some((&self[..index], &self[index + 1..]))
    }
}

fn git_output(root: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let output = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .ok()?;
    output.status.success().then_some(output.stdout)
}

fn git_text(root: &Path, args: &[&str]) -> Option<String> {
    String::from_utf8(git_output(root, args)?)
        .ok()
        .map(|value| value.trim().to_owned())
}

#[derive(Default)]
struct WalkResult {
    total: usize,
    items: Vec<WalkItem>,
    by_ext: BTreeMap<String, usize>,
    pattern: usize,
    gitignore: usize,
    empty: usize,
    large: usize,
    generated_header: usize,
    binary: usize,
}

fn collect(codebase: &Path, options: &Options) -> Result<WalkResult, (i32, String)> {
    if let Some(source_ref) = options.source_ref.as_deref() {
        return collect_ref(codebase, options, source_ref);
    }
    collect_worktree(codebase, options)
}

fn collect_worktree(codebase: &Path, options: &Options) -> Result<WalkResult, (i32, String)> {
    let exclusions = match &options.exclusions {
        Some(path) => parse_exclusions_file(path).map_err(|message| (3, message))?,
        None => Exclusions {
            patterns: DEFAULT_PATTERNS
                .iter()
                .map(|value| (*value).to_owned())
                .collect(),
            extensions: DEFAULT_EXTENSIONS
                .iter()
                .map(|value| (*value).to_owned())
                .collect(),
        },
    };
    let git = GitRepo::discover(codebase);
    let gitignored = (!options.no_gitignore && git.is_some())
        .then(|| gitignored_paths(codebase, &exclusions.extensions))
        .flatten();
    let mut result = WalkResult {
        gitignore: gitignored.as_ref().map_or(0, HashSet::len),
        ..WalkResult::default()
    };
    let mut candidates = Vec::new();
    collect_candidates(
        codebase,
        codebase,
        &exclusions,
        gitignored.as_ref(),
        &mut candidates,
        &mut result.pattern,
        &mut result.large,
    )?;
    merge_results(
        &mut result,
        process_candidates(
            candidates,
            !options.summary,
            git.is_none(),
            &options.generator_version,
        ),
    );
    if let Some(git) = &git {
        apply_git_identities(&mut result.items, git);
    }
    if !options.summary {
        result
            .items
            .sort_by(|left, right| left.path.cmp(&right.path));
    }
    Ok(result)
}

fn collect_ref(
    codebase: &Path,
    options: &Options,
    source_ref: &str,
) -> Result<WalkResult, (i32, String)> {
    let git = GitRepo::discover(codebase).ok_or_else(|| {
        (
            2,
            format!(
                "--ref requires a usable Git repository: {}",
                codebase.display()
            ),
        )
    })?;
    let commit = git
        .resolve_commit(source_ref)
        .ok_or_else(|| (2, format!("cannot resolve Git ref: {source_ref}")))?;
    let timestamp = git.commit_timestamp(&commit).unwrap_or_default();
    let exclusions = match &options.exclusions {
        Some(path) => parse_exclusions_file(path).map_err(|message| (3, message))?,
        None => Exclusions {
            patterns: DEFAULT_PATTERNS
                .iter()
                .map(|value| (*value).to_owned())
                .collect(),
            extensions: DEFAULT_EXTENSIONS
                .iter()
                .map(|value| (*value).to_owned())
                .collect(),
        },
    };
    let blobs = git
        .tree_blobs(&commit)
        .ok_or_else(|| (2, format!("cannot read Git ref: {source_ref}")))?;
    let mut selected = Vec::new();
    let mut result = WalkResult::default();
    for (path, oid) in blobs {
        let extension = Path::new(&path)
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_owned();
        if !exclusions.extensions.contains(&extension) {
            continue;
        }
        if matches_exclusion(&path, &exclusions.patterns) {
            result.pattern += 1;
            continue;
        }
        selected.push((path, oid, extension));
    }
    let oids = selected
        .iter()
        .map(|(_, oid, _)| oid.clone())
        .collect::<Vec<_>>();
    let contents = git
        .cat_files(&oids)
        .ok_or_else(|| (2, format!("cannot read blobs from Git ref: {source_ref}")))?;
    for ((path, oid, extension), content) in selected.into_iter().zip(contents) {
        if content.len() as u64 > MAX_BYTES {
            result.large += 1;
            continue;
        }
        if content.iter().all(u8::is_ascii_whitespace) {
            result.empty += 1;
            continue;
        }
        if content.contains(&0) {
            result.binary += 1;
            continue;
        }
        if generated_header(&content) {
            result.generated_header += 1;
            continue;
        }
        result.total += 1;
        *result.by_ext.entry(extension).or_default() += 1;
        if !options.summary {
            result.items.push(WalkItem {
                path,
                filesystem_path: None,
                mtime: timestamp,
                size: content.len() as u64,
                content_id: format!("git:{}:{oid}", git.object_format),
                blob_oid: oid,
                source_commit: commit.clone(),
                source_state: "committed".to_owned(),
                generator_version: options.generator_version.clone(),
            });
        }
    }
    Ok(result)
}

fn apply_git_identities(items: &mut [WalkItem], git: &GitRepo) {
    let head_blobs: HashMap<String, String> = git
        .head_commit
        .as_deref()
        .and_then(|commit| git.tree_blobs(commit))
        .unwrap_or_default()
        .into_iter()
        .collect();
    let paths = items
        .iter()
        .map(|item| git.repo_path(&item.path))
        .collect::<Vec<_>>();
    for (item, oid) in items.iter_mut().zip(git.hash_paths(&paths)) {
        if let Some(oid) = oid {
            set_git_identity(item, &oid, git, &head_blobs);
        } else {
            let hash = item
                .filesystem_path
                .as_deref()
                .map(crate::sha256::file_hex)
                .unwrap_or_default();
            item.content_id = format!("sha256:{hash}");
            item.source_state = "fallback".to_owned();
        }
    }
}

fn set_git_identity(
    item: &mut WalkItem,
    oid: &str,
    git: &GitRepo,
    head_blobs: &HashMap<String, String>,
) {
    item.content_id = format!("git:{}:{oid}", git.object_format);
    item.blob_oid = oid.to_owned();
    if head_blobs
        .get(&item.path)
        .is_some_and(|head_oid| head_oid == oid)
    {
        item.source_commit = git.head_commit.clone().unwrap_or_default();
        item.source_state = "committed".to_owned();
    } else {
        item.source_state = "provisional".to_owned();
    }
}

fn sha256_hex(content: &[u8]) -> String {
    let mut hasher = crate::sha256::Sha256::default();
    hasher.update(content);
    hasher.finish()
}

fn collect_candidates(
    root: &Path,
    directory: &Path,
    exclusions: &Exclusions,
    gitignored: Option<&HashSet<String>>,
    candidates: &mut Vec<Candidate>,
    pattern_count: &mut usize,
    large_count: &mut usize,
) -> Result<(), (i32, String)> {
    let mut entries: Vec<_> = fs::read_dir(directory)
        .map_err(|error| (2, format!("cannot read {}: {error}", directory.display())))?
        .filter_map(Result::ok)
        .collect();
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        let path = entry.path();
        let relative = path.strip_prefix(root).unwrap_or(&path);
        let relative_string = slash_path(relative);
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(_) => continue,
        };

        if file_type.is_dir() {
            if excluded_directory(&relative_string, &exclusions.patterns) {
                continue;
            }
            collect_candidates(
                root,
                &path,
                exclusions,
                gitignored,
                candidates,
                pattern_count,
                large_count,
            )?;
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        let extension = path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_owned();
        if !exclusions.extensions.contains(&extension) {
            continue;
        }
        if matches_exclusion(&relative_string, &exclusions.patterns) {
            *pattern_count += 1;
            continue;
        }
        if gitignored.is_some_and(|paths| paths.contains(&relative_string)) {
            continue;
        }

        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };
        let size = metadata.len();
        if size > MAX_BYTES {
            *large_count += 1;
            continue;
        }
        let mtime = metadata
            .modified()
            .ok()
            .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |duration| duration.as_secs());

        candidates.push(Candidate {
            path,
            relative: relative_string,
            extension,
            mtime,
            size,
        });
    }
    Ok(())
}

fn process_candidates(
    candidates: Vec<Candidate>,
    emit_items: bool,
    fallback_identity: bool,
    generator_version: &str,
) -> WalkResult {
    if candidates.len() < 2 {
        return process_candidate_chunk(
            &candidates,
            emit_items,
            fallback_identity,
            generator_version,
        );
    }

    // ponytail: cap workers at 8; file checks are I/O-bound and more threads only
    // increase contention on this local scan.
    let available = thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(1);
    let worker_count = available.min(8).min(candidates.len());
    let chunk_size = candidates.len().div_ceil(worker_count);
    let mut result = WalkResult::default();

    thread::scope(|scope| {
        let handles = candidates
            .chunks(chunk_size)
            .map(|chunk| {
                scope.spawn(move || {
                    process_candidate_chunk(chunk, emit_items, fallback_identity, generator_version)
                })
            })
            .collect::<Vec<_>>();
        for handle in handles {
            merge_results(
                &mut result,
                handle.join().expect("codegraph worker should not panic"),
            );
        }
    });
    result
}

fn process_candidate_chunk(
    candidates: &[Candidate],
    emit_items: bool,
    fallback_identity: bool,
    generator_version: &str,
) -> WalkResult {
    let mut result = WalkResult::default();
    for candidate in candidates {
        let content = match fs::read(&candidate.path) {
            Ok(content) => content,
            Err(_) => continue,
        };
        if content.iter().all(u8::is_ascii_whitespace) {
            result.empty += 1;
            continue;
        }
        if content.contains(&0) {
            result.binary += 1;
            continue;
        }
        if generated_header(&content) {
            result.generated_header += 1;
            continue;
        }

        result.total += 1;
        if emit_items {
            let content_id = if fallback_identity {
                format!("sha256:{}", sha256_hex(&content))
            } else {
                String::new()
            };
            result.items.push(WalkItem {
                path: candidate.relative.clone(),
                filesystem_path: Some(candidate.path.clone()),
                mtime: candidate.mtime,
                size: candidate.size,
                content_id,
                blob_oid: String::new(),
                source_commit: String::new(),
                source_state: if fallback_identity {
                    "fallback".to_owned()
                } else {
                    String::new()
                },
                generator_version: generator_version.to_owned(),
            });
        }
        *result
            .by_ext
            .entry(candidate.extension.clone())
            .or_default() += 1;
    }
    result
}

fn merge_results(target: &mut WalkResult, mut source: WalkResult) {
    target.total += source.total;
    target.items.append(&mut source.items);
    target.pattern += source.pattern;
    target.gitignore += source.gitignore;
    target.empty += source.empty;
    target.large += source.large;
    target.generated_header += source.generated_header;
    target.binary += source.binary;
    for (extension, count) in source.by_ext {
        *target.by_ext.entry(extension).or_default() += count;
    }
}

fn parse_exclusions_file(path: &Path) -> Result<Exclusions, String> {
    let content = fs::read_to_string(path)
        .map_err(|error| format!("exclusions file not found: {}: {error}", path.display()))?;
    let mut patterns = Vec::new();
    let mut extensions = HashSet::new();
    let mut section = String::new();
    let mut in_code = false;
    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.starts_with("##") {
            section = line.trim_start_matches('#').trim().to_owned();
            continue;
        }
        if line == "```" {
            in_code = !in_code;
            continue;
        }
        if !in_code || line.is_empty() || line.starts_with('#') {
            continue;
        }
        if section.contains("Include") {
            extensions.extend(line.split_whitespace().filter_map(|value| {
                let value = value.trim_start_matches('.');
                (!value.is_empty()).then(|| value.to_owned())
            }));
        } else {
            patterns.push(line.to_owned());
        }
    }
    Ok(Exclusions {
        patterns,
        extensions,
    })
}

fn gitignored_paths(root: &Path, extensions: &HashSet<String>) -> Option<HashSet<String>> {
    let root = root.to_string_lossy();
    let output = Command::new("git")
        .args([
            "-C",
            root.as_ref(),
            "ls-files",
            "--others",
            "--ignored",
            "--exclude-standard",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let mut paths = HashSet::new();
    for line in output.stdout.split(|byte| *byte == b'\n') {
        let path = String::from_utf8_lossy(line);
        let path = path.trim_end_matches('\r');
        let extension = Path::new(path)
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default();
        if !extensions.contains(extension) {
            continue;
        }
        paths.insert(path.replace('\\', "/"));
    }
    Some(paths)
}

fn excluded_directory(relative: &str, patterns: &[String]) -> bool {
    patterns
        .iter()
        .any(|pattern| pattern.ends_with("/**") && matches_directory_pattern(relative, pattern))
}

fn matches_exclusion(relative: &str, patterns: &[String]) -> bool {
    let basename = relative.rsplit('/').next().unwrap_or(relative);
    patterns.iter().any(|pattern| {
        glob_match(relative, pattern)
            || glob_match(basename, pattern)
            || (pattern.starts_with("**/") && glob_match(relative, &pattern[3..]))
            || (pattern.ends_with("/**") && matches_directory_pattern(relative, pattern))
    })
}

fn matches_directory_pattern(relative: &str, pattern: &str) -> bool {
    let directory = pattern.trim_start_matches("**/").trim_end_matches("/**");
    relative.split('/').any(|part| part == directory)
}

fn glob_match(value: &str, pattern: &str) -> bool {
    let value = value.as_bytes();
    let pattern = pattern.as_bytes();
    let mut states = vec![false; pattern.len() + 1];
    states[0] = true;
    for &character in value {
        let mut next = vec![false; pattern.len() + 1];
        for index in 0..pattern.len() {
            if !states[index] {
                continue;
            }
            if pattern[index] == b'*' {
                next[index] = true;
                next[index + 1] = true;
            } else if pattern[index] == character {
                next[index + 1] = true;
            }
        }
        states = next;
    }
    for index in 0..pattern.len() {
        if states[index] && pattern[index] == b'*' {
            states[index + 1] = true;
        }
    }
    states[pattern.len()]
}

fn generated_header(content: &[u8]) -> bool {
    contains_ascii_case_insensitive(content, b"generated")
        || contains_ascii_case_insensitive(content, b"do not edit")
}

fn contains_ascii_case_insensitive(content: &[u8], marker: &[u8]) -> bool {
    content.windows(marker.len()).any(|window| {
        window
            .iter()
            .zip(marker)
            .all(|(character, expected)| character.to_ascii_lowercase() == *expected)
    })
}

fn slash_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn walk_json(items: &[WalkItem]) -> String {
    let mut output = String::from("[");
    for (index, item) in items.iter().enumerate() {
        if index > 0 {
            output.push(',');
        }
        output.push_str(&format!(
            "{{\"path\":\"{}\",\"mtime\":\"{}\",\"size\":\"{}\",\"content_id\":\"{}\",\"blob_oid\":\"{}\",\"source_commit\":\"{}\",\"source_state\":\"{}\",\"generator_version\":\"{}\"}}",
            json_escape(&item.path),
            item.mtime,
            item.size,
            json_escape(&item.content_id),
            json_escape(&item.blob_oid),
            json_escape(&item.source_commit),
            json_escape(&item.source_state),
            json_escape(&item.generator_version)
        ));
    }
    output.push(']');
    output
}

fn summary_json(result: &WalkResult) -> String {
    let by_ext = result
        .by_ext
        .iter()
        .map(|(extension, count)| format!("\"{}\":{}", json_escape(extension), count))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{{\"total\":{},\"by_ext\":{{{}}},\"excluded\":{{\"pattern\":{},\"gitignore\":{},\"empty\":{},\"large\":{},\"generated_header\":{},\"binary\":{}}}}}",
        result.total,
        by_ext,
        result.pattern,
        result.gitignore,
        result.empty,
        result.large,
        result.generated_header,
        result.binary
    )
}

fn json_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            character if character.is_control() => {
                escaped.push_str(&format!("\\u{:04x}", character as u32));
            }
            character => escaped.push(character),
        }
    }
    escaped
}
