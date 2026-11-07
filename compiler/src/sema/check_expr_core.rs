//! Expression checking: the core type-checking rules.
//!
//! Split into a separate file so that `check_expr.rs` (declarations, patterns,
//! scopes) stays reviewable.

use crate::ast;
use crate::diagnostics::Diagnostic;
use crate::hir::*;
use crate::sema::check_expr::FnChecker;
use crate::types::{TyId, TyKind};

impl FnChecker<'_, '_> {
    pub fn check_expr(&mut self, e: &ast::Expr) -> HirExpr {
        self.check_expr_hinted(e, None)
    }

    /// Checks an expression whose type the surrounding context already knows.
    ///
    /// The hint is only an aid to inference: a struct literal that cannot name
    /// its own type arguments takes them from the hint, so
    /// `let v: Vec<i64> = Vec { .. }` is accepted. Nothing else changes, and a
    /// wrong hint is still reported as a type mismatch by the caller.
    pub fn check_expr_hinted(
        &mut self,
        e: &ast::Expr,
        hint: Option<crate::types::TyId>,
    ) -> HirExpr {
        let span = e.span;
        let kind = match &e.kind {
            ast::ExprKind::IntLit { value, suffix } => self.check_int_lit(*value, *suffix, span),
            ast::ExprKind::FloatLit { value, suffix, .. } => self.check_float_lit(*value, *suffix, span),
            ast::ExprKind::StrLit(s) => {
                // A string literal is a `&str`: borrowing a static buffer.
                let ty = self.cx.cx.str_ref();
                HirExpr { kind: HirExprKind::StrLit(s.clone()), ty, span }
            }
            ast::ExprKind::StrInterp(parts) => self.check_str_interp(parts, span),
            ast::ExprKind::CharLit(c) => {
                let ty = self.cx.cx.char();
                HirExpr { kind: HirExprKind::CharLit(*c), ty, span }
            }
            ast::ExprKind::BoolLit(b) => {
                let ty = self.cx.cx.bool();
                HirExpr { kind: HirExprKind::BoolLit(*b), ty, span }
            }
            ast::ExprKind::Ident(name) => self.check_ident(name, span),
            ast::ExprKind::Path(path) => self.check_path_expr(path, span),
            ast::ExprKind::Assign { place, value } => self.check_assign(place, value, span),
            ast::ExprKind::AssignOp { op, place, value } => {
                // Desugar `x op= y` into `x = x op y`; the binary form carries
                // the type rules, so compound assignment cannot differ from it.
                let place_expr = self.check_expr(place);
                let value_expr = self.check_expr(value);
                let place_ty = place_expr.ty;
                let value_ty = value_expr.ty;
                let unit = self.cx.cx.unit();
                if !self.cx.cx.is_error(place_ty)
                    && !self.cx.cx.is_error(value_ty)
                    && !self.cx.types_compatible(place_ty, value_ty)
                {
                    let want = self.cx.ty_name(place_ty);
                    let got = self.cx.ty_name(value_ty);
                    let op_text = op.as_str();
                    self.push(
                        Diagnostic::error(
                            "E1001",
                            format!("cannot apply `{op_text}=` with `{got}`, the target has type `{want}`"),
                        )
                        .at(span)
                        .with_cause("compound assignment requires both sides to have the same type"),
                    );
                    return self.error_hir(span);
                }
                // Re-run the full rules (mutability, numeric operands) through the
                // binary form so that `+=` adds no new semantics.
                let combined = ast::Expr {
                    kind: ast::ExprKind::Binary { op: *op, lhs: place.clone(), rhs: value.clone() },
                    span,
                };
                let _ = (place_expr, value_expr, unit);
                self.check_assign(place, &combined, span)
            }
            ast::ExprKind::Unary { op, operand } => self.check_unary(*op, operand, span),
            ast::ExprKind::Binary { op, lhs, rhs } => self.check_binary(*op, lhs, rhs, span),
            ast::ExprKind::Ref { mutable, operand } => self.check_ref(*mutable, operand, span),
            ast::ExprKind::Cast { expr, ty } => self.check_cast(expr, ty, span),
            ast::ExprKind::Call { callee, args, .. } => self.check_call(callee, args, span),
            ast::ExprKind::MethodCall { receiver, method, method_span, args } => {
                self.check_method_call(receiver, method, *method_span, args, span)
            }
            ast::ExprKind::FieldAccess { base, field, field_span } => {
                self.check_field_access(base, field, *field_span, span)
            }
            ast::ExprKind::Index { base, index } => self.check_index(base, index, span),
            ast::ExprKind::StructLit { path, fields, base } => {
                self.check_struct_lit(path, fields, base.as_deref(), span, hint)
            }
            ast::ExprKind::ArrayLit { elems } => self.check_array_lit(elems, span),
            ast::ExprKind::ArrayRepeat { value, count } => self.check_array_repeat(value, count, span),
            ast::ExprKind::Tuple { elems } => self.check_tuple_lit(elems, span),
            ast::ExprKind::If { cond, then_block, else_branch } => {
                self.check_if(cond, then_block, else_branch.as_deref(), span)
            }
            ast::ExprKind::Match { scrutinee, arms } => self.check_match(scrutinee, arms, span),
            ast::ExprKind::While { cond, body } => self.check_while(cond, body, span),
            ast::ExprKind::Loop { body } => {
                let body = self.check_block(body);
                let ty = self.cx.cx.unit();
                HirExpr { kind: HirExprKind::Loop { body }, ty, span }
            }
            ast::ExprKind::For { pattern, iter, body } => self.check_for(pattern, iter, body, span),
            ast::ExprKind::Block(b) => {
                let block = self.check_block(b);
                let ty = self.block_ty(&block);
                HirExpr { kind: HirExprKind::Block(block), ty, span }
            }
            ast::ExprKind::Return(value) => self.check_return(value.as_deref(), span),
            ast::ExprKind::Break(value) => {
                let v = value.as_ref().map(|e| Box::new(self.check_expr(e)));
                let ty = self.cx.cx.never();
                HirExpr { kind: HirExprKind::Break(v), ty, span }
            }
            ast::ExprKind::Continue => {
                let ty = self.cx.cx.never();
                HirExpr { kind: HirExprKind::Continue, ty, span }
            }
            ast::ExprKind::Try(inner) => self.check_try(inner, span),
            ast::ExprKind::Defer(body) => {
                let block = self.check_block(body);
                let ty = self.cx.cx.unit();
                HirExpr { kind: HirExprKind::Defer(block), ty, span }
            }
            ast::ExprKind::Move(inner) => {
                let inner = self.check_expr(inner);
                let ty = inner.ty;
                HirExpr { kind: HirExprKind::Move(Box::new(inner)), ty, span }
            }
            ast::ExprKind::Unsafe(body) => {
                self.unsafe_depth += 1;
                let block = self.check_block(body);
                self.unsafe_depth -= 1;
                let ty = self.block_ty(&block);
                HirExpr { kind: HirExprKind::Unsafe(block), ty, span }
            }
            ast::ExprKind::Range { .. } => {
                self.push(
                    Diagnostic::error("E1001", "range expressions are only supported in `for` loops")
                        .at(span)
                        .with_cause("a range on its own has no type yet")
                        .with_explanation(
                            "Range types arrive with the iterator protocol in Phase 6. Until then, \
                             `for i in a..b` is the supported form.",
                        ),
                );
                self.error_hir(span)
            }
            ast::ExprKind::Infer => {
                self.push(
                    Diagnostic::error("E1002", "cannot infer this expression's type from `_`")
                        .at(span)
                        .with_cause("`_` needs a surrounding type annotation to resolve"),
                );
                self.error_hir(span)
            }
        };
        kind
    }

    pub(crate) fn error_hir(&mut self, span: crate::source::Span) -> HirExpr {
        let ty = self.cx.cx.error();
        HirExpr { kind: HirExprKind::Error, ty, span }
    }

    // ---- literals --------------------------------------------------------

    fn check_int_lit(&mut self, value: u128, suffix: Option<crate::token::IntSuffix>, span: crate::source::Span) -> HirExpr {
        let ty = match suffix {
            Some(s) => self.cx.cx.primitive(s.as_str()).unwrap_or_else(|| self.cx.cx.fresh_int_var()),
            None => self.cx.cx.fresh_int_var(),
        };
        // Range check against the concrete type when one is known.
        if suffix.is_some() {
            if let Some((min, max)) = self.cx.cx.int_bounds(ty) {
                let unsigned = !self.cx.cx.is_signed(ty);
                let fits = if value > i128::MAX as u128 {
                    !unsigned && max == i128::MAX && false
                } else {
                    let v = value as i128;
                    v >= min && v <= max
                };
                if !fits {
                    let name = self.cx.ty_name(ty);
                    self.push(
                        Diagnostic::error("E1001", format!("integer literal `{value}` does not fit in `{name}`"))
                            .at(span)
                            .with_cause(format!("`{name}` can hold values from {min} to {max}")),
                    );
                }
            }
        }
        HirExpr { kind: HirExprKind::IntLit { value, suffix }, ty, span }
    }

    fn check_float_lit(&mut self, value: f64, suffix: Option<crate::token::FloatSuffix>, span: crate::source::Span) -> HirExpr {
        let ty = match suffix {
            Some(crate::token::FloatSuffix::F32) => self.cx.cx.primitive("f32").unwrap(),
            Some(crate::token::FloatSuffix::F64) => self.cx.cx.primitive("f64").unwrap(),
            None => self.cx.cx.fresh_float_var(),
        };
        HirExpr { kind: HirExprKind::FloatLit { value, suffix }, ty, span }
    }

    /// String interpolation: every interpolated expression must be displayable.
    fn check_str_interp(&mut self, parts: &[crate::token::StrPart], span: crate::source::Span) -> HirExpr {
        let mut out = Vec::new();
        for part in parts {
            match part {
                crate::token::StrPart::Text(t) => out.push(HirStrPart::Text(t.clone())),
                crate::token::StrPart::EscapedBrace(c) => out.push(HirStrPart::Text(c.to_string())),
                crate::token::StrPart::Expr { src, span: expr_span } => {
                    // Re-parse the interpolation payload as an expression.
                    match parse_interpolation(src, *expr_span) {
                        Some(expr) => {
                            let checked = self.check_expr(&expr);
                            self.check_displayable(&checked, *expr_span);
                            out.push(HirStrPart::Value(checked));
                        }
                        None => {
                            self.push(
                                Diagnostic::error("E0003", "cannot parse this interpolation as an expression")
                                    .at(*expr_span)
                                    .with_cause("the text inside `{...}` must be a single expression"),
                            );
                        }
                    }
                }
            }
        }
        let ty = self.cx.cx.str_();
        HirExpr { kind: HirExprKind::StrInterp { parts: out }, ty, span }
    }

    /// Formats are limited to types the runtime can render without a custom
    /// `Display` implementation.
    pub(crate) fn check_displayable(&mut self, e: &HirExpr, span: crate::source::Span) {
        if self.cx.cx.is_error(e.ty) || self.cx.cx.is_infer_var(e.ty) {
            return;
        }
        let displayable = self.cx.cx.is_integer(e.ty)
            || self.cx.cx.is_float(e.ty)
            || matches!(
                self.cx.cx.kind(e.ty),
                TyKind::Bool | TyKind::Char | TyKind::Str | TyKind::String | TyKind::Unit
            )
            || matches!(self.cx.cx.kind(e.ty), TyKind::Ref(inner) if matches!(self.cx.cx.kind(*inner), TyKind::Str));
        if !displayable {
            let name = self.cx.ty_name(e.ty);
            self.push(
                Diagnostic::error("E1012", format!("`{name}` cannot be interpolated into a string"))
                    .at(span)
                    .with_cause("only numbers, `bool`, `char`, `str` and `String` are directly printable")
                    .with_explanation(
                        "UMAY formats values through the `Display` trait. User types become \
                         printable once `impl Display for YourType` is implemented (Phase 6).",
                    ),
            );
        }
    }

    // ---- names -----------------------------------------------------------

    fn check_ident(&mut self, name: &str, span: crate::source::Span) -> HirExpr {
        // `_` is a placeholder, not a name: it never resolves and cannot be
        // inferred without a surrounding annotation.
        if name == "_" {
            self.push(
                Diagnostic::error("E1002", "cannot infer this expression's type from `_`")
                    .at(span)
                    .with_cause("`_` is a placeholder, not a value")
                    .with_explanation(
                        "`_` asks the compiler to infer a type. In an expression position there \
                         is nothing to infer from, so it is an error. Write the value, or use \
                         `_` only in a type annotation or pattern.",
                    ),
            );
            return self.error_hir(span);
        }
        if let Some(entry) = self.scopes.lookup(name) {
            let ty = entry.ty;
            return HirExpr { kind: HirExprKind::Local(entry.id), ty, span };
        }
        if let Some(entry) = self.cx.items.lookup(name) {
            match entry.res {
                Res::Fn(id) if id.0 != u32::MAX => {
                    let ty = self.fn_type(id);
                    return HirExpr { kind: HirExprKind::FnRef(id), ty, span };
                }
                Res::Const(id) => {
                    let ty = self.const_type(id);
                    return HirExpr { kind: HirExprKind::ConstRef(id), ty, span };
                }
                Res::Type(_) => {
                    // A type used as a value is usually a missing call or a
                    // missing `::Variant`.
                    self.push(
                        Diagnostic::error("E0101", format!("`{name}` is a type, not a value"))
                            .at(span)
                            .with_cause("types cannot be used where a value is expected"),
                    );
                    return self.error_hir(span);
                }
                _ => {}
            }
        }
        self.unknown_name(name, span);
        self.error_hir(span)
    }

    fn check_path_expr(&mut self, path: &ast::Path, span: crate::source::Span) -> HirExpr {
        // Only enum variants and associated function paths are valid here.
        if path.segments.len() == 2 {
            let second = &path.segments[1];
            let first = &path.segments[0];
            // `Type::method(...)` is handled by check_call; a bare path here is
            // either a unit variant or a qualified function.
            if let Some(def) = self.cx.def_by_name.get(&first.name).copied() {
                if let Some(info) = self.cx.hir.enums.iter().find(|e| e.def_id == def) {
                    if let Some(i) = info.variants.iter().position(|v| v.name == second.name) {
                        let variant = &info.variants[i];
                        if !matches!(variant.fields, VariantFields::Unit) {
                            if let VariantFields::Tuple(tys) = &variant.fields {
                                let _ = tys;
                            }
                            self.push(
                                Diagnostic::error(
                                    "E1005",
                                    format!("variant `{}::{}` needs arguments", first.name, second.name),
                                )
                                .at(span)
                                .with_cause("this variant carries fields, so it must be constructed with a value"),
                            );
                            return self.error_hir(span);
                        }
                        let ty = self.variant_type(def, i as u32);
                        return HirExpr { kind: HirExprKind::UnitVariant { def, index: i as u32 }, ty, span };
                    }
                }
            }
            if let Some(fn_id) = self.cx.methods.get(&(first.name.clone(), second.name.clone())).copied() {
                let ty = self.fn_type(fn_id);
                return HirExpr { kind: HirExprKind::FnRef(fn_id), ty, span };
            }
        }
        if path.segments.len() == 1 {
            return self.check_ident(&path.segments[0].name, span);
        }
        self.unknown_name(&path.as_string(), span);
        self.error_hir(span)
    }

    // ---- helpers on Checker's data ---------------------------------------

    pub fn fn_type(&mut self, id: FnId) -> TyId {
        let (params, ret) = match self.cx.hir.fns.iter().find(|f| f.id == id) {
            Some(f) => (f.params.clone(), f.ret),
            None => (Vec::new(), self.cx.cx.unit()),
        };
        self.cx.cx.fn_ptr(params, ret)
    }

    pub(crate) fn const_type(&mut self, id: ConstId) -> TyId {
        self.cx.hir.consts.get(id.0 as usize).map(|c| c.ty).unwrap_or_else(|| self.cx.cx.error())
    }

    pub(crate) fn variant_type(&mut self, def: crate::types::DefId, _index: u32) -> TyId {
        self.cx.cx.adt(def, Vec::new())
    }
}

/// Parses interpolation payload text into an expression, reusing the lexer and
/// parser so that interpolation accepts exactly the same grammar as other
/// expressions.
pub fn parse_interpolation(src: &str, span: crate::source::Span) -> Option<ast::Expr> {
    let mut map = crate::source::SourceMap::new();
    // A bare expression is not a valid program, so parse it inside a synthetic
    // function and take the first statement of the body.
    let wrapped = format!("fn __interp() {{ {src} }}");
    let wid = map.add("<interpolation>", wrapped.clone());
    let (tokens, errors) = crate::lexer::tokenize(&wrapped, wid);
    if !errors.is_empty() {
        return None;
    }
    let parsed = crate::parser::parse(tokens, Vec::new(), wid);
    if parsed.diagnostics.iter().any(|d| d.severity.is_fatal()) {
        return None;
    }
    let item = parsed.program.items.first()?;
    let ast::Item::Fn(f) = item else { return None };
    let body = f.body.as_ref()?;
    let _ = span;
    match (&body.stmts.len(), &body.tail) {
        (0, Some(t)) => Some((**t).clone()),
        (1, None) => match &body.stmts[0].kind {
            ast::StmtKind::Expr(e) => Some(e.clone()),
            _ => None,
        },
        _ => None,
    }
}
