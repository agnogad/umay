//! Parser: expressions and patterns. (`impl Parser` block.)

use crate::ast::*;
use crate::diagnostics::{Diagnostic, Severity};
use crate::source::Span;
use crate::token::{Keyword, TokenKind};

use super::Parser;

impl Parser {
    pub(crate) fn error_with_hint(
        &mut self,
        code: &'static str,
        message: impl Into<String>,
        span: Span,
        cause: impl Into<String>,
        explanation: impl Into<String>,
    ) {
        self.diagnostics.push(
            Diagnostic::new(code, Severity::Error, message)
                .at(span)
                .with_cause(cause)
                .with_explanation(explanation),
        );
    }

    pub fn parse_expr(&mut self) -> Expr {
        if !self.enter(self.span()) {
            self.recover_stmt();
            return Expr { kind: ExprKind::Infer, span: self.prev_span() };
        }
        let e = self.parse_assign();
        self.leave();
        e
    }

    /// Returns the 1-based line where the current expression starts, for
    /// newline-aware termination.
    fn cur_line(&self) -> u32 {
        self.line_of(self.pos)
    }

    /// Compound assignment operators and the binary operator they desugar to.
    fn compound_assign_op(kind: &TokenKind) -> Option<BinaryOp> {
        Some(match kind {
            TokenKind::PlusEq => BinaryOp::Add,
            TokenKind::MinusEq => BinaryOp::Sub,
            TokenKind::StarEq => BinaryOp::Mul,
            TokenKind::SlashEq => BinaryOp::Div,
            TokenKind::PercentEq => BinaryOp::Rem,
            TokenKind::AmpEq => BinaryOp::BitAnd,
            TokenKind::PipeEq => BinaryOp::BitOr,
            TokenKind::CaretEq => BinaryOp::BitXor,
            TokenKind::ShlEq => BinaryOp::Shl,
            TokenKind::ShrEq => BinaryOp::Shr,
            _ => return None,
        })
    }

    fn parse_assign(&mut self) -> Expr {
        let lhs = self.parse_binary(0);
        if let Some(op) = Self::compound_assign_op(self.kind()) {
            let start = lhs.span;
            let op_span = self.span();
            self.advance();
            let value = self.parse_assign();
            let span = start.to(value.span);
            let _ = op_span;
            return Expr {
                kind: ExprKind::AssignOp { op, place: Box::new(lhs), value: Box::new(value) },
                span,
            };
        }
        if self.at(&TokenKind::Eq) {
            let start = lhs.span;
            self.advance();
            let rhs = self.parse_assign();
            let span = start.to(rhs.span);
            // Plain assignment is not a binary operator: `a == b` compares and
            // `a = b` stores. They use different AST nodes so that the checker
            // cannot confuse them.
            return Expr { kind: ExprKind::Assign { place: Box::new(lhs), value: Box::new(rhs) }, span };
        }
        lhs
    }

    fn op_prec(op: &TokenKind) -> Option<(u8, BinaryOp)> {
        Some(match op {
            TokenKind::PipePipe => (2, BinaryOp::Or),
            TokenKind::AmpAmp => (3, BinaryOp::And),
            TokenKind::EqEq => (4, BinaryOp::Eq),
            TokenKind::Ne => (4, BinaryOp::Ne),
            TokenKind::Lt => (4, BinaryOp::Lt),
            TokenKind::Le => (4, BinaryOp::Le),
            TokenKind::Gt => (4, BinaryOp::Gt),
            TokenKind::Ge => (4, BinaryOp::Ge),
            TokenKind::Pipe => (5, BinaryOp::BitOr),
            TokenKind::Caret => (6, BinaryOp::BitXor),
            TokenKind::Amp => (7, BinaryOp::BitAnd),
            TokenKind::Shl => (8, BinaryOp::Shl),
            TokenKind::Shr => (8, BinaryOp::Shr),
            TokenKind::Plus => (9, BinaryOp::Add),
            TokenKind::Minus => (9, BinaryOp::Sub),
            TokenKind::Star => (10, BinaryOp::Mul),
            TokenKind::Slash => (10, BinaryOp::Div),
            TokenKind::Percent => (10, BinaryOp::Rem),
            _ => return None,
        })
    }

    fn parse_binary(&mut self, min_prec: u8) -> Expr {
        let mut lhs = self.parse_unary();
        loop {
            // Range operators bind looser than comparisons.
            if self.at(&TokenKind::DotDot) || self.at(&TokenKind::DotDotEq) {
                let inclusive = self.at(&TokenKind::DotDotEq);
                let start = lhs.span;
                self.advance();
                let end = if self.expr_ended(self.cur_line()) {
                    None
                } else {
                    Some(Box::new(self.parse_binary(2)))
                };
                let span = match &end {
                    Some(r) => start.to(r.span),
                    None => start.to(self.prev_span()),
                };
                lhs = Expr { kind: ExprKind::Range { start: Some(Box::new(lhs)), end, inclusive }, span };
                continue;
            }
            let Some((prec, op)) = Self::op_prec(self.kind()) else { break };
            if prec < min_prec {
                break;
            }
            let start = lhs.span;
            self.advance();
            let rhs = self.parse_binary(prec + 1);
            let span = start.to(rhs.span);
            lhs = Expr { kind: ExprKind::Binary { op, lhs: Box::new(lhs), rhs: Box::new(rhs) }, span };
        }
        lhs
    }

    fn parse_unary(&mut self) -> Expr {
        let start = self.span();
        match self.kind().clone() {
            TokenKind::Minus => {
                self.advance();
                let operand = self.parse_unary();
                let span = start.to(operand.span);
                Expr { kind: ExprKind::Unary { op: UnaryOp::Neg, operand: Box::new(operand) }, span }
            }
            TokenKind::Bang => {
                self.advance();
                let operand = self.parse_unary();
                let span = start.to(operand.span);
                Expr { kind: ExprKind::Unary { op: UnaryOp::Not, operand: Box::new(operand) }, span }
            }
            TokenKind::Star => {
                self.advance();
                let operand = self.parse_unary();
                let span = start.to(operand.span);
                Expr { kind: ExprKind::Unary { op: UnaryOp::Deref, operand: Box::new(operand) }, span }
            }
            TokenKind::Amp => {
                self.advance();
                let mutable = self.eat_keyword(Keyword::Mut);
                let operand = self.parse_unary();
                let span = start.to(operand.span);
                Expr { kind: ExprKind::Ref { mutable, operand: Box::new(operand) }, span }
            }
            TokenKind::Keyword(Keyword::Move) => {
                self.advance();
                let operand = self.parse_unary();
                let span = start.to(operand.span);
                Expr { kind: ExprKind::Move(Box::new(operand)), span }
            }
            _ => self.parse_postfix(),
        }
    }

    fn parse_postfix(&mut self) -> Expr {
        let mut expr = self.parse_primary();
        loop {
            match self.kind().clone() {
                TokenKind::LParen => {
                    // A `(` on the next line starts a new statement, not a call:
                    // semicolons are optional, so `let y = x == k` followed by a
                    // line beginning `(` must not become `k(..)`.
                    if self.starts_new_line() {
                        break;
                    }
                    let start = expr.span;
                    self.advance();
                    let mut args = Vec::new();
                    let mut trailing_comma = false;
                    if !self.at(&TokenKind::RParen) {
                        loop {
                            args.push(self.parse_expr());
                            if self.eat(&TokenKind::Comma) {
                                trailing_comma = true;
                                if self.at(&TokenKind::RParen) {
                                    break;
                                }
                                continue;
                            }
                            trailing_comma = false;
                            break;
                        }
                    }
                    let end = self.expect(TokenKind::RParen, "E0310", "call arguments").unwrap_or(self.prev_span());
                    expr = Expr {
                        kind: ExprKind::Call { callee: Box::new(expr), args, trailing_comma },
                        span: start.to(end),
                    };
                }
                TokenKind::LBracket => {
                    // As with `(`, a `[` on the next line begins a new statement
                    // rather than indexing the previous one.
                    if self.starts_new_line() {
                        break;
                    }
                    let start = expr.span;
                    self.advance();
                    let index = self.parse_expr();
                    let end = self.expect(TokenKind::RBracket, "E0311", "index expression").unwrap_or(index.span);
                    expr = Expr {
                        kind: ExprKind::Index { base: Box::new(expr), index: Box::new(index) },
                        span: start.to(end),
                    };
                }
                TokenKind::Dot => {
                    let start = expr.span;
                    self.advance();
                    match self.kind().clone() {
                        TokenKind::Ident(name) => {
                            let name_span = self.advance().span;
                            if self.at(&TokenKind::LParen) {
                                let (args, end) = self.parse_call_args("E0312", "method arguments");
                                expr = Expr {
                                    kind: ExprKind::MethodCall {
                                        receiver: Box::new(expr),
                                        method: name,
                                        method_span: name_span,
                                        args,
                                    },
                                    span: start.to(end),
                                };
                            } else {
                                expr = Expr {
                                    kind: ExprKind::FieldAccess {
                                        base: Box::new(expr),
                                        field: name,
                                        field_span: name_span,
                                    },
                                    span: start.to(name_span),
                                };
                            }
                        }
                        TokenKind::Int { value, .. } => {
                            // Tuple field access: `pair.0`
                            let sp = self.advance().span;
                            let field = value.to_string();
                            expr = Expr {
                                kind: ExprKind::FieldAccess { base: Box::new(expr), field, field_span: sp },
                                span: start.to(sp),
                            };
                        }
                        other => {
                            let found = other.describe();
                            self.error("E0313", format!("expected a field or method name after `.`, found {found}"), self.span());
                            break;
                        }
                    }
                }
                TokenKind::Question => {
                    let start = expr.span;
                    let end = self.advance().span;
                    expr = Expr { kind: ExprKind::Try(Box::new(expr)), span: start.to(end) };
                }
                TokenKind::Keyword(Keyword::As) => {
                    let start = expr.span;
                    self.advance();
                    let ty = self.parse_type();
                    let span = start.to(ty.span());
                    expr = Expr { kind: ExprKind::Cast { expr: Box::new(expr), ty }, span };
                }
                _ => break,
            }
        }
        expr
    }

    fn parse_call_args(&mut self, code: &'static str, context: &str) -> (Vec<Expr>, Span) {
        self.advance(); // consume '('
        let mut args = Vec::new();
        if !self.at(&TokenKind::RParen) {
            loop {
                args.push(self.parse_expr());
                if self.eat(&TokenKind::Comma) {
                    if self.at(&TokenKind::RParen) {
                        break;
                    }
                    continue;
                }
                break;
            }
        }
        let end = self.expect(TokenKind::RParen, code, context).unwrap_or(self.prev_span());
        (args, end)
    }

    fn parse_primary(&mut self) -> Expr {
        let start = self.span();
        match self.kind().clone() {
            TokenKind::Int { value, suffix } => {
                let span = self.advance().span;
                Expr { kind: ExprKind::IntLit { value, suffix }, span }
            }
            TokenKind::Float { value, suffix, text } => {
                let span = self.advance().span;
                Expr { kind: ExprKind::FloatLit { value, suffix, text }, span }
            }
            TokenKind::Str(s) => {
                let span = self.advance().span;
                Expr { kind: ExprKind::StrLit(s), span }
            }
            TokenKind::StrInterp(parts) => {
                let span = self.advance().span;
                Expr { kind: ExprKind::StrInterp(parts), span }
            }
            TokenKind::Char(c) => {
                let span = self.advance().span;
                Expr { kind: ExprKind::CharLit(c), span }
            }
            TokenKind::Keyword(Keyword::True) => {
                let span = self.advance().span;
                Expr { kind: ExprKind::BoolLit(true), span }
            }
            TokenKind::Keyword(Keyword::False) => {
                let span = self.advance().span;
                Expr { kind: ExprKind::BoolLit(false), span }
            }
            TokenKind::Keyword(Keyword::Sizeof) => {
                // `sizeof(Type)`. A size is a property of the type, so it is read
                // from a type rather than from a value, and no value is evaluated.
                let start = self.advance().span;
                if !self.at(&TokenKind::LParen) {
                    let sp = self.span();
                    self.error("E0322", "expected `(` after `sizeof`", sp);
                    return Expr { kind: ExprKind::Infer, span: start.to(sp) };
                }
                self.advance();
                let ty = self.parse_type();
                let end = self.expect(TokenKind::RParen, "E0322", "`sizeof(`").unwrap_or(start);
                Expr { kind: ExprKind::Sizeof { ty }, span: start.to(end) }
            }
            TokenKind::Underscore => {
                let span = self.advance().span;
                Expr { kind: ExprKind::Infer, span }
            }
            TokenKind::LParen => self.parse_paren(),
            TokenKind::LBracket => self.parse_array(),
            TokenKind::LBrace => {
                let block = self.parse_block();
                Expr { span: block.span, kind: ExprKind::Block(block) }
            }
            TokenKind::Keyword(Keyword::If) => self.parse_if(),
            TokenKind::Keyword(Keyword::Match) => self.parse_match(),
            TokenKind::Keyword(Keyword::While) => {
                self.advance();
                let cond = self.parse_condition();
                let body = self.parse_block();
                let span = start.to(body.span);
                Expr { kind: ExprKind::While { cond: Box::new(cond), body }, span }
            }
            TokenKind::Keyword(Keyword::Loop) => {
                self.advance();
                let body = self.parse_block();
                let span = start.to(body.span);
                Expr { kind: ExprKind::Loop { body }, span }
            }
            TokenKind::Keyword(Keyword::For) => {
                self.advance();
                let pattern = self.parse_pattern();
                self.expect(TokenKind::Keyword(Keyword::In), "E0320", "`for` loop");
                let iter = self.parse_condition();
                let body = self.parse_block();
                let span = start.to(body.span);
                Expr { kind: ExprKind::For { pattern, iter: Box::new(iter), body }, span }
            }
            TokenKind::Keyword(Keyword::Return) => {
                self.advance();
                let line = self.cur_line();
                let value = if self.expr_ended(line) { None } else { Some(Box::new(self.parse_expr())) };
                let span = start.to(self.prev_span());
                Expr { kind: ExprKind::Return(value), span }
            }
            TokenKind::Keyword(Keyword::Break) => {
                self.advance();
                let line = self.cur_line();
                let value = if self.expr_ended(line) { None } else { Some(Box::new(self.parse_expr())) };
                let span = start.to(self.prev_span());
                Expr { kind: ExprKind::Break(value), span }
            }
            TokenKind::Keyword(Keyword::Continue) => {
                let span = self.advance().span;
                Expr { kind: ExprKind::Continue, span }
            }
            TokenKind::Keyword(Keyword::Defer) => {
                self.advance();
                let body = self.parse_block();
                let span = start.to(body.span);
                Expr { kind: ExprKind::Defer(body), span }
            }
            TokenKind::Keyword(Keyword::Unsafe) => {
                self.advance();
                let body = self.parse_block();
                let span = start.to(body.span);
                Expr { kind: ExprKind::Unsafe(body), span }
            }
            TokenKind::Ident(_) | TokenKind::Keyword(Keyword::SelfValue) | TokenKind::Keyword(Keyword::SelfType) => {
                self.parse_ident_like()
            }
            TokenKind::ColonColon => {
                // Absolute path: `::std::print`
                match self.parse_path() {
                    Some(p) => Expr { span: p.span, kind: ExprKind::Path(p) },
                    None => {
                        let span = self.advance().span;
                        self.error("E0321", "expected a path after `::`", span);
                        Expr { kind: ExprKind::Infer, span }
                    }
                }
            }
            other => {
                let found = other.describe();
                let span = self.span();
                // A lexer error already produced a diagnostic for this byte
                // range; do not pile a second one on top of it.
                if !matches!(other, TokenKind::Error) {
                    self.diagnostics.push(
                        Diagnostic::error("E0322", format!("expected an expression, found {found}"))
                            .at(span)
                            .with_cause("the parser expected a value, call, literal or block here"),
                    );
                }
                // Consume the offending token to guarantee progress.
                if !matches!(other, TokenKind::Eof) {
                    self.advance();
                }
                Expr { kind: ExprKind::Infer, span }
            }
        }
    }

    /// Identifiers, `self`, `Self`, paths and struct literals.
    fn parse_ident_like(&mut self) -> Expr {
        let path_start = self.span();
        let mut segments = Vec::new();
        loop {
            let (name, sp) = match self.kind().clone() {
                TokenKind::Ident(name) => (name, self.advance().span),
                TokenKind::Keyword(Keyword::SelfValue) => ("self".to_string(), self.advance().span),
                TokenKind::Keyword(Keyword::SelfType) => ("Self".to_string(), self.advance().span),
                _ => break,
            };
            let mut args = Vec::new();
            if self.at(&TokenKind::Lt) && self.looks_like_generic_args() {
                self.advance();
                self.split_closing_angles();
                while !self.at(&TokenKind::Gt) && !self.at_eof() {
                    args.push(self.parse_type());
                    if !self.eat(&TokenKind::Comma) {
                        break;
                    }
                }
                self.expect(TokenKind::Gt, "E0330", "generic arguments");
            }
            segments.push(PathSegment { name, span: sp, args });
            if !self.eat(&TokenKind::ColonColon) {
                break;
            }
        }
        if segments.is_empty() {
            let span = self.advance().span;
            return Expr { kind: ExprKind::Infer, span };
        }
        let end = segments.last().map(|s| s.span).unwrap_or(path_start);
        let path = Path { segments, span: path_start.to(end) };

        // Struct literal: `Path { ... }`
        if self.at(&TokenKind::LBrace) && self.looks_like_struct_literal() {
            return self.parse_struct_literal(path);
        }
        if path.segments.len() == 1 && path.segments[0].args.is_empty() {
            let name = path.segments[0].name.clone();
            let span = path.span;
            Expr { kind: ExprKind::Ident(name), span }
        } else {
            Expr { span: path.span, kind: ExprKind::Path(path) }
        }
    }

    /// Heuristic for distinguishing `f::<T>()`-style generics from `<` comparison.
    /// UMAY requires `<` in expression position to be followed by a type-ish
    /// token to be treated as generic arguments.
    fn looks_like_generic_args(&self) -> bool {
        if !self.at(&TokenKind::Lt) {
            return false;
        }
        // Scan forward: generic args must be closed by `>` followed by one of
        // `(`, `::`, `{`, or `,`/`)` (in type position).
        let mut depth = 0i32;
        let mut i = 1;
        let limit = 64;
        while i < limit {
            match self.nth_kind(i) {
                TokenKind::Lt => depth += 1,
                TokenKind::Gt => {
                    if depth == 0 {
                        return matches!(
                            self.nth_kind(i + 1),
                            TokenKind::LParen | TokenKind::ColonColon | TokenKind::LBrace
                        );
                    }
                    depth -= 1;
                }
                TokenKind::Eof | TokenKind::Semi => return false,
                _ => {}
            }
            i += 1;
        }
        false
    }

    /// `Path { ... }` is a struct literal when the braces contain field-like
    /// syntax. An empty `{}` after a path is always a struct literal in
    /// expression position; UMAY requires an explicit block for closures.
    fn looks_like_struct_literal(&self) -> bool {
        // In a condition, `{` opens the body rather than a literal.
        if self.no_struct_literal {
            return false;
        }
        if self.nth_kind(1) == &TokenKind::RBrace {
            return true;
        }
        if self.nth_kind(1) == &TokenKind::DotDot {
            return true;
        }
        matches!(self.nth_kind(1), TokenKind::Ident(_) | TokenKind::Keyword(Keyword::Mut))
            && matches!(self.nth_kind(2), TokenKind::Colon | TokenKind::Comma | TokenKind::RBrace)
    }

    fn parse_struct_literal(&mut self, path: Path) -> Expr {
        let start = path.span;
        self.advance(); // consume '{'
        let mut fields = Vec::new();
        let mut base = None;
        while !self.at(&TokenKind::RBrace) && !self.at_eof() {
            if self.eat(&TokenKind::DotDot) {
                base = Some(Box::new(self.parse_expr()));
                break;
            }
            let field_span = self.span();
            let (name, name_span) = match self.expect_ident("E0340", "for the struct field") {
                Some(v) => v,
                None => break,
            };
            let value = if self.eat(&TokenKind::Colon) {
                Some(self.parse_expr())
            } else {
                None
            };
            let end = value.as_ref().map(|v| v.span).unwrap_or(name_span);
            fields.push(FieldInit { name, name_span, value, span: field_span.to(end) });
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        let end = self.expect(TokenKind::RBrace, "E0341", "struct literal").unwrap_or(self.prev_span());
        Expr { kind: ExprKind::StructLit { path, fields, base }, span: start.to(end) }
    }

    fn parse_paren(&mut self) -> Expr {
        let start = self.span();
        self.advance(); // '('
        if self.eat(&TokenKind::RParen) {
            let span = start.to(self.prev_span());
            return Expr { kind: ExprKind::Tuple { elems: Vec::new() }, span };
        }
        let first = self.parse_expr();
        if self.eat(&TokenKind::Comma) {
            let mut elems = vec![first];
            while !self.at(&TokenKind::RParen) && !self.at_eof() {
                elems.push(self.parse_expr());
                if !self.eat(&TokenKind::Comma) {
                    break;
                }
            }
            let end = self.expect(TokenKind::RParen, "E0342", "tuple expression").unwrap_or(self.prev_span());
            return Expr { kind: ExprKind::Tuple { elems }, span: start.to(end) };
        }
        let end = self.expect(TokenKind::RParen, "E0343", "parenthesized expression").unwrap_or(first.span);
        // Parenthesized expression: preserve the inner node but widen the span.
        Expr { span: start.to(end), kind: first.kind }
    }

    fn parse_array(&mut self) -> Expr {
        let start = self.span();
        self.advance(); // '['
        if self.eat(&TokenKind::RBracket) {
            let span = start.to(self.prev_span());
            return Expr { kind: ExprKind::ArrayLit { elems: Vec::new() }, span };
        }
        let first = self.parse_expr();
        if self.eat(&TokenKind::Semi) {
            let count = self.parse_expr();
            let end = self.expect(TokenKind::RBracket, "E0344", "repeat array expression").unwrap_or(count.span);
            return Expr {
                kind: ExprKind::ArrayRepeat { value: Box::new(first), count: Box::new(count) },
                span: start.to(end),
            };
        }
        let mut elems = vec![first];
        while self.eat(&TokenKind::Comma) {
            if self.at(&TokenKind::RBracket) {
                break;
            }
            elems.push(self.parse_expr());
        }
        let end = self.expect(TokenKind::RBracket, "E0345", "array literal").unwrap_or(self.prev_span());
        Expr { kind: ExprKind::ArrayLit { elems }, span: start.to(end) }
    }

    /// Parses the condition of `if`, `while` or `for`, where a `{` opens the
    /// body instead of a struct literal.
    fn parse_condition(&mut self) -> Expr {
        let saved = self.no_struct_literal;
        self.no_struct_literal = true;
        let expr = self.parse_expr();
        self.no_struct_literal = saved;
        expr
    }

    fn parse_if(&mut self) -> Expr {
        let start = self.span();
        self.advance(); // if
        let cond = self.parse_condition();
        let then_block = self.parse_block();
        let mut else_branch = None;
        if self.at_keyword(Keyword::Else) {
            self.advance();
            if self.at_keyword(Keyword::If) {
                else_branch = Some(Box::new(self.parse_if()));
            } else {
                let b = self.parse_block();
                let sp = b.span;
                else_branch = Some(Box::new(Expr { kind: ExprKind::Block(b), span: sp }));
            }
        }
        let end = else_branch.as_ref().map(|e| e.span).unwrap_or(then_block.span);
        Expr { kind: ExprKind::If { cond: Box::new(cond), then_block, else_branch }, span: start.to(end) }
    }

    fn parse_match(&mut self) -> Expr {
        let start = self.span();
        self.advance(); // match
        let scrutinee = self.parse_expr();
        if !self.eat(&TokenKind::LBrace) {
            // Tolerate `match x` with a missing body.
            self.error("E0350", "expected `{` to start the `match` body", self.span());
            return Expr { kind: ExprKind::Match { scrutinee: Box::new(scrutinee), arms: Vec::new() }, span: start.to(self.prev_span()) };
        }
        let mut arms = Vec::new();
        while !self.at(&TokenKind::RBrace) && !self.at_eof() {
            let arm_start = self.span();
            let pattern = self.parse_pattern();
            let guard = if self.at_keyword(Keyword::If) {
                self.advance();
                Some(self.parse_expr())
            } else {
                None
            };
            self.expect(TokenKind::FatArrow, "E0351", "match arm");
            let body = self.parse_expr();
            let end = body.span;
            arms.push(MatchArm { pattern, guard, body, span: arm_start.to(end) });
            if !self.eat(&TokenKind::Comma) {
                // Arms may omit the comma before `}`.
                if !self.at(&TokenKind::RBrace) {
                    break;
                }
            }
        }
        let end = self.expect(TokenKind::RBrace, "E0352", "match body").unwrap_or(self.prev_span());
        Expr { kind: ExprKind::Match { scrutinee: Box::new(scrutinee), arms }, span: start.to(end) }
    }

    // ---- patterns ---------------------------------------------------------

    pub fn parse_pattern(&mut self) -> Pattern {
        if !self.enter(self.span()) {
            let span = self.span();
            return Pattern { kind: PatternKind::Binding { name: "_".into(), mutable: false, by_ref: None }, span };
        }
        let p = self.parse_pattern_or();
        self.leave();
        p
    }

    fn parse_pattern_or(&mut self) -> Pattern {
        let start = self.span();
        let mut alternatives = vec![self.parse_pattern_atom()];
        while self.at(&TokenKind::Pipe) && !self.at(&TokenKind::PipePipe) {
            self.advance();
            alternatives.push(self.parse_pattern_atom());
        }
        if alternatives.len() == 1 {
            return alternatives.pop().unwrap();
        }
        let span = start.to(self.prev_span());
        Pattern { kind: PatternKind::Or(alternatives), span }
    }

    fn parse_pattern_atom(&mut self) -> Pattern {
        let start = self.span();
        match self.kind().clone() {
            TokenKind::Underscore => {
                let span = self.advance().span;
                Pattern { kind: PatternKind::Binding { name: "_".into(), mutable: false, by_ref: None }, span }
            }
            TokenKind::Keyword(Keyword::Mut) => {
                self.advance();
                match self.expect_ident("E0360", "for the binding pattern") {
                    Some((name, sp)) => Pattern {
                        kind: PatternKind::Binding { name, mutable: true, by_ref: None },
                        span: start.to(sp),
                    },
                    None => Pattern { kind: PatternKind::Binding { name: "_".into(), mutable: false, by_ref: None }, span: start },
                }
            }
            TokenKind::Ident(_) | TokenKind::Keyword(Keyword::SelfValue) | TokenKind::Keyword(Keyword::SelfType) => {
                self.parse_path_pattern()
            }
            TokenKind::ColonColon => self.parse_path_pattern(),
            TokenKind::Int { .. } | TokenKind::Str(_) | TokenKind::Char(_) | TokenKind::Float { .. } => {
                let e = self.parse_primary();
                let span = e.span;
                // Range patterns: `1..=5`
                if self.at(&TokenKind::DotDotEq) || self.at(&TokenKind::DotDot) {
                    let inclusive = self.at(&TokenKind::DotDotEq);
                    self.advance();
                    let end_p = self.parse_pattern_atom();
                    let span2 = start.to(end_p.span);
                    return Pattern {
                        kind: PatternKind::Range { start: Box::new(Pattern { kind: PatternKind::Literal(Box::new(e)), span }), end: Box::new(end_p), inclusive },
                        span: span2,
                    };
                }
                Pattern { kind: PatternKind::Literal(Box::new(e)), span }
            }
            TokenKind::Keyword(Keyword::True) | TokenKind::Keyword(Keyword::False) => {
                let e = self.parse_primary();
                let span = e.span;
                Pattern { kind: PatternKind::Literal(Box::new(e)), span }
            }
            TokenKind::Minus => {
                // Negative literal pattern.
                let e = self.parse_unary();
                let span = e.span;
                Pattern { kind: PatternKind::Literal(Box::new(e)), span }
            }
            TokenKind::LParen => {
                self.advance();
                let mut elems = Vec::new();
                while !self.at(&TokenKind::RParen) && !self.at_eof() {
                    elems.push(self.parse_pattern());
                    if !self.eat(&TokenKind::Comma) {
                        break;
                    }
                }
                let end = self.expect(TokenKind::RParen, "E0361", "tuple pattern").unwrap_or(self.prev_span());
                Pattern { kind: PatternKind::Tuple(elems), span: start.to(end) }
            }
            TokenKind::LBracket => {
                self.advance();
                let mut elems = Vec::new();
                let mut rest = false;
                while !self.at(&TokenKind::RBracket) && !self.at_eof() {
                    if self.eat(&TokenKind::DotDot) {
                        rest = true;
                        break;
                    }
                    elems.push(self.parse_pattern());
                    if !self.eat(&TokenKind::Comma) {
                        break;
                    }
                }
                let end = self.expect(TokenKind::RBracket, "E0362", "slice pattern").unwrap_or(self.prev_span());
                Pattern { kind: PatternKind::Slice { elems, rest }, span: start.to(end) }
            }
            other => {
                let found = other.describe();
                let span = self.span();
                self.diagnostics.push(
                    Diagnostic::error("E0363", format!("expected a pattern, found {found}"))
                        .at(span)
                        .with_cause("patterns are used in `let`, `match` arms and `for` loops"),
                );
                if !matches!(other, TokenKind::Eof) {
                    self.advance();
                }
                Pattern { kind: PatternKind::Binding { name: "_".into(), mutable: false, by_ref: None }, span }
            }
        }
    }

    fn parse_path_pattern(&mut self) -> Pattern {
        let start = self.span();
        let Some(path) = self.parse_path() else {
            let span = self.span();
            return Pattern { kind: PatternKind::Binding { name: "_".into(), mutable: false, by_ref: None }, span };
        };
        // Unit path or binding?
        if path.segments.len() == 1 && path.segments[0].args.is_empty() && !self.at(&TokenKind::LParen) && !self.at(&TokenKind::LBrace) {
            let name = path.segments[0].name.clone();
            return Pattern { kind: PatternKind::Binding { name, mutable: false, by_ref: None }, span: path.span };
        }
        if self.at(&TokenKind::LParen) {
            self.advance();
            let mut fields = Vec::new();
            while !self.at(&TokenKind::RParen) && !self.at_eof() {
                fields.push(self.parse_pattern());
                if !self.eat(&TokenKind::Comma) {
                    break;
                }
            }
            let end = self.expect(TokenKind::RParen, "E0364", "variant pattern").unwrap_or(self.prev_span());
            return Pattern { kind: PatternKind::Variant { path, kind: VariantPatternKind::Tuple(fields) }, span: start.to(end) };
        }
        if self.at(&TokenKind::LBrace) {
            self.advance();
            let mut fields = Vec::new();
            let mut rest = false;
            while !self.at(&TokenKind::RBrace) && !self.at_eof() {
                if self.eat(&TokenKind::DotDot) {
                    rest = true;
                    break;
                }
                let fspan = self.span();
                let Some((name, name_span)) = self.expect_ident("E0365", "for the pattern field") else { break };
                let pattern = if self.eat(&TokenKind::Colon) { Some(self.parse_pattern()) } else { None };
                let fend = pattern.as_ref().map(|p| p.span).unwrap_or(name_span);
                fields.push(PatternField { name, name_span, pattern, span: fspan.to(fend) });
                if !self.eat(&TokenKind::Comma) {
                    break;
                }
            }
            let end = self.expect(TokenKind::RBrace, "E0366", "struct pattern").unwrap_or(self.prev_span());
            return Pattern {
                kind: PatternKind::Variant { path, kind: VariantPatternKind::Struct { fields, rest } },
                span: start.to(end),
            };
        }
        Pattern { span: path.span, kind: PatternKind::Variant { path, kind: VariantPatternKind::Unit } }
    }
}
