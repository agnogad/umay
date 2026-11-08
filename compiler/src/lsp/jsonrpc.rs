//! Just enough JSON to speak JSON-RPC.
//!
//! A server's job is to read a request and write a reply, and both are small
//! documents. This module reads them into a `Value` and writes them back out.
//! It is deliberately not general: there are no numbers with exponents to
//! preserve, no duplicate keys to merge, and nothing here is a parser for a
//! format other than the one the editor is about to send.

use std::collections::BTreeMap;

/// A JSON value.
///
/// Objects keep their keys sorted so that two equal documents serialize
/// identically, which makes the tests comparable as text.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    /// JSON has one number type; LSP uses integers for ids and line numbers.
    Number(f64),
    String(String),
    Array(Vec<Value>),
    Object(BTreeMap<String, Value>),
}

impl Value {
    pub fn object(fields: impl IntoIterator<Item = (impl Into<String>, Value)>) -> Value {
        Value::Object(fields.into_iter().map(|(k, v)| (k.into(), v)).collect())
    }

    pub fn array(items: impl IntoIterator<Item = Value>) -> Value {
        Value::Array(items.into_iter().collect())
    }

    pub fn str(text: impl Into<String>) -> Value {
        Value::String(text.into())
    }

    pub fn int(n: impl Into<i64>) -> Value {
        Value::Number(n.into() as f64)
    }

    /// Looks a key up in an object. `None` for a non-object or a missing key.
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Object(fields) => fields.get(key),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Value::Number(n) => Some(*n as i64),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Value::Array(items) => Some(items),
            _ => None,
        }
    }

    pub fn as_object(&self) -> Option<&BTreeMap<String, Value>> {
        match self {
            Value::Object(fields) => Some(fields),
            _ => None,
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }
}

/// Parses one JSON document.
pub fn parse(text: &str) -> Result<Value, String> {
    let mut p = Parser { bytes: text.as_bytes(), pos: 0, depth: 0 };
    let value = p.value()?;
    p.skip_space();
    if p.pos != p.bytes.len() {
        return Err(format!("trailing text at byte {}", p.pos));
    }
    Ok(value)
}

/// A nesting limit, so that a malformed message cannot exhaust the stack.
const MAX_DEPTH: u32 = 128;

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
    depth: u32,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn skip_space(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn expect(&mut self, byte: u8) -> Result<(), String> {
        if self.peek() == Some(byte) {
            self.pos += 1;
            Ok(())
        } else {
            Err(format!("expected {:?} at byte {}", byte as char, self.pos))
        }
    }

    fn value(&mut self) -> Result<Value, String> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err("nested too deeply".to_string());
        }
        let result = self.value_inner();
        self.depth -= 1;
        result
    }

    fn value_inner(&mut self) -> Result<Value, String> {
        self.skip_space();
        match self.peek() {
            None => Err("unexpected end of input".to_string()),
            Some(b'{') => self.object(),
            Some(b'[') => self.array(),
            Some(b'"') => self.string().map(Value::String),
            Some(b't') => self.literal("true", Value::Bool(true)),
            Some(b'f') => self.literal("false", Value::Bool(false)),
            Some(b'n') => self.literal("null", Value::Null),
            Some(c) if c == b'-' || c.is_ascii_digit() => self.number(),
            Some(c) => Err(format!("unexpected {:?} at byte {}", c as char, self.pos)),
        }
    }

    fn literal(&mut self, word: &str, value: Value) -> Result<Value, String> {
        if self.bytes[self.pos..].starts_with(word.as_bytes()) {
            self.pos += word.len();
            Ok(value)
        } else {
            Err(format!("expected `{word}` at byte {}", self.pos))
        }
    }

    fn number(&mut self) -> Result<Value, String> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
            self.pos += 1;
        }
        if self.peek() == Some(b'.') {
            self.pos += 1;
            while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                self.pos += 1;
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.pos += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                self.pos += 1;
            }
        }
        let text = std::str::from_utf8(&self.bytes[start..self.pos]).map_err(|e| e.to_string())?;
        text.parse().map(Value::Number).map_err(|e| format!("bad number `{text}`: {e}"))
    }

    fn string(&mut self) -> Result<String, String> {
        self.expect(b'"')?;
        let mut out = String::new();
        loop {
            let byte = self.peek().ok_or("unterminated string")?;
            self.pos += 1;
            match byte {
                b'"' => return Ok(out),
                b'\\' => {
                    let escape = self.peek().ok_or("unterminated escape")?;
                    self.pos += 1;
                    match escape {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let code = self.hex4()?;
                            // A surrogate pair arrives as two escapes; the low one
                            // is consumed here so the pair becomes one char.
                            if (0xD800..0xDC00).contains(&code) {
                                if self.bytes.get(self.pos..self.pos + 2) == Some(b"\\u") {
                                    self.pos += 2;
                                    let low = self.hex4()?;
                                    if (0xDC00..0xE000).contains(&low) {
                                        let combined =
                                            0x10000 + ((code - 0xD800) << 10) + (low - 0xDC00);
                                        out.push(char::from_u32(combined).unwrap_or('\u{fffd}'));
                                        continue;
                                    }
                                    out.push('\u{fffd}');
                                    out.push(char::from_u32(low).unwrap_or('\u{fffd}'));
                                    continue;
                                }
                                out.push('\u{fffd}');
                                continue;
                            }
                            out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                        }
                        other => return Err(format!("unknown escape \\{}", other as char)),
                    }
                }
                // Read the whole UTF-8 sequence, not one byte at a time: an
                // identifier with an accent would otherwise be mangled.
                byte if byte < 0x80 => out.push(byte as char),
                _ => {
                    let start = self.pos - 1;
                    let width = utf8_width(byte);
                    self.pos = start + width;
                    let text = std::str::from_utf8(&self.bytes[start..self.pos])
                        .map_err(|_| "invalid UTF-8 in string".to_string())?;
                    out.push_str(text);
                }
            }
        }
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let digits = self.bytes.get(self.pos..self.pos + 4).ok_or("short \\u escape")?;
        let text = std::str::from_utf8(digits).map_err(|e| e.to_string())?;
        let code = u32::from_str_radix(text, 16).map_err(|e| format!("bad \\u escape: {e}"))?;
        self.pos += 4;
        Ok(code)
    }

    fn array(&mut self) -> Result<Value, String> {
        self.expect(b'[')?;
        let mut items = Vec::new();
        self.skip_space();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(Value::Array(items));
        }
        loop {
            items.push(self.value()?);
            self.skip_space();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b']') => {
                    self.pos += 1;
                    return Ok(Value::Array(items));
                }
                _ => return Err(format!("expected `,` or `]` at byte {}", self.pos)),
            }
        }
    }

    fn object(&mut self) -> Result<Value, String> {
        self.expect(b'{')?;
        let mut fields = BTreeMap::new();
        self.skip_space();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(Value::Object(fields));
        }
        loop {
            self.skip_space();
            let key = self.string()?;
            self.skip_space();
            self.expect(b':')?;
            let value = self.value()?;
            fields.insert(key, value);
            self.skip_space();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(Value::Object(fields));
                }
                _ => return Err(format!("expected `,` or `}}` at byte {}", self.pos)),
            }
        }
    }
}

fn utf8_width(first: u8) -> usize {
    match first {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        _ => 4,
    }
}

/// Serializes a value as compact JSON.
pub fn to_string(value: &Value) -> String {
    let mut out = String::new();
    write_value(value, &mut out);
    out
}

fn write_value(value: &Value, out: &mut String) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(n) => {
            // LSP ids and line numbers are integers, and `1` reads better than
            // `1.0` in a log.
            if n.fract() == 0.0 && n.abs() < 1e15 {
                out.push_str(&format!("{}", *n as i64));
            } else {
                out.push_str(&format!("{n}"));
            }
        }
        Value::String(s) => write_string(s, out),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_value(item, out);
            }
            out.push(']');
        }
        Value::Object(fields) => {
            out.push('{');
            for (i, (key, value)) in fields.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_string(key, out);
                out.push(':');
                write_value(value, out);
            }
            out.push('}');
        }
    }
}

fn write_string(text: &str, out: &mut String) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

// ---------------------------------------------------------------------------
// Framing
// ---------------------------------------------------------------------------

/// One message off the wire: the bytes of the body, without its headers.
pub fn frame(body: &str) -> Vec<u8> {
    let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(body.as_bytes());
    out
}

/// Reads one framed message.
///
/// Returns `Ok(None)` at end of input, which is how an editor says goodbye: it
/// closes the pipe rather than sending a shutdown message. Reading headers until
/// the blank line, then exactly `Content-Length` bytes, is the whole protocol.
pub fn read_frame(input: &mut impl std::io::BufRead) -> std::io::Result<Option<String>> {
    let mut length: Option<usize> = None;
    let mut saw_header = false;
    loop {
        let mut line = String::new();
        let read = input.read_line(&mut line)?;
        if read == 0 {
            return Ok(None);
        }
        saw_header = true;
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            // The blank line ends the headers.
            break;
        }
        if let Some(rest) = trimmed.strip_prefix("Content-Length:") {
            length = rest.trim().parse().ok();
        }
        // Every other header (Content-Type) is allowed and ignored.
    }
    if !saw_header {
        return Ok(None);
    }
    let Some(length) = length else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "message had no Content-Length header",
        ));
    };
    let mut body = vec![0u8; length];
    std::io::Read::read_exact(input, &mut body)?;
    Ok(Some(String::from_utf8_lossy(&body).into_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(text: &str) {
        let parsed = parse(text).expect("should parse");
        let written = to_string(&parsed);
        let again = parse(&written).expect("should parse its own output");
        assert_eq!(parsed, again);
    }

    #[test]
    fn reads_and_writes_the_documents_lsp_uses() {
        round_trip(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#);
        round_trip(r#"{"id":null,"result":{"capabilities":{"hoverProvider":true}}}"#);
        round_trip(r#"[[1,2],[3,-4.5,1e3],true,false,null]"#);
    }

    #[test]
    fn escapes_survive_a_round_trip() {
        let parsed = parse(r#""a\"b\\c\nd\te\u00e9""#).expect("should parse");
        assert_eq!(parsed.as_str(), Some("a\"b\\c\nd\teé"));
        round_trip(r#""a\"b\\c\nd\te\u00e9""#);
    }

    #[test]
    fn a_surrogate_pair_becomes_one_character() {
        // U+1F600, written the way the protocol writes it.
        let parsed = parse(r#""\ud83d\ude00""#).expect("should parse");
        assert_eq!(parsed.as_str(), Some("\u{1f600}"));
    }

    #[test]
    fn multi_byte_text_survives() {
        round_trip(r#"{"text":"fn café(ñ: i64) -> i64 {}"}"#);
    }

    #[test]
    fn malformed_input_is_an_error_not_a_panic() {
        for bad in ["", "{", "[1,", "{\"a\"}", "\"unterminated", "{}extra", "tru"] {
            assert!(parse(bad).is_err(), "`{bad}` should not parse");
        }
    }

    #[test]
    fn deep_nesting_is_refused() {
        let deep = format!("{}{}", "[".repeat(200), "]".repeat(200));
        assert!(parse(&deep).is_err(), "nesting should be limited");
    }

    #[test]
    fn an_integer_prints_without_a_decimal_point() {
        let value = Value::int(42);
        assert_eq!(to_string(&value), "42");
    }

    #[test]
    fn a_message_round_trips_through_its_frame() {
        let body = r#"{"jsonrpc":"2.0","id":1,"method":"shutdown"}"#;
        let bytes = frame(body);
        let mut reader = std::io::BufReader::new(&bytes[..]);
        let read = read_frame(&mut reader).expect("read").expect("a message");
        assert_eq!(read, body);
    }

    #[test]
    fn the_content_length_is_the_byte_count_not_the_char_count() {
        // A body with a multi-byte character is longer in bytes than in chars,
        // and the header counts bytes. Getting this wrong truncates the message
        // and the next read starts mid-document.
        let body = r#"{"text":"é"}"#;
        let bytes = frame(body);
        let header = String::from_utf8_lossy(&bytes);
        assert!(
            header.starts_with(&format!("Content-Length: {}\r\n", body.len())),
            "got header {header:?}"
        );
        let mut reader = std::io::BufReader::new(&bytes[..]);
        assert_eq!(read_frame(&mut reader).unwrap().unwrap(), body);
    }

    #[test]
    fn two_messages_in_a_row_are_read_one_each() {
        let mut bytes = frame("{\"a\":1}");
        bytes.extend(frame("{\"b\":2}"));
        let mut reader = std::io::BufReader::new(&bytes[..]);
        assert_eq!(read_frame(&mut reader).unwrap().unwrap(), "{\"a\":1}");
        assert_eq!(read_frame(&mut reader).unwrap().unwrap(), "{\"b\":2}");
        assert_eq!(read_frame(&mut reader).unwrap(), None, "end of input");
    }

    #[test]
    fn end_of_input_is_not_an_error() {
        let empty: Vec<u8> = Vec::new();
        let mut reader = std::io::BufReader::new(&empty[..]);
        assert_eq!(read_frame(&mut reader).unwrap(), None);
    }

    #[test]
    fn a_message_without_a_length_is_rejected() {
        let bytes = b"Content-Type: application/json\r\n\r\n{}".to_vec();
        let mut reader = std::io::BufReader::new(&bytes[..]);
        assert!(read_frame(&mut reader).is_err());
    }
}
