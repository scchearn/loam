//! Minimal JSON reader.
//!
//! It exists so the release-version gate can read plugin manifests without a
//! `python3` runtime and the checkpoint digest can read `hcom`/`task` output
//! without `jq`. Both were host-tool dependencies that do not exist on a stock
//! Windows machine.
//!
//! Numbers keep their literal source text rather than becoming floats, so
//! message identifiers round-trip exactly as they were written.

use std::fmt;

/// Bounded so a hostile or corrupt document cannot exhaust the stack.
const MAX_DEPTH: usize = 64;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Number(String),
    String(String),
    Array(Vec<Value>),
    Object(Vec<(String, Value)>),
}

impl Value {
    /// The value at `key`, or `None` when this is not an object or the key is
    /// absent. Duplicate keys resolve to the first, matching serde_json's
    /// default and `jq`'s last-wins inverse only where it cannot matter here.
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Object(entries) => entries
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value),
            _ => None,
        }
    }

    pub fn at(&self, index: usize) -> Option<&Value> {
        match self {
            Value::Array(items) => items.get(index),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Value::Array(items) => Some(items),
            _ => None,
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// Rendered the way `jq -r` renders a scalar in string interpolation.
    pub fn render(&self) -> String {
        match self {
            Value::Null => "null".to_owned(),
            Value::Bool(true) => "true".to_owned(),
            Value::Bool(false) => "false".to_owned(),
            Value::Number(literal) => literal.clone(),
            Value::String(value) => value.clone(),
            Value::Array(_) | Value::Object(_) => String::new(),
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.render())
    }
}

pub fn parse(input: &str) -> Result<Value, String> {
    let mut parser = Parser {
        bytes: input.as_bytes(),
        position: 0,
    };
    parser.skip_whitespace();
    let value = parser.value(0)?;
    parser.skip_whitespace();
    if parser.position != parser.bytes.len() {
        return Err(format!("trailing input at byte {}", parser.position));
    }
    Ok(value)
}

/// Parses a stream of whitespace-separated JSON values, the shape `hcom events`
/// emits. A single top-level array is flattened, so both framings work.
pub fn parse_stream(input: &str) -> Result<Vec<Value>, String> {
    let mut parser = Parser {
        bytes: input.as_bytes(),
        position: 0,
    };
    let mut values = Vec::new();
    loop {
        parser.skip_whitespace();
        if parser.position == parser.bytes.len() {
            break;
        }
        values.push(parser.value(0)?);
    }
    if values.len() == 1 {
        if let Value::Array(items) = &values[0] {
            return Ok(items.clone());
        }
    }
    Ok(values)
}

struct Parser<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl Parser<'_> {
    fn skip_whitespace(&mut self) {
        while let Some(byte) = self.bytes.get(self.position) {
            match byte {
                b' ' | b'\t' | b'\n' | b'\r' => self.position += 1,
                _ => break,
            }
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.position).copied()
    }

    fn expect(&mut self, byte: u8) -> Result<(), String> {
        if self.peek() == Some(byte) {
            self.position += 1;
            return Ok(());
        }
        Err(format!(
            "expected '{}' at byte {}",
            byte as char, self.position
        ))
    }

    fn literal(&mut self, word: &str) -> Result<(), String> {
        if self.bytes[self.position..].starts_with(word.as_bytes()) {
            self.position += word.len();
            return Ok(());
        }
        Err(format!("expected '{word}' at byte {}", self.position))
    }

    fn value(&mut self, depth: usize) -> Result<Value, String> {
        if depth > MAX_DEPTH {
            return Err(format!("nesting deeper than {MAX_DEPTH} levels"));
        }
        match self.peek() {
            Some(b'{') => self.object(depth),
            Some(b'[') => self.array(depth),
            Some(b'"') => Ok(Value::String(self.string()?)),
            Some(b't') => self.literal("true").map(|()| Value::Bool(true)),
            Some(b'f') => self.literal("false").map(|()| Value::Bool(false)),
            Some(b'n') => self.literal("null").map(|()| Value::Null),
            Some(_) => self.number(),
            None => Err("unexpected end of input".to_owned()),
        }
    }

    fn object(&mut self, depth: usize) -> Result<Value, String> {
        self.expect(b'{')?;
        let mut entries = Vec::new();
        self.skip_whitespace();
        if self.peek() == Some(b'}') {
            self.position += 1;
            return Ok(Value::Object(entries));
        }
        loop {
            self.skip_whitespace();
            let key = self.string()?;
            self.skip_whitespace();
            self.expect(b':')?;
            self.skip_whitespace();
            let value = self.value(depth + 1)?;
            entries.push((key, value));
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => self.position += 1,
                Some(b'}') => {
                    self.position += 1;
                    return Ok(Value::Object(entries));
                }
                _ => return Err(format!("expected ',' or '}}' at byte {}", self.position)),
            }
        }
    }

    fn array(&mut self, depth: usize) -> Result<Value, String> {
        self.expect(b'[')?;
        let mut items = Vec::new();
        self.skip_whitespace();
        if self.peek() == Some(b']') {
            self.position += 1;
            return Ok(Value::Array(items));
        }
        loop {
            self.skip_whitespace();
            items.push(self.value(depth + 1)?);
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => self.position += 1,
                Some(b']') => {
                    self.position += 1;
                    return Ok(Value::Array(items));
                }
                _ => return Err(format!("expected ',' or ']' at byte {}", self.position)),
            }
        }
    }

    fn string(&mut self) -> Result<String, String> {
        self.expect(b'"')?;
        let mut output = String::new();
        loop {
            let Some(byte) = self.peek() else {
                return Err("unterminated string".to_owned());
            };
            self.position += 1;
            match byte {
                b'"' => return Ok(output),
                b'\\' => {
                    let Some(escape) = self.peek() else {
                        return Err("unterminated escape".to_owned());
                    };
                    self.position += 1;
                    match escape {
                        b'"' => output.push('"'),
                        b'\\' => output.push('\\'),
                        b'/' => output.push('/'),
                        b'b' => output.push('\u{0008}'),
                        b'f' => output.push('\u{000c}'),
                        b'n' => output.push('\n'),
                        b'r' => output.push('\r'),
                        b't' => output.push('\t'),
                        b'u' => output.push(self.unicode_escape()?),
                        other => {
                            return Err(format!("unknown escape '\\{}'", other as char));
                        }
                    }
                }
                _ => {
                    // Copy the whole UTF-8 sequence: the input was a &str, so
                    // the boundaries are already known-good.
                    let start = self.position - 1;
                    let width = utf8_width(byte);
                    if start + width > self.bytes.len() {
                        return Err("truncated UTF-8 sequence".to_owned());
                    }
                    self.position = start + width;
                    match std::str::from_utf8(&self.bytes[start..self.position]) {
                        Ok(text) => output.push_str(text),
                        Err(_) => return Err("invalid UTF-8 in string".to_owned()),
                    }
                }
            }
        }
    }

    fn unicode_escape(&mut self) -> Result<char, String> {
        let first = self.hex4()?;
        // A leading surrogate is only meaningful paired with its trailing half.
        if (0xd800..0xdc00).contains(&first) {
            if self.peek() == Some(b'\\') && self.bytes.get(self.position + 1) == Some(&b'u') {
                self.position += 2;
                let second = self.hex4()?;
                if (0xdc00..0xe000).contains(&second) {
                    let combined =
                        0x10000 + (((first - 0xd800) as u32) << 10) + (second - 0xdc00) as u32;
                    return char::from_u32(combined)
                        .ok_or_else(|| "invalid surrogate pair".to_owned());
                }
                return Err("unpaired leading surrogate".to_owned());
            }
            return Err("unpaired leading surrogate".to_owned());
        }
        char::from_u32(first as u32).ok_or_else(|| "invalid \\u escape".to_owned())
    }

    fn hex4(&mut self) -> Result<u16, String> {
        if self.position + 4 > self.bytes.len() {
            return Err("truncated \\u escape".to_owned());
        }
        let mut value: u16 = 0;
        for offset in 0..4 {
            let byte = self.bytes[self.position + offset];
            let digit = match byte {
                b'0'..=b'9' => byte - b'0',
                b'a'..=b'f' => byte - b'a' + 10,
                b'A'..=b'F' => byte - b'A' + 10,
                _ => return Err("invalid hex digit in \\u escape".to_owned()),
            };
            value = value * 16 + digit as u16;
        }
        self.position += 4;
        Ok(value)
    }

    fn number(&mut self) -> Result<Value, String> {
        let start = self.position;
        if self.peek() == Some(b'-') {
            self.position += 1;
        }
        let digits_start = self.position;
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.position += 1;
        }
        if self.position == digits_start {
            return Err(format!("expected a value at byte {start}"));
        }
        if self.peek() == Some(b'.') {
            self.position += 1;
            let fraction_start = self.position;
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.position += 1;
            }
            if self.position == fraction_start {
                return Err(format!("truncated fraction at byte {}", self.position));
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.position += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.position += 1;
            }
            let exponent_start = self.position;
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.position += 1;
            }
            if self.position == exponent_start {
                return Err(format!("truncated exponent at byte {}", self.position));
            }
        }
        Ok(Value::Number(
            String::from_utf8_lossy(&self.bytes[start..self.position]).into_owned(),
        ))
    }
}

fn utf8_width(byte: u8) -> usize {
    match byte {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        _ => 4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_nested_object_and_array_paths() {
        let value = parse(
            r#"{"name":"loam","metadata":{"version":"0.8.2"},"plugins":[{"version":"0.8.2"}]}"#,
        )
        .expect("document should parse");

        assert_eq!(value.get("name").and_then(Value::as_str), Some("loam"));
        assert_eq!(
            value
                .get("metadata")
                .and_then(|meta| meta.get("version"))
                .and_then(Value::as_str),
            Some("0.8.2")
        );
        assert_eq!(
            value
                .get("plugins")
                .and_then(|plugins| plugins.at(0))
                .and_then(|plugin| plugin.get("version"))
                .and_then(Value::as_str),
            Some("0.8.2")
        );
    }

    #[test]
    fn keeps_integer_literals_exact() {
        let value = parse(r#"{"id":67948,"ratio":1.5e3}"#).expect("document should parse");

        assert_eq!(value.get("id").map(Value::render), Some("67948".to_owned()));
        assert_eq!(
            value.get("ratio").map(Value::render),
            Some("1.5e3".to_owned())
        );
    }

    #[test]
    fn renders_absent_and_null_like_jq() {
        let value = parse(r#"{"intent":null}"#).expect("document should parse");

        assert_eq!(
            value.get("intent").map(Value::render),
            Some("null".to_owned())
        );
        assert!(value.get("intent").is_some_and(Value::is_null));
        assert_eq!(value.get("missing"), None);
    }

    #[test]
    fn decodes_escapes_and_surrogate_pairs() {
        let value = parse(r#"{"text":"a\"b\\c\ndé🚀"}"#).expect("document should parse");

        assert_eq!(
            value.get("text").and_then(Value::as_str),
            Some("a\"b\\c\nd\u{e9}\u{1f680}")
        );
    }

    #[test]
    fn preserves_multibyte_text_verbatim() {
        let value =
            parse("{\"text\":\"caf\u{e9} \u{2014} \u{1f680}\"}").expect("document should parse");

        assert_eq!(
            value.get("text").and_then(Value::as_str),
            Some("caf\u{e9} \u{2014} \u{1f680}")
        );
    }

    #[test]
    fn rejects_malformed_documents() {
        for input in [
            "{",
            "{\"a\"}",
            "{\"a\":}",
            "[1,]",
            "tru",
            "{\"a\":1}{",
            "\"unterminated",
            "01x",
        ] {
            assert!(parse(input).is_err(), "input should be rejected: {input}");
        }
    }

    #[test]
    fn rejects_runaway_nesting() {
        let input = "[".repeat(MAX_DEPTH + 5);

        assert!(parse(&input).is_err());
    }

    #[test]
    fn reads_a_newline_delimited_stream() {
        let values = parse_stream("{\"id\":1}\n{\"id\":2}\n").expect("stream should parse");

        assert_eq!(values.len(), 2);
        assert_eq!(values[1].get("id").map(Value::render), Some("2".to_owned()));
    }

    #[test]
    fn flattens_a_single_top_level_array() {
        let values = parse_stream("[{\"id\":1},{\"id\":2}]").expect("stream should parse");

        assert_eq!(values.len(), 2);
        assert_eq!(values[0].get("id").map(Value::render), Some("1".to_owned()));
    }

    #[test]
    fn an_empty_stream_yields_nothing() {
        assert_eq!(
            parse_stream("  \n ").expect("empty stream is valid"),
            vec![]
        );
    }
}
