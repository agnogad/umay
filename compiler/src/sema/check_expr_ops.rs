//! Expression checking: operators, casts and calls.

use crate::ast;
use crate::diagnostics::{Diagnostic, Suggestion};
use crate::hir::*;
use crate::sema::check_expr::FnChecker;
use crate::types::{TyId, TyKind};

impl FnChecker<'_, '_> {
    // ---- unary -----------------------------------------------------------

    pub fn check_unary(&mut self, op: ast::UnaryOp, operand: &ast::Expr, span: crate::source::Span) -> HirExpr {
        self.check_unary_hinted(op, operand, span, None)
    }

    /// `-n` takes its width from the context: a `-1` passed to an `i64`
    /// parameter is an `i64`, not an `i32` widened on the way out.
    pub fn check_unary_hinted(
        &mut self,
        op: ast::UnaryOp,
        operand: &ast::Expr,
        span: crate::source::Span,
        hint: Option<TyId>,
    ) -> HirExpr {
        let operand = match hint {
            Some(h) if op == ast::UnaryOp::Neg && self.cx.cx.is_numeric(h) => {
                self.check_expr_hinted(operand, Some(h))
            }
            _ => self.check_expr(operand),
        };
        match op {
            ast::UnaryOp::Neg => {
                let ty = operand.ty;
                if self.cx.cx.is_error(ty) {
                    return HirExpr { kind: HirExprKind::Unary { op: HirUnaryOp::Neg, operand: Box::new(operand) }, ty, span };
                }
                if !self.cx.cx.is_numeric(ty) {
                    let name = self.cx.ty_name(ty);
                    self.push(
                        Diagnostic::error("E1014", format!("cannot negate a value of type `{name}`"))
                            .at(span)
                            .with_cause("unary `-` is defined only for numeric types"),
                    );
                    return self.error_hir(span);
                }
                HirExpr { kind: HirExprKind::Unary { op: HirUnaryOp::Neg, operand: Box::new(operand) }, ty, span }
            }
            ast::UnaryOp::Not => {
                let ty = operand.ty;
                if self.cx.cx.is_error(ty) {
                    return HirExpr { kind: HirExprKind::Unary { op: HirUnaryOp::Not, operand: Box::new(operand) }, ty, span };
                }
                let bool_ty = self.cx.cx.bool();
                let ok = self.cx.types_compatible(ty, bool_ty) || self.cx.cx.is_integer(ty);
                if !ok {
                    let name = self.cx.ty_name(ty);
                    self.push(
                        Diagnostic::error("E1014", format!("cannot apply `!` to a value of type `{name}`"))
                            .at(span)
                            .with_cause("`!` is defined for `bool` (logical not) and integers (bitwise not)"),
                    );
                    return self.error_hir(span);
                }
                // `!` on an integer keeps the integer type; on bool it is bool.
                let result_ty = if self.cx.cx.is_integer(ty) { ty } else { bool_ty };
                HirExpr { kind: HirExprKind::Unary { op: HirUnaryOp::Not, operand: Box::new(operand) }, ty: result_ty, span }
            }
            ast::UnaryOp::Deref => {
                let ty = operand.ty;
                match self.cx.cx.kind(ty).clone() {
                    TyKind::Ref(inner) | TyKind::RefMut(inner) => {
                        HirExpr { kind: HirExprKind::Deref(Box::new(operand)), ty: inner, span }
                    }
                    TyKind::RawPtr(inner) | TyKind::RawPtrMut(inner) => {
                        if self.unsafe_depth == 0 {
                            self.push(
                                Diagnostic::error("E0211", "dereferencing a raw pointer requires `unsafe`")
                                    .at(span)
                                    .with_cause("raw pointers bypass the borrow checker, so uses are marked")
                                    .with_suggestion(Suggestion::new(
                                        "wrap this in an `unsafe` block",
                                        span,
                                        format!("unsafe {{ {} }}", "{ ... }"),
                                    ).with_detail("the replacement is indicative; move the expression inside the block")),
                            );
                        }
                        HirExpr { kind: HirExprKind::Deref(Box::new(operand)), ty: inner, span }
                    }
                    TyKind::Error => self.error_hir(span),
                    _ => {
                        let name = self.cx.ty_name(ty);
                        self.push(
                            Diagnostic::error("E1014", format!("cannot dereference a value of type `{name}`"))
                                .at(span)
                                .with_cause("`*` requires a reference or raw pointer"),
                        );
                        self.error_hir(span)
                    }
                }
            }
        }
    }

    // ---- binary ----------------------------------------------------------

    pub fn check_binary(
        &mut self,
        op: ast::BinaryOp,
        lhs: &ast::Expr,
        rhs: &ast::Expr,
        span: crate::source::Span,
    ) -> HirExpr {
        let lhs = self.check_expr(lhs);
        let rhs = self.check_expr(rhs);
        let lop = convert_binop(op);
        let bool_ty = self.cx.cx.bool();

        // Short-circuit operators require `bool` on both sides.
        if matches!(op, ast::BinaryOp::And | ast::BinaryOp::Or) {
            for (side, side_span) in [("left", lhs.span), ("right", rhs.span)] {
                let ty = if side == "left" { lhs.ty } else { rhs.ty };
                if self.cx.cx.is_error(ty) {
                    continue;
                }
                if !self.cx.types_compatible(ty, bool_ty) {
                    let name = self.cx.ty_name(ty);
                    self.push(
                        Diagnostic::error("E1001", format!("`{}` expects `bool` operands, found `{name}` on the {side}", op.as_str()))
                            .at(side_span)
                            .with_cause("`&&` and `||` do not coerce other types to `bool`"),
                    );
                }
            }
            return HirExpr { kind: HirExprKind::Binary { op: lop, lhs: Box::new(lhs), rhs: Box::new(rhs) }, ty: bool_ty, span };
        }

        // Literal variables unify with the other operand's type.
        let (lt, rt) = self.unify_operands(lhs.ty, rhs.ty, span);

        match op {
            ast::BinaryOp::Eq
            | ast::BinaryOp::Ne
            | ast::BinaryOp::Lt
            | ast::BinaryOp::Le
            | ast::BinaryOp::Gt
            | ast::BinaryOp::Ge => {
                // Comparisons: operands must match; result is bool.
                if !self.cx.cx.is_error(lt) && !self.cx.cx.is_error(rt) && !self.cx.types_compatible(lt, rt) {
                    let lname = self.cx.ty_name(lt);
                    let rname = self.cx.ty_name(rt);
                    self.push(
                        Diagnostic::error(
                            "E1001",
                            format!("cannot compare `{lname}` with `{rname}` using `{}`", op.as_str()),
                        )
                        .at(span)
                        .with_cause("both sides of a comparison must have the same type"),
                    );
                    return self.error_hir(span);
                }
                // Ordering is only defined for numbers, chars, bools and strings.
                if matches!(op, ast::BinaryOp::Lt | ast::BinaryOp::Le | ast::BinaryOp::Gt | ast::BinaryOp::Ge) {
                    let comparable = self.cx.cx.is_numeric(lt)
                        || self.cx.cx.is_numeric(rt)
                        || matches!(self.cx.cx.kind(lt), TyKind::Char | TyKind::Bool | TyKind::Str | TyKind::String)
                        || self.cx.cx.is_error(lt);
                    if !comparable {
                        let name = self.cx.ty_name(lt);
                        self.push(
                            Diagnostic::error("E1014", format!("`{}` is not defined for `{name}`", op.as_str()))
                                .at(span)
                                .with_cause("this type has no ordering"),
                        );
                        return self.error_hir(span);
                    }
                }
                HirExpr { kind: HirExprKind::Binary { op: lop, lhs: Box::new(lhs), rhs: Box::new(rhs) }, ty: bool_ty, span }
            }
            _ => {
                // Arithmetic and bitwise: both operands must be numeric and equal.
                if self.cx.cx.is_error(lt) || self.cx.cx.is_error(rt) {
                    return self.error_hir(span);
                }
                if !self.cx.cx.is_numeric(lt) || !self.cx.cx.is_numeric(rt) {
                    let name = if !self.cx.cx.is_numeric(lt) { self.cx.ty_name(lt) } else { self.cx.ty_name(rt) };
                    self.push(
                        Diagnostic::error(
                            "E1014",
                            format!("`{}` is not defined for `{name}`", op.as_str()),
                        )
                        .at(span)
                        .with_cause("arithmetic and bitwise operators require numeric operands"),
                    );
                    return self.error_hir(span);
                }
                if !self.cx.types_compatible(lt, rt) {
                    let lname = self.cx.ty_name(lt);
                    let rname = self.cx.ty_name(rt);
                    self.push(
                        Diagnostic::error(
                            "E1001",
                            format!("cannot apply `{}` to `{lname}` and `{rname}`", op.as_str()),
                        )
                        .at(span)
                        .with_cause("UMAY never converts between numeric types implicitly; use `as` or a matching literal")
                        .with_explanation(
                            "Mixed-type arithmetic is a common source of subtle bugs. Requiring an \
                             explicit conversion keeps the intent visible at the call site.",
                        ),
                    );
                    return self.error_hir(span);
                }
                // Bitwise and shift operators require integers.
                if op.is_bitwise() && !(self.cx.cx.is_integer(lt) || self.cx.cx.is_integer_var(lt)) {
                    let name = self.cx.ty_name(lt);
                    self.push(
                        Diagnostic::error("E1014", format!("`{}` requires integer operands, found `{name}`", op.as_str()))
                            .at(span)
                            .with_cause("bitwise operators are not defined for floating-point types"),
                    );
                    return self.error_hir(span);
                }
                let result_ty = lt;
                HirExpr { kind: HirExprKind::Binary { op: lop, lhs: Box::new(lhs), rhs: Box::new(rhs) }, ty: result_ty, span }
            }
        }
    }

    /// Makes integer/float literal variables adopt the other operand's type.
    pub(crate) fn unify_operands(&mut self, lt: TyId, rt: TyId, _span: crate::source::Span) -> (TyId, TyId) {
        // A literal variable that meets a concrete type adopts it, and the
        // adoption is remembered so that the *local* the literal came from keeps
        // the same width. Without the record, `let i = 0` used as a `usize` would
        // type the expression `usize` while the local stayed four bytes wide, and
        // a 64-bit read would see its neighbours.
        if self.cx.cx.is_integer_var(lt) && !self.cx.cx.is_infer_var(rt) && self.cx.cx.is_integer(rt) {
            self.cx.settle_var(lt, rt);
            return (rt, rt);
        }
        if self.cx.cx.is_integer_var(rt) && !self.cx.cx.is_infer_var(lt) && self.cx.cx.is_integer(lt) {
            self.cx.settle_var(rt, lt);
            return (lt, lt);
        }
        if self.cx.cx.is_float_var(lt) && !self.cx.cx.is_infer_var(rt) && self.cx.cx.is_float(rt) {
            self.cx.settle_var(lt, rt);
            return (rt, rt);
        }
        if self.cx.cx.is_float_var(rt) && !self.cx.cx.is_infer_var(lt) && self.cx.cx.is_float(lt) {
            self.cx.settle_var(rt, lt);
            return (lt, lt);
        }
        // Two literal vars of the same kind unify with each other.
        if self.cx.cx.is_integer_var(lt) && self.cx.cx.is_integer_var(rt) {
            return (lt, lt);
        }
        if self.cx.cx.is_float_var(lt) && self.cx.cx.is_float_var(rt) {
            return (lt, lt);
        }
        // An integer literal next to a float literal is an error, not a promotion.
        (lt, rt)
    }

    /// `place = value`
    pub(crate) fn check_assign(&mut self, lhs: &ast::Expr, rhs: &ast::Expr, span: crate::source::Span) -> HirExpr {
        let place = self.check_expr(lhs);
        let value = self.check_expr(rhs);
        let unit = self.cx.cx.unit();

        if !self.is_place(&place) {
            self.push(
                Diagnostic::error("E1001", "left-hand side of an assignment is not a place")
                    .at(place.span)
                    .with_cause("only variables, fields, elements and dereferences can be assigned")
                    .with_explanation(
                        "A `place` is an expression that names storage: a local, an index, a \
                         field, or a dereference. Function results and literals are temporary \
                         values and cannot be assigned to.",
                    ),
            );
            return self.error_hir(span);
        }
        self.check_mutable_place(&place);

        if !self.cx.cx.is_error(place.ty) && !self.cx.cx.is_error(value.ty) && !self.cx.types_compatible(place.ty, value.ty) {
            let pname = self.cx.ty_name(place.ty);
            let vname = self.cx.ty_name(value.ty);
            self.push(
                Diagnostic::error("E1001", format!("cannot assign `{vname}` to a place of type `{pname}`"))
                    .at(value.span)
                    .with_cause("assignment requires both sides to have the same type"),
            );
            return self.error_hir(span);
        }
        HirExpr {
            kind: HirExprKind::Assign { place: Box::new(place), value: Box::new(value) },
            ty: unit,
            span,
        }
    }

    /// True if the expression names storage.
    pub fn is_place(&self, e: &HirExpr) -> bool {
        matches!(
            e.kind,
            HirExprKind::Local(_)
                | HirExprKind::Field { .. }
                | HirExprKind::TupleIndex { .. }
                | HirExprKind::Index { .. }
                | HirExprKind::Deref(_)
        ) || matches!(e.kind, HirExprKind::Error)
    }

    /// Reports an attempt to write through an immutable binding.
    pub(crate) fn check_mutable_place(&mut self, e: &HirExpr) {
        let mut cur = e;
        loop {
            match &cur.kind {
                HirExprKind::Local(id) => {
                    // A `&mut T` binding is a place that may be written through
                    // even when it was bound with a plain `let`: the mutability is
                    // in the type, and the borrow checker already proved the
                    // reference exclusive. `fn go(c: &mut C) { c.bump() }` is the
                    // ordinary case of this, and the method's `&mut self` reaches
                    // it as a write through `c`.
                    let by_mut_ref = self
                        .locals
                        .get(id.0 as usize)
                        .map(|l| matches!(self.cx.cx.kind(l.ty), crate::types::TyKind::RefMut(_)))
                        .unwrap_or(false);
                    let mutable = by_mut_ref
                        || self.locals.get(id.0 as usize).map(|l| l.mutable).unwrap_or(true);
                    if !mutable {
                        let name = self.locals.get(id.0 as usize).map(|l| l.name.clone()).unwrap_or_default();
                        self.push(
                            Diagnostic::error("E1001", format!("cannot assign to immutable binding `{name}`"))
                                .at(cur.span)
                                .with_cause("this binding was declared with `let`, not `let mut`")
                                .with_suggestion(Suggestion::new(
                                    "declare it as mutable",
                                    span_of_binding(&name, cur.span),
                                    format!("let mut {name}"),
                                ).with_detail("the replacement applies at the declaration site")),
                        );
                    }
                    return;
                }
                HirExprKind::Field { base, .. } | HirExprKind::TupleIndex { base, .. } => cur = base,
                HirExprKind::Index { base, .. } => cur = base,
                HirExprKind::Deref(inner) => {
                    // Writing through `&mut T` is allowed; the borrow checker
                    // proves the reference is exclusive in Phase 4.
                    let _ = inner;
                    return;
                }
                _ => return,
            }
        }
    }

    // ---- references, casts -----------------------------------------------

    pub fn check_ref(&mut self, mutable: bool, operand: &ast::Expr, span: crate::source::Span) -> HirExpr {
        let inner = self.check_expr(operand);
        if self.cx.cx.is_error(inner.ty) {
            return self.error_hir(span);
        }
        if !self.is_place(&inner) {
            self.push(
                Diagnostic::error("E0210", "cannot borrow a temporary value")
                    .at(inner.span)
                    .with_cause("only places can be borrowed; this is a temporary value")
                    .with_explanation(
                        "A temporary is destroyed at the end of the statement, so a reference to it \
                         would dangle. Bind the value with `let` first.",
                    ),
            );
            return self.error_hir(span);
        }
        if mutable {
            self.check_mutable_place(&inner);
        }
        let ty = if mutable { self.cx.cx.ref_mut_to(inner.ty) } else { self.cx.cx.ref_to(inner.ty) };
        HirExpr { kind: HirExprKind::Ref { mutable, place: Box::new(inner) }, ty, span }
    }

    pub fn check_cast(&mut self, expr: &ast::Expr, ty: &ast::TypeExpr, span: crate::source::Span) -> HirExpr {
        // The cast's target is the operand's context: `make(7i64).get() as i64`
        // says which instance the receiver of `get` must be, so the operand is
        // checked against it rather than in isolation.
        let generics = self.generics.clone();
        let to = self.cx.resolve_type(ty, &generics);
        let value = self.check_expr_hinted(expr, Some(to));
        if self.cx.cx.is_error(value.ty) || self.cx.cx.is_error(to) {
            return self.error_hir(span);
        }
        let from = value.ty;
        // Resolve literal variables to their target for a meaningful check.
        let from_concrete = if self.cx.cx.is_integer_var(from) {
            self.cx.cx.primitive("i32").unwrap()
        } else if self.cx.cx.is_float_var(from) {
            self.cx.cx.primitive("f64").unwrap()
        } else {
            from
        };
        let allowed = (self.cx.cx.is_numeric(from_concrete) && self.cx.cx.is_numeric(to))
            || (matches!(self.cx.cx.kind(from_concrete), TyKind::Char) && self.cx.cx.is_integer(to))
            || (matches!(self.cx.cx.kind(to), TyKind::Char) && false)
            || (matches!(self.cx.cx.kind(from_concrete), TyKind::Bool) && self.cx.cx.is_integer(to))
            || (is_pointer_like(&self.cx.cx, from_concrete)
                && self.cx.cx.is_integer(to)
                && self.unsafe_depth > 0)
            // `&str` is a NUL-terminated byte pointer, so viewing it as one is
            // a real conversion rather than a reinterpretation.
            || (is_str_ref(&self.cx.cx, from_concrete) && is_byte_ptr(&self.cx.cx, to))
            // The reverse: a byte pointer whose bytes are NUL-terminated UTF-8
            // may be viewed as a `&str`. Nothing checks either condition, so it
            // needs `unsafe`.
            || (is_byte_ptr(&self.cx.cx, from_concrete)
                && is_str_ref(&self.cx.cx, to)
                && self.unsafe_depth > 0)
            // An integer may be turned into a pointer, which is how a null or a
            // known address is written. Like the reverse, it needs `unsafe`.
            || (is_pointer_like(&self.cx.cx, to)
                && self.cx.cx.is_integer(from_concrete)
                && self.unsafe_depth > 0)
            // A reference to a contiguous buffer may be viewed as a pointer to
            // its first byte, which is how a byte array is handed to C.
            || (is_byte_ptr(&self.cx.cx, to) && is_buffer_ref(&self.cx.cx, from_concrete))
            // Raw pointers interconvert freely: `*mut T` to `*const T` is a
            // permission change, not a layout change, and C APIs are the usual
            // reason to need it. No `unsafe` here, because the cast itself cannot
            // break any rule the pointer already had.
            || (is_raw_ptr(&self.cx.cx, from_concrete) && is_raw_ptr(&self.cx.cx, to))
            // An array may be viewed as a pointer to its first element, which is
            // how a static table of routes or a byte buffer reaches an API that
            // takes a pointer.
            || (is_raw_ptr(&self.cx.cx, to) && is_array_of(&self.cx.cx, from_concrete, to))
            // A function may be viewed as a byte pointer, which is how its address
            // is handed to a C function that will call it. Nothing checks that the
            // signature at the other end matches, so it needs `unsafe`.
            || (is_byte_ptr(&self.cx.cx, to)
                && matches!(self.cx.cx.kind(from_concrete), TyKind::FnPtr { .. })
                && self.unsafe_depth > 0);
        if !allowed {
            let f = self.cx.ty_name(from_concrete);
            let t = self.cx.ty_name(to);
            let extra = if matches!(self.cx.cx.kind(to), TyKind::Char) {
                " Use `char::from_u32(value)` instead, which returns `Option<char>`."
            } else {
                ""
            };
            self.push(
                Diagnostic::error("E1013", format!("cannot cast `{f}` to `{t}` with `as`"))
                    .at(span)
                    .with_cause("`as` supports numeric conversions, `char`/integer, and pointer/`usize`")
                    .with_explanation(format!(
                        "UMAY has no implicit conversions; `as` performs a total, deterministic \
                         conversion. Casting to `char` is never allowed because most integers are \
                         not valid Unicode scalars.{extra}"
                    )),
            );
            return self.error_hir(span);
        }
        // A narrowing integer cast is legal but usually unintended.
        if let (Some((fmin, fmax)), Some((tmin, tmax))) =
            (self.cx.cx.int_bounds(from_concrete), self.cx.cx.int_bounds(to))
        {
            // Only a destination that cannot hold every source value is a
            // narrowing cast. An unsigned destination has a higher maximum but a
            // lower minimum, so both ends are compared separately; this is what
            // keeps `i64 as u64` — a same-width reinterpretation — quiet.
            let loses_values = fmin < tmin || fmax > tmax;
            if loses_values && from_concrete != to {
                let f = self.cx.ty_name(from_concrete);
                let t = self.cx.ty_name(to);
                self.push(
                    Diagnostic::warning(
                        "W0102",
                        format!("narrowing cast from `{f}` to `{t}` can silently truncate"),
                    )
                    .at(span)
                    .with_cause("the destination type cannot represent every value of the source type"),
                );
            }
        }
        HirExpr { kind: HirExprKind::Cast { expr: Box::new(value), to }, ty: to, span }
    }
}

fn is_pointer_like(types: &crate::types::TyCtxt, ty: TyId) -> bool {
    matches!(types.kind(ty), TyKind::RawPtr(_) | TyKind::RawPtrMut(_) | TyKind::Ref(_) | TyKind::RefMut(_))
}

/// True for `&str`, which the backend represents as a NUL-terminated byte
/// pointer.
fn is_array_of(types: &crate::types::TyCtxt, from: TyId, to: TyId) -> bool {
    let TyKind::Array { elem, .. } = types.kind(from) else {
        return false;
    };
    let pointed = match types.kind(to) {
        TyKind::RawPtr(inner) | TyKind::RawPtrMut(inner) => *inner,
        _ => return false,
    };
    *elem == pointed
}

fn is_str_ref(types: &crate::types::TyCtxt, ty: TyId) -> bool {
    match types.kind(ty) {
        TyKind::Ref(inner) | TyKind::RefMut(inner) => matches!(types.kind(*inner), TyKind::Str),
        _ => false,
    }
}

/// True for a reference to an array of bytes or any sized element type.
///
/// UMAY's guarantee that array elements are contiguous is what makes viewing
/// the buffer as raw bytes meaningful.
fn is_buffer_ref(types: &crate::types::TyCtxt, ty: TyId) -> bool {
    match types.kind(ty) {
        TyKind::Ref(inner) | TyKind::RefMut(inner) => matches!(types.kind(*inner), TyKind::Array { .. }),
        _ => false,
    }
}

/// True for `*const u8` and `*mut u8`.
/// Whether `ty` is a raw pointer of either mutability.
fn is_raw_ptr(types: &crate::types::TyCtxt, ty: TyId) -> bool {
    matches!(types.kind(ty), TyKind::RawPtr(_) | TyKind::RawPtrMut(_))
}

fn is_byte_ptr(types: &crate::types::TyCtxt, ty: TyId) -> bool {
    let inner = match types.kind(ty) {
        TyKind::RawPtr(inner) | TyKind::RawPtrMut(inner) => *inner,
        _ => return false,
    };
    matches!(types.kind(inner), TyKind::U8)
}

/// The span of the `let` keyword for a binding; approximates by walking back.
fn span_of_binding(name: &str, span: crate::source::Span) -> crate::source::Span {
    // The binding's span starts at its name; the `let` keyword precedes it.
    let _ = name;
    crate::source::Span { file: span.file, start: span.start.saturating_sub(4), end: span.start }
}

pub fn convert_binop(op: ast::BinaryOp) -> HirBinaryOp {
    match op {
        ast::BinaryOp::Add => HirBinaryOp::Add,
        ast::BinaryOp::Sub => HirBinaryOp::Sub,
        ast::BinaryOp::Mul => HirBinaryOp::Mul,
        ast::BinaryOp::Div => HirBinaryOp::Div,
        ast::BinaryOp::Rem => HirBinaryOp::Rem,
        ast::BinaryOp::BitAnd => HirBinaryOp::BitAnd,
        ast::BinaryOp::BitOr => HirBinaryOp::BitOr,
        ast::BinaryOp::BitXor => HirBinaryOp::BitXor,
        ast::BinaryOp::Shl => HirBinaryOp::Shl,
        ast::BinaryOp::Shr => HirBinaryOp::Shr,
        ast::BinaryOp::Eq => HirBinaryOp::Eq,
        ast::BinaryOp::Ne => HirBinaryOp::Ne,
        ast::BinaryOp::Lt => HirBinaryOp::Lt,
        ast::BinaryOp::Le => HirBinaryOp::Le,
        ast::BinaryOp::Gt => HirBinaryOp::Gt,
        ast::BinaryOp::Ge => HirBinaryOp::Ge,
        ast::BinaryOp::And => HirBinaryOp::And,
        ast::BinaryOp::Or => HirBinaryOp::Or,
    }
}
