use pulldown_cmark::{BrokenLink, Event, LinkType, Options, Parser, Tag, TagEnd};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};

const ROOTS: [&str; 4] = ["wiki", "goals", "specs", "plans"];
const IGNORED_DIRECTORIES: [&str; 5] = [".agents", ".git", "build", "dist", "target"];

struct WorkspaceIndex {
    documents: Vec<Document>,
    by_path: HashMap<String, usize>,
    by_stem: HashMap<String, Vec<usize>>,
    archive_paths: HashSet<String>,
    archive_stems: HashSet<String>,
}

struct Document {
    root: String,
    stem: String,
    workspace_relative: String,
    source: String,
    lines: LineIndex,
    parsed: ParsedDocument,
}

struct ParsedDocument {
    headings: Vec<Heading>,
    links: Vec<Link>,
    definitions: Vec<Definition>,
    wikilinks: Vec<Wikilink>,
    fragments: HashSet<String>,
}

struct Heading {
    text: String,
    range: Range<usize>,
    raw: String,
    custom_id: Option<String>,
}

struct Link {
    destination: String,
    link_type: LinkType,
    label: String,
    range: Range<usize>,
    image: bool,
}

struct Definition {
    destination: String,
    range: Range<usize>,
}

struct Wikilink {
    target: String,
    heading: Option<String>,
    range: Range<usize>,
}

struct HeadingCapture {
    range: Range<usize>,
    text: String,
    custom_id: Option<String>,
}

#[derive(Clone)]
struct LineIndex {
    starts: Vec<usize>,
}

#[derive(Clone)]
pub struct Diagnostic {
    pub file: String,
    pub line: usize,
    pub column: usize,
    pub end_line: usize,
    pub end_column: usize,
    pub rule: &'static str,
    pub rule_name: &'static str,
    pub description: &'static str,
    pub detail: String,
    pub context: String,
    pub target: Option<String>,
    pub candidates: Vec<String>,
}

#[derive(Clone, Copy)]
struct RuleSpec {
    code: &'static str,
    name: &'static str,
    description: &'static str,
}

const RULE_MD024: RuleSpec = RuleSpec {
    code: "MD024",
    name: "no-duplicate-heading",
    description: "Multiple headings with the same content",
};
const RULE_MD042: RuleSpec = RuleSpec {
    code: "MD042",
    name: "no-empty-links",
    description: "No empty links",
};
const RULE_MD051: RuleSpec = RuleSpec {
    code: "MD051",
    name: "link-fragments",
    description: "Link fragments should be valid",
};
const RULE_MD052: RuleSpec = RuleSpec {
    code: "MD052",
    name: "reference-links-images",
    description: "Reference links and images should use a label that is defined",
};
const RULE_LMD001: RuleSpec = RuleSpec {
    code: "LMD001",
    name: "missing-document",
    description: "Internal document reference does not resolve",
};
const RULE_LMD002: RuleSpec = RuleSpec {
    code: "LMD002",
    name: "ambiguous-document",
    description: "Wikilink resolves to multiple documents",
};
const RULE_LMD003: RuleSpec = RuleSpec {
    code: "LMD003",
    name: "missing-cross-document-anchor",
    description: "Cross-document anchor target does not exist",
};
const RULE_LMD004: RuleSpec = RuleSpec {
    code: "LMD004",
    name: "ambiguous-heading",
    description: "Wikilink heading target is ambiguous",
};

pub fn lint_workspace(workspace: &Path) -> Result<Vec<Diagnostic>, String> {
    let workspace = fs::canonicalize(workspace)
        .map_err(|error| format!("cannot read workspace {}: {error}", workspace.display()))?;
    if !workspace.is_dir() {
        return Err(format!(
            "workspace is not a directory: {}",
            workspace.display()
        ));
    }
    let index = build_index(&workspace)?;
    let mut diagnostics = Vec::new();
    for document in &index.documents {
        lint_document(&index, document, &mut diagnostics);
    }
    Ok(diagnostics)
}

fn build_index(workspace: &Path) -> Result<WorkspaceIndex, String> {
    let mut files = Vec::new();
    let mut archive_paths = HashSet::new();
    let mut archive_stems = HashSet::new();

    for root in ROOTS {
        let root_path = workspace.join(root);
        if !root_path.exists() {
            continue;
        }
        let root_metadata = fs::symlink_metadata(&root_path).map_err(|error| {
            format!("cannot inspect lint root {}: {error}", root_path.display())
        })?;
        if root_metadata.file_type().is_symlink() {
            continue;
        }
        if !root_metadata.is_dir() {
            return Err(format!(
                "lint root is not a directory: {}",
                root_path.display()
            ));
        }
        walk_root(
            workspace,
            root,
            &root_path,
            false,
            &mut files,
            &mut archive_paths,
            &mut archive_stems,
        )?;
    }

    files.sort_by(|left, right| left.0.cmp(&right.0));
    let mut documents = Vec::with_capacity(files.len());
    for (workspace_relative, root, _root_relative, path) in files {
        let source = fs::read_to_string(&path)
            .map_err(|error| format!("cannot read Markdown file {}: {error}", path.display()))?;
        let parsed = parse_document(&source);
        let stem = path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_owned();
        documents.push(Document {
            root,
            stem,
            workspace_relative,
            lines: LineIndex::new(&source),
            source,
            parsed,
        });
    }

    let mut by_path = HashMap::with_capacity(documents.len());
    let mut by_stem: HashMap<String, Vec<usize>> = HashMap::new();
    for (index, document) in documents.iter().enumerate() {
        by_path.insert(document.workspace_relative.clone(), index);
        by_stem
            .entry(identity_key(&document.root, &document.stem))
            .or_default()
            .push(index);
    }

    Ok(WorkspaceIndex {
        documents,
        by_path,
        by_stem,
        archive_paths,
        archive_stems,
    })
}

fn walk_root(
    workspace: &Path,
    root: &str,
    directory: &Path,
    in_archive: bool,
    files: &mut Vec<(String, String, String, PathBuf)>,
    archive_paths: &mut HashSet<String>,
    archive_stems: &mut HashSet<String>,
) -> Result<(), String> {
    let mut entries = fs::read_dir(directory)
        .map_err(|error| {
            format!(
                "cannot read lint directory {}: {error}",
                directory.display()
            )
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| {
            format!(
                "cannot enumerate lint directory {}: {error}",
                directory.display()
            )
        })?;
    entries.sort_by_key(|entry| entry.path());

    for entry in entries {
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let archive = in_archive || (root == "wiki" && name == ".archive");
            if !archive && IGNORED_DIRECTORIES.contains(&name.as_str()) {
                continue;
            }
            walk_root(
                workspace,
                root,
                &path,
                archive,
                files,
                archive_paths,
                archive_stems,
            )?;
            continue;
        }
        if !file_type.is_file() || path.extension().and_then(|value| value.to_str()) != Some("md") {
            continue;
        }

        let workspace_relative = slash_path(
            path.strip_prefix(workspace)
                .map_err(|_| format!("path escaped workspace: {}", path.display()))?,
        );
        let root_relative = slash_path(
            path.strip_prefix(workspace.join(root))
                .map_err(|_| format!("path escaped lint root: {}", path.display()))?,
        );
        if in_archive {
            archive_paths.insert(workspace_relative);
            if let Some(stem) = path.file_stem().and_then(|value| value.to_str()) {
                archive_stems.insert(identity_key(root, stem));
            }
        } else {
            files.push((workspace_relative, root.to_owned(), root_relative, path));
        }
    }
    Ok(())
}

fn parse_document(source: &str) -> ParsedDocument {
    let options = Options::ENABLE_GFM
        | Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_HEADING_ATTRIBUTES
        | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS;
    let mut parser = Parser::new_with_broken_link_callback(
        source,
        options,
        Some(|_: BrokenLink<'_>| Some(("".into(), "".into()))),
    )
    .into_offset_iter();
    let definitions = parser
        .reference_definitions()
        .iter()
        .map(|(_, definition)| Definition {
            destination: definition.dest.to_string(),
            range: definition.span.clone(),
        })
        .collect::<Vec<_>>();

    let mut headings = Vec::new();
    let mut links = Vec::new();
    let mut wikilinks = Vec::new();
    let mut heading = None;
    let mut code_depth = 0usize;
    let mut metadata_depth = 0usize;
    let mut image_depth = 0usize;
    let mut html_anchors = Vec::new();

    for (event, range) in &mut parser {
        match event {
            Event::Start(Tag::Heading { id, .. }) => {
                heading = Some(HeadingCapture {
                    range,
                    text: String::new(),
                    custom_id: id.map(|value| value.to_string()),
                });
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some(capture) = heading.take() {
                    let raw = source
                        .get(capture.range.clone())
                        .unwrap_or_default()
                        .to_owned();
                    let end = source[capture.range.start..]
                        .find('\n')
                        .map_or(source.len(), |offset| capture.range.start + offset);
                    headings.push(Heading {
                        text: normalize_heading_text(&capture.text),
                        range: capture.range.start..end,
                        raw,
                        custom_id: capture.custom_id,
                    });
                }
            }
            Event::Start(Tag::CodeBlock(_)) => code_depth += 1,
            Event::End(TagEnd::CodeBlock) => code_depth = code_depth.saturating_sub(1),
            Event::Start(Tag::MetadataBlock(_)) => metadata_depth += 1,
            Event::End(TagEnd::MetadataBlock(_)) => {
                metadata_depth = metadata_depth.saturating_sub(1)
            }
            Event::Start(Tag::Image {
                link_type,
                dest_url,
                id,
                ..
            }) => {
                image_depth += 1;
                links.push(Link {
                    destination: dest_url.to_string(),
                    link_type,
                    label: id.to_string(),
                    range,
                    image: true,
                });
            }
            Event::End(TagEnd::Image) => image_depth = image_depth.saturating_sub(1),
            Event::Start(Tag::Link {
                link_type,
                dest_url,
                id,
                ..
            }) => {
                if is_wikilink_range(source, &range) {
                    scan_wikilinks(source, &range, &mut wikilinks);
                } else {
                    links.push(Link {
                        destination: dest_url.to_string(),
                        link_type,
                        label: id.to_string(),
                        range,
                        image: false,
                    });
                }
            }
            Event::InlineHtml(value) | Event::Html(value) => {
                scan_html_anchors(value.as_ref(), range.start, &mut html_anchors);
            }
            Event::Text(value) => {
                if let Some(capture) = heading.as_mut() {
                    if code_depth == 0 && metadata_depth == 0 && image_depth == 0 {
                        capture.text.push_str(value.as_ref());
                    }
                }
                if code_depth == 0 && metadata_depth == 0 && image_depth == 0 {
                    scan_wikilinks(source, &range, &mut wikilinks);
                }
            }
            Event::Code(value) => {
                if let Some(capture) = heading.as_mut() {
                    if metadata_depth == 0 && image_depth == 0 {
                        capture.text.push_str(value.as_ref());
                    }
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                if let Some(capture) = heading.as_mut() {
                    capture.text.push(' ');
                }
            }
            _ => {}
        }
    }

    let mut fragments = HashSet::from(["#top".to_owned()]);
    let mut fragment_counts = HashMap::<String, usize>::new();
    for heading in &headings {
        let base = github_fragment(&heading.text);
        if base != "#" {
            let count = fragment_counts.entry(base.clone()).or_default();
            let fragment = if *count == 0 {
                base.clone()
            } else {
                format!("{base}-{count}")
            };
            *count += 1;
            fragments.insert(fragment);
        }
        if let Some(custom_id) = heading.custom_id.as_deref() {
            fragments.insert(hash_fragment(custom_id));
        }
        scan_html_anchors(&heading.raw, heading.range.start, &mut html_anchors);
    }
    for anchor in html_anchors {
        fragments.insert(anchor);
    }

    ParsedDocument {
        headings,
        links,
        definitions,
        wikilinks,
        fragments,
    }
}

fn lint_document(index: &WorkspaceIndex, document: &Document, diagnostics: &mut Vec<Diagnostic>) {
    let mut seen_headings = HashSet::new();
    for heading in &document.parsed.headings {
        if !seen_headings.insert(heading.text.clone()) {
            diagnostics.push(diagnostic(
                document,
                heading.range.clone(),
                RULE_MD024,
                format!("Duplicate heading: \"{}\"", heading.text),
                Some(heading.text.clone()),
                Vec::new(),
            ));
        }
    }

    for link in &document.parsed.links {
        if matches!(
            link.link_type,
            LinkType::ReferenceUnknown | LinkType::CollapsedUnknown | LinkType::ShortcutUnknown
        ) {
            if matches!(
                link.link_type,
                LinkType::ReferenceUnknown | LinkType::CollapsedUnknown
            ) && !link.label.trim().eq_ignore_ascii_case("x")
            {
                diagnostics.push(diagnostic(
                    document,
                    link.range.clone(),
                    RULE_MD052,
                    format!(
                        "Missing link or image reference definition: \"{}\"",
                        link.label
                    ),
                    Some(link.label.clone()),
                    Vec::new(),
                ));
            }
            continue;
        }
        if !link.image && (link.destination.is_empty() || link.destination == "#") {
            diagnostics.push(diagnostic(
                document,
                link.range.clone(),
                RULE_MD042,
                "Link destination is empty".to_owned(),
                Some(link.destination.clone()),
                Vec::new(),
            ));
            continue;
        }
        if !link.image {
            lint_standard_destination(
                index,
                document,
                &link.destination,
                link.range.clone(),
                diagnostics,
            );
        }
    }

    for definition in &document.parsed.definitions {
        lint_definition_fragment(index, document, definition, diagnostics);
    }

    for wikilink in &document.parsed.wikilinks {
        lint_wikilink(index, document, wikilink, diagnostics);
    }
}

fn lint_definition_fragment(
    index: &WorkspaceIndex,
    document: &Document,
    definition: &Definition,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let (_, fragment) = split_destination(&definition.destination);
    let Some(fragment) = fragment else {
        return;
    };
    if fragment.is_empty() || is_external_uri(&definition.destination) {
        return;
    }
    lint_fragment_destination(
        index,
        document,
        &definition.destination,
        definition.range.clone(),
        diagnostics,
    );
}

fn lint_standard_destination(
    index: &WorkspaceIndex,
    document: &Document,
    destination: &str,
    range: Range<usize>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if is_external_uri(destination) {
        return;
    }
    let (path, fragment) = split_destination(destination);
    if path.is_empty() {
        if fragment.is_some() {
            lint_fragment_destination(index, document, destination, range, diagnostics);
        }
        return;
    }
    let decoded_path = percent_decode(path);
    let looks_like_markdown = decoded_path.ends_with(".md") || fragment.is_some();
    if !looks_like_markdown {
        return;
    }
    let Some(relative) = resolve_workspace_path(&document.workspace_relative, &decoded_path) else {
        if !is_archived_path(index, &decoded_path) {
            diagnostics.push(missing_document_diagnostic(
                document,
                range,
                destination.to_owned(),
            ));
        }
        return;
    };
    let relative = if index.by_path.contains_key(&relative) {
        relative
    } else if !relative.ends_with(".md") {
        format!("{relative}.md")
    } else {
        relative
    };
    let Some(target_index) = index.by_path.get(&relative).copied() else {
        if !index.archive_paths.contains(&relative) {
            diagnostics.push(missing_document_diagnostic(
                document,
                range,
                destination.to_owned(),
            ));
        }
        return;
    };
    if fragment.is_some() {
        lint_fragment_against_document(
            &index.documents[target_index],
            destination,
            range,
            document,
            diagnostics,
        );
    }
}

fn lint_fragment_destination(
    index: &WorkspaceIndex,
    document: &Document,
    destination: &str,
    range: Range<usize>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let (path, _) = split_destination(destination);
    if path.is_empty() {
        lint_fragment_against_document(document, destination, range, document, diagnostics);
        return;
    }
    let decoded_path = percent_decode(path);
    let Some(relative) = resolve_workspace_path(&document.workspace_relative, &decoded_path) else {
        return;
    };
    let relative = if index.by_path.contains_key(&relative) {
        relative
    } else if !relative.ends_with(".md") {
        format!("{relative}.md")
    } else {
        relative
    };
    if let Some(target_index) = index.by_path.get(&relative).copied() {
        lint_fragment_against_document(
            &index.documents[target_index],
            destination,
            range,
            document,
            diagnostics,
        );
    }
}

fn lint_fragment_against_document(
    target: &Document,
    destination: &str,
    range: Range<usize>,
    source_document: &Document,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let (_, Some(raw_fragment)) = split_destination(destination) else {
        return;
    };
    let decoded = percent_decode(raw_fragment);
    let fragment = hash_fragment(&decoded);
    if is_line_fragment(&fragment) {
        return;
    }
    let encoded = hash_fragment(&percent_encode_component(&decoded));
    let exact =
        target.parsed.fragments.contains(&fragment) || target.parsed.fragments.contains(&encoded);
    if exact {
        return;
    }
    let rule = if target.workspace_relative == source_document.workspace_relative {
        RULE_MD051
    } else {
        RULE_LMD003
    };
    let lower = fragment.to_lowercase();
    if let Some(expected) = target
        .parsed
        .fragments
        .iter()
        .find(|candidate| candidate.to_lowercase() == lower)
    {
        diagnostics.push(diagnostic(
            source_document,
            range,
            rule,
            format!("Expected: {expected}; Actual: {fragment}"),
            Some(fragment),
            Vec::new(),
        ));
    } else {
        diagnostics.push(diagnostic(
            source_document,
            range,
            rule,
            format!("Fragment \"{fragment}\" does not match any heading or anchor"),
            Some(fragment),
            Vec::new(),
        ));
    }
}

fn lint_wikilink(
    index: &WorkspaceIndex,
    document: &Document,
    wikilink: &Wikilink,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let target = wikilink.target.trim();
    let Some(matches) = resolve_wikilink(index, document, target) else {
        return;
    };
    if matches.is_empty() {
        if !is_archived_wikilink(index, document, target) {
            diagnostics.push(missing_document_diagnostic(
                document,
                wikilink.range.clone(),
                target.to_owned(),
            ));
        }
        return;
    }
    if matches.len() > 1 {
        diagnostics.push(diagnostic(
            document,
            wikilink.range.clone(),
            RULE_LMD002,
            format!("Document identity \"{target}\" is ambiguous"),
            Some(target.to_owned()),
            matches
                .iter()
                .map(|index| index.document.workspace_relative.clone())
                .collect(),
        ));
        return;
    }
    let target_document = &index.documents[matches[0].index];
    let Some(heading) = wikilink.heading.as_deref() else {
        return;
    };
    let heading = heading.trim();
    let heading_matches = target_document
        .parsed
        .headings
        .iter()
        .filter(|candidate| candidate.text == heading)
        .count();
    let target_text = format!("{target}#{heading}");
    match heading_matches {
        0 => diagnostics.push(diagnostic(
            document,
            wikilink.range.clone(),
            RULE_LMD003,
            format!("Heading \"{heading}\" does not exist in \"{target}\""),
            Some(target_text),
            Vec::new(),
        )),
        1 => {}
        _ => diagnostics.push(diagnostic(
            document,
            wikilink.range.clone(),
            RULE_LMD004,
            format!("Heading \"{heading}\" occurs multiple times in \"{target}\""),
            Some(target_text),
            vec![heading.to_owned()],
        )),
    }
}

struct DocumentMatch {
    index: usize,
    document: DocumentPath,
}

#[derive(Clone)]
struct DocumentPath {
    workspace_relative: String,
}

fn resolve_wikilink(
    index: &WorkspaceIndex,
    document: &Document,
    target: &str,
) -> Option<Vec<DocumentMatch>> {
    let target = strip_md_suffix(target);
    if target.is_empty() {
        return Some(vec![DocumentMatch {
            index: index.by_path.get(&document.workspace_relative).copied()?,
            document: DocumentPath {
                workspace_relative: document.workspace_relative.clone(),
            },
        }]);
    }
    if target.contains('/') {
        let root_relative = normalize_path("", target)?;
        let workspace_relative = format!("{}/{}.md", document.root, root_relative);
        return Some(
            index
                .by_path
                .get(&workspace_relative)
                .copied()
                .map(|index| {
                    vec![DocumentMatch {
                        index,
                        document: DocumentPath { workspace_relative },
                    }]
                })
                .unwrap_or_default(),
        );
    }
    let key = identity_key(&document.root, target);
    Some(
        index
            .by_stem
            .get(&key)
            .into_iter()
            .flatten()
            .map(|doc_index| DocumentMatch {
                index: *doc_index,
                document: DocumentPath {
                    workspace_relative: index.documents[*doc_index].workspace_relative.clone(),
                },
            })
            .collect(),
    )
}

fn missing_document_diagnostic(
    document: &Document,
    range: Range<usize>,
    target: String,
) -> Diagnostic {
    diagnostic(
        document,
        range,
        RULE_LMD001,
        format!("Document \"{target}\" does not exist"),
        Some(target),
        Vec::new(),
    )
}

fn resolve_workspace_path(source_path: &str, target: &str) -> Option<String> {
    let base = source_path.rsplit_once('/').map_or("", |(base, _)| base);
    if target.starts_with('/') {
        normalize_path("", target.trim_start_matches('/'))
    } else {
        normalize_path(base, target)
    }
}

fn normalize_path(base: &str, target: &str) -> Option<String> {
    let mut parts = base
        .split('/')
        .filter(|part| !part.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    for part in target.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            part => parts.push(part.to_owned()),
        }
    }
    Some(parts.join("/"))
}

fn split_destination(destination: &str) -> (&str, Option<&str>) {
    let Some(hash) = destination.find('#') else {
        return (destination.split('?').next().unwrap_or(destination), None);
    };
    let path = destination[..hash]
        .split('?')
        .next()
        .unwrap_or(&destination[..hash]);
    (path, Some(&destination[hash + 1..]))
}

fn is_external_uri(destination: &str) -> bool {
    if destination.starts_with("//") {
        return true;
    }
    let Some(colon) = destination.find(':') else {
        return false;
    };
    colon > 0
        && destination[..colon]
            .chars()
            .enumerate()
            .all(|(index, value)| {
                if index == 0 {
                    value.is_ascii_alphabetic()
                } else {
                    value.is_ascii_alphanumeric() || matches!(value, '+' | '-' | '.')
                }
            })
}

fn is_archived_path(index: &WorkspaceIndex, path: &str) -> bool {
    index.archive_paths.contains(path)
}

fn is_archived_wikilink(index: &WorkspaceIndex, document: &Document, target: &str) -> bool {
    let target = strip_md_suffix(target);
    if target.contains('/') {
        normalize_path("", target)
            .map(|root_relative| {
                index
                    .archive_paths
                    .contains(&format!("{}/{}.md", document.root, root_relative))
            })
            .unwrap_or(false)
    } else {
        index
            .archive_stems
            .contains(&identity_key(&document.root, target))
    }
}

fn strip_md_suffix(value: &str) -> &str {
    value.strip_suffix(".md").unwrap_or(value)
}

fn identity_key(root: &str, stem: &str) -> String {
    format!("{root}\0{stem}")
}

fn slash_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn scan_wikilinks(source: &str, range: &Range<usize>, output: &mut Vec<Wikilink>) {
    let mut scan_range = range.clone();
    if scan_range.start > 0
        && source
            .get(scan_range.start - 1..)
            .is_some_and(|value| value.starts_with("[["))
    {
        scan_range.start -= 1;
    }
    if source
        .get(scan_range.clone())
        .is_some_and(|value| value.starts_with("[["))
    {
        scan_range.end = source[scan_range.start + 2..]
            .find("]]")
            .map_or(source.len(), |offset| scan_range.start + 2 + offset + 2);
    }
    let Some(value) = source.get(scan_range.clone()) else {
        return;
    };
    let mut cursor = 0usize;
    while let Some(open) = value[cursor..].find("[[") {
        let open = cursor + open;
        if is_escaped(source, scan_range.start + open) {
            cursor = open + 2;
            continue;
        }
        let body_start = open + 2;
        let Some(close_offset) = value[body_start..].find("]]") else {
            break;
        };
        let close = body_start + close_offset;
        let body = &value[body_start..close];
        if !body.contains('\n') {
            let (target, _) = body
                .split_once('|')
                .map_or((body, None), |(target, alias)| (target, Some(alias)));
            let (target, heading) = target
                .split_once('#')
                .map_or((target.trim(), None), |(target, heading)| {
                    (target.trim(), Some(heading.trim().to_owned()))
                });
            if !target.is_empty() || heading.is_some() {
                output.push(Wikilink {
                    target: target.to_owned(),
                    heading,
                    range: (scan_range.start + open)..(scan_range.start + close + 2),
                });
            }
        }
        cursor = close + 2;
    }
}

fn is_escaped(source: &str, offset: usize) -> bool {
    source.as_bytes()[..offset]
        .iter()
        .rev()
        .take_while(|byte| **byte == b'\\')
        .count()
        % 2
        == 1
}

fn is_wikilink_range(source: &str, range: &Range<usize>) -> bool {
    source
        .get(range.clone())
        .is_some_and(|value| value.starts_with("[["))
        || (range.start > 0
            && source
                .get(range.start - 1..)
                .is_some_and(|value| value.starts_with("[[")))
}

fn scan_html_anchors(value: &str, _offset: usize, output: &mut Vec<String>) {
    let mut cursor = 0usize;
    while let Some(open) = value[cursor..].find('<') {
        let open = cursor + open;
        let Some(close_offset) = value[open + 1..].find('>') else {
            break;
        };
        let close = open + 1 + close_offset;
        scan_html_tag(&value[open + 1..close], output);
        cursor = close + 1;
    }
}

fn scan_html_tag(tag: &str, output: &mut Vec<String>) {
    let tag = tag.trim_start();
    if tag.starts_with(['/', '!', '?']) {
        return;
    }
    let name_end = tag
        .find(|character: char| character.is_ascii_whitespace() || character == '/')
        .unwrap_or(tag.len());
    let tag_name = &tag[..name_end];
    let mut cursor = name_end;
    let bytes = tag.as_bytes();
    while cursor < bytes.len() {
        while cursor < bytes.len() && (bytes[cursor].is_ascii_whitespace() || bytes[cursor] == b'/')
        {
            cursor += 1;
        }
        let attribute_start = cursor;
        while cursor < bytes.len()
            && !bytes[cursor].is_ascii_whitespace()
            && !matches!(bytes[cursor], b'=' | b'/')
        {
            cursor += 1;
        }
        if attribute_start == cursor {
            break;
        }
        let attribute = &tag[attribute_start..cursor];
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if bytes.get(cursor) != Some(&b'=') {
            continue;
        }
        cursor += 1;
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        let Some(first) = bytes.get(cursor).copied() else {
            break;
        };
        let (value_start, value_end) = if matches!(first, b'"' | b'\'') {
            let start = cursor + 1;
            let Some(end) = tag[start..].find(char::from(first)) else {
                break;
            };
            (start, start + end)
        } else {
            let start = cursor;
            let end = tag[start..]
                .find(|character: char| character.is_ascii_whitespace() || character == '/')
                .map_or(tag.len(), |offset| start + offset);
            (start, end)
        };
        cursor = value_end + usize::from(matches!(first, b'"' | b'\''));
        let is_anchor = attribute.eq_ignore_ascii_case("id")
            || (tag_name.eq_ignore_ascii_case("a") && attribute.eq_ignore_ascii_case("name"));
        if is_anchor && value_end > value_start {
            output.push(hash_fragment(&tag[value_start..value_end]));
        }
    }
}

fn normalize_heading_text(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn github_fragment(value: &str) -> String {
    let mut fragment = String::from("#");
    for character in value.to_lowercase().chars() {
        if character.is_alphanumeric()
            || is_unicode_mark(character)
            || is_connector_punctuation(character)
            || character == '-'
        {
            fragment.push_str(&percent_encode_component(&character.to_string()));
        } else if character == ' ' {
            fragment.push('-');
        }
    }
    fragment
}

fn is_unicode_mark(value: char) -> bool {
    matches!(
        value as u32,
        0x0300..=0x036f
            | 0x0483..=0x0489
            | 0x0591..=0x05bd
            | 0x05bf
            | 0x05c1..=0x05c2
            | 0x05c4..=0x05c5
            | 0x05c7
            | 0x0610..=0x061a
            | 0x064b..=0x065f
            | 0x0670
            | 0x06d6..=0x06dc
            | 0x06df..=0x06e4
            | 0x06e7..=0x06e8
            | 0x06ea..=0x06ed
            | 0x0711
            | 0x0730..=0x074a
            | 0x07a6..=0x07b0
            | 0x07eb..=0x07f3
            | 0x0816..=0x0819
            | 0x081b..=0x0823
            | 0x0825..=0x0827
            | 0x0829..=0x082d
            | 0x0859..=0x085b
            | 0x08d3..=0x08e1
            | 0x08e3..=0x0903
            | 0x093a..=0x093c
            | 0x093e..=0x094f
            | 0x0951..=0x0957
            | 0x0962..=0x0963
            | 0x0981..=0x0983
            | 0x09bc
            | 0x09be..=0x09cd
            | 0x09d7
            | 0x09e2..=0x09e3
            | 0x09fe
            | 0x0a01..=0x0a03
            | 0x0a3c
            | 0x0a3e..=0x0a4d
            | 0x0a51
            | 0x0a70..=0x0a71
            | 0x0a75
            | 0x0a81..=0x0a83
            | 0x0abc
            | 0x0abe..=0x0acd
            | 0x0ae2..=0x0ae3
            | 0x0afa..=0x0aff
            | 0x0b01..=0x0b03
            | 0x0b3c
            | 0x0b3e..=0x0b4d
            | 0x0b56..=0x0b57
            | 0x0b62..=0x0b63
            | 0x0b82
            | 0x0bbe..=0x0bcd
            | 0x0bd7
            | 0x0c00..=0x0c04
            | 0x0c3e..=0x0c56
            | 0x0c62..=0x0c63
            | 0x0c81..=0x0c83
            | 0x0cbc
            | 0x0cbe..=0x0cdc
            | 0x0ce2..=0x0ce3
            | 0x0d00..=0x0d04
            | 0x0d3b..=0x0d4d
            | 0x0d57
            | 0x0d62..=0x0d63
            | 0x0d81..=0x0d83
            | 0x0dca
            | 0x0dcf..=0x0df2
            | 0x0e31
            | 0x0e34..=0x0e3a
            | 0x0e47..=0x0e4e
            | 0x0eb1
            | 0x0eb4..=0x0ebc
            | 0x0ebe..=0x0ebf
            | 0x0f18..=0x0f19
            | 0x0f35
            | 0x0f37
            | 0x0f39
            | 0x0f3e..=0x0f3f
            | 0x0f71..=0x0f87
            | 0x0f8d..=0x0fbc
            | 0x0fc6
            | 0x102b..=0x103e
            | 0x1056..=0x1059
            | 0x105e..=0x1060
            | 0x1062..=0x1064
            | 0x1067..=0x106d
            | 0x1071..=0x1074
            | 0x1082
            | 0x1084
            | 0x1085..=0x1086
            | 0x108d
            | 0x1090..=0x1091
            | 0x109d
            | 0x135d..=0x135f
            | 0x1712..=0x1714
            | 0x1732..=0x1734
            | 0x1752..=0x1753
            | 0x1772..=0x1773
            | 0x17b4..=0x17d3
            | 0x17dd
            | 0x180b..=0x180f
            | 0x1885..=0x1886
            | 0x18a9
            | 0x1920..=0x192b
            | 0x1930..=0x193b
            | 0x1a17..=0x1a1b
            | 0x1a55..=0x1a5e
            | 0x1a60
            | 0x1a62
            | 0x1a65..=0x1a6c
            | 0x1a73..=0x1a7c
            | 0x1a7f
            | 0x1ab0..=0x1aff
            | 0x1b00..=0x1b04
            | 0x1b34
            | 0x1b36..=0x1b44
            | 0x1b6b..=0x1b73
            | 0x1b80..=0x1b82
            | 0x1ba1
            | 0x1ba2..=0x1bad
            | 0x1be6
            | 0x1be8..=0x1bed
            | 0x1bef..=0x1bf3
            | 0x1c24..=0x1c37
            | 0x1c40..=0x1c49
            | 0x1c50..=0x1c59
            | 0x1c7f
            | 0x1cd0..=0x1cf9
            | 0x1dc0..=0x1dff
            | 0x20d0..=0x20ff
            | 0x2cef..=0x2cf1
            | 0x2d7f
            | 0x2de0..=0x2dff
            | 0xa66f..=0xa67f
            | 0xa69e..=0xa69f
            | 0xa6f0..=0xa6f1
            | 0xa802
            | 0xa806
            | 0xa80b
            | 0xa823..=0xa827
            | 0xa82c
            | 0xa880..=0xa881
            | 0xa8b4..=0xa8c5
            | 0xa8e0..=0xa8f1
            | 0xa8ff
            | 0xa926..=0xa92f
            | 0xa947..=0xa953
            | 0xa980..=0xa983
            | 0xa9b3
            | 0xa9b4..=0xa9c0
            | 0xa9e5
            | 0xaa29..=0xaa36
            | 0xaa43
            | 0xaa4c..=0xaa4d
            | 0xaa7b..=0xaa7d
            | 0xaab0
            | 0xaab2..=0xaab4
            | 0xaab7..=0xaab8
            | 0xaabe..=0xaabf
            | 0xaac1
            | 0xaaec..=0xaaed
            | 0xaaf3..=0xaaf4
            | 0xaaf6
            | 0xabe5
            | 0xabe8..=0xabea
            | 0xabec
            | 0xabed
            | 0xfb1e
            | 0xfe00..=0xfe0f
            | 0xfe20..=0xfe2f
            | 0xff9e..=0xff9f
            | 0x101fd
            | 0x102e0
            | 0x10376..=0x1037a
            | 0x10a01..=0x10a0f
            | 0x10a38..=0x10a3f
            | 0x10ae5
            | 0x10d24..=0x10d27
            | 0x10eab..=0x10eac
            | 0x10f46..=0x10f50
            | 0x11001
            | 0x11038..=0x11046
            | 0x11070..=0x11076
            | 0x11080..=0x11082
            | 0x110b0..=0x110ba
            | 0x11100..=0x11102
            | 0x11127..=0x11134
            | 0x11145..=0x11146
            | 0x11173
            | 0x11180..=0x11182
            | 0x111b3..=0x111c0
            | 0x111c5..=0x111c8
            | 0x111ca..=0x111cc
            | 0x1122c..=0x11237
            | 0x1123e
            | 0x112df
            | 0x112e0..=0x112ea
            | 0x11300..=0x11304
            | 0x1133b..=0x1134c
            | 0x11357
            | 0x11362..=0x11363
            | 0x11366..=0x1136c
            | 0x11370..=0x11374
            | 0x11435..=0x11446
            | 0x114b0..=0x114c3
            | 0x114c6
            | 0x114c8..=0x114cf
            | 0x115af..=0x115c0
            | 0x115dc..=0x115dd
            | 0x11630..=0x11640
            | 0x116ab..=0x116b7
            | 0x116b9
            | 0x1171d..=0x1172b
            | 0x11740..=0x11746
            | 0x1182c..=0x1183a
            | 0x118a9
            | 0x118e0..=0x118f2
            | 0x11930..=0x1193b
            | 0x1193f
            | 0x11941
            | 0x11943
            | 0x119d1..=0x119e0
            | 0x119e2
            | 0x119e4
            | 0x11a01..=0x11a0a
            | 0x11a33..=0x11a39
            | 0x11a3b..=0x11a3e
            | 0x11a47
            | 0x11a51..=0x11a5b
            | 0x11a8a..=0x11a99
            | 0x11c30..=0x11c3f
            | 0x11c92..=0x11ca7
            | 0x11ca9..=0x11cb6
            | 0x11d31..=0x11d45
            | 0x11d47
            | 0x11d90..=0x11d97
            | 0x11ef3..=0x11ef6
            | 0x11f00..=0x11f02
            | 0x11f34..=0x11f3a
            | 0x11f40
            | 0x11f42
            | 0x13447..=0x13455
            | 0x16af0..=0x16af4
            | 0x16b30..=0x16b36
            | 0x16f4f
            | 0x16f51..=0x16f87
            | 0x16f8f..=0x16f92
            | 0x1bc9d..=0x1bc9e
            | 0x1d165..=0x1d169
            | 0x1d16d..=0x1d172
            | 0x1d17b..=0x1d182
            | 0x1d185..=0x1d18b
            | 0x1d1aa..=0x1d1ad
            | 0x1d242..=0x1d244
            | 0x1da00..=0x1da36
            | 0x1da3b..=0x1da6c
            | 0x1da75
            | 0x1da84
            | 0x1da9b..=0x1da9f
            | 0x1daa1..=0x1dab0
            | 0x1e000..=0x1e006
            | 0x1e008..=0x1e018
            | 0x1e01b..=0x1e021
            | 0x1e023..=0x1e024
            | 0x1e026..=0x1e02a
            | 0x1e130..=0x1e136
            | 0x1e2ae
            | 0x1e2ec..=0x1e2ef
            | 0x1e4ec..=0x1e4ef
            | 0x1e5ee..=0x1e5ef
            | 0x1e6ec..=0x1e6ef
            | 0x1e8d0..=0x1e8d6
            | 0x1e944..=0x1e94a
            | 0x1ed01..=0x1ed2d
            | 0x1ef00..=0x1ef02
            | 0x1ef80..=0x1ef87
            | 0x1ef90..=0x1ef97
            | 0x1ef99..=0x1ef9f
            | 0x1efb0..=0x1efb0
            | 0x1f3fb..=0x1f3ff
    )
}

fn is_connector_punctuation(value: char) -> bool {
    matches!(
        value as u32,
        0x005f | 0x203f | 0x2040 | 0x2054 | 0xfe33..=0xfe34 | 0xfe4d..=0xfe4f | 0xff3f
    )
}

fn hash_fragment(value: &str) -> String {
    if value.starts_with('#') {
        value.to_owned()
    } else {
        format!("#{value}")
    }
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let (Some(high), Some(low)) =
                (hex_value(bytes[index + 1]), hex_value(bytes[index + 2]))
            {
                decoded.push((high << 4) | low);
                index += 3;
                continue;
            }
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

fn percent_encode_component(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        if byte.is_ascii_alphanumeric()
            || matches!(
                byte,
                b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')'
            )
        {
            encoded.push(*byte as char);
        } else {
            encoded.push_str(&format!("%{:02X}", byte));
        }
    }
    encoded
}

fn hex_value(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn is_line_fragment(value: &str) -> bool {
    let Some(value) = value.strip_prefix('#') else {
        return false;
    };
    let Some(value) = value.strip_prefix('L') else {
        return false;
    };
    let first_digits = value.chars().take_while(char::is_ascii_digit).count();
    if first_digits == 0 {
        return false;
    }
    let rest = &value[first_digits..];
    let rest = if let Some(rest) = rest.strip_prefix('C') {
        let count = rest.chars().take_while(char::is_ascii_digit).count();
        if count == 0 {
            return false;
        }
        &rest[count..]
    } else {
        rest
    };
    if rest.is_empty() {
        return true;
    }
    let Some(rest) = rest.strip_prefix("-L") else {
        return false;
    };
    let count = rest.chars().take_while(char::is_ascii_digit).count();
    if count == 0 {
        return false;
    }
    let rest = &rest[count..];
    if rest.is_empty() {
        return true;
    }
    if let Some(rest) = rest.strip_prefix('C') {
        let count = rest.chars().take_while(char::is_ascii_digit).count();
        count > 0 && rest[count..].is_empty()
    } else {
        false
    }
}

impl LineIndex {
    fn new(source: &str) -> Self {
        let mut starts = vec![0];
        for (index, byte) in source.bytes().enumerate() {
            if byte == b'\n' {
                starts.push(index + 1);
            }
        }
        Self { starts }
    }

    fn position(&self, source: &str, offset: usize) -> (usize, usize) {
        let offset = offset.min(source.len());
        let mut low = 0usize;
        let mut high = self.starts.len();
        while low + 1 < high {
            let middle = (low + high) / 2;
            if self.starts[middle] <= offset {
                low = middle;
            } else {
                high = middle;
            }
        }
        let line_start = self.starts[low];
        (low + 1, source[line_start..offset].chars().count() + 1)
    }

    fn context(&self, source: &str, offset: usize) -> String {
        let (line, _) = self.position(source, offset);
        let start = self.starts[line - 1];
        let end = source[start..]
            .find('\n')
            .map_or(source.len(), |offset| start + offset);
        source[start..end].trim_end_matches('\r').to_owned()
    }
}

fn diagnostic(
    document: &Document,
    range: Range<usize>,
    rule: RuleSpec,
    detail: String,
    target: Option<String>,
    candidates: Vec<String>,
) -> Diagnostic {
    let start = range.start.min(document.source.len());
    let end = range.end.min(document.source.len()).max(start);
    let (line, column) = document.lines.position(&document.source, start);
    let (end_line, end_column) = document.lines.position(&document.source, end);
    Diagnostic {
        file: document.workspace_relative.clone(),
        line,
        column,
        end_line,
        end_column,
        rule: rule.code,
        rule_name: rule.name,
        description: rule.description,
        detail,
        context: document.lines.context(&document.source, start),
        target,
        candidates,
    }
}
