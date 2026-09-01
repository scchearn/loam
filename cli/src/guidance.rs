//! The `guidance` lint domain: a Loam-owned, generated memory-map region inside
//! the workspace `AGENTS.md`.
//!
//! Loam owns only the bytes between the two markers. Everything else in the
//! guidance file — including the `## Memory` prose above the opening marker — is
//! human-authored and is never rewritten.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use crate::lint::{Finding, Severity};
use crate::memory;
use crate::state;

/// Opening marker. Fixed contract string shared with the guidance skills.
pub const MAP_OPEN: &str =
    "<!-- loam:memory-map · generated from wiki/index.md · do not edit by hand -->";
/// Closing marker. Fixed contract string shared with the guidance skills.
pub const MAP_CLOSE: &str = "<!-- /loam:memory-map -->";
/// Heading the region lives under when Loam has to seed it.
pub const MEMORY_HEADING: &str = "## Memory";
/// Stable prose seeded once above the region. Human-editable, never regenerated.
pub const MEMORY_PROSE: &str = "This project keeps a **Loam memory** — agent-owned markdown in `wiki/`. Consult it\nbefore non-trivial work and keep it current. Start at `wiki/index.md`.";

/// Slugs listed inline per page type before the region truncates.
const INLINE_THRESHOLD: usize = 30;
/// Page-type directories, in the order they are rendered.
const GROUPS: [(&str, &str); 4] = [
    ("topics", "Topics"),
    ("entities", "Entities"),
    ("concepts", "Concepts"),
    ("analyses", "Analyses"),
];

/// Guidance-domain findings (`GDN*`). Silent when the workspace has no wiki.
pub fn findings(workspace: &Path, out: &mut Vec<Finding>) {
    let Some(wiki_root) = state::resolve_wiki_root(workspace) else {
        return;
    };

    let agents = fs::read_to_string(workspace.join("AGENTS.md")).unwrap_or_default();
    match find_region(&agents) {
        None => out.push(Finding::file(
            "GDN001",
            "guidance-map-missing",
            Severity::Warning,
            "AGENTS.md",
            "`AGENTS.md` has no `loam:memory-map` region",
        )),
        Some((start, end)) => {
            let current = &agents[start..end];
            let generated = region(workspace, &wiki_root);
            if normalize(current) != normalize(&generated) {
                let present = region_slugs(current);
                let expected = region_slugs(&generated);
                let mut finding = Finding::file(
                    "GDN002",
                    "guidance-map-stale",
                    Severity::Warning,
                    "AGENTS.md",
                    "`loam:memory-map` region no longer matches the wiki",
                );
                let added = join(expected.difference(&present));
                let removed = join(present.difference(&expected));
                if !added.is_empty() {
                    finding = finding.with_evidence("added", &added);
                }
                if !removed.is_empty() {
                    finding = finding.with_evidence("removed", &removed);
                }
                out.push(finding);
            }
        }
    }

    // The shim is read-only for Loam: it is reported, never written.
    let shim = workspace.join("CLAUDE.md");
    if shim.is_file() && fs::read_to_string(&shim).unwrap_or_default().trim() != "@AGENTS.md" {
        out.push(Finding::file(
            "GDN003",
            "guidance-claude-shim",
            Severity::Warning,
            "CLAUDE.md",
            "`CLAUDE.md` should contain exactly `@AGENTS.md`",
        ));
    }
}

/// Inserts or regenerates the region in the workspace `AGENTS.md`. Returns
/// whether the file changed. A workspace with no wiki, or no `AGENTS.md` to
/// append to, is a no-op — seeding a fresh guidance file is the skill's job.
pub fn fix(workspace: &Path) -> Result<bool, String> {
    let Some(wiki_root) = state::resolve_wiki_root(workspace) else {
        return Ok(false);
    };
    let path = workspace.join("AGENTS.md");
    let Ok(existing) = fs::read_to_string(&path) else {
        return Ok(false);
    };

    let crlf = existing.contains("\r\n");
    let newline = if crlf { "\r\n" } else { "\n" };
    let generated = with_newlines(&region(workspace, &wiki_root), newline);

    let updated = match find_region(&existing) {
        Some((start, end)) => {
            if existing[start..end] == generated {
                return Ok(false);
            }
            format!("{}{generated}{}", &existing[..start], &existing[end..])
        }
        None => {
            let mut text = existing.clone();
            if !text.is_empty() && !text.ends_with('\n') {
                text.push_str(newline);
            }
            let prose = with_newlines(MEMORY_PROSE, newline);
            text.push_str(&format!(
                "{newline}{MEMORY_HEADING}{newline}{newline}{prose}{newline}{newline}{generated}{newline}"
            ));
            text
        }
    };

    fs::write(&path, updated).map_err(|error| format!("{}: {error}", path.display()))?;
    Ok(true)
}

/// The generated region, markers included, LF-terminated.
///
/// Pure and deterministic: the wiki page tree is the only input, so repeated
/// generation is byte-identical and diffs stay stable.
pub fn region(workspace: &Path, wiki_root: &Path) -> String {
    let display_root = memory::relative(workspace, wiki_root);
    let pages = memory::durable_pages(wiki_root);

    let mut lines = vec![MAP_OPEN.to_owned()];
    for (directory, label) in GROUPS {
        let slugs = group_slugs(&pages, directory);
        if !slugs.is_empty() {
            lines.push(render_group(label, &slugs));
        }
    }
    let code_pages = pages
        .iter()
        .filter(|page| page.starts_with("code/") && *page != memory::CODE_HUB)
        .count();
    if code_pages > 0 {
        lines.push(format!(
            "Code graph: {code_pages} pages → {}",
            memory::join_display(&display_root, memory::CODE_HUB)
        ));
    }
    lines.push(MAP_CLOSE.to_owned());
    lines.join("\n")
}

/// Byte offsets of the region in `text`, markers included.
pub fn find_region(text: &str) -> Option<(usize, usize)> {
    let start = text.find(MAP_OPEN)?;
    let close = text[start..].find(MAP_CLOSE)? + start;
    Some((start, close + MAP_CLOSE.len()))
}

/// Sorted slugs for one page-type directory. `_index.md` is the reserved hub
/// name and is never a slug.
fn group_slugs(pages: &[String], directory: &str) -> Vec<String> {
    let prefix = format!("{directory}/");
    let mut slugs: Vec<String> = pages
        .iter()
        .filter(|page| page.starts_with(&prefix))
        .map(|page| memory::stem(page))
        .filter(|slug| slug != "_index")
        .collect();
    slugs.sort();
    slugs.dedup();
    slugs
}

fn render_group(label: &str, slugs: &[String]) -> String {
    let total = slugs.len();
    let shown = total.min(INLINE_THRESHOLD);
    let listed = slugs[..shown].join(" · ");
    if total > shown {
        format!(
            "{label} ({total}): {listed} … (+{} more, see index.md)",
            total - shown
        )
    } else {
        format!("{label} ({total}): {listed}")
    }
}

/// Slugs a rendered region lists, ignoring counts and truncation markers.
fn region_slugs(region: &str) -> BTreeSet<String> {
    let mut slugs = BTreeSet::new();
    for line in region.lines() {
        let Some((head, rest)) = line.split_once(": ") else {
            continue;
        };
        if !head.trim_end().ends_with(')') {
            continue;
        }
        for token in rest.split('·') {
            // The truncation suffix rides on the last listed slug.
            let token = token.split('…').next().unwrap_or_default().trim();
            if token.is_empty() || token.starts_with('…') || token.starts_with("(+") {
                continue;
            }
            slugs.insert(token.to_owned());
        }
    }
    slugs
}

/// Whitespace-insensitive form, so a cosmetic reflow is not reported stale.
fn normalize(region: &str) -> String {
    region
        .lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn with_newlines(text: &str, newline: &str) -> String {
    if newline == "\n" {
        text.to_owned()
    } else {
        text.replace('\n', newline)
    }
}

fn join<'a>(values: impl Iterator<Item = &'a String>) -> String {
    values.cloned().collect::<Vec<_>>().join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// A wiki tree built from `<dir>/<slug>` page paths.
    fn wiki(label: &str, pages: &[String]) -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after epoch")
            .as_nanos();
        let workspace = std::env::temp_dir().join(format!("loam-gdn-{label}-{nonce}"));
        for page in pages {
            let path = workspace.join("wiki").join(page);
            fs::create_dir_all(path.parent().expect("page should have a parent"))
                .expect("wiki directory should be created");
            fs::write(&path, "# Page\n").expect("page should be written");
        }
        fs::create_dir_all(workspace.join("wiki")).expect("wiki root should exist");
        fs::write(workspace.join("wiki/index.md"), "# Index\n\n## Overview\n")
            .expect("index should be written");
        workspace
    }

    fn generated(workspace: &std::path::Path) -> String {
        region(workspace, &workspace.join("wiki"))
    }

    fn pages(paths: &[&str]) -> Vec<String> {
        paths.iter().map(|path| (*path).to_owned()).collect()
    }

    #[test]
    fn guidance_groups_pages_by_type_in_taxonomy_order() {
        let workspace = wiki(
            "order",
            &pages(&[
                "analyses/gtm-assessment.md",
                "concepts/oauth-scopes.md",
                "entities/ovhcloud.md",
                "entities/skillcorner.md",
                "topics/authentication.md",
            ]),
        );
        assert_eq!(
            generated(&workspace),
            format!(
                "{MAP_OPEN}\n\
                 Topics (1): authentication\n\
                 Entities (2): ovhcloud · skillcorner\n\
                 Concepts (1): oauth-scopes\n\
                 Analyses (1): gtm-assessment\n\
                 {MAP_CLOSE}"
            )
        );
    }

    #[test]
    fn guidance_omits_empty_groups_and_the_code_pointer() {
        let workspace = wiki("empty", &pages(&["topics/authentication.md"]));
        assert_eq!(
            generated(&workspace),
            format!("{MAP_OPEN}\nTopics (1): authentication\n{MAP_CLOSE}")
        );
    }

    #[test]
    fn guidance_renders_an_empty_but_valid_region_for_a_fresh_wiki() {
        let workspace = wiki("fresh", &[]);
        assert_eq!(generated(&workspace), format!("{MAP_OPEN}\n{MAP_CLOSE}"));
    }

    #[test]
    fn guidance_points_at_the_code_hub_when_code_pages_exist() {
        let workspace = wiki(
            "code",
            &pages(&[
                "topics/authentication.md",
                "code/_index.md",
                "code/cli-lint.md",
                "code/cli-memory.md",
            ]),
        );
        assert!(generated(&workspace).contains("Code graph: 2 pages → wiki/code/_index.md"));
    }

    #[test]
    fn guidance_truncates_a_category_over_the_threshold() {
        let paths: Vec<String> = (0..INLINE_THRESHOLD + 4)
            .map(|index| format!("topics/topic-{index:03}.md"))
            .collect();
        let workspace = wiki("truncate", &paths);
        let line = generated(&workspace)
            .lines()
            .find(|line| line.starts_with("Topics"))
            .expect("topics group should render")
            .to_owned();

        assert!(line.starts_with(&format!("Topics ({}): topic-000 ·", INLINE_THRESHOLD + 4)));
        assert!(
            line.ends_with("topic-029 … (+4 more, see index.md)"),
            "{line}"
        );
        assert_eq!(line.matches(" · ").count(), INLINE_THRESHOLD - 1);
    }

    #[test]
    fn guidance_generation_is_idempotent() {
        let workspace = wiki(
            "idempotent",
            &pages(&["topics/beta.md", "topics/alpha.md", "entities/gamma.md"]),
        );
        assert_eq!(generated(&workspace), generated(&workspace));
    }

    #[test]
    fn guidance_never_lists_the_reserved_hub_name_as_a_slug() {
        let workspace = wiki("hub", &pages(&["topics/_index.md", "topics/alpha.md"]));
        assert_eq!(
            generated(&workspace),
            format!("{MAP_OPEN}\nTopics (1): alpha\n{MAP_CLOSE}")
        );
    }

    #[test]
    fn guidance_region_slugs_ignore_counts_and_truncation_markers() {
        let text = format!(
            "{MAP_OPEN}\n\
             Topics (34): alpha · beta … (+4 more, see index.md)\n\
             Code graph: 12 pages → wiki/code/_index.md\n\
             {MAP_CLOSE}"
        );
        let slugs = region_slugs(&text);
        assert_eq!(
            slugs.into_iter().collect::<Vec<_>>(),
            vec!["alpha".to_owned(), "beta".to_owned()]
        );
    }

    #[test]
    fn guidance_find_region_spans_both_markers() {
        let text = format!("# Guide\n\n{MAP_OPEN}\nTopics (0):\n{MAP_CLOSE}\n\nTail\n");
        let (start, end) = find_region(&text).expect("region should be found");
        assert!(text[start..end].starts_with(MAP_OPEN));
        assert!(text[start..end].ends_with(MAP_CLOSE));
    }

    #[test]
    fn guidance_normalization_ignores_cosmetic_reflow() {
        let one = format!("{MAP_OPEN}\nTopics (2): alpha · beta\n{MAP_CLOSE}");
        let reflowed = format!("{MAP_OPEN}\n\nTopics (2):   alpha ·  beta\n{MAP_CLOSE}");
        assert_eq!(normalize(&one), normalize(&reflowed));
    }
}
