//! Recursive-descent parser with error recovery.
//!
//! The parser consumes the token stream and produces an [`ast::Program`] plus a
//! list of [`Diagnostic`]s. It never panics on malformed input: unexpected
//! tokens produce a diagnostic and synchronisation resumes at the next
//! statement or item boundary.
//!
//! Grammar source of truth: `docs/language-specification.md` §Grammar.

mod expr;
mod items;
mod stmt;


use crate::ast::*;
use crate::diagnostics::{Diagnostic, Suggestion};
use crate::lexer::LexError;
use crate::source::{FileId, Span};
use crate::token::{Keyword, Token, TokenKind};

/// Parsed output: the AST plus any diagnostics produced.
pub struct ParseOutput {
    pub program: Program,
    pub diagnostics: Vec<Diagnostic>,
}

pub fn parse(tokens: Vec<Token>, lex_errors: Vec<LexError>, file: FileId) -> ParseOutput {
    let mut p = Parser::new(tokens, file);
    let diagnostics = crate::lexer::lex_errors_to_diagnostics(lex_errors);
    p.diagnostics.extend(diagnostics);
    let program = p.parse_program();
    ParseOutput { program, diagnostics: p.diagnostics }
}

pub struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    #[allow(dead_code)]
    file: FileId,
    /// 1-based line number for each token, used for newline-aware statement
    /// termination. Maps byte offset -> line via the source text.
    lines: Vec<u32>,
    pub diagnostics: Vec<Diagnostic>,
    /// Nesting depth guard against stack overflow on adversarial input.
    depth: usize,
    /// True while parsing the condition of `if`, `while` or `for`.
    ///
    /// In that position `{` opens the body, not a struct literal, so
    /// `if x > y { a }` must read `{ a }` as a block. Rust resolves the same
    /// ambiguity the same way; without it, `y { a }` parses as a struct literal
    /// and the `else` that follows becomes a syntax error.
    no_struct_literal: bool,
    /// Set once the nesting limit has been reported, so the cascade of recovery
    /// errors that follows is not printed on top of it.
    too_deep: bool,
}

const MAX_DEPTH: usize = 256;

impl Parser {
    pub fn new(tokens: Vec<Token>, file: FileId) -> Self {
        debug_assert!(!tokens.is_empty(), "token stream must end with Eof");
        // Line numbers are derived from token spans without needing the source
        // text: the lexer records spans, and the caller passes a line table.
        let line_of = tokens.iter().map(|t| t.line).collect();
        Parser {
            tokens,
            pos: 0,
            file,
            lines: line_of,
            diagnostics: Vec::new(),
            depth: 0,
            no_struct_literal: false,
            too_deep: false,
        }
    }

    /// 1-based line of the token at index `i`.
    fn line_of(&self, i: usize) -> u32 {
        self.lines.get(i.min(self.lines.len().saturating_sub(1))).copied().unwrap_or(0)
    }

    // ---- token access -----------------------------------------------------

    fn cur(&self) -> &Token {
        &self.tokens[self.pos.min(self.tokens.len() - 1)]
    }

    fn kind(&self) -> &TokenKind {
        &self.cur().kind
    }

    fn span(&self) -> Span {
        self.cur().span
    }

    fn nth(&self, n: usize) -> &Token {
        let i = (self.pos + n).min(self.tokens.len() - 1);
        &self.tokens[i]
    }

    fn nth_kind(&self, n: usize) -> &TokenKind {
        &self.nth(n).kind
    }

    fn at(&self, kind: &TokenKind) -> bool {
        self.kind() == kind
    }

    /// Splits a `>>` in two when a generic parameter list is being closed.
    ///
    /// The lexer makes `>>` one token because that is what a shift operator is,
    /// and it cannot know that the same two characters also close two nested
    /// generic lists: `Option<Vec<i64>>`. Before a `>` is consumed in a type or
    /// generic-argument position, a `>>` is therefore taken apart, keeping the
    /// second `>` as the next token.
    fn split_closing_angles(&mut self) {
        if !matches!(self.kind(), TokenKind::Shr) {
            return;
        }
        let here = self.tokens[self.pos].clone();
        let mut first = here.clone();
        first.kind = TokenKind::Gt;
        first.span = crate::source::Span::new(here.span.file, here.span.start, here.span.start + 1);
        let mut second = here.clone();
        second.kind = TokenKind::Gt;
        second.span = crate::source::Span::new(here.span.file, here.span.start + 1, here.span.end);
        self.tokens[self.pos] = first;
        self.tokens.insert(self.pos + 1, second);
    }

    fn at_keyword(&self, kw: Keyword) -> bool {
        matches!(self.kind(), TokenKind::Keyword(k) if *k == kw)
    }

    fn advance(&mut self) -> Token {
        let t = self.tokens[self.pos.min(self.tokens.len() - 1)].clone();
        if self.pos < self.tokens.len() - 1 {
            self.pos += 1;
        }
        t
    }

    fn eat(&mut self, kind: &TokenKind) -> bool {
        if self.at(kind) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn eat_keyword(&mut self, kw: Keyword) -> bool {
        if self.at_keyword(kw) {
            self.advance();
            true
        } else {
            false
        }
    }

    /// Whether the token at the cursor starts on a later line than the one
    /// before it. Semicolons are optional, so this is how a statement ends: a
    /// `(` or `[` on a new line begins a new statement, not a call.
    fn starts_new_line(&self) -> bool {
        if self.pos == 0 {
            return false;
        }
        let prev = &self.tokens[(self.pos - 1).min(self.tokens.len() - 1)];
        self.cur().line > prev.line
    }

    /// A `*` that starts a line glued to its operand (`*res = x`) dereferences
    /// and so begins a new statement; `* rhs` still continues a multiply.
    fn deref_starts_line(&self) -> bool {
        self.at(&TokenKind::Star)
            && self.starts_new_line()
            && self.tokens.get(self.pos + 1).is_some_and(|n| n.span.start == self.cur().span.end)
    }

    fn prev_span(&self) -> Span {
        if self.pos == 0 {
            self.span()
        } else {
            self.tokens[(self.pos - 1).min(self.tokens.len() - 1)].span
        }
    }

    fn at_eof(&self) -> bool {
        matches!(self.kind(), TokenKind::Eof)
    }

    // ---- errors -----------------------------------------------------------

    pub fn error(&mut self, code: &'static str, message: impl Into<String>, span: Span) {
        self.diagnostics.push(Diagnostic::error(code, message).at(span));
    }

    fn expect(&mut self, kind: TokenKind, code: &'static str, context: &str) -> Option<Span> {
        if kind == TokenKind::Gt {
            self.split_closing_angles();
        }
        if self.at(&kind) {
            Some(self.advance().span)
        } else {
            let found = self.kind().describe();
            let sp = self.span();
            // Past the nesting limit every expected delimiter is a consequence
            // of the one error already reported, not a second problem.
            if self.too_deep {
                return None;
            }
            self.diagnostics.push(
                Diagnostic::error(code, format!("expected {} {}, found {}", kind.describe(), context, found))
                    .at(sp)
                    .with_cause(format!("the parser needed `{}` to finish the {}", kind.symbol(), context)),
            );
            None
        }
    }

    fn expect_ident(&mut self, code: &'static str, context: &str) -> Option<(String, Span)> {
        match self.kind().clone() {
            TokenKind::Ident(name) => {
                let sp = self.advance().span;
                Some((name, sp))
            }
            TokenKind::Keyword(kw) => {
                // Common mistake: keyword used as a name.
                let sp = self.span();
                let name = kw.as_str().to_string();
                self.diagnostics.push(
                    Diagnostic::new(
                        code,
                        crate::diagnostics::Severity::Error,
                        format!("expected a name {}, found keyword `{name}`", context),
                    )
                    .at(sp)
                    .with_cause(format!("`{name}` is a reserved word and cannot be used as an identifier"))
                    .with_suggestion(Suggestion::new(
                        format!("rename to `{name}_`"),
                        sp,
                        format!("{name}_"),
                    )),
                );
                self.advance();
                Some((name, sp))
            }
            other => {
                let found = other.describe();
                let sp = self.span();
                self.diagnostics.push(
                    Diagnostic::error(code, format!("expected a name {}, found {}", context, found))
                        .at(sp)
                        .with_cause("an identifier was required here"),
                );
                None
            }
        }
    }

    /// Consumes an optional statement terminator. Semicolons are optional in
    /// UMAY, so a missing terminator is not an error; this helper exists to
    /// swallow one when present.
    fn eat_semi(&mut self) -> bool {
        self.eat(&TokenKind::Semi)
    }

    /// Skips tokens until a likely item/statement boundary (error recovery).
    fn recover_item(&mut self) {
        let mut depth = 0i32;
        while !self.at_eof() {
            match self.kind() {
                TokenKind::LBrace => {
                    depth += 1;
                    self.advance();
                }
                TokenKind::RBrace => {
                    if depth == 0 {
                        self.advance();
                        return;
                    }
                    depth -= 1;
                    self.advance();
                    if depth == 0 {
                        return;
                    }
                }
                TokenKind::Semi if depth == 0 => {
                    self.advance();
                    return;
                }
                TokenKind::Keyword(Keyword::Fn | Keyword::Struct | Keyword::Enum | Keyword::Impl | Keyword::Const | Keyword::Use | Keyword::Trait)
                    if depth == 0 =>
                {
                    return
                }
                _ => {
                    self.advance();
                }
            }
        }
    }

    fn recover_stmt(&mut self) {
        let mut depth = 0i32;
        while !self.at_eof() {
            match self.kind() {
                TokenKind::LBrace => {
                    depth += 1;
                    self.advance();
                }
                TokenKind::RBrace => {
                    if depth == 0 {
                        return;
                    }
                    depth -= 1;
                    self.advance();
                }
                TokenKind::Semi if depth == 0 => {
                    self.advance();
                    return;
                }
                _ => {
                    self.advance();
                }
            }
        }
    }

    // ---- depth guard ------------------------------------------------------

    fn enter(&mut self, span: Span) -> bool {
        if self.depth >= MAX_DEPTH {
            // Reported once. The remaining input is still full of closing
            // delimiters that no longer match anything, so reporting recovery
            // errors too would print one real complaint followed by hundreds of
            // consequences of it -- 256 of them for 5000 nested parentheses,
            // burying the one line the author needs to read.
            if !self.too_deep {
                self.too_deep = true;
                self.diagnostics.push(
                    Diagnostic::error("E0052", "expression or type nesting is too deep")
                        .at(span)
                        .with_cause(format!("nesting exceeded {} levels", MAX_DEPTH))
                        .with_explanation(
                            "UMAY limits nesting depth to keep compilation bounded and predictable.",
                        ),
                );
            }
            return false;
        }
        self.depth += 1;
        true
    }

    fn leave(&mut self) {
        self.depth = self.depth.saturating_sub(1);
    }

    // ---- file / item level -----------------------------------------------

    // parse_program, parse_item, attributes, and all `parse_type*` live in
    // `parser_items.rs`; expression/statement parsing lives in
    // `parser_expr.rs`. Both are `impl Parser` blocks (same crate).
}
