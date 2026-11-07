//! Expression checking: calls, method calls, field access and literals.

use crate::ast;
use crate::diagnostics::{Diagnostic, Suggestion};
use crate::hir::*;
use crate::sema::check_expr::FnChecker;
use crate::sema::scope::suggest_name;
use crate::types::{TyId, TyKind};

impl FnChecker<'_, '_> {
    // ---- calls -----------------------------------------------------------

    pub fn check_call(
        &mut self,
        callee: &ast::Expr,
        args: &[ast::Expr],
        span: crate::source::Span,
    ) -> HirExpr {
        // `Type::Variant(...)` constructs an enum variant.
        if let ast::ExprKind::Path(path) = &callee.kind {
            if path.segments.len() == 2 {
                if let Some(e) = self.try_variant_ctor(path, args, span) {
                    return e;
                }
            }
        }

        let callee_hir = self.check_expr(callee);
        let arg_hirs: Vec<HirExpr> = args.iter().map(|a| self.check_expr(a)).collect();

        let TyKind::FnPtr { params, ret } = self.cx.cx.kind(callee_hir.ty).clone() else {
            if !self.cx.cx.is_error(callee_hir.ty) {
                let name = self.cx.ty_name(callee_hir.ty);
                self.push(
                    Diagnostic::error("E1014", format!("`{name}` is not callable"))
                        .at(callee.span)
                        .with_cause("only function pointers and functions can be called")
                        .with_explanation(
                            "Calling a value needs a function type, written `fn(A, B) -> C`. In UMAY \
                             methods are called with `receiver.method(...)`, not by naming them \
                             bare.",
                        ),
                );
            }
            return self.error_hir(span);
        };

        self.check_call_args(&params, &arg_hirs, span, "function");
        HirExpr {
            kind: HirExprKind::Call { callee: Box::new(callee_hir), args: arg_hirs },
            ty: ret,
            span,
        }
    }

    /// Checks arity and per-argument types for a call.
    fn check_call_args(&mut self, params: &[TyId], args: &[HirExpr], span: crate::source::Span, what: &str) {
        if params.len() != args.len() {
            let d = Diagnostic::error(
                "E1004",
                format!("this {what} takes {} argument(s), but {} were supplied", params.len(), args.len()),
            )
            .at(span)
            .with_cause("the number of arguments must match the declaration");
            self.push(d);
        }
        for (i, (p, a)) in params.iter().zip(args).enumerate() {
            if self.cx.cx.is_error(*p) || self.cx.cx.is_error(a.ty) {
                continue;
            }
            // An integer literal can adopt the parameter type.
            if self.cx.cx.is_integer_var(a.ty) && self.cx.cx.is_integer(*p) {
                continue;
            }
            if self.cx.cx.is_float_var(a.ty) && self.cx.cx.is_float(*p) {
                continue;
            }
            if !self.cx.types_compatible(*p, a.ty) {
                let want = self.cx.ty_name(*p);
                let got = self.cx.ty_name(a.ty);
                self.push(
                    Diagnostic::error(
                        "E1001",
                        format!("argument {} has type `{got}`, but parameter {} expects `{want}`", i + 1, i + 1),
                    )
                    .at(a.span)
                    .with_cause("arguments are matched by position; no implicit conversion happens"),
                );
            }
        }
    }

    /// Attempts `Enum::Variant(args)` construction.
    pub(crate) fn try_variant_ctor(
        &mut self,
        path: &ast::Path,
        args: &[ast::Expr],
        span: crate::source::Span,
    ) -> Option<HirExpr> {
        let enum_name = &path.segments[0].name;
        let def = *self.cx.def_by_name.get(enum_name)?;
        let info = self.cx.hir.enums.iter().find(|e| e.def_id == def)?.clone();
        let variant_name = &path.segments[1].name;
        let index = info.variants.iter().position(|v| v.name == *variant_name)?;
        let ty = self.cx.cx.adt(def, Vec::new());
        let args: Vec<HirExpr> = args.iter().map(|a| self.check_expr(a)).collect();
        let variant = &info.variants[index];

        match &variant.fields {
            VariantFields::Unit => {
                if !args.is_empty() {
                    self.push(
                        Diagnostic::error(
                            "E1005",
                            format!("variant `{enum_name}::{variant_name}` takes no arguments"),
                        )
                        .at(span)
                        .with_cause("this variant was declared without fields"),
                    );
                }
            }
            VariantFields::Tuple(field_tys) => {
                if field_tys.len() != args.len() {
                    self.push(
                        Diagnostic::error(
                            "E1005",
                            format!(
                                "variant `{enum_name}::{variant_name}` takes {} field(s), but {} were supplied",
                                field_tys.len(),
                                args.len()
                            ),
                        )
                        .at(span)
                        .with_cause("tuple variant patterns and constructors must match the declaration"),
                    );
                }
                for (p, a) in field_tys.iter().zip(&args) {
                    self.check_variant_field(*p, a);
                }
            }
            VariantFields::Struct(fields) => {
                // Struct variants require named fields; report the correct form.
                if !args.is_empty() {
                    self.push(
                        Diagnostic::error(
                            "E1005",
                            format!("variant `{enum_name}::{variant_name}` uses named fields"),
                        )
                        .at(span)
                        .with_cause("this variant was declared with `{ ... }` fields")
                        .with_explanation(
                            "Struct variants are constructed like structs: \
                             `Enum::Variant { field: value }`.",
                        ),
                    );
                }
                let _ = fields;
            }
        }
        Some(HirExpr {
            kind: HirExprKind::TupleVariant {
                def,
                index: index as u32,
                args,
            },
            ty,
            span,
        })
    }

    pub(crate) fn check_variant_field(&mut self, want: TyId, got: &HirExpr) {
        if self.cx.cx.is_error(want) || self.cx.cx.is_error(got.ty) {
            return;
        }
        if (self.cx.cx.is_integer_var(got.ty) && self.cx.cx.is_integer(want))
            || (self.cx.cx.is_float_var(got.ty) && self.cx.cx.is_float(want))
        {
            return;
        }
        if !self.cx.types_compatible(want, got.ty) {
            let w = self.cx.ty_name(want);
            let g = self.cx.ty_name(got.ty);
            self.push(
                Diagnostic::error("E1001", format!("variant field expects `{w}`, found `{g}`"))
                    .at(got.span)
                    .with_cause("constructor arguments must exactly match the declared field types"),
            );
        }
    }

    // ---- method calls ----------------------------------------------------

    pub fn check_method_call(
        &mut self,
        receiver: &ast::Expr,
        method: &str,
        method_span: crate::source::Span,
        args: &[ast::Expr],
        span: crate::source::Span,
    ) -> HirExpr {
        let recv = self.check_expr(receiver);
        let arg_hirs: Vec<HirExpr> = args.iter().map(|a| self.check_expr(a)).collect();

        // Raw pointer intrinsics. A raw pointer has no nominal type to hang
        // methods on, so `offset` is recognised by the receiver's type.
        if matches!(self.cx.cx.kind(recv.ty), TyKind::RawPtr(_) | TyKind::RawPtrMut(_)) {
            return self.check_raw_ptr_method(recv, method, method_span, &arg_hirs, span);
        }

        let recv_base = self.strip_refs(recv.ty);
        let Some(type_name) = self.nominal_name(recv_base) else {
            if !self.cx.cx.is_error(recv.ty) {
                let name = self.cx.ty_name(recv.ty);
                self.push(
                    Diagnostic::error("E1014", format!("`{name}` has no methods"))
                        .at(method_span)
                        .with_cause("this type is not a struct or enum, so it cannot have method implementations"),
                );
            }
            return self.error_hir(span);
        };

        // A method from a real `impl` block wins over a built-in of the same name.
        // The built-ins exist for types a program cannot write an `impl` for
        // (`str`, arrays); once the standard library defines `String` in ordinary
        // UMAY source, its own methods must be the ones that run.
        let user_impl = self.cx.methods.get(&(type_name.clone(), method.to_string())).copied();

        if user_impl.is_none() {
            if let Some(e) =
                self.builtin_method(recv.clone(), &type_name, method, &arg_hirs, method_span, span)
            {
                return e;
            }
        }

        let Some(fn_id) = user_impl else {
            let candidates: Vec<String> = self
                .cx
                .methods_of_type
                .get(&type_name)
                .map(|v| v.clone())
                .unwrap_or_default();
            let mut d = Diagnostic::error("E0101", format!("`{type_name}` has no method `{method}`"))
                .at(method_span)
                .with_cause("no `impl` block for this type defines that method");
            if let Some(s) = suggest_name(method, candidates.iter().map(String::as_str)) {
                d = d.with_suggestion(Suggestion::new(format!("did you mean `{s}`?"), method_span, s));
            } else if candidates.is_empty() {
                d = d.with_explanation(format!("`{type_name}` has no `impl` blocks at all."));
            } else {
                let list = candidates.join(", ");
                d = d.with_explanation(format!("Available methods: {list}."));
            }
            self.push(d);
            return self.error_hir(span);
        };

        let (params, ret, has_self, self_mutable, _self_by_ref) = match self.cx.hir.fns.iter().find(|f| f.id == fn_id) {
            Some(f) => (f.params.clone(), f.ret, f.has_self, f.self_mutable, f.self_by_ref),
            None => return self.error_hir(span),
        };
        if !has_self {
            self.push(
                Diagnostic::error(
                    "E1011",
                    format!("`{type_name}::{method}` has no `self` parameter"),
                )
                .at(method_span)
                .with_cause("methods called with `receiver.method()` must take `self`")
                .with_suggestion(Suggestion::new(
                    "call it as an associated function instead",
                    method_span,
                    format!("{type_name}::{method}(...)"),
                ).with_detail("move the receiver into the argument list")),
            );
            return self.error_hir(span);
        }

        // Receiver mutability is checked against `self`'s declaration.
        if self_mutable {
            self.check_mutable_place(&recv);
        }
        // Receiver compatibility, with the automatic borrows UMAY performs:
        //   * `&self`  accepts `T`, `&T` and `&mut T`
        //   * `&mut self` accepts `T` and `&mut T`, never `&T`
        //   * `self` accepts `T` and auto-derefs `&T`
        let expected_self = params.first().copied().unwrap_or_else(|| self.cx.cx.error());
        if !self.cx.cx.is_error(expected_self) && !self.cx.cx.is_error(recv.ty) {
            let want_base = self.strip_refs(expected_self);
            let base_ok = want_base == recv_base;
            let compatible = match self.cx.cx.kind(expected_self).clone() {
                TyKind::Ref(_) => true,
                TyKind::RefMut(_) => !matches!(self.cx.cx.kind(recv.ty), TyKind::Ref(_)),
                _ => true,
            };
            if !base_ok || !compatible {
                let want = self.cx.ty_name(expected_self);
                let got = self.cx.ty_name(recv.ty);
                self.push(
                    Diagnostic::error(
                        "E1001",
                        format!("method `{method}` expects a receiver of type `{want}`, found `{got}`"),
                    )
                    .at(receiver.span)
                    .with_cause(
                        "the receiver must be compatible with how `self` is declared; \
                         `&mut self` cannot be called on a shared reference",
                    ),
                );
                return self.error_hir(span);
            }
        }

        let rest = &params[1..];
        self.check_call_args(rest, &arg_hirs, span, "method");
        HirExpr {
            kind: HirExprKind::MethodCall {
                receiver: Box::new(recv),
                method: fn_id,
                args: arg_hirs,
            },
            ty: ret,
            span,
        }
    }

    pub(crate) fn strip_refs(&self, mut ty: TyId) -> TyId {
        loop {
            match self.cx.cx.kind(ty) {
                TyKind::Ref(inner) | TyKind::RefMut(inner) => ty = *inner,
                _ => return ty,
            }
        }
    }

    /// The declared name of a struct or enum type.
    pub(crate) fn nominal_name(&self, ty: TyId) -> Option<String> {
        match self.cx.cx.kind(ty) {
            TyKind::Adt { def, .. } => Some(self.cx.hir.def_name(*def)),
            TyKind::Str => Some("str".to_string()),
            TyKind::String => Some("String".to_string()),
            TyKind::Array { .. } | TyKind::Slice(_) => Some("slice".to_string()),
            _ => None,
        }
    }

    /// Methods available on built-in types without an `impl` block.
    fn builtin_method(
        &mut self,
        recv: HirExpr,
        type_name: &str,
        method: &str,
        args: &[HirExpr],
        method_span: crate::source::Span,
        span: crate::source::Span,
    ) -> Option<HirExpr> {
        let usize_ty = self.cx.cx.primitive("usize").unwrap();
        let is_int_ret = usize_ty;
        let ret = match (type_name, method) {
            ("str", "len") | ("String", "len") => {
                self.builtin_arg_count(args, 0, method, method_span);
                is_int_ret
            }
            ("str", "is_empty") | ("String", "is_empty") => {
                self.builtin_arg_count(args, 0, method, method_span);
                self.cx.cx.bool()
            }
            ("str", "chars") | ("str", "bytes") | ("slice", "iter") => {
                self.builtin_arg_count(args, 0, method, method_span);
                self.cx.cx.unit()
            }
            ("slice", "len") => {
                self.builtin_arg_count(args, 0, method, method_span);
                is_int_ret
            }
            _ => return None,
        };
        let mut all_args = vec![recv];
        all_args.extend(args.iter().cloned());
        // Built-in methods lower to a synthetic call so that later phases have a
        // single call representation.
        Some(HirExpr {
            kind: HirExprKind::BuiltinMethod {
                name: format!("{type_name}::{method}"),
                receiver: Box::new(all_args.remove(0)),
                args: all_args,
            },
            ty: ret,
            span,
        })
    }

    pub(crate) fn builtin_arg_count(&mut self, args: &[HirExpr], expected: usize, method: &str, span: crate::source::Span) {
        if args.len() != expected {
            self.push(
                Diagnostic::error(
                    "E1004",
                    format!("method `{method}` takes {expected} argument(s), but {} were supplied", args.len()),
                )
                .at(span)
                .with_cause("this is a built-in method with a fixed signature"),
            );
        }
    }

    // ---- field & index access --------------------------------------------

    pub fn check_field_access(
        &mut self,
        base: &ast::Expr,
        field: &str,
        field_span: crate::source::Span,
        span: crate::source::Span,
    ) -> HirExpr {
        let base_hir = self.check_expr(base);
        if self.cx.cx.is_error(base_hir.ty) {
            return self.error_hir(span);
        }
        let base_ty = self.strip_refs(base_hir.ty);

        // Tuple index fields are written `t.0`, lexed as a field named "0".
        if let Ok(idx) = field.parse::<usize>() {
            if let TyKind::Tuple(elems) = self.cx.cx.kind(base_ty).clone() {
                return match elems.get(idx) {
                    Some(&ty) => HirExpr {
                        kind: HirExprKind::TupleIndex { base: Box::new(base_hir), index: idx as u32 },
                        ty,
                        span,
                    },
                    None => {
                        let len = elems.len();
                        self.push(
                            Diagnostic::error(
                                "E1006",
                                format!("tuple index {idx} is out of bounds for a {len}-element tuple"),
                            )
                            .at(field_span)
                            .with_cause("tuple indices start at 0 and must be less than the tuple's length"),
                        );
                        self.error_hir(span)
                    }
                };
            }
        }

        let TyKind::Adt { def, .. } = self.cx.cx.kind(base_ty).clone() else {
            let name = self.cx.ty_name(base_ty);
            self.push(
                Diagnostic::error("E1006", format!("`{name}` has no fields"))
                    .at(field_span)
                    .with_cause("only structs and tuples support field access"),
            );
            return self.error_hir(span);
        };
        let Some(info) = self.cx.hir.structs.iter().find(|s| s.def_id == def).cloned() else {
            let name = self.cx.hir.def_name(def);
            self.push(
                Diagnostic::error("E1006", format!("`{name}` is an enum and has no fields"))
                    .at(field_span)
                    .with_cause("enum values must be destructured with `match`")
                    .with_explanation(
                        "Each variant may carry different data, so field access is not meaningful. \
                         Pattern-match the variant first.",
                    ),
            );
            return self.error_hir(span);
        };
        match info
            .fields
            .iter()
            .enumerate()
            .find(|(_, f)| f.name == field)
        {
            Some((index, f)) => {
                let ty = f.ty;
                HirExpr {
                    kind: HirExprKind::Field {
                        base: Box::new(base_hir),
                        name: field.to_string(),
                        index: index as u32,
                    },
                    ty,
                    span,
                }
            }
            None => {
                let candidates: Vec<&str> = info.fields.iter().map(|f| f.name.as_str()).collect();
                let mut d = Diagnostic::error(
                    "E1006",
                    format!("struct `{}` has no field `{field}`", info.name),
                )
                .at(field_span)
                .with_cause("the field name does not appear in the struct declaration");
                if let Some(s) = suggest_name(field, candidates.iter().copied()) {
                    d = d.with_suggestion(Suggestion::new(format!("did you mean `{s}`?"), field_span, s));
                } else if !candidates.is_empty() {
                    d = d.with_explanation(format!("Available fields: {}.", candidates.join(", ")));
                }
                self.push(d);
                self.error_hir(span)
            }
        }
    }

    pub fn check_index(&mut self, base: &ast::Expr, index: &ast::Expr, span: crate::source::Span) -> HirExpr {
        let base_hir = self.check_expr(base);
        let index_hir = self.check_expr(index);
        if self.cx.cx.is_error(base_hir.ty) {
            return self.error_hir(span);
        }
        let base_ty = self.strip_refs(base_hir.ty);

        // The index must be an integer.
        let int_ok = self.cx.cx.is_integer(index_hir.ty) || self.cx.cx.is_integer_var(index_hir.ty);
        if !int_ok && !self.cx.cx.is_error(index_hir.ty) {
            let name = self.cx.ty_name(index_hir.ty);
            self.push(
                Diagnostic::error("E1001", format!("array index must be an integer, found `{name}`"))
                    .at(index_hir.span)
                    .with_cause("indices select by position and are always integers"),
            );
        }

        let elem = match self.cx.cx.kind(base_ty).clone() {
            TyKind::Array { elem, .. } | TyKind::Slice(elem) => elem,
            TyKind::Error => return self.error_hir(span),
            _ => {
                let name = self.cx.ty_name(base_ty);
                self.push(
                    Diagnostic::error("E1014", format!("cannot index a value of type `{name}`"))
                        .at(base.span)
                        .with_cause("only arrays, slices and `Vec` support `[...]`"),
                );
                return self.error_hir(span);
            }
        };
        HirExpr {
            kind: HirExprKind::Index { base: Box::new(base_hir), index: Box::new(index_hir) },
            ty: elem,
            span,
        }
    }

    // ---- literal constructors --------------------------------------------

    pub fn check_struct_lit(
        &mut self,
        path: &ast::Path,
        fields: &[ast::FieldInit],
        base: Option<&ast::Expr>,
        span: crate::source::Span,
    ) -> HirExpr {
        let name = path.segments.last().map(|s| s.name.clone()).unwrap_or_default();
        let Some(def) = self.cx.def_by_name.get(&name).copied() else {
            self.unknown_name(&name, path.span);
            for f in fields {
                match &f.value {
                    Some(v) => {
                        self.check_expr(v);
                    }
                    None => {
                        self.check_expr(&ast::Expr { kind: ast::ExprKind::Ident(f.name.clone()), span: f.name_span });
                    }
                }
            }
            return self.error_hir(span);
        };
        let Some(info) = self.cx.hir.structs.iter().find(|s| s.def_id == def).cloned() else {
            self.push(
                Diagnostic::error("E1001", format!("`{name}` is not a struct"))
                    .at(path.span)
                    .with_cause("struct literals can only construct structs"),
            );
            return self.error_hir(span);
        };

        let base_hir = base.map(|b| self.check_expr(b));
        if let Some(b) = &base_hir {
            let want = self.cx.cx.adt(def, Vec::new());
            if !self.cx.cx.is_error(b.ty) && !self.cx.types_compatible(want, b.ty) {
                let got = self.cx.ty_name(b.ty);
                self.push(
                    Diagnostic::error(
                        "E1001",
                        format!("struct update base has type `{got}`, expected `{name}`"),
                    )
                    .at(b.span)
                    .with_cause("`..base` must be a value of the same struct type"),
                );
            }
        }

        let mut inits = Vec::new();
        let mut provided: Vec<String> = Vec::new();
        for f in fields {
            // `{ x }` is shorthand for `{ x: x }`.
            let value = match &f.value {
                Some(v) => self.check_expr(v),
                None => self.check_expr(&ast::Expr {
                    kind: ast::ExprKind::Ident(f.name.clone()),
                    span: f.name_span,
                }),
            };
            let Some(field) = info.fields.iter().find(|sf| sf.name == f.name).cloned() else {
                let candidates: Vec<&str> = info.fields.iter().map(|sf| sf.name.as_str()).collect();
                let mut d = Diagnostic::error(
                    "E1006",
                    format!("struct `{name}` has no field `{}`", f.name),
                )
                .at(f.name_span)
                .with_cause("this field is not declared on the struct");
                if let Some(s) = suggest_name(&f.name, candidates.iter().copied()) {
                    d = d.with_suggestion(Suggestion::new(format!("did you mean `{s}`?"), f.name_span, s));
                }
                self.push(d);
                inits.push(HirFieldInit { name: f.name.clone(), span: f.name_span, index: 0, value });
                continue;
            };
            if provided.contains(&f.name) {
                self.push(
                    Diagnostic::error("E1010", format!("field `{}` is initialised twice", f.name))
                        .at(f.name_span)
                        .with_cause("each field may appear at most once in a struct literal"),
                );
            } else {
                provided.push(f.name.clone());
            }
            let field_index = info
                .fields
                .iter()
                .position(|sf| sf.name == f.name)
                .unwrap_or(0) as u32;
            if self.cx.cx.is_error(field.ty) || self.cx.cx.is_error(value.ty) {
                inits.push(HirFieldInit {
                    name: f.name.clone(),
                    span: f.name_span,
                    index: field_index,
                    value,
                });
                continue;
            }
            if (self.cx.cx.is_integer_var(value.ty) && self.cx.cx.is_integer(field.ty))
                || (self.cx.cx.is_float_var(value.ty) && self.cx.cx.is_float(field.ty))
                || self.cx.types_compatible(field.ty, value.ty)
            {
                // A literal takes the declared field type, so that no inference
                // variable survives into MIR.
                let value = self.coerce_literal(value, field.ty);
                inits.push(HirFieldInit {
                    name: f.name.clone(),
                    span: f.name_span,
                    index: field_index,
                    value,
                });
            } else {
                let want = self.cx.ty_name(field.ty);
                let got = self.cx.ty_name(value.ty);
                self.push(
                    Diagnostic::error(
                        "E1001",
                        format!("field `{}` expects `{want}`, found `{got}`", f.name),
                    )
                    .at(value.span)
                    .with_cause("struct field initialisers must match the declared field type"),
                );
                inits.push(HirFieldInit {
                    name: f.name.clone(),
                    span: f.name_span,
                    index: field_index,
                    value,
                });
            }
        }

        // Every field must be supplied unless the base update covers it.
        if base_hir.is_none() {
            let missing: Vec<&str> = info
                .fields
                .iter()
                .filter(|sf| !provided.contains(&sf.name))
                .map(|sf| sf.name.as_str())
                .collect();
            if !missing.is_empty() {
                let tail = if missing.len() == 1 { "a value" } else { "values" };
                self.push(
                    Diagnostic::error(
                        "E1009",
                        format!("field{} `{}` not initialised", if missing.len() == 1 { "" } else { "s" }, missing.join("`, `")),
                    )
                    .at(span)
                    .with_cause(format!("struct literals must provide {tail} for every field"))
                    .with_explanation(
                        "Partial initialisation is not allowed because it would leave the value \
                         in an invalid state. Use `..base` to copy the remaining fields from an \
                         existing value.",
                    ),
                );
            }
        }

        let ty = self.cx.cx.adt(def, Vec::new());
        HirExpr {
            kind: HirExprKind::StructLit { def, fields: inits, base: base_hir.map(Box::new) },
            ty,
            span,
        }
    }

    pub fn check_array_lit(&mut self, elems: &[ast::Expr], span: crate::source::Span) -> HirExpr {
        if elems.is_empty() {
            self.push(
                Diagnostic::error("E1001", "cannot infer the element type of an empty array")
                    .at(span)
                    .with_cause("there are no elements to take a type from")
                    .with_suggestion(Suggestion::new(
                        "annotate the binding",
                        span,
                        "let xs: [T; 0] = []".to_string(),
                    ).with_detail("replace `T` with the element type")),
            );
            return self.error_hir(span);
        }
        let checked: Vec<HirExpr> = elems.iter().map(|e| self.check_expr(e)).collect();
        let first_ty = checked[0].ty;
        let mut elem_ty = first_ty;
        for e in checked.iter().skip(1) {
            if self.cx.cx.is_error(e.ty) || self.cx.cx.is_error(elem_ty) {
                continue;
            }
            if self.cx.cx.is_integer_var(elem_ty) && self.cx.cx.is_integer(e.ty) {
                elem_ty = e.ty;
                continue;
            }
            if self.cx.cx.is_float_var(elem_ty) && self.cx.cx.is_float(e.ty) {
                elem_ty = e.ty;
                continue;
            }
            if !self.cx.types_compatible(elem_ty, e.ty) {
                let first = self.cx.ty_name(elem_ty);
                let got = self.cx.ty_name(e.ty);
                self.push(
                    Diagnostic::error(
                        "E1001",
                        format!("array element has type `{got}`, but earlier elements have type `{first}`"),
                    )
                    .at(e.span)
                    .with_cause("every element of an array must have the same type"),
                );
            }
        }
        // Literal variables become the resolved element type where possible.
        let resolved = self.resolve_var_for(elem_ty);
        let len = checked.len() as u64;
        let ty = self.cx.cx.array(resolved, len);
        HirExpr {
            kind: HirExprKind::ArrayLit { elems: checked },
            ty,
            span,
        }
    }

    pub fn check_array_repeat(&mut self, value: &ast::Expr, count: &ast::Expr, span: crate::source::Span) -> HirExpr {
        let value = self.check_expr(value);
        let n = match &count.kind {
            ast::ExprKind::IntLit { value: v, .. } => u64::try_from(*v).ok(),
            _ => None,
        };
        let Some(n) = n else {
            let _ = self.check_expr(count);
            self.push(
                Diagnostic::error("E1001", "array repeat count must be an integer literal")
                    .at(count.span)
                    .with_cause("the length must be known at compile time"),
            );
            return self.error_hir(span);
        };
        if n == 0 {
            self.push(
                Diagnostic::error("E1001", "array repeat count must be at least 1")
                    .at(count.span)
                    .with_cause("a zero-length repeat gives no element type")
                    .with_suggestion(Suggestion::new("use an explicit empty array", count.span, "1")),
            );
        }
        let elem = self.resolve_var_for(value.ty);
        if !self.cx.cx.is_error(elem) && !self.cx.cx.is_copy(elem) {
            let name = self.cx.ty_name(elem);
            self.push(
                Diagnostic::error(
                    "E0203",
                    format!("cannot repeat a value of type `{name}`: it is not `Copy`"),
                )
                .at(value.span)
                .with_cause("repeating would duplicate an owning value")
                .with_explanation(
                    "`[value; n]` copies the value `n` times. Owning values may only be \
                     duplicated explicitly, for example by constructing each element in a loop.",
                ),
            );
        }
        let ty = self.cx.cx.array(elem, n);
        HirExpr {
            kind: HirExprKind::ArrayRepeat { value: Box::new(value), count: n },
            ty,
            span,
        }
    }

    pub fn check_tuple_lit(&mut self, elems: &[ast::Expr], span: crate::source::Span) -> HirExpr {
        let checked: Vec<HirExpr> = elems.iter().map(|e| self.check_expr(e)).collect();
        let tys: Vec<TyId> = checked.iter().map(|e| self.resolve_var_for(e.ty)).collect();
        let ty = self.cx.cx.tuple(tys);
        HirExpr {
            kind: HirExprKind::Tuple { elems: checked },
            ty,
            span,
        }
    }

    /// Resolves a literal variable to its default so that structural types stay
    /// concrete. Integer literals default to `i32`, float literals to `f64`.
    pub(crate) fn resolve_var_for(&mut self, ty: TyId) -> TyId {
        if self.cx.cx.is_integer_var(ty) {
            return self.cx.cx.primitive("i32").unwrap();
        }
        if self.cx.cx.is_float_var(ty) {
            return self.cx.cx.primitive("f64").unwrap();
        }
        ty
    }
}

impl FnChecker<'_, '_> {
    /// Handles the intrinsics available on a raw pointer.
    ///
    /// `offset(n)` moves the pointer by `n` elements of the pointee type. The
    /// multiplication by the element size happens in code generation, so the
    /// count stays in elements and never in bytes.
    fn check_raw_ptr_method(
        &mut self,
        recv: HirExpr,
        method: &str,
        method_span: crate::source::Span,
        args: &[HirExpr],
        span: crate::source::Span,
    ) -> HirExpr {
        let ptr_ty = recv.ty;
        match method {
            "offset" => {
                if args.len() != 1 {
                    self.push(
                        Diagnostic::error(
                            "E1004",
                            format!("`offset` takes 1 argument, found {}", args.len()),
                        )
                        .at(method_span)
                        .with_cause("`ptr.offset(n)` moves a raw pointer by `n` elements"),
                    );
                    return self.error_hir(span);
                }
                let count = &args[0];
                if !self.cx.cx.is_integer(count.ty) && !self.cx.cx.is_integer_var(count.ty) {
                    let name = self.cx.ty_name(count.ty);
                    self.push(
                        Diagnostic::error(
                            "E1001",
                            format!("`offset` expects an integer count, found `{name}`"),
                        )
                        .at(count.span)
                        .with_cause("the offset counts elements, so it must be an integer"),
                    );
                    return self.error_hir(span);
                }
                HirExpr {
                    kind: HirExprKind::PtrOffset {
                        ptr: Box::new(recv),
                        count: Box::new(count.clone()),
                    },
                    ty: ptr_ty,
                    span,
                }
            }
            other => {
                let name = self.cx.ty_name(ptr_ty);
                self.push(
                    Diagnostic::error("E1014", format!("`{name}` has no method `{other}`"))
                        .at(method_span)
                        .with_cause("raw pointers only provide `offset`"),
                );
                self.error_hir(span)
            }
        }
    }
}
