//! Lexer: turns source text into a stream of tokens.
//!
//! The lexer is deliberately simple and dependency-free. All errors are
//! reported through a [`LexError`] list; the token stream is still produced so
//! the parser can recover and report more issues.

use crate::diagnostics::{Diagnostic, Severity};
use crate::source::{FileId, Span};
use crate::token::{FloatSuffix, IntSuffix, Keyword, Token, TokenKind};

/// A single lexing failure with its span.
#[derive(Debug)]
pub struct LexError {
    pub code: &'static str,
    pub message: String,
    pub span: Span,
}

pub struct Lexer<'a> {
    src: &'a str,
    file: FileId,
    /// Byte offset of the current position.
    pos: usize,
    tokens: Vec<Token>,
    errors: Vec<LexError>,
    /// Whether a `{` (interpolation) was left unclosed at EOF.
    pending_interp_brace: bool,
}

pub fn tokenize(src: &str, file: FileId) -> (Vec<Token>, Vec<LexError>) {
    let mut lx = Lexer { src, file, pos: 0, tokens: Vec::new(), errors: Vec::new(), pending_interp_brace: false };
    lx.run();
    assign_lines(src, &mut lx.tokens);
    (lx.tokens, lx.errors)
}

/// Fills in the `line` field of every token. A single forward pass over the
/// source keeps this O(n) and independent of UTF-8 width.
fn assign_lines(src: &str, tokens: &mut [Token]) {
    let mut line = 1u32;
    let mut scanned = 0usize;
    for tok in tokens.iter_mut() {
        let target = tok.span.start as usize;
        while scanned < target && scanned < src.len() {
            if src.as_bytes()[scanned] == b'\n' {
                line += 1;
            }
            scanned += 1;
        }
        tok.line = line;
    }
}

impl<'a> Lexer<'a> {
    fn peek(&self) -> Option<char> {
        self.src[self.pos..].chars().next()
    }

    fn peek_at(&self, n: usize) -> Option<char> {
        self.src[self.pos..].chars().nth(n)
    }

    fn peek2(&self) -> Option<char> {
        self.peek_at(1)
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += c.len_utf8();
        Some(c)
    }

    fn at(&self, text: &str) -> bool {
        self.src[self.pos..].starts_with(text)
    }

    fn span_from(&self, start: usize) -> Span {
        Span::new(self.file, start as u32, self.pos as u32)
    }

    fn push(&mut self, kind: TokenKind, start: usize) {
        self.tokens.push(Token::new(kind, self.span_from(start)));
    }

    fn error(&mut self, code: &'static str, message: impl Into<String>, start: usize) {
        self.errors.push(LexError { code, message: message.into(), span: self.span_from(start) });
    }

    fn run(&mut self) {
        while let Some(c) = self.peek() {
            let start = self.pos;
            match c {
                c if c.is_whitespace() => {
                    self.bump();
                }
                '/' if self.at("//") => self.line_comment(),
                '/' if self.at("/*") => self.block_comment(start),
                '/' => {
                    self.bump();
                    if self.at("=") {
                        self.bump();
                        self.push(TokenKind::SlashEq, start);
                    } else {
                        self.push(TokenKind::Slash, start);
                    }
                }
                '"' => self.string(start),
                '\'' => self.char(start),
                c if c.is_ascii_digit() => self.number(start),
                c if is_ident_start(c) => self.ident_or_keyword(start),
                _ => self.punct(start),
            }
        }
        self.push(TokenKind::Eof, self.pos);
    }

    fn line_comment(&mut self) {
        while let Some(_c) = self.peek() {
            if self.peek() == Some('\n') {
                break;
            }
            self.bump();
        }
    }

    fn block_comment(&mut self, start: usize) {
        // consume "/*"
        self.bump();
        self.bump();
        let mut depth = 1usize;
        while self.peek().is_some() {
            if self.at("/*") {
                depth += 1;
                self.bump();
                self.bump();
            } else if self.at("*/") {
                depth -= 1;
                self.bump();
                self.bump();
                if depth == 0 {
                    return;
                }
            } else {
                self.bump();
            }
        }
        self.error("E0004", "unterminated block comment", start);
    }

    fn string(&mut self, start: usize) {
        // consume the opening quote
        self.bump();
        let mut interp = false;
        let mut parts: Vec<crate::token::StrPart> = Vec::new();
        // Literal text accumulated since the last interpolation boundary.
        let mut text_buf = String::new();

        loop {
            match self.peek() {
                None => {
                    self.error("E0002", "unterminated string literal", start);
                    break;
                }
                Some('"') => {
                    self.bump();
                    break;
                }
                Some('\\') => match self.escape() {
                    Some(ch) => text_buf.push(ch),
                    None => text_buf.push('\\'),
                },
                Some('{') if !interp && self.peek2() == Some('{') => {
                    // `{{` is a literal brace, not an interpolation.
                    self.bump();
                    self.bump();
                    text_buf.push('{');
                }
                Some('}') if !interp && self.peek2() == Some('}') => {
                    // `}}` is a literal brace.
                    self.bump();
                    self.bump();
                    text_buf.push('}');
                }
                Some('{') => {
                    interp = true;
                    self.bump();
                    let expr_start = self.pos;
                    let mut depth = 1usize;
                    loop {
                        match self.peek() {
                            None => {
                                self.error("E0003", "unterminated interpolation expression", start);
                                self.pending_interp_brace = true;
                                return;
                            }
                            Some('{') => {
                                depth += 1;
                                self.bump();
                            }
                            Some('}') => {
                                depth -= 1;
                                if depth == 0 {
                                    self.bump();
                                    break;
                                }
                                self.bump();
                            }
                            Some(_) => {
                                self.bump();
                            }
                        }
                    }
                    let expr_src = self.src[expr_start..self.pos - 1].to_string();
                    parts.push(crate::token::StrPart::Text(std::mem::take(&mut text_buf)));
                    parts.push(crate::token::StrPart::Expr {
                        src: expr_src,
                        span: Span::new(self.file, expr_start as u32, (self.pos - 1) as u32),
                    });
                }
                Some(ch) => {
                    self.bump();
                    text_buf.push(ch);
                }
            }
        }

        if interp {
            if !text_buf.is_empty() {
                parts.push(crate::token::StrPart::Text(text_buf));
            }
            self.push(TokenKind::StrInterp(parts), start);
        } else {
            self.push(TokenKind::Str(text_buf), start);
        }
    }

    /// Handles an escape sequence after a consumed `\`. Returns the resolved
    /// character, or `None` if the escape was invalid (a diagnostic is added).
    fn escape(&mut self) -> Option<char> {
        // consume backslash
        self.bump();
        match self.bump() {
            None => {
                self.error("E0002", "unterminated string literal", self.pos.saturating_sub(1));
                None
            }
            Some('n') => Some('\n'),
            Some('t') => Some('\t'),
            Some('r') => Some('\r'),
            Some('0') => Some('\0'),
            Some('\\') => Some('\\'),
            Some('"') => Some('"'),
            Some('\'') => Some('\''),
            Some('{') => Some('{'),
            Some('}') => Some('}'),
            Some('x') => self.hex_escape(2),
            Some('u') => self.unicode_escape(),
            Some(other) => {
                let start = self.pos.saturating_sub(2);
                self.error("E0007", format!("unknown escape sequence `\\{other}`"), start);
                None
            }
        }
    }

    fn hex_escape(&mut self, digits: usize) -> Option<char> {
        let mut value = 0u32;
        for _ in 0..digits {
            match self.peek().and_then(|c| c.to_digit(16)) {
                Some(d) => {
                    value = value * 16 + d;
                    self.bump();
                }
                None => {
                    self.error("E0007", "invalid hexadecimal escape", self.pos);
                    return None;
                }
            }
        }
        char::from_u32(value)
    }

    fn unicode_escape(&mut self) -> Option<char> {
        if self.bump() != Some('{') {
            self.error("E0007", "expected `{` after `\\u`", self.pos);
            return None;
        }
        let mut value = 0u32;
        let start = self.pos;
        let mut any = false;
        loop {
            match self.peek() {
                Some('}') => {
                    self.bump();
                    break;
                }
                Some(c) if c.is_ascii_hexdigit() => {
                    value = value.saturating_mul(16).saturating_add(c.to_digit(16).unwrap());
                    any = true;
                    self.bump();
                }
                Some(_) => {
                    self.error("E0007", "invalid unicode escape", start);
                    return None;
                }
                None => {
                    self.error("E0007", "unterminated unicode escape", start);
                    return None;
                }
            }
        }
        if !any {
            self.error("E0007", "empty unicode escape", start);
            return None;
        }
        char::from_u32(value).or_else(|| {
            self.error("E0007", "invalid unicode scalar value", start);
            None
        })
    }

    fn char(&mut self, start: usize) {
        // consume opening quote
        self.bump();
        let ch = match self.peek() {
            None => {
                self.error("E0005", "unterminated character literal", start);
                return;
            }
            Some('\'') => {
                self.error("E0005", "empty character literal", start);
                self.bump();
                return;
            }
            Some('\\') => match self.escape() {
                Some(c) => c,
                None => {
                    // Skip to the closing quote to avoid cascades.
                    while let Some(c) = self.peek() {
                        if c == '\'' {
                            break;
                        }
                        self.bump();
                    }
                    return;
                }
            },
            Some(c) => {
                self.bump();
                c
            }
        };
        match self.peek() {
            Some('\'') => {
                self.bump();
                self.push(TokenKind::Char(ch), start);
            }
            Some(_) => {
                let extra = self.bump().unwrap();
                self.error("E0005", format!("character literal has too many characters (found `{ch}{extra}`)"), start);
                // Try to recover: skip to the next quote.
                while let Some(c) = self.peek() {
                    if c == '\'' {
                        self.bump();
                        break;
                    }
                    self.bump();
                }
            }
            None => {
                self.error("E0005", "unterminated character literal", start);
            }
        }
    }

    fn number(&mut self, start: usize) {
        let mut text = String::new();
        // Detect radix prefixes.
        if self.peek() == Some('0') {
            if let Some(radix_char) = self.peek2() {
                if matches!(radix_char, 'x' | 'X' | 'b' | 'B' | 'o' | 'O') {
                    let radix = match radix_char {
                        'x' | 'X' => 16,
                        'b' | 'B' => 2,
                        _ => 8,
                    };
                    self.bump();
                    self.bump();
                    let mut value = 0u128;
                    let mut any = false;
                    while let Some(c) = self.peek() {
                        if c == '_' {
                            self.bump();
                            continue;
                        }
                        match c.to_digit(radix) {
                            Some(d) => {
                                any = true;
                                value = value.saturating_mul(radix as u128).saturating_add(d as u128);
                                self.bump();
                            }
                            None => break,
                        }
                    }
                    if !any {
                        self.error("E0006", format!("empty digits in {radix}-base literal"), start);
                    }
                    let suffix = self.suffix();
                    if matches!(self.peek(), Some(c) if c.is_ascii_alphabetic() || c == '_') {
                        self.error("E0006", "unexpected trailing characters after numeric literal", start);
                    }
                    let span_kind = TokenKind::Int { value, suffix };
                    self.tokens.push(Token::new(span_kind, self.span_from(start)));
                    return;
                }
            }
        }

        let mut is_float = false;
        while let Some(c) = self.peek() {
            match c {
                '0'..='9' | '_' => {
                    text.push(c);
                    self.bump();
                }
                '.' if self.peek2().map(|c| c.is_ascii_digit()).unwrap_or(false) && !is_float => {
                    is_float = true;
                    text.push('.');
                    self.bump();
                }
                _ => break,
            }
        }
        if !is_float {
            let mut clean = String::with_capacity(text.len());
            for c in text.chars() {
                if c != '_' {
                    clean.push(c);
                }
            }
            if let Ok(value) = clean.parse::<u128>() {
                let suffix = self.suffix();
                if matches!(self.peek(), Some(c) if c.is_ascii_alphabetic() || c == '_') {
                    self.error("E0006", "unexpected trailing characters after numeric literal", start);
                }
                self.tokens.push(Token::new(TokenKind::Int { value, suffix }, self.span_from(start)));
            } else {
                self.error("E0006", format!("integer literal `{clean}` is too large"), start);
                self.tokens.push(Token::new(TokenKind::Int { value: 0, suffix: None }, self.span_from(start)));
            }
        } else {
            let suffix = self.float_suffix();
            if matches!(self.peek(), Some(c) if c.is_ascii_alphabetic() || c == '_') {
                self.error("E0006", "unexpected trailing characters after numeric literal", start);
            }
            let mut clean = String::with_capacity(text.len());
            for c in text.chars() {
                if c != '_' {
                    clean.push(c);
                }
            }
            let value = clean.parse::<f64>().unwrap_or(f64::NAN);
            self.tokens.push(Token::new(TokenKind::Float { value, suffix, text: clean }, self.span_from(start)));
        }
    }

    fn suffix(&mut self) -> Option<IntSuffix> {
        let mut ident = String::new();
        while let Some(c) = self.peek() {
            if c.is_ascii_alphanumeric() || c == '_' {
                ident.push(c);
                self.bump();
            } else {
                break;
            }
        }
        if ident.is_empty() {
            return None;
        }
        if let Some(s) = IntSuffix::parse(&ident) {
            return Some(s);
        }
        // Could be a float suffix or garbage; leave it to the caller's trailing
        // character check. Return None.
        None
    }

    /// Consumes a float type suffix (`f32` / `f64`) if present.
    fn float_suffix(&mut self) -> Option<FloatSuffix> {
        let mut ident = String::new();
        while let Some(c) = self.peek() {
            if c.is_ascii_alphanumeric() {
                ident.push(c);
                self.bump();
            } else {
                break;
            }
        }
        match ident.as_str() {
            "f32" => Some(FloatSuffix::F32),
            "f64" => Some(FloatSuffix::F64),
            "" => None,
            _ => {
                // Unknown suffix: flag it (e.g. `1.5xyz`).
                self.error("E0006", format!("unknown float suffix `{ident}`"), self.pos.saturating_sub(ident.len()));
                None
            }
        }
    }

    fn ident_or_keyword(&mut self, start: usize) {
        let mut name = String::new();
        while let Some(c) = self.peek() {
            if is_ident_continue(c) {
                name.push(c);
                self.bump();
            } else {
                break;
            }
        }
        if let Some(kw) = Keyword::from_str(&name) {
            self.push(TokenKind::Keyword(kw), start);
        } else {
            self.push(TokenKind::Ident(name), start);
        }
    }

    fn punct(&mut self, start: usize) {
        let c = self.bump().unwrap();

        // Three-character operators are checked first so that `<<=` is never
        // split into `<<` + `=`.
        if let Some(next) = self.peek() {
            if let Some(third) = self.peek2() {
                let kind = match (c, next, third) {
                    ('<', '<', '=') => Some(TokenKind::ShlEq),
                    ('>', '>', '=') => Some(TokenKind::ShrEq),
                    _ => None,
                };
                if let Some(k) = kind {
                    self.bump();
                    self.bump();
                    self.push(k, start);
                    return;
                }
            }
        }

        // Two-character operators.
        if let Some(next) = self.peek() {
            let kind = match (c, next) {
                (':', ':') => Some(TokenKind::ColonColon),
                ('-', '>') => Some(TokenKind::Arrow),
                ('-', '=') => Some(TokenKind::MinusEq),
                ('+', '=') => Some(TokenKind::PlusEq),
                ('*', '=') => Some(TokenKind::StarEq),
                ('/', '=') => Some(TokenKind::SlashEq),
                ('%', '=') => Some(TokenKind::PercentEq),
                ('=', '=') => Some(TokenKind::EqEq),
                ('=', '>') => Some(TokenKind::FatArrow),
                ('!', '=') => Some(TokenKind::Ne),
                ('<', '<') => Some(TokenKind::Shl),
                ('<', '=') => Some(TokenKind::Le),
                ('>', '>') => Some(TokenKind::Shr),
                ('>', '=') => Some(TokenKind::Ge),
                ('&', '&') => Some(TokenKind::AmpAmp),
                ('&', '=') => Some(TokenKind::AmpEq),
                ('|', '|') => Some(TokenKind::PipePipe),
                ('|', '=') => Some(TokenKind::PipeEq),
                ('^', '=') => Some(TokenKind::CaretEq),
                _ => None,
            };
            if let Some(k) = kind {
                self.bump();
                self.push(k, start);
                return;
            }
        }

        // `..=` must be tested before `..`.
        if c == '.' && self.at("..=") {
            self.bump();
            self.bump();
            self.push(TokenKind::DotDotEq, start);
            return;
        }
        if c == '.' && self.at(".") {
            self.bump();
            self.push(TokenKind::DotDot, start);
            return;
        }

        let single = match c {
            '(' => TokenKind::LParen,
            ')' => TokenKind::RParen,
            '{' => TokenKind::LBrace,
            '}' => TokenKind::RBrace,
            '[' => TokenKind::LBracket,
            ']' => TokenKind::RBracket,
            ',' => TokenKind::Comma,
            ';' => TokenKind::Semi,
            ':' => TokenKind::Colon,
            '.' => TokenKind::Dot,
            '-' => TokenKind::Minus,
            '+' => TokenKind::Plus,
            '*' => TokenKind::Star,
            '%' => TokenKind::Percent,
            '=' => TokenKind::Eq,
            '!' => TokenKind::Bang,
            '<' => TokenKind::Lt,
            '>' => TokenKind::Gt,
            '&' => TokenKind::Amp,
            '|' => TokenKind::Pipe,
            '^' => TokenKind::Caret,
            '?' => TokenKind::Question,
            '@' => TokenKind::At,
            '#' => TokenKind::Hash,
            '_' => TokenKind::Underscore,
            other => {
                self.error("E0001", format!("unexpected character `{other}`"), start);
                TokenKind::Error
            }
        };
        self.push(single, start);
    }
}

fn is_ident_start(c: char) -> bool {
    c.is_alphabetic() || c == '_'
}

fn is_ident_continue(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Converts lexer errors into diagnostics (used by the driver / tests).
pub fn lex_errors_to_diagnostics(errors: Vec<LexError>) -> Vec<Diagnostic> {
    errors
        .into_iter()
        .map(|e| {
            // E0001 (unknown character) is recoverable; keep it an error since
            // the offending text is genuinely not part of UMAY.
            let sev = Severity::Error;
            let msg = e.message;
            let mut d = Diagnostic::new(e.code, sev, msg).at(e.span);
            match e.code {
                "E0001" => {
                    d = d.with_cause("the character is not valid UMAY syntax");
                }
                "E0002" => {
                    d = d.with_cause("a `\"` closing quote was never found");
                }
                "E0003" => {
                    d = d.with_cause("an interpolation `{...}` was not closed with `}`");
                }
                "E0004" => {
                    d = d.with_cause("a `/*` block comment was never closed with `*/`");
                }
                "E0005" => {
                    d = d.with_cause("a `'` character literal is malformed");
                }
                "E0006" => {
                    d = d.with_cause("the numeric literal is malformed or out of range");
                }
                "E0007" => {
                    d = d.with_cause("the escape sequence is not valid");
                }
                _ => {}
            }
            d
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::SourceMap;

    fn lex(src: &str) -> (Vec<Token>, Vec<LexError>) {
        let mut map = SourceMap::new();
        let id = map.add("test.um", src);
        tokenize(src, id)
    }

    fn kinds(src: &str) -> Vec<TokenKind> {
        let (tokens, _) = lex(src);
        tokens.into_iter().map(|t| t.kind).collect()
    }

    fn skip_errors(src: &str) -> Vec<TokenKind> {
        let mut ks = kinds(src);
        ks.retain(|k| *k != TokenKind::Error);
        ks
    }

    #[test]
    fn empty_source() {
        let ks = kinds("");
        assert_eq!(ks, vec![TokenKind::Eof]);
    }

    #[test]
    fn keywords_and_idents() {
        let ks = skip_errors("fn main() let mut struct x abc_2");
        assert_eq!(
            ks,
            vec![
                TokenKind::Keyword(Keyword::Fn),
                TokenKind::Ident("main".into()),
                TokenKind::LParen,
                TokenKind::RParen,
                TokenKind::Keyword(Keyword::Let),
                TokenKind::Keyword(Keyword::Mut),
                TokenKind::Keyword(Keyword::Struct),
                TokenKind::Ident("x".into()),
                TokenKind::Ident("abc_2".into()),
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn operators() {
        let ks = skip_errors("+ - * / % == != < <= > >= && || ! & | ^ << >> += -= <<= >>= -> => = ::");
        assert_eq!(
            ks,
            vec![
                TokenKind::Plus,
                TokenKind::Minus,
                TokenKind::Star,
                TokenKind::Slash,
                TokenKind::Percent,
                TokenKind::EqEq,
                TokenKind::Ne,
                TokenKind::Lt,
                TokenKind::Le,
                TokenKind::Gt,
                TokenKind::Ge,
                TokenKind::AmpAmp,
                TokenKind::PipePipe,
                TokenKind::Bang,
                TokenKind::Amp,
                TokenKind::Pipe,
                TokenKind::Caret,
                TokenKind::Shl,
                TokenKind::Shr,
                TokenKind::PlusEq,
                TokenKind::MinusEq,
                TokenKind::ShlEq,
                TokenKind::ShrEq,
                TokenKind::Arrow,
                TokenKind::FatArrow,
                TokenKind::Eq,
                TokenKind::ColonColon,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn integer_literals() {
        let ks = kinds("0 42 1_000 0x2a 0b101 0o17");
        assert_eq!(
            ks,
            vec![
                TokenKind::Int { value: 0, suffix: None },
                TokenKind::Int { value: 42, suffix: None },
                TokenKind::Int { value: 1000, suffix: None },
                TokenKind::Int { value: 42, suffix: None },
                TokenKind::Int { value: 5, suffix: None },
                TokenKind::Int { value: 15, suffix: None },
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn suffixed_literals() {
        let ks = kinds("42u64 1.5f32 7i32");
        assert_eq!(
            ks,
            vec![
                TokenKind::Int { value: 42, suffix: Some(IntSuffix::U64) },
                TokenKind::Float { value: 1.5, suffix: Some(FloatSuffix::F32), text: "1.5".into() },
                TokenKind::Int { value: 7, suffix: Some(IntSuffix::I32) },
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn string_and_escapes() {
        let ks = kinds(r#""hello\nworld""#);
        assert_eq!(ks, vec![TokenKind::Str("hello\nworld".into()), TokenKind::Eof]);
    }

    #[test]
    fn interpolation() {
        let (tokens, errs) = lex(r#""Hello, {name}! {x + 1}""#);
        assert!(errs.is_empty());
        let TokenKind::StrInterp(parts) = &tokens[0].kind else {
            panic!("expected interpolation, got {:?}", tokens[0].kind);
        };
        // Parts: "Hello, ", {name}, "! ", {x + 1}
        assert_eq!(parts.len(), 4);
        assert_eq!(parts[0], crate::token::StrPart::Text("Hello, ".into()));
        assert_eq!(
            parts[1],
            crate::token::StrPart::Expr { src: "name".into(), span: Span::new(tokens[0].span.file, 9, 13) }
        );
    }

    #[test]
    fn escaped_braces() {
        let ks = kinds(r#""literal {{ brace""#);
        assert_eq!(ks, vec![TokenKind::Str("literal { brace".into()), TokenKind::Eof]);
    }

    #[test]
    fn char_literal() {
        let ks = kinds(r#"'a' '\n' '\\'"#);
        assert_eq!(
            ks,
            vec![
                TokenKind::Char('a'),
                TokenKind::Char('\n'),
                TokenKind::Char('\\'),
                TokenKind::Eof
            ]
        );
    }

    #[test]
    fn comments_are_skipped() {
        let ks = kinds("// line\nlet x = 1 /* block /* nested */ done */ + 2");
        assert_eq!(
            ks,
            vec![
                TokenKind::Keyword(Keyword::Let),
                TokenKind::Ident("x".into()),
                TokenKind::Eq,
                TokenKind::Int { value: 1, suffix: None },
                TokenKind::Plus,
                TokenKind::Int { value: 2, suffix: None },
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn unterminated_string_reports_error() {
        let (_, errs) = lex("\"oops");
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].code, "E0002");
    }

    #[test]
    fn unterminated_block_comment_reports_error() {
        let (_, errs) = lex("/* nope");
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].code, "E0004");
    }

    #[test]
    fn unknown_escape_reports_error() {
        let (_, errs) = lex(r#""\q""#);
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].code, "E0007");
    }

    #[test]
    fn unexpected_character_reports_error() {
        let (_, errs) = lex("let $x = 1");
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].code, "E0001");
    }

    #[test]
    fn unterminated_interpolation_reports_error() {
        let (_, errs) = lex(r#""abc { x"#);
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].code, "E0003");
    }
}
