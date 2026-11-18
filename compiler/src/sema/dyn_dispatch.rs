//! `dyn Trait` coercion and method dispatch.
//!
//! A trait object is a two-word fat pointer: a data pointer and the address of
//! a static vtable (ADR 0018). The vtable starts with a three-word prefix --
//! `{ size, align, drop }` -- followed by shim functions with one uniform ABI
//! `(data: *mut u8, args...) -> ret`, so an indirect call needs no new MIR
//! terminator and no backend-specific calling convention. Each shim casts the
//! data pointer back to the concrete type and calls the impl's method.

use crate::diagnostics::Diagnostic;
use crate::hir::*;
use crate::sema::check_expr::FnChecker;
use crate::sema::Checker;
use crate::source::Span;
use crate::types::{DefId, TyId, TyKind};

/// Byte offset of the first method shim in a vtable: three prefix words.
const METHOD_SLOT_BASE: u128 = 24;

impl<'c, 's> FnChecker<'c, 's> {
    /// Wraps a `&Concrete` into a borrowed `dyn Trait`, or a concrete value into
    /// an owned one. The latter allocates the value and keeps it alive behind a
    /// compiler-generated, non-`Copy` struct so drop elaboration destroys it.
    pub(crate) fn coerce_to_dyn(&mut self, value: HirExpr, dyn_ty: TyId, span: Span) -> HirExpr {
        let TyKind::DynTrait { bounds } = self.cx.cx.kind(dyn_ty).clone() else {
            return value;
        };
        let Some(trait_name) = bounds.first().map(|b| b.name.clone()) else {
            return value;
        };
        let base = self.strip_refs(value.ty);
        let TyKind::Adt { def, .. } = self.cx.cx.kind(base).clone() else {
            self.push(
                Diagnostic::error("E1014", "only a reference to a named type can become a trait object")
                    .at(span)
                    .with_cause("a `dyn` value needs a concrete implementor and its vtable"),
            );
            return value;
        };
        let type_name = self.cx.hir.def_name(def);
        let (size, align, drop, entries) =
            self.cx.build_dyn_vtable(&trait_name, &type_name, base, span);
        let u8 = self.cx.cx.primitive("u8").unwrap_or_else(|| self.cx.cx.error());
        let vtable_ty = self.cx.cx.raw_ptr(u8);
        let vtable = HirExpr {
            kind: HirExprKind::VTable { size, align, drop, entries },
            ty: vtable_ty,
            span: Span::DUMMY,
        };
        let borrowed = matches!(self.cx.cx.kind(value.ty), TyKind::Ref(_) | TyKind::RefMut(_));
        if borrowed {
            return HirExpr {
                kind: HirExprKind::DynCast { value: Box::new(value), vtable: Box::new(vtable) },
                ty: dyn_ty,
                span,
            };
        }
        self.owning_dyn(value, dyn_ty, base, size, vtable, span)
    }

    /// Allocates `value`, writes it through a cast pointer (the `template.um`
    /// pattern), stores the fat pointer in a non-`Copy` struct, and returns that
    /// struct's fat pointer. The struct local stays live until the enclosing
    /// function exits, so its `Drop` frees the allocation only after the object
    /// can no longer be called through.
    fn owning_dyn(
        &mut self,
        value: HirExpr,
        dyn_ty: TyId,
        base: TyId,
        size: u64,
        vtable: HirExpr,
        span: Span,
    ) -> HirExpr {
        let u8 = self.cx.cx.primitive("u8").unwrap_or_else(|| self.cx.cx.error());
        let data_ty = self.cx.cx.raw_ptr_mut(u8);
        let usize_ty = self.cx.cx.usize_ty();
        let unit = self.cx.cx.unit();

        // A fresh nominal type: ADTs are non-Copy until a derive says otherwise,
        // so drop elaboration gives its local a `Terminator::Drop`.
        let generated = self.cx.hir.structs.iter().filter(|s| s.name.starts_with("__umay_dyn_own_")).count();
        let name = format!("__umay_dyn_own_{generated}");
        let def = DefId((self.cx.hir.structs.len() + self.cx.hir.enums.len()) as u32);
        let own_ty = self.cx.cx.adt(def, Vec::new());
        self.cx.hir.structs.push(StructInfo {
            name: name.clone(),
            span,
            def_id: def,
            generics: Vec::new(),
            fields: vec![FieldInfo { name: "fat".into(), span, ty: dyn_ty, ty_span: span }],
            is_copy: false,
        });
        self.cx.hir.defs.push(DefInfo {
            name,
            span,
            def_id: def,
            kind: DefKind::Struct,
            is_pub: false,
        });

        let ptr_local = LocalId(self.locals.len() as u32);
        self.locals.push(LocalInfo { name: "__dyn_data".into(), ty: data_ty, mutable: false, span });
        self.local_tys.insert(ptr_local, data_ty);
        let own_local = LocalId(self.locals.len() as u32);
        self.locals.push(LocalInfo { name: "__dyn_own".into(), ty: own_ty, mutable: false, span });
        self.local_tys.insert(own_local, own_ty);

        let alloc = HirExpr {
            kind: HirExprKind::Call {
                callee: Box::new(HirExpr {
                    kind: HirExprKind::NamedFn("umay_alloc".into()),
                    ty: self.cx.cx.fn_ptr(vec![usize_ty], data_ty),
                    span,
                }),
                args: vec![HirExpr {
                    kind: HirExprKind::IntLit { value: size as u128, suffix: None },
                    ty: usize_ty,
                    span,
                }],
            },
            ty: data_ty,
            span,
        };
        let dest_ty = self.cx.cx.raw_ptr_mut(base);
        let cast = HirExpr {
            kind: HirExprKind::Cast {
                expr: Box::new(HirExpr { kind: HirExprKind::Local(ptr_local), ty: data_ty, span }),
                to: dest_ty,
            },
            ty: dest_ty,
            span,
        };
        let write = HirExpr {
            kind: HirExprKind::Assign {
                place: Box::new(HirExpr { kind: HirExprKind::Deref(Box::new(cast)), ty: base, span }),
                value: Box::new(value),
            },
            ty: unit,
            span,
        };
        let fat = HirExpr {
            kind: HirExprKind::DynCast {
                value: Box::new(HirExpr { kind: HirExprKind::Local(ptr_local), ty: data_ty, span }),
                vtable: Box::new(vtable),
            },
            ty: dyn_ty,
            span,
        };
        let own_lit = HirExpr {
            kind: HirExprKind::StructLit {
                def,
                fields: vec![HirFieldInit { name: "fat".into(), span, index: 0, value: fat }],
                base: None,
            },
            ty: own_ty,
            span,
        };
        let binding = |local: LocalId, ty: TyId| HirPattern {
            kind: HirPatternKind::Binding { local, mutable: false },
            ty,
            span,
        };
        HirExpr {
            kind: HirExprKind::Block(HirBlock {
                stmts: vec![
                    HirStmt {
                        kind: HirStmtKind::Let {
                            bindings: vec![ptr_local],
                            init: Some(Box::new(alloc)),
                            pattern: binding(ptr_local, data_ty),
                        },
                        span,
                    },
                    HirStmt { kind: HirStmtKind::Expr(write), span },
                    HirStmt {
                        kind: HirStmtKind::Let {
                            bindings: vec![own_local],
                            init: Some(Box::new(own_lit)),
                            pattern: binding(own_local, own_ty),
                        },
                        span,
                    },
                ],
                tail: Some(Box::new(HirExpr {
                    kind: HirExprKind::Field {
                        base: Box::new(HirExpr { kind: HirExprKind::Local(own_local), ty: own_ty, span }),
                        index: 0,
                        name: "fat".into(),
                    },
                    ty: dyn_ty,
                    span,
                })),
                span,
                locals_introduced: 2,
            }),
            ty: dyn_ty,
            span,
        }
    }

    /// Resolves `receiver.method(args)` on a trait object into an indirect call
    /// through the corresponding vtable slot.
    pub(crate) fn check_dyn_method_call(
        &mut self,
        receiver: &HirExpr,
        method: &str,
        arg_hirs: Vec<HirExpr>,
        span: Span,
    ) -> Option<HirExpr> {
        let base = self.strip_refs(receiver.ty);
        let TyKind::DynTrait { bounds } = self.cx.cx.kind(base).clone() else {
            return None;
        };
        let trait_name = bounds.first()?.name.clone();
        let info = self.cx.hir.traits.iter().find(|t| t.name == trait_name)?.clone();
        let slot = info.methods.iter().position(|m| m.name == method)? as u128;
        let sig = info.methods[slot as usize].clone();

        let u8 = self.cx.cx.primitive("u8").unwrap_or_else(|| self.cx.cx.error());
        let data_ty = self.cx.cx.raw_ptr_mut(u8);
        let vtable_ty = self.cx.cx.raw_ptr(u8);
        let data = HirExpr {
            kind: HirExprKind::Field { base: Box::new(receiver.clone()), index: 0, name: "data".into() },
            ty: data_ty,
            span,
        };
        let vtable = HirExpr {
            kind: HirExprKind::Field { base: Box::new(receiver.clone()), index: 1, name: "vtable".into() },
            ty: vtable_ty,
            span,
        };

        let fn_ty = {
            let mut params = vec![data_ty];
            params.extend(sig.params.iter().copied());
            self.cx.cx.fn_ptr(params, sig.ret)
        };
        let usize_ty = self.cx.cx.primitive("usize").unwrap_or_else(|| self.cx.cx.error());
        let offset = HirExpr {
            kind: HirExprKind::IntLit { value: METHOD_SLOT_BASE + slot * 8, suffix: None },
            ty: usize_ty,
            span: Span::DUMMY,
        };
        // A `*const fn` already *is* a code address in UMAY, so a vtable slot
        // holding one needs two dereferences: the first loads the slot's value
        // from the static table, the second names the function (and is the one
        // the backend's `fn_ptr_place` folds away).
        let slot_value_ty = self.cx.cx.raw_ptr(fn_ty);
        let slot_ptr_ty = self.cx.cx.raw_ptr(slot_value_ty);
        // `PtrOffset` scales by the destination's pointee size, so the byte
        // count is computed on a byte pointer and then cast to the slot type.
        let byte_slot = HirExpr {
            kind: HirExprKind::PtrOffset { ptr: Box::new(vtable), count: Box::new(offset) },
            ty: vtable_ty,
            span: Span::DUMMY,
        };
        let slot_ptr = HirExpr {
            kind: HirExprKind::Cast { expr: Box::new(byte_slot), to: slot_ptr_ty },
            ty: slot_ptr_ty,
            span: Span::DUMMY,
        };
        let slot_value = HirExpr {
            kind: HirExprKind::Deref(Box::new(slot_ptr)),
            ty: slot_value_ty,
            span: Span::DUMMY,
        };
        let callee = HirExpr {
            kind: HirExprKind::Deref(Box::new(slot_value)),
            ty: fn_ty,
            span: Span::DUMMY,
        };
        let mut args = vec![data];
        args.extend(arg_hirs);
        Some(HirExpr {
            kind: HirExprKind::Call { callee: Box::new(callee), args },
            ty: sig.ret,
            span,
        })
    }
}

impl<'a> Checker<'a> {
    /// One vtable per (trait, concrete type), in trait declaration order.
    ///
    /// The first three words are `{ size, align, drop }`; what follows are shims,
    /// not the impl methods themselves: an impl method takes `&Self`/`Self`,
    /// while a slot must take an untyped data pointer. The shim casts that
    /// pointer back and forwards the call.
    #[allow(clippy::type_complexity)]
    pub(crate) fn build_dyn_vtable(
        &mut self,
        trait_name: &str,
        type_name: &str,
        concrete_ty: TyId,
        span: Span,
    ) -> (u64, u64, Option<FnId>, Vec<FnId>) {
        let Some(info) = self.hir.traits.iter().find(|t| t.name == trait_name).cloned() else {
            self.error("E1017", span, format!("no trait `{trait_name}` is declared"));
            return (0, 1, None, Vec::new());
        };
        let (size, align) = {
            let mut layouts = crate::layout::Layouts::new(&mut self.cx, &self.hir);
            match layouts.layout(concrete_ty) {
                Ok(layout) => (layout.size, layout.align()),
                Err(_) => (0, 1),
            }
        };
        let drop_name = format!("__umay_drop_{type_name}");
        let drop = if let Some(existing) = self.hir.fns.iter().find(|f| f.name == drop_name).map(|f| f.id) {
            Some(existing)
        } else {
            Some(self.make_drop_shim(&drop_name, span))
        };
        let mut entries = Vec::new();
        for method in &info.methods {
            // An impl method wins; a trait method with a default body supplies
            // the slot when the impl omits it. The default Fn is specialized for
            // this concrete type, so the shim's cast receiver matches it.
            let impl_fn = self
                .methods
                .get(&(type_name.to_string(), method.name.clone()))
                .copied()
                .or(method.default_fn);
            let Some(impl_fn) = impl_fn else {
                self.error(
                    "E1017",
                    span,
                    format!("`{type_name}` does not implement `{trait_name}::{}`", method.name),
                );
                continue;
            };
            let (has_self, self_by_ref, self_mutable, arg_tys, ret) =
                match self.hir.fns.iter().find(|f| f.id == impl_fn) {
                    Some(f) => {
                        let skip = usize::from(f.has_self);
                        (f.has_self, f.self_by_ref, f.self_mutable, f.params[skip..].to_vec(), f.ret)
                    }
                    None => (false, false, false, method.params.clone(), method.ret),
                };
            let _ = has_self;
            entries.push(self.make_dyn_shim(
                trait_name,
                type_name,
                &method.name,
                impl_fn,
                concrete_ty,
                &arg_tys,
                ret,
                self_by_ref,
                self_mutable,
            ));
        }
        (size, align, drop, entries)
    }

    /// `__umay_drop_<Type>(*mut u8)`: the deallocation half of an owned trait
    /// object. The data word handed to every shim is the original `umay_alloc`
    /// pointer, so releasing it is just a call to the runtime allocator.
    fn make_drop_shim(&mut self, name: &str, span: Span) -> FnId {
        let u8 = self.cx.primitive("u8").unwrap_or_else(|| self.cx.error());
        let data_ty = self.cx.raw_ptr_mut(u8);
        let unit = self.cx.unit();
        let fn_id = FnId(self.hir.fns.len() as u32);
        let call = HirExpr {
            kind: HirExprKind::Call {
                callee: Box::new(HirExpr {
                    kind: HirExprKind::NamedFn("umay_dealloc".into()),
                    ty: self.cx.fn_ptr(vec![data_ty], unit),
                    span,
                }),
                args: vec![HirExpr { kind: HirExprKind::Local(LocalId(0)), ty: data_ty, span }],
            },
            ty: unit,
            span,
        };
        self.hir.fns.push(HirFn {
            id: fn_id,
            name: name.to_string(),
            span,
            generics: Vec::new(),
            params: vec![data_ty],
            ret: unit,
            has_self: false,
            self_mutable: false,
            self_by_ref: false,
            locals: vec![LocalInfo { name: "__data".into(), ty: data_ty, mutable: false, span }],
            body: Some(HirBlock {
                stmts: Vec::new(),
                tail: Some(Box::new(call)),
                span,
                locals_introduced: 0,
            }),
            is_pub: false,
            owner: None,
            link_name: None,
            variadic: false,
            is_async: false,
            declared_local_tys: std::collections::HashMap::new(),
        });
        fn_id
    }

    #[allow(clippy::too_many_arguments)]
    fn make_dyn_shim(
        &mut self,
        trait_name: &str,
        type_name: &str,
        method: &str,
        impl_fn: FnId,
        concrete_ty: TyId,
        arg_tys: &[TyId],
        ret: TyId,
        self_by_ref: bool,
        self_mutable: bool,
    ) -> FnId {
        let u8 = self.cx.primitive("u8").unwrap_or_else(|| self.cx.error());
        let data_ty = self.cx.raw_ptr_mut(u8);
        let fn_id = FnId(self.hir.fns.len() as u32);
        let name = format!("__umay_dyn_{trait_name}_{type_name}_{method}");

        let mut params = vec![data_ty];
        params.extend_from_slice(arg_tys);
        let mut locals = vec![LocalInfo {
            name: "__data".into(),
            ty: data_ty,
            mutable: false,
            span: Span::DUMMY,
        }];
        for (i, ty) in arg_tys.iter().enumerate() {
            locals.push(LocalInfo {
                name: format!("__arg{i}"),
                ty: *ty,
                mutable: false,
                span: Span::DUMMY,
            });
        }
        let self_local = LocalId(locals.len() as u32);

        let cast_ty = self.cx.raw_ptr_mut(concrete_ty);
        let cast = HirExpr {
            kind: HirExprKind::Cast {
                expr: Box::new(HirExpr {
                    kind: HirExprKind::Local(LocalId(0)),
                    ty: data_ty,
                    span: Span::DUMMY,
                }),
                to: cast_ty,
            },
            ty: cast_ty,
            span: Span::DUMMY,
        };
        let deref = HirExpr {
            kind: HirExprKind::Deref(Box::new(cast)),
            ty: concrete_ty,
            span: Span::DUMMY,
        };
        let receiver = if self_by_ref {
            let ty = if self_mutable { self.cx.ref_mut_to(concrete_ty) } else { self.cx.ref_to(concrete_ty) };
            HirExpr {
                kind: HirExprKind::Ref { mutable: self_mutable, place: Box::new(deref) },
                ty,
                span: Span::DUMMY,
            }
        } else {
            deref
        };
        locals.push(LocalInfo {
            name: "__self".into(),
            ty: receiver.ty,
            mutable: self_mutable,
            span: Span::DUMMY,
        });

        let args = arg_tys
            .iter()
            .enumerate()
            .map(|(i, ty)| HirExpr {
                kind: HirExprKind::Local(LocalId(i as u32 + 1)),
                ty: *ty,
                span: Span::DUMMY,
            })
            .collect();
        let tail = HirExpr {
            kind: HirExprKind::MethodCall {
                receiver: Box::new(HirExpr {
                    kind: HirExprKind::Local(self_local),
                    ty: receiver.ty,
                    span: Span::DUMMY,
                }),
                method: impl_fn,
                args,
            },
            ty: ret,
            span: Span::DUMMY,
        };
        let body = HirBlock {
            stmts: vec![HirStmt {
                kind: HirStmtKind::Let {
                    bindings: vec![self_local],
                    init: Some(Box::new(receiver.clone())),
                    pattern: HirPattern {
                        kind: HirPatternKind::Binding { local: self_local, mutable: self_mutable },
                        ty: receiver.ty,
                        span: Span::DUMMY,
                    },
                },
                span: Span::DUMMY,
            }],
            tail: Some(Box::new(tail)),
            span: Span::DUMMY,
            locals_introduced: 1,
        };

        self.hir.fns.push(HirFn {
            id: fn_id,
            name,
            span: Span::DUMMY,
            generics: Vec::new(),
            params,
            ret,
            has_self: false,
            self_mutable: false,
            self_by_ref: false,
            locals,
            body: Some(body),
            is_pub: false,
            owner: None,
            link_name: None,
            variadic: false,
            is_async: false,
            declared_local_tys: std::collections::HashMap::new(),
        });
        fn_id
    }
}
