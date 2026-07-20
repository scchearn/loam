use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
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
    if args.next().as_deref() != Some("walk") {
        usage();
        return 1;
    }

    let Some(codebase) = args.next() else {
        usage();
        return 1;
    };

    let mut options = Options::default();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--summary" => options.summary = true,
            "--no-gitignore" => options.no_gitignore = true,
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

pub fn pending_count(codebase: &Path, wiki_root: &Path) -> Option<usize> {
    let walk = collect(codebase, &Options::default()).ok()?;
    let index = read_code_index(wiki_root);
    let mut pending = 0;

    for item in walk.items {
        let Some(record) = index.get(&item.path) else {
            pending += 1;
            continue;
        };
        if !is_epoch(&record.ingested_at) || item.mtime <= record.ingested_at.parse().unwrap_or(0) {
            continue;
        }

        let same_size = record
            .source_size
            .as_deref()
            .and_then(|size| size.parse::<u64>().ok())
            .is_some_and(|size| size == item.size);
        let same_hash = same_size
            && !record.content_hash.is_empty()
            && compute_hash(&codebase.join(&item.path)) == record.content_hash;
        if !same_hash {
            pending += 1;
        }
    }
    Some(pending)
}

struct IndexRecord {
    ingested_at: String,
    source_size: Option<String>,
    content_hash: String,
}

fn read_code_index(wiki_root: &Path) -> HashMap<String, IndexRecord> {
    let mut index = HashMap::new();
    for directory in [wiki_root.join("code"), wiki_root.join("entities")] {
        let Ok(entries) = fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if !path.is_file() || path.extension().and_then(|value| value.to_str()) != Some("md") {
                continue;
            }
            let Some((source_path, record)) = parse_index_page(&path) else {
                continue;
            };
            index.entry(source_path).or_insert(record);
        }
    }
    index
}

fn parse_index_page(path: &Path) -> Option<(String, IndexRecord)> {
    let content = fs::read_to_string(path).ok()?;
    let mut in_frontmatter = false;
    let mut source_path = None;
    let mut ingested_at = None;
    let mut source_size = None;
    let mut content_hash = None;
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
            _ => {}
        }
    }
    Some((
        source_path.filter(|value| !value.is_empty())?,
        IndexRecord {
            ingested_at: ingested_at.filter(|value| !value.is_empty())?,
            source_size,
            content_hash: content_hash.unwrap_or_default(),
        },
    ))
}

fn is_epoch(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
}

fn compute_hash(path: &Path) -> String {
    for command in ["sha256sum", "shasum"] {
        let mut process = Command::new(command);
        if command == "shasum" {
            process.args(["-a", "256"]);
        }
        let Ok(output) = process.arg(path).output() else {
            continue;
        };
        if !output.status.success() {
            continue;
        }
        if let Some(hash) = String::from_utf8_lossy(&output.stdout)
            .split_whitespace()
            .next()
        {
            return hash.to_ascii_lowercase();
        }
    }
    String::new()
}

fn usage() {
    eprintln!(
        "Usage: loam codegraph walk <codebase-root> [--exclusions <file>] [--summary] [--no-gitignore]"
    );
}

#[derive(Default)]
struct Options {
    summary: bool,
    no_gitignore: bool,
    exclusions: Option<PathBuf>,
}

struct Exclusions {
    patterns: Vec<String>,
    extensions: HashSet<String>,
}

struct WalkItem {
    path: String,
    mtime: u64,
    size: u64,
}

struct Candidate {
    path: PathBuf,
    relative: String,
    extension: String,
    mtime: u64,
    size: u64,
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
    let gitignored = (!options.no_gitignore)
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
        process_candidates(candidates, !options.summary),
    );
    if !options.summary {
        result
            .items
            .sort_by(|left, right| left.path.cmp(&right.path));
    }
    Ok(result)
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

fn process_candidates(candidates: Vec<Candidate>, emit_items: bool) -> WalkResult {
    if candidates.len() < 2 {
        return process_candidate_chunk(&candidates, emit_items);
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
            .map(|chunk| scope.spawn(move || process_candidate_chunk(chunk, emit_items)))
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

fn process_candidate_chunk(candidates: &[Candidate], emit_items: bool) -> WalkResult {
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
            result.items.push(WalkItem {
                path: candidate.relative.clone(),
                mtime: candidate.mtime,
                size: candidate.size,
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
            "{{\"path\":\"{}\",\"mtime\":\"{}\",\"size\":\"{}\"}}",
            json_escape(&item.path),
            item.mtime,
            item.size
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
