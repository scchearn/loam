use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const TZ_FIELDS: &[&str] = &[
    "created_at",
    "updated_at",
    "approved_at",
    "started_at",
    "completed_at",
];
const EMDASH: &str = "\u{2014}";

pub fn run(mut args: impl Iterator<Item = String>) -> i32 {
    let Some(mode) = args.next() else {
        usage();
        return 1;
    };
    let Some(wiki_root) = args.next() else {
        usage();
        return 1;
    };

    let mut offset = None;
    while let Some(arg) = args.next() {
        if arg != "--offset" {
            usage();
            return 1;
        }
        let Some(value) = args.next() else {
            usage();
            return 1;
        };
        offset = Some(value);
    }
    let offset = offset.unwrap_or_else(default_offset);
    let root = Path::new(&wiki_root);
    if !root.is_dir() {
        println!(
            "{{\"error\":\"wiki root not found: {}\"}}",
            json_escape(&wiki_root)
        );
        return 1;
    }

    let files = match markdown_files(root) {
        Ok(files) => files,
        Err(message) => {
            eprintln!("Error: {message}");
            return 1;
        }
    };

    match mode.as_str() {
        "check" => check(&files, &offset),
        "fix" => fix(&files, &offset),
        _ => {
            usage();
            1
        }
    }
}

pub fn drift_count(root: &Path) -> usize {
    let Ok(files) = markdown_files(root) else {
        return 0;
    };
    files
        .iter()
        .map(|(relative, path)| scan_file(relative, path, "+00:00").len())
        .sum()
}

fn usage() {
    eprintln!("Usage: loam datecheck <check|fix> <wiki-root> [--offset +HH:MM]");
}

fn check(files: &[(String, PathBuf)], offset: &str) -> i32 {
    let mut drift = false;
    for (relative, path) in files {
        for finding in scan_file(relative, path, offset) {
            println!("{finding}");
            drift = true;
        }
    }
    if drift {
        2
    } else {
        0
    }
}

fn fix(files: &[(String, PathBuf)], offset: &str) -> i32 {
    let mut fixed = 0;
    for (relative, path) in files {
        if scan_file(relative, path, offset).is_empty() {
            continue;
        }
        if fix_file(path, offset) {
            println!("{}", json_escape(relative));
            fixed += 1;
        }
    }
    println!(
        "{{\"mode\":\"fix\",\"offset\":\"{}\",\"files_fixed\":{fixed}}}",
        json_escape(offset)
    );
    0
}

fn markdown_files(root: &Path) -> Result<Vec<(String, PathBuf)>, String> {
    let mut files = Vec::new();
    collect_markdown_files(root, root, &mut files)?;
    files.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(files)
}

fn collect_markdown_files(
    root: &Path,
    directory: &Path,
    files: &mut Vec<(String, PathBuf)>,
) -> Result<(), String> {
    let mut entries: Vec<_> = fs::read_dir(directory)
        .map_err(|error| format!("cannot read {}: {error}", directory.display()))?
        .filter_map(Result::ok)
        .collect();
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(_) => continue,
        };
        if file_type.is_dir() {
            collect_markdown_files(root, &path, files)?;
        } else if file_type.is_file()
            && path.extension().and_then(|extension| extension.to_str()) == Some("md")
        {
            let relative = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            files.push((relative, path));
        }
    }
    Ok(())
}

fn scan_file(relative: &str, path: &Path, offset: &str) -> Vec<String> {
    let Ok(content) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut findings = Vec::new();
    let mut in_frontmatter = false;

    for (index, raw_line) in content.split('\n').enumerate() {
        let line_number = index + 1;
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);

        if line_number == 1 && line == "---" {
            in_frontmatter = true;
            continue;
        }
        if in_frontmatter && line == "---" {
            in_frontmatter = false;
            continue;
        }

        if in_frontmatter {
            for field in TZ_FIELDS {
                let Some(value) = frontmatter_value(line, field) else {
                    continue;
                };
                if value.is_empty() || value == "null" {
                    continue;
                }
                if has_legacy_tz(value) {
                    findings.push(finding(
                        relative,
                        line_number,
                        field,
                        value,
                        "legacy_tz",
                        &format!("replace with {offset}"),
                    ));
                } else if is_bare_timestamp(value) {
                    findings.push(finding(
                        relative,
                        line_number,
                        field,
                        value,
                        "missing_offset",
                        &format!("add {offset}"),
                    ));
                }
            }
        }

        if let Some(value) = captured_value(line) {
            if has_legacy_tz(value) {
                findings.push(finding(
                    relative,
                    line_number,
                    "Captured",
                    value,
                    "legacy_tz",
                    &format!("replace with {offset}"),
                ));
            } else if is_bare_timestamp(value) {
                findings.push(finding(
                    relative,
                    line_number,
                    "Captured",
                    value,
                    "missing_offset",
                    &format!("add {offset}"),
                ));
            }
        }

        if let Some(separator) = decision_separator(line) {
            findings.push(finding(
                relative,
                line_number,
                "decisions_log",
                separator,
                "wrong_separator",
                "use em-dash \u{2014}",
            ));
        }
    }
    findings
}

fn finding(file: &str, line: usize, field: &str, value: &str, issue: &str, fix: &str) -> String {
    format!(
        "{{\"file\":\"{}\",\"line\":{line},\"field\":\"{}\",\"value\":\"{}\",\"issue\":\"{issue}\",\"fix\":\"{}\"}}",
        json_escape(file),
        json_escape(field),
        json_escape(value),
        json_escape(fix),
    )
}

fn frontmatter_value<'a>(line: &'a str, field: &str) -> Option<&'a str> {
    let rest = line.strip_prefix(field)?.strip_prefix(':')?;
    rest.chars()
        .next()?
        .is_whitespace()
        .then(|| rest.trim_start())
}

fn captured_value(line: &str) -> Option<&str> {
    let rest = line.strip_prefix('-')?.trim_start();
    let rest = rest.strip_prefix("Captured:")?;
    rest.chars()
        .next()?
        .is_whitespace()
        .then(|| rest.trim_start())
}

fn has_legacy_tz(value: &str) -> bool {
    has_legacy_label(value, false)
}

fn has_legacy_label(value: &str, include_z: bool) -> bool {
    for label in ["SAST", "UTC", "UT"] {
        if value.ends_with(&format!(" {label}")) {
            return true;
        }
    }
    if include_z && value.ends_with(" Z") {
        return true;
    }
    let Some(index) = value.rfind(" GMT") else {
        return false;
    };
    let label = &value[index + 1..];
    label.len() > 4
        && label.starts_with("GMT")
        && matches!(label.as_bytes().get(3), Some(b'+') | Some(b'-'))
        && label.as_bytes()[4..].iter().all(u8::is_ascii_digit)
}

fn is_bare_timestamp(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() < 15 || !value.is_char_boundary(10) || !is_date(&value[..10]) {
        return false;
    }
    let mut index = 10;
    while bytes.get(index).is_some_and(u8::is_ascii_whitespace) {
        index += 1;
    }
    index + 5 == bytes.len()
        && bytes[index].is_ascii_digit()
        && bytes[index + 1].is_ascii_digit()
        && bytes[index + 2] == b':'
        && bytes[index + 3].is_ascii_digit()
        && bytes[index + 4].is_ascii_digit()
}

fn is_timestamp_base(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 16
        && bytes[0..4].iter().all(u8::is_ascii_digit)
        && bytes[4] == b'-'
        && bytes[5..7].iter().all(u8::is_ascii_digit)
        && bytes[7] == b'-'
        && bytes[8..10].iter().all(u8::is_ascii_digit)
        && bytes[10] == b' '
        && bytes[11..13].iter().all(u8::is_ascii_digit)
        && bytes[13] == b':'
        && bytes[14..16].iter().all(u8::is_ascii_digit)
}

fn decision_separator(line: &str) -> Option<&str> {
    let rest = line.strip_prefix('-')?.trim_start();
    if rest.len() < 10 || !rest.is_char_boundary(10) || !is_date(&rest[..10]) {
        return None;
    }
    let after_date = &rest[10..];
    if after_date.trim_start().starts_with(EMDASH) {
        return None;
    }

    let mut separator_end = 0;
    let mut separator_seen = false;
    for (index, character) in after_date.char_indices() {
        if !separator_seen && character.is_ascii_whitespace() {
            separator_end = index + character.len_utf8();
            continue;
        }
        if !separator_seen && matches!(character, '-' | ':') {
            separator_seen = true;
            separator_end = index + character.len_utf8();
            continue;
        }
        if separator_seen && character.is_ascii_whitespace() {
            separator_end = index + character.len_utf8();
            continue;
        }
        break;
    }
    if !separator_seen {
        return None;
    }
    after_date[separator_end..]
        .chars()
        .next()
        .filter(|character| character.is_alphanumeric())
        .map(|_| &after_date[..separator_end])
}

fn is_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 10
        && bytes[0..4].iter().all(u8::is_ascii_digit)
        && bytes[4] == b'-'
        && bytes[5..7].iter().all(u8::is_ascii_digit)
        && bytes[7] == b'-'
        && bytes[8..10].iter().all(u8::is_ascii_digit)
}

fn fix_file(path: &Path, offset: &str) -> bool {
    let Ok(before) = fs::read_to_string(path) else {
        return false;
    };
    let mut after = String::with_capacity(before.len() + 32);
    for segment in before.split_inclusive('\n') {
        let has_newline = segment.ends_with('\n');
        let without_newline = segment.strip_suffix('\n').unwrap_or(segment);
        let has_carriage_return = without_newline.ends_with('\r');
        let line = without_newline
            .strip_suffix('\r')
            .unwrap_or(without_newline);
        after.push_str(&normalize_line(line, offset));
        if has_newline {
            if has_carriage_return {
                after.push('\r');
            }
            after.push('\n');
        }
    }
    if before == after || fs::write(path, after).is_err() {
        return false;
    }
    true
}

fn normalize_line(line: &str, offset: &str) -> String {
    for field in TZ_FIELDS {
        let prefix = format!("{field}: ");
        if let Some(value) = line.strip_prefix(&prefix) {
            if let Some(base) = canonical_base(value) {
                return format!("{prefix}{base} {offset}");
            }
        }
    }

    if let Some(value) = line.strip_prefix("- Captured: ") {
        if let Some(base) = canonical_base(value) {
            return format!("- Captured: {base} {offset}");
        }
    }

    if line.len() >= 12
        && line.starts_with("- ")
        && line.is_char_boundary(2)
        && line.is_char_boundary(12)
        && is_date(&line[2..12])
    {
        let after_date = &line[12..];
        if let Some(title) = after_date.strip_prefix(" - ") {
            return format!("- {} {EMDASH} {title}", &line[2..12]);
        }
        if let Some(title) = after_date.strip_prefix(": ") {
            return format!("- {} {EMDASH} {title}", &line[2..12]);
        }
    }
    line.to_owned()
}

fn canonical_base(value: &str) -> Option<&str> {
    if is_timestamp_base(value) {
        return Some(value);
    }
    if value.len() < 18
        || !value.is_char_boundary(16)
        || !is_timestamp_base(&value[..16])
        || &value[16..17] != " "
    {
        return None;
    }
    has_legacy_label(value, true).then_some(&value[..16])
}

fn default_offset() -> String {
    let raw = Command::new("date")
        .arg("+%z")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .unwrap_or_default();
    let bytes = raw.as_bytes();
    if bytes.len() == 5
        && matches!(bytes[0], b'+' | b'-')
        && bytes[1..].iter().all(u8::is_ascii_digit)
    {
        format!("{}{}:{}", &raw[0..1], &raw[1..3], &raw[3..5])
    } else {
        "+00:00".to_owned()
    }
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
