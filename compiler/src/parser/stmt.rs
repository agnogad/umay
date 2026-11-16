//! Parser: statements and blocks. (`impl Parser` block.)
//!
//! Statement termination uses newline awareness: semicolons are optional, so a
//! statement ends at a line break unless the surrounding tokens show the
//! expression must continue (trailing operator, postfix, or a leading
//! continuation token on the next line).

use crate::ast::*;
use crate::token::{Keyword, TokenKind};

use super::Parser;

impl Parser {
    /// True if the previous consumed token cannot end an expression, i.e. the
    /// expression must continue on the next line.
    fn prev_requires_continuation(&self) -> bool {
        if self.pos == 0 {
            return false;
        }
        matches!(
            self.tokens[self.pos - 1].kind,
            TokenKind::Plus
                | TokenKind::Minus
                | TokenKind::Star
                | TokenKind::Slash
                | TokenKind::Percent
                | TokenKind::Eq
                | TokenKind::EqEq
                | TokenKind::Ne
                | TokenKind::Lt
                | TokenKind::Le
                | TokenKind::Gt
                | TokenKind::Ge
                | TokenKind::Amp
                | TokenKind::Pipe
                | TokenKind::Caret
                | TokenKind::AmpAmp
                | TokenKind::PipePipe
                | TokenKind::Shl
                | TokenKind::Shr
                | TokenKind::Comma
                | TokenKind::Dot
                | TokenKind::Colon
                | TokenKind::Arrow
                | TokenKind::FatArrow
                | TokenKind::PlusEq
                | TokenKind::MinusEq
                | TokenKind::StarEq
                | TokenKind::SlashEq
                | TokenKind::PercentEq
                | TokenKind::AmpEq
                | TokenKind::PipeEq
                | TokenKind::CaretEq
                | TokenKind::ShlEq
                | TokenKind::ShrEq
                | TokenKind::Question
                | TokenKind::LParen
                | TokenKind::LBracket
                | TokenKind::LBrace
        )
    }

    /// True if the next token can legally continue the current expression.
    fn next_can_continue_expr(&self) -> bool {
        matches!(
            self.kind(),
            TokenKind::Plus
                | TokenKind::Minus
                | TokenKind::Star
                | TokenKind::Slash
                | TokenKind::Percent
                | TokenKind::EqEq
                | TokenKind::Ne
                | TokenKind::Lt
                | TokenKind::Le
                | TokenKind::Gt
                | TokenKind::Ge
                | TokenKind::Amp
                | TokenKind::Pipe
                | TokenKind::Caret
                | TokenKind::AmpAmp
                | TokenKind::PipePipe
                | TokenKind::Shl
                | TokenKind::Shr
                | TokenKind::Eq
                | TokenKind::PlusEq
                | TokenKind::MinusEq
                | TokenKind::StarEq
                | TokenKind::SlashEq
                | TokenKind::PercentEq
                | TokenKind::AmpEq
                | TokenKind::PipeEq
                | TokenKind::CaretEq
                | TokenKind::ShlEq
                | TokenKind::ShrEq
                | TokenKind::Dot
                | TokenKind::ColonColon
                | TokenKind::LParen
                | TokenKind::LBracket
                | TokenKind::Question
                | TokenKind::DotDot
                | TokenKind::DotDotEq
        )
    }

    /// True if the next token unambiguously begins a new statement.
    fn next_starts_new_statement(&self) -> bool {
        matches!(
            self.kind(),
            TokenKind::Keyword(
                Keyword::Let
                    | Keyword::Return
                    | Keyword::If
                    | Keyword::While
                    | Keyword::Loop
                    | Keyword::For
                    | Keyword::Break
                    | Keyword::Continue
                    | Keyword::Defer
                    | Keyword::Fn
                    | Keyword::Struct
                    | Keyword::Enum
                    | Keyword::Impl
                    | Keyword::Trait
                    | Keyword::Const
                    | Keyword::Use
                    | Keyword::Mod
                    | Keyword::Type
            )
        )
    }

    /// Decides whether the expression currently being parsed has ended.
    ///
    /// Called *before* consuming the next token, with `start_line` the line on
    /// which the expression began.
    pub(crate) fn expr_ended(&self, start_line: u32) -> bool {
        if self.at(&TokenKind::Semi) || self.at_eof() {
            return true;
        }
        if self.at(&TokenKind::RBrace) || self.at(&TokenKind::RParen) || self.at(&TokenKind::RBracket)
            || self.at(&TokenKind::Comma)
        {
            return true;
        }
        if self.next_starts_new_statement() {
            return true;
        }
        // A newline ends the expression unless either side requires continuation.
        let cur_line = self.line_of(self.pos);
        if cur_line > start_line && cur_line > self.line_of(self.pos.saturating_sub(1)) {
            if self.prev_requires_continuation() {
                return false;
            }
            if self.deref_starts_line() {
                return true;
            }
            return !self.next_can_continue_expr();
        }
        false
    }

    // ---- blocks & statements ----------------------------------------------

    pub fn parse_block(&mut self) -> Block {
        let start = self.span();
        self.expect(TokenKind::LBrace, "E0300", "block");
        let mut stmts = Vec::new();
        let mut tail: Option<Box<Expr>> = None;

        while !self.at(&TokenKind::RBrace) && !self.at_eof() {
            if self.eat(&TokenKind::Semi) {
                continue;
            }
            let Some((stmt, ended_with_semi)) = self.parse_stmt() else {
                self.recover_stmt();
                continue;
            };
            let is_expr = matches!(stmt.kind, StmtKind::Expr(_));
            if is_expr && !ended_with_semi && self.at(&TokenKind::RBrace) {
                if let StmtKind::Expr(e) = stmt.kind {
                    tail = Some(Box::new(e));
                    break;
                }
            }
            stmts.push(stmt);
        }

        if self.at_eof() {
            self.error("E0301", "unexpected end of file: block is missing its closing `}`", self.span());
        }
        let end = self.expect(TokenKind::RBrace, "E0302", "block").unwrap_or(self.prev_span());
        Block { stmts, tail, span: start.to(end) }
    }

    /// Parses one statement. Returns `(stmt, ended_with_explicit_semicolon)`.
    fn parse_stmt(&mut self) -> Option<(Stmt, bool)> {
        let start = self.span();
        match self.kind().clone() {
            TokenKind::Keyword(Keyword::Let) => {
                self.advance();
                let mut let_stmt = self.parse_let(start)?;
                let semi = self.eat_semi();
                let end = self.prev_span();
                let_stmt.span = start.to(end);
                let span = start.to(end);
                Some((Stmt { kind: StmtKind::Let(let_stmt), span }, semi))
            }
            TokenKind::Keyword(Keyword::Fn)
            | TokenKind::Keyword(Keyword::Struct)
            | TokenKind::Keyword(Keyword::Enum)
            | TokenKind::Keyword(Keyword::Impl)
            | TokenKind::Keyword(Keyword::Trait)
            | TokenKind::Keyword(Keyword::Const)
            | TokenKind::Keyword(Keyword::Use)
            | TokenKind::Keyword(Keyword::Mod)
            | TokenKind::Keyword(Keyword::Type)
            | TokenKind::Hash => {
                let item = self.parse_item()?;
                let semi = self.eat_semi();
                let end = self.prev_span();
                let span = start.to(end);
                Some((Stmt { kind: StmtKind::Item(Box::new(item)), span }, semi))
            }
            _ => {
                let expr = self.parse_expr();
                let semi = self.eat_semi();
                let end = self.prev_span();
                let span = start.to(end);
                Some((Stmt { kind: StmtKind::Expr(expr), span }, semi))
            }
        }
    }

    fn parse_let(&mut self, start: crate::source::Span) -> Option<LetStmt> {
        let mutable = self.eat_keyword(Keyword::Mut);
        let pattern = self.parse_pattern();
        let mut ty = None;
        if self.eat(&TokenKind::Colon) {
            ty = Some(self.parse_type());
        }
        let mut init = None;
        if self.eat(&TokenKind::Eq) {
            init = Some(self.parse_expr());
        }
        let mut else_block = None;
        if self.at_keyword(Keyword::Else) {
            self.advance();
            else_block = Some(self.parse_block());
        }
        let end = self.prev_span();
        if init.is_none() && else_block.is_none() {
            self.error_with_hint(
                "E0303",
                "`let` binding has no initializer",
                start,
                "every `let` in UMAY must be initialized at its declaration",
                "deferred initialization is not permitted, which is what makes definite-initialization checking sound",
            );
        }
        Some(LetStmt { mutable, pattern, ty, init, else_block, span: start.to(end) })
    }
}
