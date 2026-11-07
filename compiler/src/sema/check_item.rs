//! Item collection and signature resolution.
//!
//! Pass 1 gathers names so that declaration order does not matter. Pass 2
//! resolves field/enum/signature types to interned types. Pass 3 checks bodies.
//!
//! Structs and enums are registered in two stages because a type may refer to
//! itself or to a later declaration:
//!
//! * first the `DefId` and name exist (so `struct Node { next: Option<Node> }`
//!   resolves),
//! * then fields are resolved.

use std::collections::HashMap;

use crate::ast;
use crate::diagnostics::{Diagnostic, Suggestion};
use crate::hir::{
    ConstId, DefId, DefInfo, DefKind, EnumInfo, FieldInfo, FnId, HirConst, HirFn,
    StructInfo, TraitInfo, TraitMethodInfo, VariantFields, VariantInfo,
};
use crate::sema::scope::{ItemEntry, ItemKind};
use crate::sema::Checker;
use crate::source::Span;
use crate::types::{TyId, TyKind};

impl<'a> Checker<'a> {
    /// Registers every item name. No type resolution happens here.
    pub(crate) fn collect_items(&mut self, program: &ast::Program, _order: &crate::sema::DeclOrder) {
        self.collect_items_in(&program.items);
    }

    /// The next unused type definition id.
    ///
    /// Structs and enums must not collide: a size or a layout is looked up by
    /// this id alone, so two type definitions must never share one. Counting
    /// both lists together keeps the ids unique across the two.
    fn next_def_id(&self) -> u32 {
        (self.hir.structs.len() + self.hir.enums.len()) as u32
    }

    fn collect_items_in(&mut self, items: &[ast::Item]) {
        // ---- types ----
        for item in items {
            match item {
                ast::Item::Struct(s) => {
                    // Structs and enums share one `DefId` space: a size or a layout
                    // is looked up by that id alone, so two definitions must never
                    // carry the same one. The definition list holds both, which
                    // makes its length the shared counter.
                    let def = DefId(self.next_def_id());
                    self.register_type_name(&s.name, s.name_span, def, ItemKind::Struct, DefKind::Struct);
                    self.hir.structs.push(StructInfo {
                        name: s.name.clone(),
                        span: s.name_span,
                        def_id: def,
                        generics: s.generics.iter().map(|g| g.name.clone()).collect(),
                        fields: Vec::new(),
                        is_copy: has_derive(&s.attrs, "Copy"),
                    });
                    self.hir.defs.push(DefInfo {
                        name: s.name.clone(),
                        span: s.name_span,
                        def_id: def,
                        kind: DefKind::Struct,
                        is_pub: true,
                    });
                    self.def_by_name.insert(s.name.clone(), def);
                }
                ast::Item::Enum(e) => {
                    let def = DefId(self.next_def_id());
                    self.register_type_name(&e.name, e.name_span, def, ItemKind::Enum, DefKind::Enum);
                    self.hir.enums.push(EnumInfo {
                        name: e.name.clone(),
                        span: e.name_span,
                        def_id: def,
                        generics: e.generics.iter().map(|g| g.name.clone()).collect(),
                        variants: Vec::new(),
                    });
                    self.hir.defs.push(DefInfo {
                        name: e.name.clone(),
                        span: e.name_span,
                        def_id: def,
                        kind: DefKind::Enum,
                        is_pub: true,
                    });
                    self.def_by_name.insert(e.name.clone(), def);
                }
                _ => {}
            }
        }

        // Duplicate type names are detected before the enums' variant names are
        // registered, so that a variant cannot silently shadow a struct.
        for item in items {
            if let ast::Item::Trait(t) = item {
                let def = DefId(self.next_def_id());
                // Traits get their own def space entry for diagnostics only.
                let entry = ItemEntry {
                    res: crate::hir::Res::Type(DefId(u32::MAX)),
                    kind: ItemKind::Trait,
                    span: t.name_span,
                    arity: 0,
                };
                if let Err(existing) = self.items.declare(t.name.clone(), entry) {
                    self.duplicate_name(&t.name, t.name_span, existing.span, existing.kind);
                } else {
                    self.traits.insert(t.name.clone(), t.clone());
                    self.hir.traits.push(TraitInfo {
                        name: t.name.clone(),
                        span: t.name_span,
                        generics: t.generics.iter().map(|g| g.name.clone()).collect(),
                        methods: Vec::new(),
                    });
                }
                let _ = def;
            }
        }

        // ---- functions (free functions) ----
        for item in items {
            if let ast::Item::Fn(f) = item {
                self.register_fn_name(&f.name, f.name_span, f.params.len());
            }
        }

        // ---- constants ----
        for item in items {
            if let ast::Item::Const(c) = item {
                let id = ConstId(self.hir.consts.len() as u32);
                let entry = ItemEntry {
                    res: crate::hir::Res::Const(id),
                    kind: ItemKind::Const,
                    span: c.name_span,
                    arity: 0,
                };
                if let Err(existing) = self.items.declare(c.name.clone(), entry) {
                    self.duplicate_name(&c.name, c.name_span, existing.span, existing.kind);
                } else {
                    self.const_by_name.insert(c.name.clone(), id);
                    self.hir.consts.push(HirConst {
                        name: c.name.clone(),
                        span: c.name_span,
                        ty: self.cx.error(),
                        value: crate::hir::HirExpr {
                            kind: crate::hir::HirExprKind::Error,
                            ty: self.cx.error(),
                            span: c.name_span,
                        },
                    });
                    self.pending_consts.push((id, c.clone()));
                }
            }
        }

        // ---- impl blocks: methods ----
        for item in items {
            if let ast::Item::Impl(block) = item {
                self.collect_impl(block);
            }
        }

        // ---- nested modules ----
        for item in items {
            if let ast::Item::Mod(m) = item {
                if let Some(inner) = &m.items {
                    self.collect_items_in(inner);
                }
            }
        }
    }

    fn register_type_name(
        &mut self,
        name: &str,
        span: Span,
        def: DefId,
        kind: ItemKind,
        _def_kind: DefKind,
    ) {
        let entry = ItemEntry { res: crate::hir::Res::Type(def), kind, span, arity: 0 };
        if let Err(existing) = self.items.declare(name.to_string(), entry) {
            self.duplicate_name(name, span, existing.span, existing.kind);
        }
    }

    fn register_fn_name(&mut self, name: &str, span: Span, arity: usize) -> FnId {
        let id = FnId(self.hir.fns.len() as u32);
        let entry = ItemEntry {
            res: crate::hir::Res::Fn(id),
            kind: ItemKind::Fn,
            span,
            arity,
        };
        if let Err(existing) = self.items.declare(name.to_string(), entry) {
            self.duplicate_name(name, span, existing.span, existing.kind);
            return id;
        }
        self.fn_by_name.insert(name.to_string(), id);
        self.hir.fns.push(HirFn {
            id,
            name: name.to_string(),
            span,
            generics: Vec::new(),
            params: Vec::new(),
            ret: self.cx.unit(),
            has_self: false,
            self_mutable: false,
            self_by_ref: false,
            locals: Vec::new(),
            body: None,
            is_pub: true,
            owner: None,
        });
        id
    }

    fn duplicate_name(&mut self, name: &str, span: Span, first_span: Span, first_kind: ItemKind) {
        self.push(
            Diagnostic::error("E1010", format!("`{name}` is defined more than once"))
                .at(span)
                .with_label(first_span, format!("first defined here as a {}", first_kind.describe()))
                .with_cause("names in the same scope must be unique"),
        );
    }

    fn collect_impl(&mut self, block: &ast::ImplBlock) {
        let Some(self_ty_name) = type_name_of(&block.self_ty) else {
            return;
        };
        for method in &block.methods {
            let id = FnId(self.hir.fns.len() as u32);
            self.methods.insert((self_ty_name.clone(), method.name.clone()), id);
            self.methods_of_type.entry(self_ty_name.clone()).or_default().push(method.name.clone());
            self.hir.fns.push(HirFn {
                id,
                name: method.name.clone(),
                span: method.name_span,
                generics: Vec::new(),
                params: Vec::new(),
                ret: self.cx.unit(),
                has_self: method.self_param.is_some(),
                self_mutable: method.self_param.as_ref().map(|s| s.mutable).unwrap_or(false),
                self_by_ref: method.self_param.as_ref().map(|s| s.by_ref.is_some()).unwrap_or(false),
                locals: Vec::new(),
                body: None,
                is_pub: true,
                owner: Some(self_ty_name.clone()),
            });
            }
    }

    // -----------------------------------------------------------------------
    // Pass 2: signatures
    // -----------------------------------------------------------------------

    pub(crate) fn resolve_signatures(&mut self, program: &ast::Program) {
        self.resolve_signatures_in(&program.items);
        self.resolve_traits();
    }

    fn resolve_traits(&mut self) {
        let trait_names: Vec<String> = self.traits.keys().cloned().collect();
        let mut index = 0usize;
        for name in trait_names {
            let Some(decl) = self.traits.get(&name).cloned() else { continue };
            let mut methods = Vec::new();
            for m in &decl.methods {
                let params: Vec<TyId> =
                    m.params.iter().map(|p| self.resolve_type(&p.ty, &decl.generics.iter().map(|g| g.name.clone()).collect::<Vec<_>>())).collect();
                let ret = match &m.ret {
                    Some(t) => self.resolve_type(t, &decl.generics.iter().map(|g| g.name.clone()).collect::<Vec<_>>()),
                    None => self.cx.unit(),
                };
                methods.push(TraitMethodInfo { name: m.name.clone(), span: m.name_span, params, ret });
            }
            if let Some(info) = self.hir.traits.get_mut(index) {
                info.methods = methods;
            }
            index += 1;
        }
    }

    fn resolve_signatures_in(&mut self, items: &[ast::Item]) {
        for item in items {
            match item {
                ast::Item::Struct(s) => {
                    let generics: Vec<String> = s.generics.iter().map(|g| g.name.clone()).collect();
                    let def = self.def_by_name.get(&s.name).copied();
                    let mut fields = Vec::new();
                    for f in &s.fields {
                        let ty = self.resolve_type(&f.ty, &generics);
                        fields.push(FieldInfo { name: f.name.clone(), span: f.name_span, ty, ty_span: f.ty.span() });
                    }
                    if let Some(def) = def {
                        if let Some(info) = self.hir.structs.iter_mut().find(|x| x.def_id == def) {
                            info.fields = fields;
                        }
                    }
                }
                ast::Item::Enum(e) => {
                    let generics: Vec<String> = e.generics.iter().map(|g| g.name.clone()).collect();
                    let def = self.def_by_name.get(&e.name).copied();
                    let mut variants = Vec::new();
                    let mut seen: HashMap<String, Span> = HashMap::new();
                    for v in &e.variants {
                        if let Some(&first) = seen.get(&v.name) {
                            self.error("E1010", v.name_span, format!("variant `{}` is defined more than once", v.name));
                            self.error("E1010", first, "first defined here");
                        } else {
                            seen.insert(v.name.clone(), v.name_span);
                        }
                        let fields = if let Some(tys) = &v.tuple_fields {
                            VariantFields::Tuple(tys.iter().map(|t| self.resolve_type(t, &generics)).collect())
                        } else if let Some(fs) = &v.struct_fields {
                            VariantFields::Struct(
                                fs.iter()
                                    .map(|f| FieldInfo {
                                        name: f.name.clone(),
                                        span: f.name_span,
                                        ty: self.resolve_type(&f.ty, &generics),
                                        ty_span: f.ty.span(),
                                    })
                                    .collect(),
                            )
                        } else {
                            VariantFields::Unit
                        };
                        variants.push(VariantInfo { name: v.name.clone(), span: v.name_span, fields });
                    }
                    if let Some(def) = def {
                        if let Some(info) = self.hir.enums.iter_mut().find(|x| x.def_id == def) {
                            info.variants = variants;
                        }
                    }
                }
                ast::Item::Fn(f) => {
                    self.check_fn_decl_shape(f);
                    self.resolve_fn_signature(f, None, &[]);
                }
                ast::Item::Impl(block) => {
                    let self_ty_name = type_name_of(&block.self_ty);
                    // A method of a generic type may name the block's parameters
                    // in its own signature, so `impl<T> Vec<T> { fn new() -> Vec<T> }`
                    // resolves `T` against the block, not against the method.
                    let block_generics: Vec<String> =
                        block.generics.iter().map(|g| g.name.clone()).collect();
                    for m in &block.methods {
                        self.resolve_fn_signature(m, self_ty_name.as_deref(), &block_generics);
                    }
                }
                ast::Item::Mod(m) => {
                    if let Some(inner) = &m.items {
                        self.resolve_signatures_in(inner);
                    }
                }
                _ => {}
            }
        }
    }

    fn resolve_fn_signature(
        &mut self,
        f: &ast::FnDecl,
        receiver: Option<&str>,
        outer_generics: &[String],
    ) {
        let mut generics: Vec<String> = outer_generics.to_vec();
        generics.extend(f.generics.iter().map(|g| g.name.clone()));
        // A bound is what lets a method be called on a parameter at all, so it
        // is recorded where the parameter is declared rather than at the call.
        let bound_scope: Vec<String> = generics.clone();
        for g in &f.generics {
            for bound in &g.bounds {
                if let Some(seg) = bound.segments.last() {
                    let name = seg.name.clone();
                    let args: Vec<TyId> =
                        seg.args.iter().map(|a| self.resolve_type(a, &bound_scope)).collect();
                    let entry = crate::types::ParamBound { param: g.name.clone(), args, trait_name: name };
                    if !self.cx.param_bounds.iter().any(|b| b.param == entry.param && b.trait_name == entry.trait_name) {
                        self.cx.param_bounds.push(entry);
                    }
                }
            }
        }
        let mut params = Vec::new();
        // `self` receiver becomes parameter 0.
        let mut has_self = false;
        let mut self_mutable = false;
        let mut self_by_ref = false;
        if let Some(sp) = &f.self_param {
            has_self = true;
            self_mutable = sp.mutable;
            self_by_ref = sp.by_ref.is_some();
            let ty = match receiver.and_then(|r| self.def_by_name.get(r).copied()) {
                Some(def) => {
                    // The receiver carries the impl block's parameters, so a
                    // method of `impl<T> Box<T>` has `&Box<T>` as its `self`.
                    // Leaving the arguments off would make the body's `T`
                    // invisible to every later check.
                    let args: Vec<TyId> =
                        outer_generics.iter().map(|g| self.cx.param(g.clone())).collect();
                    let base = self.cx.adt(def, args);
                    if self_by_ref {
                        if self_mutable {
                            self.cx.ref_mut_to(base)
                        } else {
                            self.cx.ref_to(base)
                        }
                    } else {
                        base
                    }
                }
                None => self.cx.error(),
            };
            params.push(ty);
        }
        for p in &f.params {
            params.push(self.resolve_type(&p.ty, &generics));
        }
        let ret = match &f.ret {
            Some(t) => self.resolve_type(t, &generics),
            None => self.cx.unit(),
        };
        // Find the HirFn slot: free function by name, method by (type, name).
        let slot = match receiver {
            Some(ty) => self.methods.get(&(ty.to_string(), f.name.clone())).copied(),
            None => self.fn_by_name.get(&f.name).copied(),
        };
        if let Some(id) = slot {
            if let Some(info) = self.hir.fns.iter_mut().find(|x| x.id == id) {
                info.generics = generics;
                info.params = params;
                info.ret = ret;
                info.has_self = has_self;
                info.self_mutable = self_mutable;
                info.self_by_ref = self_by_ref;
            }
        }
    }

    /// Resolves an AST type expression to an interned type.
    ///
    /// Unknown names produce `E1003` and resolve to `Error` so the rest of the
    /// function is still checked.
    pub(crate) fn resolve_type(&mut self, ty: &ast::TypeExpr, generics: &[String]) -> TyId {
        match ty {
            ast::TypeExpr::Path(path) => {
                if path.segments.len() == 1 && !path.segments[0].args.is_empty() {
                    // Generic type application: `Option<Node>`, `Result<T, E>`.
                    let seg = &path.segments[0];
                    let args: Vec<TyId> = seg.args.iter().map(|a| self.resolve_type(a, generics)).collect();
                    if let Some(def) = self.def_by_name.get(&seg.name).copied() {
                        return self.cx.adt(def, args);
                    }
                    self.unknown_type(&seg.name, seg.span);
                    return self.cx.error();
                }
                if path.segments.len() == 1 && path.segments[0].args.is_empty() {
                    let name = &path.segments[0].name;
                    if generics.iter().any(|g| g == name) {
                        return self.cx.param(name.clone());
                    }
                    if name == "Self" {
                        return self.cx.error();
                    }
                    if let Some(def) = self.def_by_name.get(name).copied() {
                        return self.cx.adt(def, Vec::new());
                    }
                    if let Some(prim) = self.cx.primitive(name) {
                        return prim;
                    }
                }
                // Unknown or generic path.
                if let Some(seg) = path.segments.last() {
                    let args: Vec<TyId> =
                        seg.args.iter().map(|a| self.resolve_type(a, generics)).collect();
                    if let Some(def) = self.def_by_name.get(&seg.name).copied() {
                        return self.cx.adt(def, args);
                    }
                }
                let display = path.as_string();
                self.unknown_type(&display, path.span);
                self.cx.error()
            }
            ast::TypeExpr::Ref { mutable, inner, .. } => {
                let inner = self.resolve_type(inner, generics);
                if *mutable {
                    self.cx.ref_mut_to(inner)
                } else {
                    self.cx.ref_to(inner)
                }
            }
            ast::TypeExpr::Ptr { mutable, inner, .. } => {
                let inner = self.resolve_type(inner, generics);
                if *mutable {
                    self.cx.raw_ptr_mut(inner)
                } else {
                    self.cx.raw_ptr(inner)
                }
            }
            ast::TypeExpr::Array { elem, len, span } => {
                let elem = self.resolve_type(elem, generics);
                match const_int_value(len) {
                    Some(n) => self.cx.array(elem, n),
                    None => {
                        self.error("E1001", *span, "array length must be an integer constant");
                        self.cx.error()
                    }
                }
            }
            ast::TypeExpr::Slice { elem, .. } => {
                let elem = self.resolve_type(elem, generics);
                self.cx.slice(elem)
            }
            ast::TypeExpr::Tuple { elems, .. } => {
                let elems: Vec<TyId> = elems.iter().map(|e| self.resolve_type(e, generics)).collect();
                self.cx.tuple(elems)
            }
            ast::TypeExpr::Fn { params, ret, .. } => {
                let params: Vec<TyId> = params.iter().map(|p| self.resolve_type(p, generics)).collect();
                let ret = match ret {
                    Some(r) => self.resolve_type(r, generics),
                    None => self.cx.unit(),
                };
                self.cx.fn_ptr(params, ret)
            }
            ast::TypeExpr::ImplTrait { span, .. } | ast::TypeExpr::DynTrait { span, .. } => {
                self.error("E1003", *span, "`impl Trait` and `dyn Trait` are not supported yet");
                self.cx.error()
            }
            ast::TypeExpr::Infer { span } => {
                self.error("E1002", *span, "`_` is not allowed in a type position here");
                self.cx.error()
            }
            ast::TypeExpr::Never { .. } => self.cx.never(),
        }
    }

    fn unknown_type(&mut self, name: &str, span: Span) {
        let candidates = self
            .items
            .all_names()
            .into_iter()
            .chain(std::iter::once("i32"))
            .chain(std::iter::once("String"));
        let mut d = Diagnostic::error("E1003", format!("unknown type `{name}`")).at(span);
        d = d.with_cause("no struct, enum, trait or type alias with this name is in scope");
        if let Some(sugg) = crate::sema::scope::suggest_name(name, candidates) {
            d = d.with_suggestion(Suggestion::new(
                format!("did you mean `{sugg}`?"),
                span,
                sugg.clone(),
            ));
        }
        self.push(d);
    }

    // -----------------------------------------------------------------------
    // Pass 3: constants
    // -----------------------------------------------------------------------

    pub(crate) fn check_consts(&mut self) {
        let pending = std::mem::take(&mut self.pending_consts);
        for (id, decl) in pending {
            let generics: Vec<String> = Vec::new();
            let declared_ty = decl.ty.as_ref().map(|t| self.resolve_type(t, &generics));
            let (value, ty) = {
                let mut fcx = self.fn_context(None);
                let v = fcx.check_expr(&decl.value);
                let vty = v.ty;
                (v, declared_ty.unwrap_or(vty))
            };
            if let Some(declared) = declared_ty {
                if !self.cx.is_error(ty) && !self.cx.is_error(value.ty) && !self.types_compatible(declared, value.ty) {
                    let msg = format!(
                        "constant `{}` has type {}, but its initializer has type {}",
                        decl.name,
                        self.ty_name(declared),
                        self.ty_name(value.ty)
                    );
                    self.error("E1001", decl.value.span, msg);
                }
            }
            let idx = id.0 as usize;
            if idx < self.hir.consts.len() {
                self.hir.consts[idx].ty = ty;
                self.hir.consts[idx].value = value;
            }
        }
    }

    /// Structural type compatibility used before full unification exists.
    pub(crate) fn types_compatible(&mut self, a: TyId, b: TyId) -> bool {
        if a == b {
            return true;
        }
        if self.cx.is_error(a) || self.cx.is_error(b) {
            return true;
        }
        // Literal variables unify with any numeric type.
        if self.cx.is_integer_var(a) && self.cx.is_integer(b) {
            return true;
        }
        if self.cx.is_integer_var(b) && self.cx.is_integer(a) {
            return true;
        }
        if self.cx.is_float_var(a) && self.cx.is_float(b) {
            return true;
        }
        if self.cx.is_float_var(b) && self.cx.is_float(a) {
            return true;
        }
        if self.cx.is_integer_var(a) && self.cx.is_integer_var(b) {
            return true;
        }
        if self.cx.is_float_var(a) && self.cx.is_float_var(b) {
            return true;
        }
        // A declaration's `T` is not a type a value can have; it stands for
        // whatever the use site chose. Comparing it with the chosen type is
        // therefore always acceptable, and the instantiation is what makes the
        // two sides agree.
        if matches!(self.cx.kind(a), TyKind::Param { .. })
            || matches!(self.cx.kind(b), TyKind::Param { .. })
        {
            return true;
        }
        match (self.cx.kind(a).clone(), self.cx.kind(b).clone()) {
            (TyKind::Tuple(x), TyKind::Tuple(y)) => {
                x.len() == y.len() && x.iter().zip(&y).all(|(&p, &q)| self.types_compatible(p, q))
            }
            (TyKind::Array { elem: e1, len: l1 }, TyKind::Array { elem: e2, len: l2 }) => {
                l1 == l2 && self.types_compatible(e1, e2)
            }
            (TyKind::Slice(e1), TyKind::Slice(e2)) => self.types_compatible(e1, e2),
            (TyKind::Ref(e1), TyKind::Ref(e2)) => self.types_compatible(e1, e2),
            // A `*mut T` may be passed where a `*const T` is expected. Both are
            // one address with the same layout, and the callee only gains the
            // promise not to write, so no conversion is needed at the call; this
            // is the same reasoning that lets the `as` cast work without
            // `unsafe`.
            (TyKind::RawPtr(e1), TyKind::RawPtr(e2)) => self.types_compatible(e1, e2),
            (TyKind::RawPtr(e1), TyKind::RawPtrMut(e2)) => self.types_compatible(e1, e2),
            // A `&str` may be passed where a `*const u8` is expected. A string
            // literal is already a pointer to NUL-terminated bytes with the same
            // layout, so `print("...")` needs no cast; requiring one would make
            // the prelude's most common call the ugliest line in a program.
            (TyKind::RawPtr(inner), TyKind::Ref(e)) if matches!(self.cx.kind(inner), TyKind::U8) => {
                matches!(self.cx.kind(e), TyKind::Str)
            }
            (TyKind::RefMut(e1), TyKind::RefMut(e2)) => self.types_compatible(e1, e2),
            // A declaration without arguments is a nominal skeleton: `impl<T> Box<T>`
            // writes its receiver as a bare `Box`, and any instantiation of the
            // same struct is the type it means.
            (TyKind::Adt { def: d1, args: a1 }, TyKind::Adt { def: d2, args: a2 }) => {
                d1 == d2
                    && (a1.is_empty()
                        || a2.is_empty()
                        || (a1.len() == a2.len()
                            && a1.iter().zip(&a2).all(|(&p, &q)| self.types_compatible(p, q))))
            }
            (TyKind::FnPtr { params: p1, ret: r1 }, TyKind::FnPtr { params: p2, ret: r2 }) => {
                p1.len() == p2.len()
                    && p1.iter().zip(&p2).all(|(&x, &y)| self.types_compatible(x, y))
                    && self.types_compatible(r1, r2)
            }
            _ => false,
        }
    }

    /// Creates a fresh function-level checking context.
    pub(crate) fn fn_context(&mut self, fn_id: Option<FnId>) -> crate::sema::check_expr::FnChecker<'_, 'a> {
        crate::sema::check_expr::FnChecker::new(self, fn_id)
    }

    // ---- body checking ----------------------------------------------------

    pub(crate) fn check_bodies(&mut self, program: &ast::Program) {
        let items = program.items.clone();
        self.check_bodies_in(&items);
    }

    /// Checks that a declaration's body and `extern` marker agree.
    ///
    /// A function with no body and no `extern` is almost always a forgotten body,
    /// so it is reported rather than silently treated as a declaration of a
    /// symbol that does not exist.
    fn check_fn_decl_shape(&mut self, f: &ast::FnDecl) {
        match (f.body.is_some(), f.is_extern) {
            (true, true) => {
                self.push(
                    Diagnostic::error("E0197", format!("`extern fn {}` must not have a body", f.name))
                        .at(f.name_span)
                        .with_cause("an `extern` declaration promises the function exists elsewhere")
                        .with_suggestion(Suggestion::new(
                            "remove `extern`",
                            f.extern_span.unwrap_or(f.fn_span),
                            String::new(),
                        )),
                );
            }
            (false, false) => {
                self.push(
                    Diagnostic::error("E0198", format!("function `{}` has no body", f.name))
                        .at(f.name_span)
                        .with_cause("a function that is only meant to be called must be declared `extern`")
                        .with_suggestion(Suggestion::new("insert `extern`", f.fn_span, "extern fn")),
                );
            }
            _ => {}
        }
    }

    pub(crate) fn check_bodies_in(&mut self, items: &[ast::Item]) {
        for item in items {
            match item {
                ast::Item::Fn(f) => {
                    if let Some(id) = self.fn_by_name.get(&f.name).copied() {
                        self.check_one_body(id, f, &[]);
                    }
                }
                ast::Item::Impl(block) => {
                    let ty_name = type_name_of(&block.self_ty);
                    // The block's parameters are in scope inside each method, so
                    // the body can see that `T` is `T`.
                    let block_generics: Vec<String> =
                        block.generics.iter().map(|g| g.name.clone()).collect();
                    for m in &block.methods {
                        let slot = ty_name.as_ref().and_then(|t| self.methods.get(&(t.clone(), m.name.clone())).copied());
                        if let Some(id) = slot {
                            self.check_one_body(id, m, &block_generics);
                        }
                    }
                }
                ast::Item::Mod(m) => {
                    if let Some(inner) = &m.items {
                        let inner = inner.clone();
                        self.check_bodies_in(&inner);
                    }
                }
                _ => {}
            }
        }
    }

    fn check_one_body(&mut self, id: FnId, f: &ast::FnDecl, generics: &[String]) {
        let Some(body) = &f.body else { return };
        let body = body.clone();
        let f = f.clone();
        let params: Vec<TyId> = self.hir.fns.iter().find(|x| x.id == id).map(|x| x.params.clone()).unwrap_or_default();
        let declared_ret = self.hir.fns.iter().find(|x| x.id == id).map(|x| x.ret).unwrap_or_else(|| self.cx.unit());
        let has_self = f.self_param.is_some();

        let mut fcx = self.fn_context(Some(id));
        // The body may name the declaration's parameters: inside `impl<T> Vec<T>`
        // a literal `Vec { .. }` means the `Vec<T>` being defined.
        fcx.generics.extend(generics.iter().cloned());
        fcx.generics.extend(f.generics.iter().map(|g| g.name.clone()));
        if has_self {
            let sp = f.self_param.as_ref().map(|s| s.span).unwrap_or(f.name_span);
            let ty = params.first().copied().unwrap_or_else(|| fcx.cx.cx.error());
            fcx.declare_param("self", ty, sp, f.self_param.as_ref().map(|s| s.mutable).unwrap_or(false));
        }
        for (i, p) in f.params.iter().enumerate() {
            let ty = params.get(i + if has_self { 1 } else { 0 }).copied().unwrap_or_else(|| fcx.cx.cx.error());
            let name = p.name.clone().unwrap_or_else(|| format!("_{i}"));
            fcx.declare_param(&name, ty, p.name_span, false);
        }
        fcx.ret_ty = declared_ret;
        let block = fcx.check_block(&body);
        let block_ty = fcx.block_ty(&block);

        // Return type check: the body must produce the declared type.
        if !fcx.cx.cx.is_error(declared_ret) && !fcx.cx.cx.is_error(block_ty) {
            let compatible = match (fcx.cx.cx.kind(declared_ret).clone(), fcx.cx.cx.kind(block_ty).clone()) {
                (TyKind::Unit, _) if block_reaches_end_without_value(&block) => true,
                // A body that diverges (`{ return 1 }`, `{ panic!() }`) never
                // produces a value, so it satisfies any return type.
                (_, TyKind::Never) => true,
                _ => fcx.cx.types_compatible(declared_ret, block_ty),
            };
            if !compatible {
                let msg = format!(
                    "function `{}` returns {}, but its body has type {}",
                    f.name,
                    fcx.cx.ty_name(declared_ret),
                    fcx.cx.ty_name(block_ty)
                );
                let d = Diagnostic::error("E1001", msg)
                    .at(block.span)
                    .with_cause("the body's value must match the declared return type")
                    .with_suggestion(Suggestion::new(
                        "add an explicit `return` or change the return type",
                        block.span,
                        String::new(),
                    ).with_detail("the replacement is empty because the fix depends on intent"));
                fcx.cx.push(d);
            }
        }

        let defaults = fcx.literal_defaults();
        let mut locals = fcx.finish_locals();
        let mut body_hir = block;
        // Rewrite unresolved literal types in the body so that lowering sees
        // concrete types everywhere.
        crate::hir::rewrite_types(&mut body_hir, &defaults);
        for l in &mut locals {
            if let Some(t) = defaults.get(&l.ty) {
                l.ty = *t;
            }
        }
        if let Some(info) = self.hir.fns.iter_mut().find(|x| x.id == id) {
            info.body = Some(body_hir);
            info.locals = locals;
        }
    }
}

/// True when every path through the block falls off the end (type `()`),
/// used to avoid a spurious "returns unit" error.
fn block_reaches_end_without_value(block: &crate::hir::HirBlock) -> bool {
    block.tail.is_none()
}

fn has_derive(attrs: &[ast::Attribute], name: &str) -> bool {
    attrs.iter().any(|a| a.name == "derive" && a.args.iter().any(|x| x == name))
}

/// Returns the simple type name of a self type, e.g. `Counter` for `Counter`.
fn type_name_of(ty: &ast::TypeExpr) -> Option<String> {
    match ty {
        ast::TypeExpr::Path(p) if p.segments.len() == 1 => Some(p.segments[0].name.clone()),
        _ => None,
    }
}

/// Evaluates an integer constant expression used as an array length.
fn const_int_value(e: &ast::Expr) -> Option<u64> {
    match &e.kind {
        ast::ExprKind::IntLit { value, .. } => u64::try_from(*value).ok(),
        _ => None,
    }
}
