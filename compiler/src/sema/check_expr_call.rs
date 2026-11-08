//! Expression checking: calls, method calls, field access and literals.

use crate::ast;
use crate::diagnostics::{Diagnostic, Suggestion};
use crate::hir::*;
use crate::sema::check_expr::FnChecker;
use crate::sema::scope::suggest_name;
use crate::types::{TyId, TyKind};

impl FnChecker<'_, '_> {
    // ---- calls -----------------------------------------------------------

    /// Checks a call, optionally knowing what type the context expects.
    ///
    /// The hint only reaches a function that cannot name its own type parameters
    /// from its arguments, such as `Vec::with_capacity(n)`: the annotation on the
    /// binding is what says which `Vec` it is.
    pub fn check_call(
        &mut self,
        callee: &ast::Expr,
        args: &[ast::Expr],
        span: crate::source::Span,
        hint: Option<crate::types::TyId>,
    ) -> HirExpr {
        // `Type::Variant(...)` constructs an enum variant.
        if let ast::ExprKind::Path(path) = &callee.kind {
            if path.segments.len() == 2 {
                if let Some(e) = self.try_variant_ctor(path, args, span, hint) {
                    return e;
                }
                // `Type::fn(args)` reaches a function defined in the type's `impl`
                // block, which is how a constructor with no receiver is written.
                if let Some(e) = self.try_associated_fn(path, args, span, hint) {
                    return e;
                }
            }
        }

        // `thread_spawn<A, R>(work, arg)` hands a value to another thread. The
        // markers say which values may cross and nothing else checks them, so the
        // demand is made where the crossing happens.
        if let ast::ExprKind::Path(path) = &callee.kind {
            if path.segments.len() == 1 && path.segments[0].name == "thread_spawn" {
                return self.check_thread_spawn(path, args, span, hint);
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
        // The callee's bounds are promises its body is checked against, so the
        // caller is where they have to be kept. Without this a `T: Send` bound
        // would say nothing to a caller who never asked whether a type could
        // cross, which is the whole reason the bound was written.
        if let HirExprKind::FnRef(id) = callee_hir.kind {
            self.check_bounds_of(id, &params, &arg_hirs, span);
        }
        HirExpr {
            kind: HirExprKind::Call { callee: Box::new(callee_hir), args: arg_hirs },
            ty: ret,
            span,
        }
    }

    /// Checks that the types a call actually uses satisfy the declaration's bounds.
    ///
    /// The parameter names come from the declaration; the types come from the
    /// arguments, matched the way the body's own parameters are. A parameter the
    /// arguments do not name is left alone: `Vec::new()` says nothing about `T`,
    /// and the annotation at the binding is where that is settled.
    fn check_bounds_of(
        &mut self,
        callee: FnId,
        params: &[TyId],
        args: &[HirExpr],
        span: crate::source::Span,
    ) {
        let Some(decl) = self.cx.hir.fns.iter().find(|f| f.id == callee) else { return };
        let decl_params = decl.params.clone();
        let decl_name = decl.name.clone();
        // Only a marker is checked here. A marker declares no methods, so its
        // only meaning is the promise it makes, and a promise nothing checks is
        // not a rule. A trait with methods is different: UMAY resolves a method
        // call on a parameter structurally, by finding one that fits, so a
        // program that never wrote `impl Iterator for Counter` still runs, and
        // demanding the `impl` would reject working code.
        let bounds: Vec<crate::types::ParamBound> = self
            .cx
            .cx
            .param_bounds
            .iter()
            .filter(|b| b.owner_name == decl_name && self.cx.hir.traits.iter().any(|t| t.name == b.trait_name && t.methods.is_empty()))
            .cloned()
            .collect();
        if bounds.is_empty() {
            return;
        }
        let _ = params;
        // Every parameter the call pinned down, from the declared and actual types.
        let mut fixed: std::collections::HashMap<String, TyId> = std::collections::HashMap::new();
        for (d, a) in decl_params.iter().zip(args.iter()) {
            unify_bound(&mut self.cx.cx, *d, a.ty, &mut fixed);
        }
        for bound in bounds {
            let Some(ty) = fixed.get(&bound.param).copied() else { continue };
            // A type still naming a parameter is not this call's business: the
            // caller is generic too, and its own bound is checked where *it* is
            // called. `mentions_unresolved_param` cannot be asked here, because it
            // reports every argument-less type -- `NotSend` is one, and so is the
            // generic case it is really about.
            if self.cx.cx.is_error(ty) || self.cx.cx.first_param(ty).is_some() {
                continue;
            }
            if !self.satisfies_bound(ty, &bound.trait_name) {
                let name = self.cx.ty_name(ty);
                let mut d = Diagnostic::error(
                    "E1017",
                    format!("`{name}` does not implement `{}`", bound.trait_name),
                )
                .at(span)
                .with_cause(format!(
                    "`{decl_name}` requires its parameter `{}` to implement `{}`",
                    bound.param, bound.trait_name
                ))
                .with_explanation(
                    "A bound is a promise the callee's body relies on: it may call the \
                     trait's methods, or use the marker to decide whether a value may cross \
                     a thread boundary. The caller is where the promise is kept, so the \
                     argument's type has to carry the same `impl`.",
                );
                if let Some(ty_name) = self.nominal_name(ty) {
                    d = d.with_suggestion(Suggestion::new(
                        format!("add `impl {} for {ty_name} {{}}`", bound.trait_name),
                        span,
                        format!("impl {} for {ty_name} {{}}", bound.trait_name),
                    ));
                }
                self.push(d);
            }
        }
    }

    /// True when `ty` has an `impl` of the named trait.
    fn satisfies_bound(&mut self, ty: TyId, trait_name: &str) -> bool {
        if trait_name == "Send" {
            return self.is_send(ty);
        }
        let Some(impls) = self
            .cx
            .hir
            .traits
            .iter()
            .find(|t| t.name == trait_name)
            .map(|t| t.impls.clone())
        else {
            return true;
        };
        let name = match self.cx.cx.kind(ty).clone() {
            TyKind::Adt { def, .. } => self.cx.hir.def_name(def),
            _ => self.cx.ty_name(ty),
        };
        impls.iter().any(|i| i.type_name == name)
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
    /// `Type::fn(args)` — a function in `Type`'s `impl` block with no receiver.
    ///
    /// Returns `None` when the first segment is not a type this program declares
    /// or the block has no such function, so the caller can carry on treating the
    /// path as a value.
    fn try_associated_fn(
        &mut self,
        path: &ast::Path,
        args: &[ast::Expr],
        span: crate::source::Span,
        hint: Option<crate::types::TyId>,
    ) -> Option<HirExpr> {
        let type_name = path.segments[0].name.clone();
        let fn_name = path.segments[1].name.clone();
        let fn_id = *self.cx.methods.get(&(type_name.clone(), fn_name.clone()))?;
        // A method with a receiver is not callable through the type: it needs an
        // instance, and saying so is more useful than an unknown-name error.
        let info = self.cx.hir.fns.iter().find(|f| f.id == fn_id).cloned()?;
        if !info.params.is_empty() && info.has_self {
            self.push(
                Diagnostic::error(
                    "E1014",
                    format!("`{type_name}::{fn_name}` takes a receiver, so it is called on a value"),
                )
                .at(span)
                .with_cause("this function needs an instance of the type")
                .with_suggestion(Suggestion::new(
                    format!("write `value.{fn_name}(..)`"),
                    path.span,
                    format!("value.{fn_name}"),
                )),
            );
            return Some(self.error_hir(span));
        }
        // Arguments are checked against the declaration the same way a free
        // function call's are; the callee is the function itself, not a path.
        // Each argument is told its parameter's type, so a bare literal such as
        // `-1` takes that width instead of the default one.
        let self_param = if info.has_self { 1 } else { 0 };
        let arg_hirs: Vec<HirExpr> = args
            .iter()
            .enumerate()
            .map(|(i, a)| {
                let expected = info.params.get(i + self_param).copied();
                self.check_expr_hinted(a, expected)
            })
            .collect();
        self.check_call_args(&info.params, &arg_hirs, span, "function");
        // A method of `impl<T>` names `T` in its result, and its arguments may not
        // mention the parameter at all. The context's expected type is then the
        // only thing that says which instance is meant.
        let ret = match hint {
            Some(h) if self.cx.cx.first_param(info.ret).is_some() => {
                match (self.cx.cx.kind(info.ret).clone(), self.cx.cx.kind(h).clone()) {
                    (
                        TyKind::Adt { def: d1, .. },
                        TyKind::Adt { def: d2, args },
                    ) if d1 == d2 => self.cx.cx.adt(d1, args),
                    _ => info.ret,
                }
            }
            _ => info.ret,
        };
        let callee = HirExpr {
            kind: HirExprKind::FnRef(fn_id),
            ty: self.cx.cx.fn_ptr(info.params.clone(), ret),
            span: path.span,
        };
        Some(HirExpr {
            kind: HirExprKind::Call { callee: Box::new(callee), args: arg_hirs },
            ty: ret,
            span,
        })
    }

    pub(crate) fn try_variant_ctor(
        &mut self,
        path: &ast::Path,
        args: &[ast::Expr],
        span: crate::source::Span,
        hint: Option<crate::types::TyId>,
    ) -> Option<HirExpr> {
        let enum_name = &path.segments[0].name;
        let def = *self.cx.def_by_name.get(enum_name)?;
        let info = self.cx.hir.enums.iter().find(|e| e.def_id == def)?.clone();
        let variant_name = &path.segments[1].name;
        let index = info.variants.iter().position(|v| v.name == *variant_name)?;
        let args: Vec<HirExpr> = args.iter().map(|a| self.check_expr(a)).collect();
        let variant = info.variants[index].clone();
        // A generic enum infers its arguments from the payload it is given, the
        // same way a struct literal does: `Option::Some(7)` is an `Option<i32>`.
        let mut inferred: std::collections::HashMap<String, crate::types::TyId> =
            std::collections::HashMap::new();
        let mut literal_inferred: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        if !info.generics.is_empty() {
            let field_tys: Vec<crate::types::TyId> = match &variant.fields {
                VariantFields::Tuple(tys) => tys.clone(),
                VariantFields::Struct(fields) => fields.iter().map(|f| f.ty).collect(),
                VariantFields::Unit => Vec::new(),
            };
            for (p, a) in field_tys.iter().zip(args.iter()) {
                let crate::types::TyKind::Param { name } = self.cx.cx.kind(*p).clone() else {
                    continue;
                };
                if self.cx.cx.is_error(a.ty) {
                    continue;
                }
                let mut ty = a.ty;
                if self.cx.cx.is_integer_var(ty) {
                    ty = self.cx.cx.i32_ty();
                } else if self.cx.cx.is_float_var(ty) {
                    ty = self.cx.cx.f64_ty();
                }
                // A bare literal is recorded but flagged so a hint can still
                // overrule it: `Option::Some(1)` defaults to `i32`, and an
                // annotation of `Option<i64>` is a better answer than the default.
                if self.cx.cx.is_integer_var(a.ty) || self.cx.cx.is_float_var(a.ty) {
                    literal_inferred.insert(name.clone());
                }
                inferred.entry(name).or_insert(ty);
            }
        }
        // What the payload does not say comes from the type the expression is
        // being given. `Result::Err(e)` names `E` and nothing names `T`, so a
        // return type of `Result<i64, i64>` has to supply it.
        let mut from_hint: Vec<crate::types::TyId> = Vec::new();
        if let Some(hint_ty) = hint {
            if let crate::types::TyKind::Adt { def: hdef, args: hargs } = self.cx.cx.kind(hint_ty) {
                if *hdef == def && hargs.len() == info.generics.len() {
                    from_hint = hargs.clone();
                }
            }
        }
        let mut ty_args: Vec<crate::types::TyId> = Vec::with_capacity(info.generics.len());
        for (position, g) in info.generics.iter().enumerate() {
            // A parameter no payload mentions stays an error: guessing would pick
            // the wrong type silently, and an annotation is the way to say it.
            let hinted = from_hint.get(position).copied();
            let guess = inferred.get(g).copied();
            // The annotation wins only over a defaulted literal; a payload with a
            // concrete type is what the variant really is.
            let chosen = match (hinted, guess) {
                (Some(h), Some(_)) if literal_inferred.contains(g) => Some(h),
                (_, Some(g0)) => Some(g0),
                (h, None) => h,
            };
            match chosen {
                Some(t) => ty_args.push(t),
                None => {
                    self.push(
                        Diagnostic::error(
                            "E1001",
                            format!("cannot infer the type parameter `{g}` of `{enum_name}`"),
                        )
                        .at(span)
                        .with_cause("this variant carries no value that could name the parameter")
                        .with_explanation(
                            "A variant like `Option::None` holds no data, so nothing in the \
                             expression says what `T` is. Give the binding a type, as in \
                             `let empty: Option<i64> = Option::None`, and the value takes it.",
                        ),
                    );
                    ty_args.push(self.cx.cx.error());
                }
            }
        }
        let ty = self.cx.cx.adt(def, ty_args);

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
                    let want = self.cx.cx.substitute(*p, &inferred);
                    self.check_variant_field(want, a);
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
        hint: Option<crate::types::TyId>,
    ) -> HirExpr {
        let recv = self.check_expr(receiver);
        let arg_hirs: Vec<HirExpr> = args.iter().map(|a| self.check_expr(a)).collect();

        // Raw pointer intrinsics. A raw pointer has no nominal type to hang
        // methods on, so `offset` is recognised by the receiver's type.
        if matches!(self.cx.cx.kind(recv.ty), TyKind::RawPtr(_) | TyKind::RawPtrMut(_)) {
            return self.check_raw_ptr_method(recv, method, method_span, &arg_hirs, span);
        }

        let recv_base = self.strip_refs(recv.ty);
        // A method on a generic parameter has no `impl` block to name it: only
        // `T: Counter` does. The call is recorded against the trait and resolved
        // once monomorphisation knows what `T` is.
        if let TyKind::Param { name } = self.cx.cx.kind(recv_base) {
            let param = name.clone();
            if let Some((trait_name, trait_args)) = self.bound_trait_of(&param, method) {
                // The result type comes from the trait's own declaration, written
                // in terms of the trait's parameters, so `T: Iterator<i64>` makes
                // `next()` return `Option<i64>` rather than `Option<T>`.
                let declared_ret = self
                    .cx
                    .hir
                    .traits
                    .iter()
                    .find(|t| t.name == trait_name)
                    .and_then(|t| t.methods.iter().find(|m| m.name == method))
                    .map(|m| m.ret);
                let ret = match declared_ret {
                    Some(r) => {
                        let names: Vec<String> = self
                            .cx
                            .hir
                            .traits
                            .iter()
                            .find(|t| t.name == trait_name)
                            .map(|t| t.generics.clone())
                            .unwrap_or_default();
                        let mut subst = std::collections::HashMap::new();
                        for (n, t) in names.iter().zip(trait_args.iter()) {
                            subst.insert(n.clone(), *t);
                        }
                        self.cx.cx.substitute(r, &subst)
                    }
                    None => self.cx.cx.error(),
                };
                return HirExpr {
                    ty: ret,
                    span,
                    kind: HirExprKind::TraitMethodCall {
                        receiver: Box::new(recv),
                        trait_name,
                        method: method.to_string(),
                        method_span,
                        args: arg_hirs,
                    },
                };
            }
        }
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

        // The receiver's instance decides the method's parameters. A receiver
        // written as `make(7i64).get()` has no instance: `make` answers `B<T>` at
        // check time, and only monomorphisation knows it is `B<i64>`. The declared
        // result and the context the call sits in are what name it, and both are
        // available here, so the gap is closed where it is visible instead of
        // being deferred to a call site that never learns the type.
        let mut recv_base = recv_base;
        if matches!(self.cx.cx.kind(recv_base), TyKind::Adt { .. })
            && self.cx.cx.first_param(recv_base).is_some()
        {
            if let Some(fixed) =
                self.adopt_receiver_instance(&type_name, method, recv_base, ret, hint)
            {
                recv_base = fixed;
            }
        }
        // Receiver compatibility, with the automatic borrows UMAY performs:
        //   * `&self`  accepts `T`, `&T` and `&mut T`
        //   * `&mut self` accepts `T` and `&mut T`, never `&T`
        //   * `self` accepts `T` and auto-derefs `&T`
        let expected_self = params.first().copied().unwrap_or_else(|| self.cx.cx.error());
        if !self.cx.cx.is_error(expected_self) && !self.cx.cx.is_error(recv.ty) {
            let want_base = self.strip_refs(expected_self);
            // A generic method's `self` names parameters the receiver has already
            // chosen, so the two are compared structurally rather than by
            // interned identity.
            let base_ok = self.cx.types_compatible(want_base, recv_base);
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

        // A method of a generic type may name the type's parameters in its own
        // signature, so the receiver's choice decides them: `Box<i64>::get -> T`
        // is `-> i64` once the receiver is a `Box<i64>`.
        let subst = self.instance_of(recv_base);
        let rest = &params[1..];
        let rest: Vec<crate::types::TyId> =
            rest.iter().map(|t| self.cx.cx.substitute(*t, &subst)).collect();
        self.check_call_args(&rest, &arg_hirs, span, "method");
        let ret = self.cx.cx.substitute(ret, &subst);
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

    /// Fills in a receiver's instance from the method's declared result.
    ///
    /// `fn get(self) -> T` on `impl<T> B<T>` declares a result that mentions the
    /// parameters the receiver is missing, so a result the context asks to be
    /// `i64` says the receiver is `B<i64>`. Only parameters the result mentions
    /// can be filled this way; anything left unknown keeps the receiver as it
    /// was, which the backend reports rather than miscompiles.
    fn adopt_receiver_instance(
        &mut self,
        type_name: &str,
        method: &str,
        recv_base: TyId,
        declared_ret: TyId,
        hint: Option<TyId>,
    ) -> Option<TyId> {
        let hint = hint?;
        let names = self
            .cx
            .method_generics
            .get(&(type_name.to_string(), method.to_string()))?
            .1
            .clone();
        let TyKind::Adt { def, args } = self.cx.cx.kind(recv_base).clone() else { return None };
        if args.len() != names.len() {
            return None;
        }
        let TyKind::Param { name } = self.cx.cx.kind(declared_ret).clone() else { return None };
        let position = names.iter().position(|g| *g == name)?;
        let mut fixed = args;
        fixed[position] = hint;
        Some(self.cx.cx.adt(def, fixed))
    }

    /// The type arguments an instantiated type carries, keyed by parameter name.
    ///
    /// `Box<i64>` gives `{T: i64}`; a type with no arguments gives an empty map,
    /// which is what makes substitution a no-op for non-generic code.
    pub(crate) fn instance_of(
        &mut self,
        ty: crate::types::TyId,
    ) -> std::collections::HashMap<String, crate::types::TyId> {
        let mut subst = std::collections::HashMap::new();
        let base = self.strip_refs(ty);
        if let crate::types::TyKind::Adt { def, args } = self.cx.cx.kind(base).clone() {
            // Both kinds of type definition are looked up: an enum may have an
            // `impl` block just as a struct does, and its arguments are what name
            // the parameters used in its methods' signatures.
            let generics = self
                .cx
                .hir
                .structs
                .iter()
                .find(|s| s.def_id == def)
                .map(|s| s.generics.clone())
                .or_else(|| {
                    self.cx
                        .hir
                        .enums
                        .iter()
                        .find(|e| e.def_id == def)
                        .map(|e| e.generics.clone())
                });
            if let Some(generics) = generics {
                for (name, arg) in generics.iter().zip(args.iter()) {
                    subst.insert(name.clone(), *arg);
                }
            }
        }
        subst
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
    /// The trait that gives the parameter `param` a method called `method`.
    ///
    /// A parameter may carry several bounds and a trait may declare several
    /// methods, so the first `(bound, method)` pair that matches wins. When none
    /// does, the caller falls through to its usual "no methods" diagnostic.
    fn bound_trait_of(&self, param: &str, method: &str) -> Option<(String, Vec<TyId>)> {
        // Every bound on this parameter name is tried, and the trait that declares
        // the method wins. A parameter is named `T` in many functions, so the name
        // alone does not identify one -- but it does identify the method: a trait
        // that does not declare `next` cannot answer `it.next()`.
        //
        // Which function declared a bound is recorded, but it is deliberately not
        // used to filter: the id a bound was filed under and the id the body is
        // checked under can differ between the prelude and the program, and a
        // filter on it made a bound invisible for no reason a user could see.
        for bound in &self.cx.cx.param_bounds {
            if bound.param != param {
                continue;
            }
            let Some(info) = self.cx.hir.traits.iter().find(|t| t.name == bound.trait_name) else {
                continue;
            };
            if info.methods.iter().any(|m| m.name == method) {
                return Some((bound.trait_name.clone(), bound.args.clone()));
            }
        }
        return None;
    }

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
                // A field of a generic struct has the type its instantiation chose:
                // `Pair<i64, i32>::first` is an `i64`, not an `A`.
                let subst = self.instance_of(base_ty);
                let ty = self.cx.cx.substitute(f.ty, &subst);
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
        hint: Option<crate::types::TyId>,
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

        // A generic struct's literals infer their type arguments from the field
        // values: `Box { value: 7 }` means `Box<i32>`. The arguments are collected
        // first so that every field is checked against the *same* instantiation,
        // which is what makes `Pair { a: 1, b: 2 }` one type and not two.
        let mut inferred: std::collections::HashMap<String, crate::types::TyId> =
            std::collections::HashMap::new();
        let mut literal_inferred: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        if !info.generics.is_empty() {
            // An explicit argument written with `::<..>` outranks anything the
            // fields suggest: the caller said what the type is.
            let explicit: Vec<TyId> = path
                .segments
                .last()
                .map(|seg| seg.args.iter().map(|a| self.cx.resolve_type(a, &[])).collect())
                .unwrap_or_default();
            if explicit.len() == info.generics.len() {
                for (name, ty) in info.generics.iter().zip(explicit.iter()) {
                    inferred.insert(name.clone(), *ty);
                }
            }
            for f in fields {
                let Some(value_ast) = &f.value else { continue };
                let Some(field) = info.fields.iter().find(|sf| sf.name == f.name) else { continue };
                // Only a field whose type is exactly one parameter tells us
                // anything; `*mut T` or `T` nested in another type is left to a
                // later pass rather than guessed at.
                if let crate::types::TyKind::Param { name } = self.cx.cx.kind(field.ty).clone() {
                    let probed = self.check_expr(value_ast);
                    if self.cx.cx.is_error(probed.ty) {
                        continue;
                    }
                    // A bare literal is evidence too: `Box { value: 7 }` sets the
                    // parameter to the literal's type, which is settled to its
                    // default width in the same pass that settles every other
                    // unresolved literal.
                    // A bare literal is recorded at the type it will actually be
                    // stored as. Keeping the unresolved variable would let the
                    // struct's layout be asked for a width the literal never takes.
                    let mut ty = probed.ty;
                    // `i32` and `f64` are the documented defaults for an unsuffixed
                    // literal, so instantiating at them matches what the literal
                    // would be stored as anywhere else.
                    if self.cx.cx.is_integer_var(ty) {
                        ty = self.cx.cx.i32_ty();
                    } else if self.cx.cx.is_float_var(ty) {
                        ty = self.cx.cx.f64_ty();
                    }
                    if let std::collections::hash_map::Entry::Vacant(slot) = inferred.entry(name) {
                        slot.insert(ty);
                    }
                }
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
            let field_ty = self.cx.cx.substitute(field.ty, &inferred);
            if (self.cx.cx.is_integer_var(value.ty) && self.cx.cx.is_integer(field_ty))
                || (self.cx.cx.is_float_var(value.ty) && self.cx.cx.is_float(field_ty))
                || self.cx.types_compatible(field_ty, value.ty)
            {
                // A literal takes the declared field type, so that no inference
                // variable survives into MIR.
                let value = self.coerce_literal(value, field_ty);
                inits.push(HirFieldInit {
                    name: f.name.clone(),
                    span: f.name_span,
                    index: field_index,
                    value,
                });
            } else {
                let want = self.cx.ty_name(field_ty);
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

        // The value's type carries the instantiation, so every later phase sees
        // `Vec<i64>` rather than a bare `Vec`, and layout can size the fields.
        // The expected type, when the context supplied one, names the instance
        // directly: `let v: Vec<i64> = Vec { .. }` needs no field to say what `T`
        // is. It is used only for parameters nothing else could infer.
        let hinted: Vec<crate::types::TyId> = match hint.map(|h| self.cx.cx.kind(h).clone()) {
            Some(crate::types::TyKind::Adt { def: d, args }) if d == def => args,
            _ => Vec::new(),
        };
        let mut args: Vec<crate::types::TyId> = Vec::with_capacity(info.generics.len());
        for (index, g) in info.generics.iter().enumerate() {
            match hinted
                .get(index)
                .copied()
                .filter(|t| !self.cx.cx.is_error(*t))
                .or_else(|| inferred.get(g).copied())
                // Inside the declaration's own `impl`, an uninstantiated literal is
                // written in terms of the block's parameters: `Vec { .. }` in
                // `impl<T> Vec<T>` is the `Vec<T>` being defined.
                .or_else(|| {
                    if self.generics.iter().any(|x| x == g) {
                        Some(self.cx.cx.param(g.clone()))
                    } else {
                        None
                    }
                }) {
                Some(t) => args.push(t),
                None => {
                    self.push(
                        Diagnostic::error(
                            "E1001",
                            format!("cannot infer the type parameter `{g}` of `{name}`"),
                        )
                        .at(span)
                        .with_cause("no field initialiser has a type that names this parameter")
                        .with_explanation(
                            "A struct literal infers its type arguments from the values it is \
                             given. A parameter that no supplied field mentions cannot be \
                             inferred, and guessing a default would silently pick the wrong type.",
                        ),
                    );
                    args.push(self.cx.cx.error());
                }
            }
        }
        let ty = self.cx.cx.adt(def, args);
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
    /// `thread_spawn<A, R>(work, arg)`: moves `arg` to a thread and returns a
    /// handle whose join answers `R`.
    ///
    /// The type arguments are written because nothing else in the call names them:
    /// `A` is the value crossing the boundary and `R` is what the thread answers.
    /// Requiring them is what makes the `Send` check possible at all -- by the
    /// time the argument reaches the thread it would otherwise be a `*mut u8`,
    /// with the type already gone.
    fn check_thread_spawn(
        &mut self,
        path: &ast::Path,
        args: &[ast::Expr],
        span: crate::source::Span,
        hint: Option<crate::types::TyId>,
    ) -> HirExpr {
        let mut ty_args: Vec<TyId> = path.segments[0]
            .args
            .iter()
            .map(|a| {
                let generics = self.generics.clone();
                self.cx.resolve_type(a, &generics)
            })
            .collect();
        if ty_args.len() != 2 {
            self.push(
                Diagnostic::error(
                    "E1004",
                    format!(
                        "`thread_spawn` takes 2 type arguments, found {}",
                        ty_args.len()
                    ),
                )
                .at(path.span)
                .with_cause("the value crossing the boundary and the result are both unnameable otherwise")
                .with_suggestion(Suggestion::new(
                    "write both type arguments",
                    path.span,
                    "thread_spawn::<A, R>(..)",
                )),
            );
            return self.error_hir(span);
        }
        let result_ty = ty_args.pop().unwrap_or_else(|| self.cx.cx.error());
        let value_ty = ty_args.pop().unwrap_or_else(|| self.cx.cx.error());
        let arg_hirs: Vec<HirExpr> = args
            .iter()
            .enumerate()
            .map(|(i, a)| {
                // The first argument is the value; the rest belong to the callee
                // and are checked by its own signature.
                let expected = if i == 1 { Some(value_ty) } else { None };
                self.check_expr_hinted(a, expected)
            })
            .collect();
        if arg_hirs.len() != 2 {
            self.push(
                Diagnostic::error(
                    "E1004",
                    format!(
                        "`thread_spawn` takes 2 arguments, but {} were supplied",
                        arg_hirs.len()
                    ),
                )
                .at(span)
                .with_cause("a spawn takes the work to run and the value it is given"),
            );
            return self.error_hir(span);
        }
        // The value crossing the boundary is the one the marker governs.
        self.require_send(value_ty, arg_hirs[1].span, "the value handed to `thread_spawn`");
        let callee = HirExpr {
            kind: HirExprKind::NamedFn("umay_thread_spawn".to_string()),
            ty: self.cx.cx.fn_ptr(Vec::new(), result_ty),
            span: path.span,
        };
        HirExpr {
            kind: HirExprKind::Call { callee: Box::new(callee), args: arg_hirs },
            ty: result_ty,
            span,
        }
    }

    /// Reports `E1016` unless `ty` carries the `Send` marker.
    ///
    /// The answer is read from the `impl Send for ..` blocks the standard library
    /// and the program write, never inferred: a guess here would be a memory bug
    /// introduced by the compiler rather than prevented by it.
    pub(crate) fn require_send(&mut self, ty: TyId, span: crate::source::Span, what: &str) {
        if self.cx.cx.is_error(ty) || self.cx.cx.is_infer_var(ty) {
            return;
        }
        if self.is_send(ty) {
            return;
        }
        // A borrow is refused for a different reason than a value is, and the
        // marker that answers for it is `Sync`. Naming the wrong one would send
        // the reader looking for an `impl` that would not help.
        let borrowed = match self.cx.cx.kind(ty).clone() {
            TyKind::Ref(inner) => Some(inner),
            _ => None,
        };
        let subject = borrowed.unwrap_or(ty);
        let name = self.cx.ty_name(subject);
        let marker = if borrowed.is_some() { "Sync" } else { "Send" };
        let mut d = Diagnostic::error("E1016", format!("`{name}` does not implement `{marker}`"))
            .at(span);
        d = if borrowed.is_some() {
            d.with_cause(format!(
                "{what} is a borrow that crosses to another thread, and two threads may then \
                 read the same value at once"
            ))
            .with_explanation(
                "`Send` says a value may move to another thread; `Sync` says it may be \
                 shared with one. A borrow that crosses the boundary is shared, so the \
                 value behind it has to say it is safe for that. Writing \
                 `impl Sync for Type {}` is how a type answers yes, and nothing is \
                 inferred: forgetting the `impl` is an error, which is the safe direction.",
            )
        } else {
            d.with_cause(format!("{what} is moved to another thread, and the marker says whether it may be"))
            .with_explanation(
                "Ownership and borrows are statements about one thread; a value that moves \
                 to another escapes them, so the language asks the type to say it is safe. \
                 Writing `impl Send for Type {}` is how a type answers yes, and nothing is \
                 inferred: forgetting the `impl` is an error, which is the safe direction.",
            )
        };
        if let Some(ty_name) = self.nominal_name(subject) {
            d = d.with_suggestion(Suggestion::new(
                format!("add `impl {marker} for {ty_name} {{}}`"),
                span,
                format!("impl {marker} for {ty_name} {{}}"),
            ));
        }
        self.push(d);
    }

    /// True when `ty` carries the `Send` marker.
    pub(crate) fn is_send(&mut self, ty: TyId) -> bool {
        if self.cx.cx.is_error(ty) {
            return true;
        }
        // A parameter carries whatever its bound promised -- and only what *its*
        // bound promised. Every generic function names its parameter `T`, so a
        // lookup by name alone lets one function's `T: Send` answer for another's
        // `T`, which would let a value that is not `Send` cross a thread boundary
        // simply by being passed through a forwarder that never claimed it could.
        if let TyKind::Param { name } = self.cx.cx.kind(ty).clone() {
            return self.cx.cx.param_bounds.iter().any(|b| {
                if b.param != name || b.trait_name != "Send" {
                    return false;
                }
                // A bound belongs to the declaration that wrote it. Two functions
                // may each have a `T: Send` or, more to the point, one may have it
                // and another may not, so the owner is what keeps them apart.
                // The body being checked is the one the checker was made for,
                // not whatever function the last signature happened to leave in
                // `current_fn`: a bound is written by one declaration and read by
                // that declaration's body, and the two must agree.
                let here = self.fn_id;
                match here {
                    Some(id) => {
                        let name = self.cx.hir.fns.iter().find(|f| f.id == id).map(|f| f.name.as_str());
                        match name {
                            Some(name) => b.owner_name == name,
                            None => true,
                        }
                    }
                    None => b.owner == None,
                }
            });
        }
        let Some(impls) = self
            .cx
            .hir
            .traits
            .iter()
            .find(|t| t.name == "Send")
            .map(|t| t.impls.clone())
        else {
            return false;
        };
        match self.cx.cx.kind(ty).clone() {
            TyKind::Adt { def, args } => {
                let name = self.cx.hir.def_name(def);
                let Some(info) = impls.iter().find(|i| i.type_name == name) else {
                    return false;
                };
                // `impl<T: Send> Send for Vec<T>` promises nothing until every
                // argument is itself `Send`.
                if info.bounded_all {
                    return args.iter().all(|a| self.is_send(*a));
                }
                true
            }
            TyKind::Ref(inner) => {
                // A shared borrow may cross exactly when the value behind it is
                // safe to hold in two places at once, which is what `Sync` says.
                // The borrow itself is not what is at risk; the value is.
                self.is_sync(inner)
            }
            TyKind::RawPtr(_) | TyKind::RawPtrMut(_) => {
                // A raw pointer carries no guarantee at all, and no checker will
                // ever look at it again, so it is refused rather than assumed.
                false
            }
            _ => {
                let name = self.cx.ty_name(ty);
                impls.iter().any(|i| i.type_name == name)
            }
        }
    }

    /// True when a value of `ty` may be held by two threads at once.
    ///
    /// This is the question a shared borrow raises: `&T` is only safe to send if
    /// `T` is `Sync`, because two threads would otherwise read and write the same
    /// memory. `Send` answers whether a value may *move*; this answers whether it
    /// may be *shared*, and the two are separate promises a type makes.
    pub(crate) fn is_sync(&mut self, ty: TyId) -> bool {
        if self.cx.cx.is_error(ty) {
            return true;
        }
        // As with `Send`, a parameter carries only what its own declaration's
        // bound promised, keyed by the function's name so that one function's
        // `T: Sync` cannot answer for another's `T`.
        if let TyKind::Param { name } = self.cx.cx.kind(ty).clone() {
            return self.cx.cx.param_bounds.iter().any(|b| {
                if b.param != name || b.trait_name != "Sync" {
                    return false;
                }
                match self.fn_id {
                    Some(id) => {
                        let here = self.cx.hir.fns.iter().find(|f| f.id == id).map(|f| f.name.as_str());
                        match here {
                            Some(name) => b.owner_name == name,
                            None => true,
                        }
                    }
                    None => b.owner == None,
                }
            });
        }
        let Some(impls) = self
            .cx
            .hir
            .traits
            .iter()
            .find(|t| t.name == "Sync")
            .map(|t| t.impls.clone())
        else {
            return false;
        };
        match self.cx.cx.kind(ty).clone() {
            TyKind::Adt { def, args } => {
                let name = self.cx.hir.def_name(def);
                let Some(info) = impls.iter().find(|i| i.type_name == name) else {
                    return false;
                };
                if info.bounded_all {
                    return args.iter().all(|a| self.is_sync(*a));
                }
                true
            }
            TyKind::Ref(inner) => self.is_send(inner),
            TyKind::RawPtr(_) | TyKind::RawPtrMut(_) => false,
            _ => {
                let name = self.cx.ty_name(ty);
                impls.iter().any(|i| i.type_name == name)
            }
        }
    }

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

/// Fills `fixed` in from one declared/actual type pair.
///
/// The same walk `mono` does to pick an instance, but here it is answering a
/// different question: which concrete type each of the declaration's parameters
/// was used at, so that the bound written against that parameter can be checked.
fn unify_bound(
    types: &mut crate::types::TyCtxt,
    declared: TyId,
    actual: TyId,
    fixed: &mut std::collections::HashMap<String, TyId>,
) {
    match (types.kind(declared).clone(), types.kind(actual).clone()) {
        (TyKind::Param { name }, _) => {
            fixed.entry(name).or_insert(actual);
        }
        (TyKind::Ref(d), TyKind::Ref(a))
        | (TyKind::RefMut(d), TyKind::RefMut(a))
        | (TyKind::RawPtr(d), TyKind::RawPtr(a))
        | (TyKind::RawPtrMut(d), TyKind::RawPtrMut(a))
        | (TyKind::Slice(d), TyKind::Slice(a)) => unify_bound(types, d, a, fixed),
        (TyKind::Ref(d), _) | (TyKind::RefMut(d), _) => unify_bound(types, d, actual, fixed),
        (TyKind::Adt { def: d1, args: d_args }, TyKind::Adt { def: d2, args: a_args }) if d1 == d2 => {
            for (d, a) in d_args.iter().zip(a_args.iter()) {
                unify_bound(types, *d, *a, fixed);
            }
        }
        (TyKind::Array { elem: d, .. }, TyKind::Array { elem: a, .. }) => {
            unify_bound(types, d, a, fixed)
        }
        _ => {}
    }
}
