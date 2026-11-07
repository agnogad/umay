//! Expression checking and local type inference.
//!
//! A [`FnChecker`] carries the state of one function body: the local-variable
//! scopes, the current function id, and the declared return type. It delegates
//! item-level questions (types, functions, methods) back to the [`Checker`].
//!
//! Inference is local to the body. Integer and float literals start as
//! `IntVar`/`FloatVar` and unify with the first concrete type they meet; if they
//! never meet one they default to `i32`/`f64` at the end of the statement.

use std::collections::HashMap;

use crate::ast;
use crate::diagnostics::{Diagnostic, Suggestion};
use crate::hir::*;
use crate::sema::scope::{describe_res, suggest_name, LocalEntry, ValueScopes};
use crate::sema::Checker;
use crate::source::Span;
use crate::types::{TyId, TyKind};

/// Per-function checking state.
pub struct FnChecker<'c, 's> {
    /// The shared checker. The two lifetimes differ because the borrow of the
    /// checker is shorter than the source map the checker holds.
    pub cx: &'c mut Checker<'s>,
    /// The function being checked. `None` for constants.
    #[allow(dead_code)]
    pub fn_id: Option<FnId>,
    pub scopes: ValueScopes,
    pub locals: Vec<LocalInfo>,
    pub ret_ty: TyId,
    /// Generics in scope for this body.
    pub generics: Vec<String>,
    /// Recorded defaults for literal vars at the end of the body.
    pub pending_defaults: Vec<(TyId, Span)>,
    /// Depth of `unsafe` blocks currently open.
    pub unsafe_depth: u32,
    /// Local types, indexed by LocalId, to support `finish_locals`.
    pub local_tys: HashMap<LocalId, TyId>,
}

impl<'c, 's> FnChecker<'c, 's> {
    pub fn new(cx: &'c mut Checker<'s>, fn_id: Option<FnId>) -> Self {
        let ret = cx.cx.unit();
        FnChecker {
            cx,
            fn_id,
            scopes: ValueScopes::new(),
            locals: Vec::new(),
            ret_ty: ret,
            generics: Vec::new(),
            pending_defaults: Vec::new(),
            unsafe_depth: 0,
            local_tys: HashMap::new(),
        }
    }

    // ---- locals -----------------------------------------------------------

    pub fn declare_param(&mut self, name: &str, ty: TyId, span: Span, mutable: bool) -> LocalId {
        self.declare_local(name, ty, span, mutable)
    }

    pub fn declare_local(&mut self, name: &str, ty: TyId, span: Span, mutable: bool) -> LocalId {
        let id = LocalId(self.locals.len() as u32);
        self.locals.push(LocalInfo { name: name.to_string(), ty, mutable, span });
        self.local_tys.insert(id, ty);
        self.scopes.declare(name.to_string(), LocalEntry { id, mutable, ty });
        id
    }

    /// Maps still-unresolved literal types to their defaults.
    ///
    /// Every `TyId` that was an integer or float literal variable maps to
    /// `i32`/`f64`. Applying the same map to locals and to expression types
    /// keeps them consistent, which matters downstream: a temporary typed
    /// `{integer}` would not be recognised as `Copy`.
    pub fn literal_defaults(&mut self) -> std::collections::HashMap<TyId, TyId> {
        let mut map = std::collections::HashMap::new();
        for (ty, _) in &self.pending_defaults {
            if self.cx.cx.is_integer_var(*ty) {
                if let Some(t) = self.cx.cx.primitive("i32") {
                    map.insert(*ty, t);
                }
            } else if self.cx.cx.is_float_var(*ty) {
                if let Some(t) = self.cx.cx.primitive("f64") {
                    map.insert(*ty, t);
                }
            }
        }
        map
    }

    pub fn finish_locals(mut self) -> Vec<LocalInfo> {
        // A literal variable that met a concrete type during checking takes that
        // type, so a local initialised from a literal and later used where a
        // concrete integer is expected keeps one width throughout.
        let settled = self.cx.settled_vars.clone();
        for l in self.locals.iter_mut() {
            if let Some(&t) = settled.get(&l.ty) {
                l.ty = t;
            }
        }
        // Apply literal defaults to locals that never got a concrete type.
        for (ty, _span) in std::mem::take(&mut self.pending_defaults) {
            if self.cx.cx.is_integer_var(ty) {
                let i32t = self.cx.cx.primitive("i32").unwrap();
                for l in self.locals.iter_mut() {
                    if l.ty == ty {
                        l.ty = i32t;
                    }
                }
            } else if self.cx.cx.is_float_var(ty) {
                let f64t = self.cx.cx.primitive("f64").unwrap();
                for l in self.locals.iter_mut() {
                    if l.ty == ty {
                        l.ty = f64t;
                    }
                }
            }
        }
        std::mem::take(&mut self.locals)
    }

    /// The type of a block: its tail expression's type, or `()`.
    ///
    /// A block whose last statement diverges also has type `!`, because control
    /// never reaches the end of the block.
    pub fn block_ty(&mut self, block: &HirBlock) -> TyId {
        if let Some(e) = &block.tail {
            return e.ty;
        }
        if let Some(last) = block.stmts.last() {
            if let HirStmtKind::Expr(e) = &last.kind {
                if matches!(self.cx.cx.kind(e.ty), TyKind::Never) {
                    return e.ty;
                }
            }
        }
        self.cx.cx.unit()
    }

    // ---- diagnostics helpers ---------------------------------------------

    pub(crate) fn error(&mut self, code: &'static str, span: Span, message: impl Into<String>) {
        self.cx.error(code, span, message);
    }

    pub(crate) fn push(&mut self, d: Diagnostic) {
        self.cx.push(d);
    }

    pub(crate) fn _err_expr(&mut self, span: Span) -> HirExpr {
        let ty = self.cx.cx.error();
        HirExpr { kind: HirExprKind::Error, ty, span }
    }

    // ---- blocks & statements ---------------------------------------------

    pub fn check_block(&mut self, block: &ast::Block) -> HirBlock {
        self.scopes.push_frame();
        let before = self.locals.len();
        let mut stmts = Vec::new();
        for stmt in &block.stmts {
            self.check_stmt(stmt, &mut stmts);
        }
        let tail = block.tail.as_ref().map(|e| Box::new(self.check_expr(e)));
        // Block value type check.
        if let Some(t) = &tail {
            if self.cx.cx.is_error(t.ty) {
                // already reported
            }
        }
        self.scopes.pop_frame();
        HirBlock {
            stmts,
            tail,
            span: block.span,
            locals_introduced: (self.locals.len() - before) as u32,
        }
    }

    fn check_stmt(&mut self, stmt: &ast::Stmt, out: &mut Vec<HirStmt>) {
        match &stmt.kind {
            ast::StmtKind::Let(l) => {
                let (bindings, init) = self.check_let(l);
                out.push(HirStmt { kind: HirStmtKind::Let { bindings, init }, span: stmt.span });
            }
            ast::StmtKind::Expr(e) => {
                let checked = self.check_expr(e);
                out.push(HirStmt { kind: HirStmtKind::Expr(checked), span: stmt.span });
            }
            ast::StmtKind::Item(item) => {
                // Nested items are declared at collection time in a later phase;
                // for Phase 2 we report clearly that ordering is unsupported.
                let span = item.span();
                self.push(
                    Diagnostic::error("E1001", "items nested inside blocks are not supported yet")
                        .at(span)
                        .with_cause("Phase 2 collects item names program-wide, not per block")
                        .with_explanation(
                            "Moving the declaration to the top level of the file is the intended \
                             workaround; block-scoped items arrive with module support.",
                        ),
                );
            }
        }
    }

    /// Checks a `let`, introducing locals for every binding in its pattern.
    fn check_let(&mut self, l: &ast::LetStmt) -> (Vec<LocalId>, Option<Box<HirExpr>>) {
        let annotation = l.ty.as_ref().map(|t| {
            let generics = self.generics.clone();
            self.cx.resolve_type(t, &generics)
        });

        // The annotation, when present, is an inference hint for the initialiser:
        // a generic struct literal can take its type arguments from it.
        let mut init: Option<Box<HirExpr>> = l.init.as_ref().map(|e| {
            let hint = annotation.filter(|a| !self.cx.cx.is_error(*a));
            Box::new(self.check_expr_hinted(e, hint))
        });

        // Unify the initializer's type with the annotation.
        let binding_ty = match (annotation, &init) {
            (Some(a), Some(i)) => {
                // A literal initialiser adopts the annotated type, and must fit
                // in it: `let x: u8 = 300` is an error, not a silent wrap.
                self.check_literal_fits(i, a);
                // An integer literal can initialise a raw pointer or `usize`
                // slot; there is no other way to write a null pointer value.
                let literal_into_pointer =
                    self.cx.cx.is_integer_var(i.ty) && is_pointer_like_ty(&self.cx.cx, a);
                if !literal_into_pointer
                    && !self.cx.cx.is_error(a)
                    && !self.cx.cx.is_error(i.ty)
                    && !self.cx.types_compatible(a, i.ty)
                {
                    let msg = format!(
                        "type mismatch: expected {}, found {}",
                        self.cx.ty_name(a),
                        self.cx.ty_name(i.ty)
                    );
                    let d = Diagnostic::error("E1001", msg)
                        .at(i.span)
                        .with_cause("the annotated type and the initializer's type must be identical");
                    self.push(d);
                }
                // An initialiser that could not name a generic parameter takes the
                // annotation's instance: `let x: Option<i64> = Option::None` is the
                // only thing that says which `Option` `None` belongs to.
                if let Some(e) = init.as_deref_mut() {
                    if mentions_unresolved_param(&self.cx.cx, e.ty) {
                        let fixed = adopt_instance(&mut self.cx.cx, e.ty, a);
                        retype(e, fixed);
                    }
                }
                a
            }
            (Some(a), None) => a,
            (None, Some(i)) => i.ty,
            (None, None) => self.cx.cx.error(),
        };

        // Record literal vars so they can default at the end.
        if self.cx.cx.is_infer_var(binding_ty) {
            self.pending_defaults.push((binding_ty, l.span));
        }

        let pattern = self.bind_pattern(&l.pattern, binding_ty, l.mutable);
        let mut ids = Vec::new();
        collect_hir_binding_locals(&pattern, &mut ids);
        // The initializer is kept so that MIR can evaluate it. Destructuring is
        // handled during lowering, which reads the parts out of the temporary.
        (ids, init)
    }

    /// Verifies that a literal initialiser fits the annotated type.
    ///
    /// Without this, `let x: u8 = 300` would pass the compatibility check
    /// (an integer literal is compatible with any integer type) and then be
    /// truncated by the backend.
    /// Gives a bare literal the type it is being used at.
    ///
    /// Only inference variables are replaced; a concrete type is left alone.
    /// The literal's value is unchanged, which is correct because the caller has
    /// already checked that it fits.
    pub fn coerce_literal(&mut self, mut e: HirExpr, want: crate::types::TyId) -> HirExpr {
        if self.cx.cx.is_integer_var(e.ty) && self.cx.cx.is_integer(want) {
            e.ty = want;
        } else if self.cx.cx.is_float_var(e.ty) && self.cx.cx.is_float(want) {
            e.ty = want;
        }
        e
    }

    fn check_literal_fits(&mut self, init: &HirExpr, annotated: crate::types::TyId) {
        if self.cx.cx.is_error(annotated) || self.cx.cx.is_error(init.ty) {
            return;
        }
        if !self.cx.cx.is_integer(annotated) {
            return;
        }
        let value = match &init.kind {
            HirExprKind::IntLit { value, suffix } if suffix.is_none() => *value,
            _ => return,
        };
        let Some((min, max)) = self.cx.cx.int_bounds(annotated) else { return };
        let fits = if value > i128::MAX as u128 {
            false
        } else {
            let v = value as i128;
            v >= min && v <= max
        };
        if !fits {
            let name = self.cx.ty_name(annotated);
            self.push(
                Diagnostic::error("E1001", format!("integer literal `{value}` does not fit in `{name}`"))
                    .at(init.span)
                    .with_cause(format!("`{name}` can hold values from {min} to {max}"))
                    .with_explanation(
                        "UMAY never silently truncates a literal. Choose a wider type or a \
                         value that fits.",
                    ),
            );
        }
    }

    /// Introduces locals for a pattern and produces its HIR form.
    ///
    /// Binding patterns create locals; every other pattern validates against
    /// `ty` and records resolved indices so later phases never re-resolve names.
    pub(crate) fn bind_pattern(&mut self, pat: &ast::Pattern, ty: TyId, mutable: bool) -> HirPattern {
        let kind = self.bind_pattern_kind(pat, ty, mutable);
        HirPattern { kind, ty, span: pat.span }
    }

    fn bind_pattern_kind(&mut self, pat: &ast::Pattern, ty: TyId, mutable: bool) -> HirPatternKind {
        match &pat.kind {
            ast::PatternKind::Binding { name, mutable: pmut, .. } => {
                if name == "_" {
                    return HirPatternKind::Wildcard;
                }
                if self.scopes.declared_in_current_frame(name) {
                    self.push(
                        Diagnostic::warning("W0101", format!("binding `{name}` shadows an earlier binding in the same scope"))
                            .at(pat.span)
                            .with_cause("two `let` statements in one block introduced the same name"),
                    );
                }
                let is_mut = mutable || *pmut;
                let id = self.declare_local(name, ty, pat.span, is_mut);
                HirPatternKind::Binding { local: id, mutable: is_mut }
            }
            ast::PatternKind::Tuple(elems) => {
                match self.cx.cx.kind(ty).clone() {
                    TyKind::Tuple(tys) => {
                        if tys.len() != elems.len() {
                            let msg = format!(
                                "tuple pattern has {} elements but the value has type {} ({} elements)",
                                elems.len(),
                                self.cx.ty_name(ty),
                                tys.len()
                            );
                            self.error("E1001", pat.span, msg);
                            let err = self.cx.cx.error();
                            let subs = elems.iter().map(|p| self.bind_pattern(p, err, mutable)).collect();
                            return HirPatternKind::Tuple(subs);
                        }
                        let subs = elems.iter().zip(tys).map(|(p, t)| self.bind_pattern(p, t, mutable)).collect();
                        HirPatternKind::Tuple(subs)
                    }
                    TyKind::Error => {
                        let err = self.cx.cx.error();
                        HirPatternKind::Tuple(elems.iter().map(|p| self.bind_pattern(p, err, mutable)).collect())
                    }
                    _ => {
                        let name = self.cx.ty_name(ty);
                        self.error("E1001", pat.span, format!("cannot destructure a value of type `{name}` as a tuple"));
                        let err = self.cx.cx.error();
                        HirPatternKind::Tuple(elems.iter().map(|p| self.bind_pattern(p, err, mutable)).collect())
                    }
                }
            }
            ast::PatternKind::Struct { path, fields, rest } => {
                let name = path.segments.last().map(|s| s.name.clone()).unwrap_or_default();
                let Some(def) = self.cx.def_by_name.get(&name).copied() else {
                    self.error("E1003", path.span, format!("unknown type `{name}` in pattern"));
                    let err = self.cx.cx.error();
                    let fields = fields.iter().map(|f| self.pattern_field_err(f, err, mutable)).collect();
                    return HirPatternKind::Struct { def: crate::types::DefId(u32::MAX), fields, rest: *rest };
                };
                let struct_fields = self.cx.hir.structs.iter().find(|s| s.def_id == def).map(|s| s.fields.clone()).unwrap_or_default();
                // The scrutinee must be that struct.
                let want = self.cx.cx.adt(def, Vec::new());
                if !self.cx.cx.is_error(ty) && !self.cx.types_compatible(ty, want) {
                    let a = self.cx.ty_name(want);
                    let b = self.cx.ty_name(ty);
                    self.push(
                        Diagnostic::error("E1001", format!("pattern expects `{a}` but the value has type `{b}`"))
                            .at(path.span)
                            .with_cause("the pattern's struct does not match the scrutinee's type"),
                    );
                }
                let mut hfields = Vec::new();
                for f in fields {
                    let found = struct_fields.iter().position(|sf| sf.name == f.name);
                    let field_ty = match found {
                        Some(i) => struct_fields[i].ty,
                        None => {
                            let n = f.name.clone();
                            let span = f.name_span;
                            let candidates: Vec<String> = struct_fields.iter().map(|sf| sf.name.clone()).collect();
                            let mut d = Diagnostic::error("E1006", format!("`{name}` has no field `{n}`")).at(span);
                            if let Some(s) = suggest_name(&n, candidates.iter().map(String::as_str)) {
                                d = d.with_suggestion(Suggestion::new(format!("did you mean `{s}`?"), span, s));
                            } else if !candidates.is_empty() {
                                d = d.with_explanation(format!("Available fields: {}.", candidates.join(", ")));
                            }
                            self.push(d);
                            self.cx.cx.error()
                        }
                    };
                    let sub = match &f.pattern {
                        Some(p) => self.bind_pattern(p, field_ty, mutable),
                        None => {
                            let id = self.declare_local(&f.name, field_ty, f.name_span, mutable);
                            HirPattern {
                                kind: HirPatternKind::Binding { local: id, mutable },
                                ty: field_ty,
                                span: f.name_span,
                            }
                        }
                    };
                    hfields.push(HirPatternField {
                        index: found.unwrap_or(0) as u32,
                        pattern: sub,
                        span: f.name_span,
                    });
                }
                HirPatternKind::Struct { def, fields: hfields, rest: *rest }
            }
            ast::PatternKind::Variant { path, kind } => self.bind_variant_pattern(path, kind, ty, mutable),
            ast::PatternKind::Or(alts) => {
                let mut subs = Vec::new();
                let mut expected: Option<Vec<String>> = None;
                for alt in alts {
                    let sub = self.bind_pattern(alt, ty, mutable);
                    let mut names = Vec::new();
                    collect_hir_binding_names(&self.locals, &sub, &mut names);
                    names.sort();
                    match &expected {
                        None => expected = Some(names),
                        Some(exp) => {
                            if *exp != names {
                                self.push(
                                    Diagnostic::error("E1008", "all alternatives of an or-pattern must bind the same names")
                                        .at(alt.span)
                                        .with_cause(format!(
                                            "this alternative binds [{}], but a previous one binds [{}]",
                                            names.join(", "),
                                            exp.join(", ")
                                        )),
                                );
                            }
                        }
                    }
                    subs.push(sub);
                }
                HirPatternKind::Or(subs)
            }
            ast::PatternKind::Literal(lit) => {
                let span = lit.span;
                let kind = match &lit.kind {
                    ast::ExprKind::BoolLit(b) => HirPatternKind::BoolLit(*b),
                    ast::ExprKind::IntLit { value, suffix } => {
                        let v = *value;
                        if let Some(s) = suffix {
                            let tmp = self.cx.cx.primitive(s.as_str());
                            let _ = tmp;
                        }
                        HirPatternKind::IntLit { value: v, negative: false }
                    }
                    ast::ExprKind::CharLit(c) => HirPatternKind::CharLit(*c),
                    ast::ExprKind::StrLit(s) => HirPatternKind::StrLit(s.clone()),
                    _ => {
                        self.push(
                            Diagnostic::error("E1001", "this expression cannot be used as a pattern")
                                .at(span)
                                .with_cause("only literals, bindings, tuples, structs and variants are patterns"),
                        );
                        HirPatternKind::Error
                    }
                };
                // Verify the literal's type against the scrutinee.
                let checked = self.check_expr(lit);
                if !self.cx.cx.is_error(ty) && !self.cx.cx.is_error(checked.ty) {
                    let scrut = if self.cx.cx.is_integer_var(ty) { checked.ty } else { ty };
                    if !self.cx.types_compatible(scrut, checked.ty) {
                        let want = self.cx.ty_name(ty);
                        let got = self.cx.ty_name(checked.ty);
                        self.push(
                            Diagnostic::error(
                                "E1001",
                                format!("literal pattern has type `{got}`, but the value has type `{want}`"),
                            )
                            .at(span)
                            .with_cause("patterns must have the scrutinee's type"),
                        );
                    }
                }
                kind
            }
            ast::PatternKind::Range { start, end, inclusive } => {
                let lo = pattern_int_bound(start);
                let hi = pattern_int_bound(end);
                // The bounds must both be integer literals for a range pattern.
                for (b, sp) in [(lo, start.span), (hi, end.span)] {
                    if b.is_none() {
                        self.push(
                            Diagnostic::error("E1001", "range pattern bounds must be integer literals")
                                .at(sp)
                                .with_cause("range patterns compare against constants known at compile time"),
                        );
                    }
                }
                if let (Some(lo), Some(hi)) = (lo, hi) {
                    if lo > hi {
                        self.push(
                            Diagnostic::error("E1001", format!("empty range pattern `{lo}..{hi}`"))
                                .at(pat.span)
                                .with_cause("the start of a range must not exceed its end"),
                        );
                    }
                }
                let neg = lo.map(|v| v < 0).unwrap_or(false);
                let kind = if *inclusive {
                    HirPatternKind::Range { start: lo.unwrap_or(0), end: hi.unwrap_or(0) }
                } else {
                    HirPatternKind::RangeExclusive { start: lo.unwrap_or(0), end: hi.unwrap_or(0) }
                };
                let _ = neg;
                kind
            }
            ast::PatternKind::Slice { elems, rest } => {
                let elem_ty = match self.cx.cx.kind(ty).clone() {
                    TyKind::Array { elem, len } => {
                        let fixed = elems.len() as u64;
                        if !*rest && fixed != len {
                            self.push(
                                Diagnostic::error(
                                    "E1001",
                                    format!("slice pattern has {fixed} element(s), but the array has {len}"),
                                )
                                .at(pat.span)
                                .with_cause("a fixed slice pattern must list every element"),
                            );
                        }
                        if *rest && fixed > len {
                            self.push(
                                Diagnostic::error("E1001", "slice pattern lists more elements than the array holds")
                                    .at(pat.span)
                                    .with_cause("`..` covers the remaining elements, which cannot be negative"),
                            );
                        }
                        elem
                    }
                    TyKind::Slice(elem) => elem,
                    TyKind::Error => self.cx.cx.error(),
                    _ => {
                        let name = self.cx.ty_name(ty);
                        self.push(
                            Diagnostic::error("E1001", format!("cannot match a slice pattern against `{name}`"))
                                .at(pat.span)
                                .with_cause("slice patterns require an array or slice"),
                        );
                        self.cx.cx.error()
                    }
                };
                let subs = elems.iter().map(|p| self.bind_pattern(p, elem_ty, mutable)).collect();
                HirPatternKind::Slice { elems: subs, rest: *rest }
            }
        }
    }

    /// A struct-pattern field whose struct could not be resolved.
    fn pattern_field_err(&mut self, f: &ast::PatternField, ty: TyId, mutable: bool) -> HirPatternField {
        let sub = match &f.pattern {
            Some(p) => self.bind_pattern(p, ty, mutable),
            None => {
                let id = self.declare_local(&f.name, ty, f.name_span, mutable);
                HirPattern { kind: HirPatternKind::Binding { local: id, mutable }, ty, span: f.name_span }
            }
        };
        HirPatternField { index: 0, pattern: sub, span: f.name_span }
    }

    fn bind_variant_pattern(
        &mut self,
        path: &ast::Path,
        kind: &ast::VariantPatternKind,
        ty: TyId,
        mutable: bool,
    ) -> HirPatternKind {
        let err = self.cx.cx.error();
        let Some((def, index)) = self.resolve_variant_path(path) else {
            let fields = match kind {
                ast::VariantPatternKind::Tuple(ps) => {
                    VariantPatternFields::Tuple(ps.iter().map(|p| self.bind_pattern(p, err, mutable)).collect())
                }
                ast::VariantPatternKind::Struct { fields, rest } => {
                    let fs = fields.iter().map(|f| self.pattern_field_err(f, err, mutable)).collect();
                    VariantPatternFields::Struct { fields: fs, rest: *rest }
                }
                ast::VariantPatternKind::Unit => VariantPatternFields::Unit,
            };
            return HirPatternKind::Variant { def: crate::types::DefId(u32::MAX), index: 0, fields };
        };

        let enum_ty = self.cx.cx.adt(def, Vec::new());
        if !self.cx.cx.is_error(ty) && !self.cx.types_compatible(ty, enum_ty) {
            let expected = self.cx.ty_name(enum_ty);
            let found = self.cx.ty_name(ty);
            self.push(
                Diagnostic::error("E1001", format!("pattern expects `{expected}` but the value has type `{found}`"))
                    .at(path.span)
                    .with_cause("the pattern's enum does not match the scrutinee's type"),
            );
        }
        let variant = self
            .cx
            .hir
            .enums
            .iter()
            .find(|e| e.def_id == def)
            .and_then(|e| e.variants.get(index as usize))
            .cloned();
        let Some(variant) = variant else { return HirPatternKind::Error };

        // The variant's field types are written in terms of the enum's
        // parameters, so they are instantiated from the scrutinee's own type:
        // matching `Option<i64>` binds an `i64`, not a `T`.
        let generic_names: Vec<String> = self
            .cx
            .hir
            .enums
            .iter()
            .find(|e| e.def_id == def)
            .map(|e| e.generics.clone())
            .unwrap_or_default();
        let enum_args: Vec<crate::types::TyId> = match self.cx.cx.kind(ty).clone() {
            crate::types::TyKind::Adt { def: d, args } if d == def => args,
            _ => Vec::new(),
        };
        let field_tys: Vec<crate::types::TyId> = match &variant.fields {
            VariantFields::Tuple(tys) => tys
                .iter()
                .map(|t| self.instance_field(*t, &generic_names, &enum_args))
                .collect(),
            VariantFields::Struct(fs) => fs
                .iter()
                .map(|f| self.instance_field(f.ty, &generic_names, &enum_args))
                .collect(),
            VariantFields::Unit => Vec::new(),
        };

        let fields = match (kind, &variant.fields) {
            (ast::VariantPatternKind::Unit, VariantFields::Unit) => VariantPatternFields::Unit,
            (ast::VariantPatternKind::Tuple(ps), VariantFields::Tuple(_tys)) => {
                let tys = &field_tys;
                if ps.len() != tys.len() {
                    self.push(
                        Diagnostic::error(
                            "E1005",
                            format!(
                                "variant `{}` has {} field(s) but the pattern supplies {}",
                                variant.name,
                                tys.len(),
                                ps.len()
                            ),
                        )
                        .at(path.span)
                        .with_cause("variant pattern arity must match the variant's declaration"),
                    );
                }
                VariantPatternFields::Tuple(
                    ps.iter()
                        .enumerate()
                        .map(|(i, p)| {
                            let t = tys.get(i).copied().unwrap_or(err);
                            self.bind_pattern(p, t, mutable)
                        })
                        .collect(),
                )
            }
            (ast::VariantPatternKind::Struct { fields, rest }, VariantFields::Struct(_vfs)) => {
                let vfs: Vec<crate::hir::FieldInfo> = _vfs
                    .iter()
                    .zip(field_tys.iter())
                    .map(|(f, t)| crate::hir::FieldInfo { ty: *t, ..f.clone() })
                    .collect();
                let vfs = &vfs;
                let mut out = Vec::new();
                for f in fields {
                    let found = vfs.iter().position(|vf| vf.name == f.name);
                    let fty = match found {
                        Some(i) => vfs[i].ty,
                        None => {
                            let n = f.name.clone();
                            let sp = f.name_span;
                            let candidates: Vec<String> = vfs.iter().map(|vf| vf.name.clone()).collect();
                            let mut d = Diagnostic::error("E1006", format!("variant `{}` has no field `{n}`", variant.name))
                                .at(sp)
                                .with_cause("this field is not declared on the variant");
                            if let Some(s) = suggest_name(&n, candidates.iter().map(String::as_str)) {
                                d = d.with_suggestion(Suggestion::new(format!("did you mean `{s}`?"), sp, s));
                            }
                            self.push(d);
                            err
                        }
                    };
                    let sub = match &f.pattern {
                        Some(p) => self.bind_pattern(p, fty, mutable),
                        None => {
                            let id = self.declare_local(&f.name, fty, f.name_span, mutable);
                            HirPattern { kind: HirPatternKind::Binding { local: id, mutable }, ty: fty, span: f.name_span }
                        }
                    };
                    out.push(HirPatternField { index: found.unwrap_or(0) as u32, pattern: sub, span: f.name_span });
                }
                VariantPatternFields::Struct { fields: out, rest: *rest }
            }
            _ => {
                let name = variant.name.clone();
                self.push(
                    Diagnostic::error("E1001", format!("pattern for variant `{name}` uses the wrong form"))
                        .at(path.span)
                        .with_cause("this variant was declared with a different field shape"),
                );
                return HirPatternKind::Error;
            }
        };
        HirPatternKind::Variant { def, index, fields }
    }

    /// Resolves `Enum::Variant` to `(DefId, variant index)`.
    pub fn resolve_variant_path(&mut self, path: &ast::Path) -> Option<(crate::types::DefId, u32)> {
        let segs: Vec<String> = path.segments.iter().map(|s| s.name.clone()).collect();
        // Accept both `Enum::Variant` and a bare `Variant` if unambiguous.
        let (enum_name, variant_name) = match segs.len() {
            2 => (segs[0].clone(), segs[1].clone()),
            1 => {
                let vname = segs[0].clone();
                let mut found: Option<(String, u32)> = None;
                for e in &self.cx.hir.enums {
                    if let Some(i) = e.variants.iter().position(|v| v.name == vname) {
                        if found.is_some() {
                            found = None; // ambiguous
                            break;
                        }
                        found = Some((e.name.clone(), i as u32));
                    }
                }
                match found {
                    Some((e, i)) => {
                        let def = self.cx.def_by_name.get(&e).copied()?;
                        return Some((def, i));
                    }
                    None => {
                        self.error("E0101", path.span, format!("unknown enum variant `{vname}`"));
                        return None;
                    }
                }
            }
            _ => {
                self.error("E1003", path.span, "enum variants must be written `Enum::Variant`");
                return None;
            }
        };
        let Some(def) = self.cx.def_by_name.get(&enum_name).copied() else {
            self.unknown_name(&enum_name, path.span);
            return None;
        };
        let Some(info) = self.cx.hir.enums.iter().find(|e| e.def_id == def) else {
            self.push(
                Diagnostic::error("E1003", format!("`{enum_name}` is not an enum"))
                    .at(path.span)
                    .with_cause("only enums have variants"),
            );
            return None;
        };
        match info.variants.iter().position(|v| v.name == variant_name) {
            Some(i) => Some((def, i as u32)),
            None => {
                let suggestion = suggest_name(&variant_name, info.variants.iter().map(|v| v.name.as_str()));
                let mut d = Diagnostic::error(
                    "E1007",
                    format!("enum `{enum_name}` has no variant `{variant_name}`"),
                )
                .at(path.segments[1].span)
                .with_cause("the variant name does not exist on this enum");
                if let Some(s) = suggestion {
                    d = d.with_suggestion(Suggestion::new(
                        format!("did you mean `{s}`?"),
                        path.segments[1].span,
                        s,
                    ));
                }
                self.push(d);
                None
            }
        }
    }

    /// Reports an unknown name with a suggestion drawn from every visible name.
    pub fn unknown_name(&mut self, name: &str, span: Span) {
        let mut candidates: Vec<String> = self.cx.items.all_names().into_iter().map(str::to_string).collect();
        for l in &self.locals {
            candidates.push(l.name.clone());
        }
        let suggestion = suggest_name(name, candidates.iter().map(String::as_str));
        let mut d = Diagnostic::error("E0101", format!("unknown name `{name}`"))
            .at(span)
            .with_cause("no local variable, function, constant or type with this name is in scope");
        if let Some(s) = suggestion {
            d = d.with_suggestion(Suggestion::new(format!("did you mean `{s}`?"), span, s));
        }
        self.push(d);
    }

}

/// True for `*const T`, `*mut T`, `&T` and `&mut T`.
fn is_pointer_like_ty(types: &crate::types::TyCtxt, ty: crate::types::TyId) -> bool {
    matches!(
        types.kind(ty),
        TyKind::RawPtr(_) | TyKind::RawPtrMut(_)
    )
}

/// Collects every local a HIR pattern binds.
fn collect_hir_binding_locals(pat: &HirPattern, out: &mut Vec<LocalId>) {
    match &pat.kind {
        HirPatternKind::Binding { local, .. } => out.push(*local),
        HirPatternKind::Tuple(ps) | HirPatternKind::Or(ps) => {
            ps.iter().for_each(|p| collect_hir_binding_locals(p, out))
        }
        HirPatternKind::Struct { fields, .. } => {
            fields.iter().for_each(|f| collect_hir_binding_locals(&f.pattern, out))
        }
        HirPatternKind::Variant { fields, .. } => match fields {
            VariantPatternFields::Tuple(ps) => ps.iter().for_each(|p| collect_hir_binding_locals(p, out)),
            VariantPatternFields::Struct { fields, .. } => {
                fields.iter().for_each(|f| collect_hir_binding_locals(&f.pattern, out))
            }
            VariantPatternFields::Unit => {}
        },
        HirPatternKind::Slice { elems, .. } => {
            elems.iter().for_each(|p| collect_hir_binding_locals(p, out))
        }
        _ => {}
    }
}

/// Collects the names a HIR pattern binds, for or-pattern agreement.
fn collect_hir_binding_names(locals: &[LocalInfo], pat: &HirPattern, out: &mut Vec<String>) {
    let mut ids = Vec::new();
    collect_hir_binding_locals(pat, &mut ids);
    for id in ids {
        if let Some(l) = locals.get(id.0 as usize) {
            out.push(l.name.clone());
        }
    }
}

/// Extracts an integer literal from a range-pattern bound.
fn pattern_int_bound(p: &ast::Pattern) -> Option<i128> {
    match &p.kind {
        ast::PatternKind::Literal(e) => match &e.kind {
            ast::ExprKind::IntLit { value, .. } => i128::try_from(*value).ok(),
            ast::ExprKind::Unary { op: ast::UnaryOp::Neg, operand } => match &operand.kind {
                ast::ExprKind::IntLit { value, .. } => i128::try_from(*value).ok().map(|v| -v),
                _ => None,
            },
            _ => None,
        },
        _ => None,
    }
}

/// Describes what a resolution refers to, for "not callable" style errors.
#[allow(dead_code)]
fn describe_res_for_error(res: Res) -> &'static str {
    describe_res(res)
}

/// True when `ty` is or contains an `Error` standing in for an uninferrable
/// generic parameter.
fn mentions_unresolved_param(types: &crate::types::TyCtxt, ty: crate::types::TyId) -> bool {
    if types.is_error(ty) {
        return false;
    }
    match types.kind(ty) {
        // No arguments at all means a generic declaration was named without an
        // instance, which is the same gap as an unresolved parameter.
        crate::types::TyKind::Adt { args, .. } => {
            args.is_empty() || args.iter().any(|a| types.is_error(*a))
        }
        _ => false,
    }
}

/// Rebuilds `found` using `want`'s type arguments, position by position.
///
/// The two are the same declaration, so the annotation is exactly the instance the
/// initialiser meant but could not write.
fn adopt_instance(
    types: &mut crate::types::TyCtxt,
    found: crate::types::TyId,
    want: crate::types::TyId,
) -> crate::types::TyId {
    match (types.kind(found).clone(), types.kind(want).clone()) {
        (
            crate::types::TyKind::Adt { def: d1, .. },
            crate::types::TyKind::Adt { def: d2, args },
        ) if d1 == d2 => types.adt(d1, args),
        _ => found,
    }
}

/// Replaces an expression's type, and the types of the `Var`/`Aggregate` nodes
/// underneath it, with `ty`.
fn retype(e: &mut HirExpr, ty: crate::types::TyId) {
    e.ty = ty;
}

impl<'c, 's> FnChecker<'c, 's> {
    /// Instantiates a variant field's declared type with the scrutinee's
    /// arguments, leaving it alone when there are none to substitute.
    fn instance_field(
        &mut self,
        declared: crate::types::TyId,
        names: &[String],
        args: &[crate::types::TyId],
    ) -> crate::types::TyId {
        if names.is_empty() || args.is_empty() {
            return declared;
        }
        let mut subst = std::collections::HashMap::new();
        for (name, arg) in names.iter().zip(args.iter()) {
            subst.insert(name.clone(), *arg);
        }
        self.cx.cx.substitute(declared, &subst)
    }
}
