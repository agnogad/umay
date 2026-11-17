//! Expression checking: control flow.

use crate::ast;
use crate::diagnostics::{Diagnostic, Suggestion};
use crate::hir::*;
use crate::sema::check_expr::FnChecker;
use crate::types::{TyId, TyKind};

impl FnChecker<'_, '_> {
    // ---- if --------------------------------------------------------------

    pub fn check_if(
        &mut self,
        cond: &ast::Expr,
        then_block: &ast::Block,
        else_branch: Option<&ast::Expr>,
        span: crate::source::Span,
    ) -> HirExpr {
        let cond = self.check_condition(cond, "if");
        let then_block = self.check_block(then_block);
        let then_ty = self.block_ty(&then_block);
        let else_hir = else_branch.map(|e| Box::new(self.check_expr(e)));

        let ty = match &else_hir {
            None => {
                // Without `else`, the branch's value is discarded.
                // A diverging branch (`{ break }`, `{ return }`, `{ panic!() }`)
                // produces no value to discard, so there is nothing to warn
                // about: the branch leaves the `if` altogether.
                let diverges = matches!(self.cx.cx.kind(then_ty), TyKind::Never);
                if !self.cx.cx.is_error(then_ty)
                    && !is_unit(&self.cx.cx, then_ty)
                    && !diverges
                    && then_block.tail.is_some()
                {
                    let name = self.cx.ty_name(then_ty);
                    self.push(
                        Diagnostic::warning(
                            "W0103",
                            format!("`if` without `else` discards a value of type `{name}`"),
                        )
                        .at(then_block.span)
                        .with_cause("the branch produces a value but there is no other branch to match it")
                        .with_suggestion(Suggestion::new(
                            "add an `else` branch, or end the block with `;`",
                            then_block.span,
                            String::new(),
                        ).with_detail("the replacement depends on intent")),
                    );
                }
                self.cx.cx.unit()
            }
            Some(e) => {
                let else_ty = e.ty;
                let _ = &else_ty;
                if self.cx.cx.is_error(then_ty) || self.cx.cx.is_error(else_ty) {
                    self.cx.cx.error()
                } else if is_never(&self.cx.cx, then_ty) {
                    else_ty
                } else if is_never(&self.cx.cx, else_ty) {
                    then_ty
                } else if self.cx.types_compatible(then_ty, else_ty) {
                    // A branch that is only a literal has no type of its own to
                    // keep: `if n == 0 { 8 } else { n * 2 }` is the type of
                    // `n * 2`, and keeping the literal's `i32` would truncate the
                    // other branch's value to make the two agree.
                    let then_var_ty = self.cx.cx.is_integer_var(then_ty);
                    let else_var_ty = self.cx.cx.is_integer_var(else_ty);
                    if then_var_ty && !else_var_ty {
                        else_ty
                    } else {
                        then_ty
                    }
                } else if self.cx.types_compatible(else_ty, then_ty) {
                    else_ty
                } else {
                    let a = self.cx.ty_name(then_ty);
                    let b = self.cx.ty_name(else_ty);
                    self.push(
                        Diagnostic::error(
                            "E1001",
                            format!("`if` and `else` have incompatible types: `{a}` and `{b}`"),
                        )
                        .at(e.span)
                        .with_cause("an `if`/`else` expression must produce one type on both branches"),
                    );
                    self.cx.cx.error()
                }
            }
        };

        HirExpr { kind: HirExprKind::If { cond: Box::new(cond), then_block, else_branch: else_hir }, ty, span }
    }

    /// Checks that a condition is `bool`, with a targeted message for the
    /// common "assignment instead of comparison" mistake.
    pub(crate) fn check_condition(&mut self, cond: &ast::Expr, context: &str) -> HirExpr {
        let hir = self.check_expr(cond);
        if self.cx.cx.is_error(hir.ty) {
            return hir;
        }
        let bool_ty = self.cx.cx.bool();
        if self.cx.types_compatible(hir.ty, bool_ty) {
            return hir;
        }
        let name = self.cx.ty_name(hir.ty);
        let mut d = Diagnostic::error(
            "E1001",
            format!("`{context}` condition must be `bool`, found `{name}`"),
        )
        .at(cond.span)
        .with_cause("conditions are not implicitly converted to `bool`");
        // `if x = 1` is the classic mistake; suggest `==`.
        if let ast::ExprKind::Binary { op: ast::BinaryOp::Eq, lhs, .. } = &cond.kind {
            let lhs_name = match &lhs.kind {
                ast::ExprKind::Ident(n) => Some(n.clone()),
                _ => None,
            };
            d = d.with_suggestion(Suggestion::new(
                "this is an assignment; did you mean to compare?",
                cond.span,
                String::new(),
            ).with_detail(format!(
                "replace `=` with `==`{}",
                lhs_name.map(|n| format!(", i.e. `{n} == ...`")).unwrap_or_default()
            )));
        } else if self.cx.cx.is_integer(hir.ty) {
            d = d.with_suggestion(Suggestion::new(
                "compare explicitly",
                cond.span,
                String::new(),
            ).with_detail("for example `x != 0`"));
        }
        self.push(d);
        // Keep the expression as checked; the diagnostic already explains that
        // it is not a `bool`. Returning `bool` here would hide later errors.
        hir
    }

    // ---- match -----------------------------------------------------------

    pub fn check_match(&mut self, scrutinee: &ast::Expr, arms: &[ast::MatchArm], span: crate::source::Span) -> HirExpr {
        let mut scrut = self.check_expr(scrutinee);
        // Matching on a reference inspects the value behind it. Without this a
        // method taking `&self` could not match on its own receiver, which is the
        // shape `Option::is_some` has.
        let pointee = match self.cx.cx.kind(scrut.ty) {
            crate::types::TyKind::Ref(inner) | crate::types::TyKind::RefMut(inner) => Some(*inner),
            _ => None,
        };
        if let Some(ty) = pointee {
            let span = scrut.span;
            scrut = HirExpr { kind: HirExprKind::Deref(Box::new(scrut)), ty, span };
        }
        let scrut_ty = self.resolve_scrutinee_ty(scrut.ty);

        if arms.is_empty() {
            self.push(
                Diagnostic::error("E1001", "`match` must have at least one arm")
                    .at(span)
                    .with_cause("an empty `match` cannot produce a value"),
            );
            return self.error_hir(span);
        }

        let mut checked_arms = Vec::new();
        let mut result_ty: Option<TyId> = None;
        for arm in arms {
            self.scopes.push_frame();
            let hir_pattern = self.bind_pattern(&arm.pattern, scrut_ty, false);
            let guard_hir = arm.guard.as_ref().map(|g| self.check_expr(g));
            // The type the earlier arms settled on is the best hint the later ones
            // can have: `Result::Err(_) => Option::None` reads its `T` from the
            // `Ok` arm above it.
            let body = self.check_expr_hinted(&arm.body, result_ty);
            self.scopes.pop_frame();
            let body_ty = body.ty;
            result_ty = Some(match result_ty {
                None => body_ty,
                Some(prev) => {
                    if self.cx.cx.is_error(prev) || self.cx.cx.is_error(body_ty) {
                        self.cx.cx.error()
                    } else if is_never(&self.cx.cx, body_ty) {
                        prev
                    } else if is_never(&self.cx.cx, prev) {
                        body_ty
                    } else if self.cx.types_compatible(prev, body_ty) {
                        prev
                    } else if self.cx.types_compatible(body_ty, prev) {
                        body_ty
                    } else {
                        let a = self.cx.ty_name(prev);
                        let b = self.cx.ty_name(body_ty);
                        self.push(
                            Diagnostic::error(
                                "E1001",
                                format!("match arms have incompatible types: `{a}` and `{b}`"),
                            )
                            .at(body.span)
                            .with_cause("every arm of a `match` must produce the same type"),
                        );
                        self.cx.cx.error()
                    }
                },
            });
            checked_arms.push(HirMatchArm {
                pattern: hir_pattern,
                guard: guard_hir,
                body,
                span: arm.span,
            });
        }

        // Exhaustiveness.
        if !self.cx.cx.is_error(scrut_ty) {
            self.check_exhaustive(scrut_ty, arms, scrutinee.span);
        }

        let ty = result_ty.unwrap_or_else(|| self.cx.cx.unit());
        HirExpr {
            kind: HirExprKind::Match { scrutinee: Box::new(scrut), arms: checked_arms },
            ty,
            span,
        }
    }

    /// Literal variables in a scrutinee position must be concrete to allow
    /// exhaustiveness reasoning; they default to `i32`/`f64`.
    pub(crate) fn resolve_scrutinee_ty(&mut self, ty: TyId) -> TyId {
        if self.cx.cx.is_integer_var(ty) {
            return self.cx.cx.primitive("i32").unwrap();
        }
        if self.cx.cx.is_float_var(ty) {
            return self.cx.cx.primitive("f64").unwrap();
        }
        ty
    }

    // ---- loops -----------------------------------------------------------

    pub fn check_while(&mut self, cond: &ast::Expr, body: &ast::Block, span: crate::source::Span) -> HirExpr {
        let cond = self.check_condition(cond, "while");
        let body = self.check_block(body);
        let ty = self.cx.cx.unit();
        HirExpr { kind: HirExprKind::While { cond: Box::new(cond), body }, ty, span }
    }

    pub fn check_for(
        &mut self,
        pattern: &ast::Pattern,
        iter: &ast::Expr,
        body: &ast::Block,
        span: crate::source::Span,
    ) -> HirExpr {
        // `for i in a..b` and `for x in array` are the two supported forms.
        let elem_ty;
        let iter_ty = if let ast::ExprKind::Range { start, end, inclusive } = &iter.kind {
            let start_hir = match start {
                Some(e) => self.check_expr(e),
                None => self.error_hir(iter.span),
            };
            let end_hir = match end {
                Some(e) => self.check_expr(e),
                None => self.error_hir(iter.span),
            };
            let elem = self.resolve_iter_elem(start_hir.ty);
            if !self.cx.cx.is_error(end_hir.ty) && !self.cx.cx.is_error(elem) && !self.cx.types_compatible(elem, end_hir.ty) {
                let a = self.cx.ty_name(elem);
                let b = self.cx.ty_name(end_hir.ty);
                self.push(
                    Diagnostic::error(
                        "E1001",
                        format!("range start and end have different types: `{a}` and `{b}`"),
                    )
                    .at(iter.span)
                    .with_cause("both bounds of a range must have the same type"),
                );
            }
            if !self.cx.cx.is_integer(elem) {
                let name = self.cx.ty_name(elem);
                self.push(
                    Diagnostic::error("E1014", format!("cannot iterate over a range of `{name}`"))
                        .at(iter.span)
                        .with_cause("only integer ranges are iterable"),
                );
            }
            let _ = inclusive;
            elem_ty = elem;
            HirExpr {
                kind: HirExprKind::RangeExpr {
                    start: Box::new(start_hir),
                    end: Box::new(end_hir),
                    inclusive: *inclusive,
                    elem,
                },
                ty: elem,
                span: iter.span,
            }
        } else {
            let h = self.check_expr(iter);
            let elem = match self.cx.cx.kind(self.strip_refs_for_iter(h.ty)).clone() {
                TyKind::Array { elem, .. } | TyKind::Slice(elem) => elem,
                TyKind::Error => self.cx.cx.error(),
                _ => {
                    let name = self.cx.ty_name(h.ty);
                    self.push(
                        Diagnostic::error("E1014", format!("`{name}` is not iterable"))
                            .at(iter.span)
                            .with_cause("`for` accepts integer ranges, arrays and slices")
                            .with_explanation(
                                "Iterator support for user types arrives with the iterator \
                                 protocol in Phase 6. For now iterate a range or a collection.",
                            ),
                    );
                    self.cx.cx.error()
                }
            };
            elem_ty = elem;
            HirExpr { kind: HirExprKind::IterSource { source: Box::new(h), elem }, ty: elem, span: iter.span }
        };

        self.scopes.push_frame();
        let hir_pattern = self.bind_pattern(pattern, elem_ty, false);
        let mut pattern_locals = Vec::new();
        collect_for_locals(&hir_pattern, &mut pattern_locals);
        let body_hir = self.check_block(body);
        self.scopes.pop_frame();
        let ty = self.cx.cx.unit();
        HirExpr {
            kind: HirExprKind::For { pattern_locals, iter: Box::new(iter_ty), body: body_hir },
            ty,
            span,
        }
    }

    pub(crate) fn strip_refs_for_iter(&self, mut ty: TyId) -> TyId {
        loop {
            match self.cx.cx.kind(ty) {
                TyKind::Ref(inner) | TyKind::RefMut(inner) => ty = *inner,
                _ => return ty,
            }
        }
    }

    pub(crate) fn resolve_iter_elem(&mut self, ty: TyId) -> TyId {
        if self.cx.cx.is_integer_var(ty) {
            return self.cx.cx.primitive("i32").unwrap();
        }
        ty
    }

    // ---- jumps -----------------------------------------------------------

    pub fn check_return(&mut self, value: Option<&ast::Expr>, span: crate::source::Span) -> HirExpr {
        let ret = self.ret_ty;
        // The return type is a hint: it is what tells `Option::None` which `T` it
        // is, since the variant itself carries no value to name one.
        let v = value.map(|e| Box::new(self.check_expr_hinted(e, Some(ret))));
        match &v {
            Some(expr) => {
                if !self.cx.cx.is_error(ret) && !self.cx.cx.is_error(expr.ty) && !self.cx.types_compatible(ret, expr.ty) {
                    // A literal can adopt the return type.
                    let literal_ok = (self.cx.cx.is_integer_var(expr.ty) && self.cx.cx.is_integer(ret))
                        || (self.cx.cx.is_float_var(expr.ty) && self.cx.cx.is_float(ret));
                    if !literal_ok {
                        let want = self.cx.ty_name(ret);
                        let got = self.cx.ty_name(expr.ty);
                        self.push(
                            Diagnostic::error(
                                "E1001",
                                format!("returned value has type `{got}`, but the function returns `{want}`"),
                            )
                            .at(expr.span)
                            .with_cause("`return` must produce the declared return type"),
                        );
                    }
                }
            }
            None => {
                if !self.cx.cx.is_error(ret) && !is_unit(&self.cx.cx, ret) {
                    let want = self.cx.ty_name(ret);
                    self.push(
                        Diagnostic::error(
                            "E1001",
                            format!("`return` without a value in a function returning `{want}`"),
                        )
                        .at(span)
                        .with_cause("the declared return type is not `()`"),
                    );
                }
            }
        }
        HirExpr { kind: HirExprKind::Return(v), ty: self.cx.cx.never(), span }
    }

    pub fn check_try(&mut self, inner: &ast::Expr, span: crate::source::Span) -> HirExpr {
        let inner = self.check_expr(inner);
        let ty = self.cx.cx.error();
        let inner_ty = self.strip_refs_for_iter(inner.ty);
        let TyKind::Adt { def, .. } = self.cx.cx.kind(inner_ty).clone() else {
            if !self.cx.cx.is_error(inner.ty) {
                let name = self.cx.ty_name(inner.ty);
                self.push(
                    Diagnostic::error("E1014", format!("`?` cannot be applied to `{name}`"))
                        .at(span)
                        .with_cause("`?` requires an `Option` or `Result`"),
                );
            }
            return HirExpr { kind: HirExprKind::Try(Box::new(inner)), ty, span };
        };
        let enum_name = self.cx.hir.def_name(def);
        if enum_name != "Option" && enum_name != "Result" {
            self.push(
                Diagnostic::error("E1014", format!("`?` cannot be applied to `{enum_name}`"))
                    .at(span)
                    .with_cause("`?` is defined only for `Option` and `Result`"),
            );
            return HirExpr { kind: HirExprKind::Try(Box::new(inner)), ty, span };
        }
        // Determine the success type from the variant fields.
        let success_ty = self
            .cx
            .hir
            .enums
            .iter()
            .find(|e| e.def_id == def)
            .and_then(|e| e.variants.iter().find(|v| v.name == "Some" || v.name == "Ok"))
            .and_then(|v| match &v.fields {
                VariantFields::Tuple(tys) => tys.first().copied(),
                _ => None,
            })
            .unwrap_or_else(|| self.cx.cx.unit());

        // The enclosing function must return the same wrapper type.
        let ret_wrapped = matches!(self.cx.cx.kind(self.strip_refs_for_iter(self.ret_ty)), TyKind::Adt { .. });
        if !ret_wrapped && !self.cx.cx.is_error(self.ret_ty) {
            let ret_name = self.cx.ty_name(self.ret_ty);
            self.push(
                Diagnostic::error(
                    "E1015",
                    format!("`?` requires the enclosing function to return `Option` or `Result`, found `{ret_name}`"),
                )
                .at(span)
                .with_cause("the error value is propagated to the caller, so the caller must accept it")
                .with_explanation(
                    "`expr?` returns early if the value is `None`/`Err`. That is only meaningful \
                     if the function's own return type can carry the failure.",
                ),
            );
        }
        HirExpr { kind: HirExprKind::Try(Box::new(inner)), ty: success_ty, span }
    }
}

/// Locals introduced by a `for` loop's pattern.
fn collect_for_locals(pat: &crate::hir::HirPattern, out: &mut Vec<LocalId>) {
    match &pat.kind {
        crate::hir::HirPatternKind::Binding { local, .. } => out.push(*local),
        crate::hir::HirPatternKind::Tuple(ps) | crate::hir::HirPatternKind::Or(ps) => {
            ps.iter().for_each(|p| collect_for_locals(p, out))
        }
        crate::hir::HirPatternKind::Struct { fields, .. } => {
            fields.iter().for_each(|f| collect_for_locals(&f.pattern, out))
        }
        _ => {}
    }
}

fn is_unit(types: &crate::types::TyCtxt, ty: TyId) -> bool {
    matches!(types.kind(ty), TyKind::Unit)
}

fn is_never(types: &crate::types::TyCtxt, ty: TyId) -> bool {
    matches!(types.kind(ty), TyKind::Never)
}
