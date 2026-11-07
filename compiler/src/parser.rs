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
}

const MAX_DEPTH: usize = 256;

impl Parser {
    pub fn new(tokens: Vec<Token>, file: FileId) -> Self {
        debug_assert!(!tokens.is_empty(), "token stream must end with Eof");
        // Line numbers are derived from token spans without needing the source
        // text: the lexer records spans, and the caller passes a line table.
        let line_of = tokens.iter().map(|t| t.line).collect();
        Parser { tokens, pos: 0, file, lines: line_of, diagnostics: Vec::new(), depth: 0 }
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
        if self.at(&kind) {
            Some(self.advance().span)
        } else {
            let found = self.kind().describe();
            let sp = self.span();
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
            self.diagnostics.push(
                Diagnostic::error("E0052", "expression or type nesting is too deep")
                    .at(span)
                    .with_cause(format!("nesting exceeded {} levels", MAX_DEPTH))
                    .with_explanation("UMAY limits nesting depth to keep compilation bounded and predictable."),
            );
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
