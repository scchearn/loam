//! `loam.canon/v1`: the canonical byte form a seal binds to.
//!
//! Input is the raw bytes strictly between a seal span's marker lines. The
//! algorithm, which the floor scripts (`seal-canon.sh` / `seal-canon.ps1`)
//! mirror, except that the floors refuse (exit 2, use the runtime) any file
//! holding a lone CR, where the runtime accepts it per step 2:
//!
//! 1. The bytes must be valid UTF-8; anything else is an error. A leading
//!    BOM is ordinary content and is not stripped.
//! 2. CRLF and lone CR become LF.
//! 3. Spaces (U+0020) and tabs (U+0009) are stripped from the end of every
//!    line. No other whitespace is touched.
//! 4. Leading and trailing blank lines (lines empty after step 3) are
//!    dropped. Interior blank lines are kept as they are.
//! 5. The result ends in exactly one LF. Content with no lines left
//!    canonicalizes to the empty byte string.
//!
//! There is no Unicode normalization (NFC), no reflow and no markdown
//! re-escaping: a reflowed paragraph has different lines and so hashes
//! differently.

use crate::sha256;
use std::fmt;

/// Identifier recorded in seal records for this algorithm.
pub const CANON_ID: &str = "loam.canon/v1";

#[derive(Debug, PartialEq, Eq)]
pub enum CanonError {
    /// The span is not valid UTF-8; `valid_up_to` is the first bad byte offset.
    InvalidUtf8 { valid_up_to: usize },
}

impl fmt::Display for CanonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CanonError::InvalidUtf8 { valid_up_to } => {
                write!(f, "invalid UTF-8 at byte {valid_up_to}")
            }
        }
    }
}

impl std::error::Error for CanonError {}

/// Canonicalize `input` under `loam.canon/v1`.
pub fn canonicalize(input: &[u8]) -> Result<Vec<u8>, CanonError> {
    let text = std::str::from_utf8(input).map_err(|error| CanonError::InvalidUtf8 {
        valid_up_to: error.valid_up_to(),
    })?;
    let mut lines: Vec<&str> = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        let end = rest.find(['\r', '\n']).unwrap_or(rest.len());
        lines.push(rest[..end].trim_end_matches([' ', '\t']));
        let terminator = &rest[end..];
        let skip = if terminator.starts_with("\r\n") {
            2
        } else if terminator.is_empty() {
            0
        } else {
            1
        };
        rest = &terminator[skip..];
    }
    let first = lines.iter().position(|line| !line.is_empty());
    let last = lines.iter().rposition(|line| !line.is_empty());
    let mut out = Vec::new();
    if let (Some(first), Some(last)) = (first, last) {
        for line in &lines[first..=last] {
            out.extend_from_slice(line.as_bytes());
            out.push(b'\n');
        }
    }
    Ok(out)
}

/// Lowercase hex SHA-256 of the canonical form of `input`.
pub fn digest(input: &[u8]) -> Result<String, CanonError> {
    Ok(sha256::hex(&canonicalize(input)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canon(input: &str) -> String {
        String::from_utf8(canonicalize(input.as_bytes()).unwrap()).unwrap()
    }

    #[test]
    fn crlf_and_lone_cr_become_lf() {
        assert_eq!(canon("a\r\nb\rc\n"), "a\nb\nc\n");
    }

    #[test]
    fn trailing_spaces_and_tabs_are_stripped_per_line() {
        assert_eq!(canon("a \t \nb\t\n"), "a\nb\n");
    }

    #[test]
    fn leading_and_trailing_blank_lines_drop_but_interior_ones_stay() {
        assert_eq!(canon("\n  \n\na\n\n\nb\n \n\n"), "a\n\n\nb\n");
    }

    #[test]
    fn exactly_one_final_lf() {
        assert_eq!(canon("a"), "a\n");
        assert_eq!(canon("a\n\n\n"), "a\n");
    }

    #[test]
    fn leading_indentation_is_preserved() {
        assert_eq!(canon("  a\n\tb\n"), "  a\n\tb\n");
    }

    #[test]
    fn other_whitespace_is_not_stripped() {
        assert_eq!(canon("a\u{00A0}\n"), "a\u{00A0}\n");
    }

    #[test]
    fn empty_and_blank_only_input_is_empty() {
        assert_eq!(canon(""), "");
        assert_eq!(canon("\r\n \t\n\r"), "");
    }

    #[test]
    fn no_unicode_normalization() {
        let composed = "\u{00e9}\n";
        let decomposed = "e\u{0301}\n";
        assert_ne!(canon(composed), canon(decomposed));
    }

    #[test]
    fn reflow_changes_the_digest() {
        let one = digest(b"alpha beta gamma\n").unwrap();
        let two = digest(b"alpha beta\ngamma\n").unwrap();
        assert_ne!(one, two);
    }

    #[test]
    fn equivalent_encodings_share_a_digest() {
        let lf = digest(b"alpha\n\nbeta\n").unwrap();
        let messy = digest(b"\r\nalpha  \r\n\r\nbeta\t\r\n\r\n").unwrap();
        assert_eq!(lf, messy);
    }

    #[test]
    fn invalid_utf8_is_an_error() {
        assert_eq!(
            canonicalize(b"ok\xff"),
            Err(CanonError::InvalidUtf8 { valid_up_to: 2 })
        );
    }

    #[test]
    fn known_digest_of_a_single_line() {
        // sha256("a\n")
        assert_eq!(
            digest(b"a").unwrap(),
            "87428fc522803d31065e7bce3cf03fe475096631e5e07bbd7a0fde60c4cf25c7"
        );
    }
}
