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
                    self.register_type_name(&s.name, s.name_span, def, ItemKind::Struct, DefKind::Struct, s.is_pub);
                    let is_copy = has_derive(&s.attrs, "Copy");
                    if is_copy {
                        // Bridge HIR `StructInfo.is_copy` into `TyCtxt` so that
                        // `is_copy` can answer for this ADT in every later phase.
                        self.cx.mark_copy(def);
                    }
                    self.hir.structs.push(StructInfo {
                        name: s.name.clone(),
                        span: s.name_span,
                        def_id: def,
                        generics: s.generics.iter().map(|g| g.name.clone()).collect(),
                        fields: Vec::new(),
                        is_copy,
                    });
                    self.hir.defs.push(DefInfo {
                        name: s.name.clone(),
                        span: s.name_span,
                        def_id: def,
                        kind: DefKind::Struct,
                        is_pub: s.is_pub,
                    });
                    // Keyed by the name the item is reachable under, so a type
                    // inside a module is found as `module::Name`.
                    self.def_by_name.insert(self.scoped_name(&s.name), def);
                    self.visibility.insert(self.scoped_name(&s.name), s.is_pub);
                }
                ast::Item::Enum(e) => {
                    let def = DefId(self.next_def_id());
                    self.register_type_name(&e.name, e.name_span, def, ItemKind::Enum, DefKind::Enum, e.is_pub);
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
                        is_pub: e.is_pub,
                    });
                    self.def_by_name.insert(self.scoped_name(&e.name), def);
                    self.visibility.insert(self.scoped_name(&e.name), e.is_pub);
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
                if let Err(existing) = self.declare_scoped(&t.name, entry, true) {
                    self.duplicate_name(&t.name, t.name_span, existing.span, existing.kind);
                } else {
                    self.traits.insert(t.name.clone(), t.clone());
                    self.hir.traits.push(TraitInfo {
                        name: t.name.clone(),
                        span: t.name_span,
                        generics: t.generics.iter().map(|g| g.name.clone()).collect(),
                        supertraits: Vec::new(),
                        impls: Vec::new(),
                        methods: Vec::new(),
                    });
                }
                let _ = def;
            }
        }

        // ---- functions (free functions) ----
        for item in items {
            if let ast::Item::Fn(f) = item {
                self.register_fn_name(&f.name, f.name_span, f.params.len(), f.is_pub);
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
                if let Err(existing) = self.declare_scoped(&c.name, entry, c.is_pub) {
                    self.duplicate_name(&c.name, c.name_span, existing.span, existing.kind);
                } else {
                    // Keyed the way the body pass will look it up, so a module's
                    // constant is found under `module::NAME`.
                    self.const_by_name.insert(self.scoped_name(&c.name), id);
                    self.visibility.insert(self.scoped_name(&c.name), c.is_pub);
                    self.hir.consts.push(HirConst {
                        // A module's constant is emitted under its qualified name,
                        // for the same reason a function is: two modules may each
                        // declare `LIMIT`, and the assembler has no scopes.
                        name: self.scoped_name(&c.name),
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
        //
        // A module's items are reachable two ways: by their bare name, which is
        // how an inline module has always worked and how the prelude is used,
        // and by `module::name`. Only the second needs the prefix, so both are
        // registered rather than moving everything behind a path.
        for item in items {
            if let ast::Item::Mod(m) = item {
                if let Some(inner) = &m.items {
                    self.collect_items_qualified(inner, &m.name);
                }
            }
        }
    }

    /// Collects the items of a module, registering each under `module::name` too.
    ///
    /// The names a module declares are exactly the names that were not in the
    /// table before it was collected, so the difference is taken rather than a
    /// list being threaded through every `collect_*` helper.
    fn collect_items_qualified(&mut self, items: &[ast::Item], module: &str) {
        let before: std::collections::HashSet<String> =
            self.items.all_names().into_iter().map(str::to_string).collect();
        // While this module's items are collected, names are registered already
        // qualified and not bare: `mod a;` and `mod b;` may both declare `run`,
        // and a bare `run` from either would be a duplicate of the other, or --
        // worse -- resolve to whichever was collected first.
        // A module inside a module keeps both names: `a::b::name`, not `b::name`,
        // so the prefix is composed with whatever encloses it.
        let qualified = match &self.module_prefix {
            Some(outer) => format!("{outer}::{module}"),
            None => module.to_string(),
        };
        let outer = self.module_prefix.replace(qualified);
        self.collect_items_in(items);
        self.module_prefix = outer;

        // The items are already registered under their qualified names by
        // `scoped_name`, which is set for the whole of `collect_items_in`. This
        // used to re-prefix the diff of the name set afterwards, which now that
        // registration qualifies as it goes would produce `module::module::name`.
        let _ = before;
    }

    /// The definition a type name refers to, under its qualified spelling.
    ///
    /// Fields and variants are attached to a definition found by name, and a
    /// module's type is only registered as `module::Name`. Looking up the bare
    /// name found nothing, so a module's enum was registered with no variants and
    /// every use of one was "no such variant".
    fn lookup_type_def(&self, name: &str) -> Option<DefId> {
        let key = self.scoped_name(name);
        self.def_by_name
            .get(&key)
            .or_else(|| self.def_by_name.get(name))
            .copied()
    }

    /// A name as it is registered: qualified when it belongs to a module, and
    /// also reachable by its bare name from inside that module when it is `pub`.
    ///
    /// A private item must NOT get a bare entry in the global item scope: that
    /// entry let any other module call `m::helper` simply as `helper()`, which
    /// made `pub` enforcement trivial to bypass. Inside its own module a bare
    /// private name is resolved by the `module::name` fallback in `check_ident`.
    fn declare_scoped(&mut self, name: &str, entry: ItemEntry, is_pub: bool) -> Result<(), ItemEntry>
    where
        ItemEntry: Clone,
    {
        let key = self.scoped_name(name);
        let result = self.items.declare(key, entry.clone());
        if self.module_prefix.is_some() && is_pub {
            // The bare name is a second spelling, not a second definition: a
            // clash there is the duplicate the qualified insert already reported.
            let _ = self.items.declare(name.to_string(), entry);
        }
        result
    }

    /// A name as it is registered, qualified when it belongs to a module.
    fn scoped_name(&self, name: &str) -> String {
        match &self.module_prefix {
            Some(module) => format!("{module}::{name}"),
            None => name.to_string(),
        }
    }

    /// Rejects a cross-module reference to an item that was not declared `pub`.
    ///
    /// Only a qualified spelling crosses a module boundary. A name whose owner
    /// is the current module is that module referring to its own item by its
    /// full name, which is always allowed.
    ///
    /// A private item is never registered under its bare name outside its own
    /// module, so `m::helper` cannot be reached as a bare `helper()` either.
    pub(crate) fn check_path_visibility(&mut self, spelled: &str, span: Span, what: &str) {
        let Some((owner, _)) = spelled.rsplit_once("::") else { return };
        if self.module_prefix.as_deref() == Some(owner) {
            return;
        }
        if self.visibility.get(spelled).copied() == Some(false) {
            self.push(
                Diagnostic::error("E1018", format!("`{spelled}` is private"))
                    .at(span)
                    .with_cause(format!(
                        "a {what} declared without `pub` is visible only inside its own module `{owner}`"
                    ))
                    .with_explanation(format!(
                        "Write `pub {what} {name}` in the module that declares it to make it part \
                         of that module's public surface.",
                        name = spelled.rsplit("::").next().unwrap_or(spelled)
                    )),
            );
        }
    }

    /// Checks a call to a method declared in an `impl`. Returns false (and
    /// reports `E1018`) when the method is private to another module.
    pub(crate) fn check_method_visibility(
        &mut self,
        type_name: &str,
        method: &str,
        span: Span,
    ) -> bool {
        let Some(&(is_pub, ref owner)) =
            self.method_visibility.get(&(type_name.to_string(), method.to_string()))
        else {
            return true;
        };
        if is_pub || owner == &self.module_prefix {
            return true;
        }
        let owner = owner.clone().unwrap_or_default();
        self.push(
            Diagnostic::error("E1018", format!("`{type_name}::{method}` is private"))
                .at(span)
                .with_cause(format!(
                    "a method declared without `pub` is visible only inside its own module `{owner}`"
                ))
                .with_explanation(format!(
                    "Write `pub fn {method}` in the `impl` in `{owner}` to make it callable here."
                )),
        );
        false
    }

    fn register_type_name(
        &mut self,
        name: &str,
        span: Span,
        def: DefId,
        kind: ItemKind,
        _def_kind: DefKind,
        is_pub: bool,
    ) {
        let entry = ItemEntry { res: crate::hir::Res::Type(def), kind, span, arity: 0 };
        if let Err(existing) = self.declare_scoped(name, entry, is_pub) {
            // A name inside a module is only a duplicate of another name in the
            // same module; the qualified key makes that the only comparison.
            self.duplicate_name(name, span, existing.span, existing.kind);
        }
    }

    fn register_fn_name(&mut self, name: &str, span: Span, arity: usize, is_pub: bool) -> FnId {
        let id = FnId(self.hir.fns.len() as u32);
        let entry = ItemEntry {
            res: crate::hir::Res::Fn(id),
            kind: ItemKind::Fn,
            span,
            arity,
        };
        let key = self.scoped_name(name);
        self.visibility.insert(key.clone(), is_pub);
        if let Err(existing) = self.declare_scoped(name, entry, is_pub) {
            self.duplicate_name(name, span, existing.span, existing.kind);
            return id;
        }
        // Keyed both ways: a module's function is reached by its qualified name,
        // and a body check that walks the item list looks it up by the name the
        // item declares. Without the qualified entry both dependencies' `run`
        // pointed at whichever was registered last.
        self.fn_by_name.insert(name.to_string(), id);
        if let Some(module) = &self.module_prefix {
            self.fn_by_name.insert(format!("{module}::{name}"), id);
        }
        // A function that belongs to a module is emitted under its qualified
        // name, because the bare one is ambiguous as soon as two modules declare
        // it -- and the linker, unlike sema, has no scope to disambiguate with.
        // Everything outside a module keeps its plain name.
        self.hir.fns.push(HirFn {
            id,
            name: key, 
            span,
            generics: Vec::new(),
            params: Vec::new(),
            ret: self.cx.unit(),
            has_self: false,
            self_mutable: false,
            self_by_ref: false,
            locals: Vec::new(),
            body: None,
            is_pub,
            owner: None,
            link_name: None,
            variadic: false,
            is_async: false,
            declared_local_tys: Default::default(),
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

    /// Registers a compiler-synthesized trait impl for a named type.
    ///
    /// Used for the closure call traits: the `FnId` is an already-created
    /// function (the closure trampoline), and this records the same evidence a
    /// user-written `impl Trait for Type` would -- the trait's `impls` list and
    /// the method maps that drive method resolution. No new function is created
    /// and no bound-checking semantics change.
    pub(crate) fn register_synthetic_impl(
        &mut self,
        trait_name: &str,
        type_name: &str,
        method: &str,
        impl_fn: FnId,
    ) {
        self.record_trait_impl(trait_name, type_name, Vec::new(), false, Span::DUMMY);
        // A synthesized method is compiler-generated, so it is visible from
        // every module: there is no source-level `pub` to consult.
        self.register_impl_method(type_name, method, impl_fn, true);
    }

    /// Appends one `impl Trait for Type` entry to the named trait.
    ///
    /// Shared with `collect_impl` so a synthesized impl and a source impl are
    /// recorded identically.
    fn record_trait_impl(
        &mut self,
        trait_name: &str,
        type_name: &str,
        params: Vec<String>,
        bounded_all: bool,
        span: Span,
    ) {
        if let Some(info) = self.hir.traits.iter_mut().find(|t| t.name == trait_name) {
            info.impls.push(crate::hir::TraitImplInfo {
                type_name: type_name.to_string(),
                generics: params,
                bounded_all,
                span,
            });
        }
    }

    /// Filed a method under its owner type the same way for source and
    /// synthesized impls.
    fn register_impl_method(&mut self, type_name: &str, method: &str, id: FnId, is_pub: bool) {
        let method_key = (type_name.to_string(), method.to_string());
        self.methods.insert(method_key.clone(), id);
        self.method_visibility.insert(method_key, (is_pub, self.module_prefix.clone()));
        self.methods_of_type.entry(type_name.to_string()).or_default().push(method.to_string());
    }

    fn collect_impl(&mut self, block: &ast::ImplBlock) {
        // The type an impl block belongs to is named the way the type is
        // registered. Inside a module that is `module::Type`, and a method's
        // `self` is laid out by looking the owner up: with the bare name the
        // lookup found nothing and the frame for `self` could not be built.
        let Some(self_ty_name) = type_name_of(&block.self_ty) else {
            return;
        };
        // `impl Trait for Type` says the type carries the trait. A marker trait
        // has no methods to record it by, so the block itself is the evidence,
        // and a call that demands the marker reads this list.
        if let Some(trait_name) = block.trait_path.as_ref().and_then(|p| p.segments.last()) {
            let params: Vec<String> = block.generics.iter().map(|g| g.name.clone()).collect();
            let bounded_all = !params.is_empty()
                && params.iter().all(|p| {
                    block
                        .generics
                        .iter()
                        .find(|g| &g.name == p)
                        .map(|g| g.bounds.iter().any(|b| b.segments.last().map(|s| s.name == trait_name.name).unwrap_or(false)))
                        .unwrap_or(false)
                });
            self.record_trait_impl(&trait_name.name, &self_ty_name, params, bounded_all, block.span);
        }
        for method in &block.methods {
            let id = FnId(self.hir.fns.len() as u32);
            self.register_impl_method(&self_ty_name, &method.name, id, method.is_pub);
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
                is_pub: method.is_pub,
                owner: Some(self_ty_name.clone()),
                link_name: None,
                variadic: false,
                is_async: false,
                declared_local_tys: Default::default(),
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
        // The methods are filed under the trait that declares them, by name.
        //
        // Walking `self.traits` (a map) and writing to `hir.traits` by position
        // meant the two orders had to agree, and a map has no order: adding one
        // empty trait was enough to shift every method onto its neighbour, so
        // `T: Agent` then answered "no method `step`". The name says which trait
        // the methods belong to, and it costs nothing to use it.
        let mut trait_names: Vec<String> = self.traits.keys().cloned().collect();
        trait_names.sort();

        // Pass A: resolve and validate supertrait bounds. Names are stored on
        // `TraitInfo` so object-safety and callers can see them without the AST.
        for name in &trait_names {
            let Some(decl) = self.traits.get(name).cloned() else { continue };
            let mut supers = Vec::new();
            for path in &decl.supertraits {
                let Some(sname) = path.segments.last().map(|s| s.name.clone()) else { continue };
                if !self.traits.contains_key(&sname) {
                    self.error("E1025", path.span, format!("unknown supertrait `{sname}` of trait `{name}`"));
                    continue;
                }
                if supers.contains(&sname) {
                    self.error("E1025", path.span, format!("duplicate supertrait `{sname}` on trait `{name}`"));
                    continue;
                }
                supers.push(sname);
            }
            if let Some(info) = self.hir.traits.iter_mut().find(|t| t.name == *name) {
                info.supertraits = supers;
            }
        }

        // Pass B: flatten methods in ADR 0018 §4 order — each supertrait's
        // methods (recursively) first, then the trait's own — and dedup by name.
        // The resulting order is the vtable slot order used by dyn dispatch.
        let mut memo: HashMap<String, Vec<(TraitMethodInfo, Option<ast::FnDecl>)>> = HashMap::new();
        for name in &trait_names {
            let mut visiting = Vec::new();
            let mut flat = self.flatten_trait_methods(name, &mut visiting, &mut memo);
            self.synthesize_defaults(name, &mut flat);
            let methods: Vec<TraitMethodInfo> = flat.into_iter().map(|(m, _)| m).collect();
            if let Some(info) = self.hir.traits.iter_mut().find(|t| t.name == *name) {
                info.methods = methods;
            }
        }
    }

    /// Flattens a trait's own and inherited methods into ADR 0018 §4 order.
    /// The `FnDecl` alongside each method keeps a default body available for
    /// synthesis; it is stripped before the result is stored in HIR.
    fn flatten_trait_methods(
        &mut self,
        name: &str,
        visiting: &mut Vec<String>,
        memo: &mut HashMap<String, Vec<(TraitMethodInfo, Option<ast::FnDecl>)>>,
    ) -> Vec<(TraitMethodInfo, Option<ast::FnDecl>)> {
        if let Some(cached) = memo.get(name) {
            return cached.clone();
        }
        if visiting.iter().any(|n| n == name) {
            let span = self.traits.get(name).map(|d| d.name_span).unwrap_or(Span::DUMMY);
            self.error("E1025", span, format!("cyclic supertrait bound involving `{name}`"));
            return Vec::new();
        }
        visiting.push(name.to_string());
        let Some(decl) = self.traits.get(name).cloned() else {
            visiting.pop();
            return Vec::new();
        };
        let mut out: Vec<(TraitMethodInfo, Option<ast::FnDecl>)> = Vec::new();
        for path in &decl.supertraits {
            let Some(sname) = path.segments.last().map(|s| s.name.clone()) else { continue };
            let inherited = self.flatten_trait_methods(&sname, visiting, memo);
            for item in inherited {
                if !out.iter().any(|(m, _)| m.name == item.0.name) {
                    out.push(item);
                }
            }
        }
        let generics: Vec<String> = decl.generics.iter().map(|g| g.name.clone()).collect();
        for m in &decl.methods {
            // A method's own type parameters are in scope in its signature too,
            // so `fn generic<T>(x: T)` must not report E1003 on `T`.
            let mut sig_generics = generics.clone();
            sig_generics.extend(m.generics.iter().map(|g| g.name.clone()));
            let params: Vec<TyId> = m.params.iter().map(|p| self.resolve_type(&p.ty, &sig_generics)).collect();
            let ret = match &m.ret {
                Some(t) => self.resolve_type(t, &sig_generics),
                None => self.cx.unit(),
            };
            let info = TraitMethodInfo {
                name: m.name.clone(),
                span: m.name_span,
                params,
                ret,
                default_fn: None,
            };
            if !out.iter().any(|(x, _)| x.name == info.name) {
                out.push((info, m.body.is_some().then(|| m.clone())));
            }
        }
        visiting.pop();
        memo.insert(name.to_string(), out.clone());
        out
    }

    /// Synthesizes a concrete `HirFn` for every method carrying a default body.
    ///
    /// The body is checked once per implementing type with `self` bound to that
    /// concrete type, so a default body may call other methods on `self`. The
    /// synthesized function is registered as a method fallback: a real impl
    /// method with the same name still wins. `TraitMethodInfo.default_fn` keeps
    /// the first specialization for the vtable fallback.
    fn synthesize_defaults(
        &mut self,
        trait_name: &str,
        methods: &mut [(TraitMethodInfo, Option<ast::FnDecl>)],
    ) {
        let impls: Vec<String> = self
            .hir
            .traits
            .iter()
            .find(|t| t.name == trait_name)
            .map(|t| t.impls.iter().map(|i| i.type_name.clone()).collect())
            .unwrap_or_default();
        let generics: Vec<String> = self
            .traits
            .get(trait_name)
            .map(|d| d.generics.iter().map(|g| g.name.clone()).collect())
            .unwrap_or_default();
        for (info, body) in methods.iter_mut() {
            let Some(m) = body.clone() else { continue };
            // Source-text check keeps the ceiling cheap: `Self` has no HIR type
            // in v2, so a default body that spells it (outside the `self`
            // receiver) would otherwise lower to an error-typed expression.
            let body_has_self = m
                .body
                .as_ref()
                .map(|b| text_mentions_self(self.sources.file(b.span.file).span_text(b.span)))
                .unwrap_or(false);
            if m.ret.as_ref().is_some_and(type_mentions_self)
                || m.params.iter().any(|p| type_mentions_self(&p.ty))
                || body_has_self
            {
                self.error(
                    "E1026",
                    m.name_span,
                    format!("default method `{trait_name}::{}` mentions `Self`", m.name),
                );
                continue;
            }
            if impls.is_empty() {
                // No concrete type to specialize against; the body is never
                // reachable, so only a signature-carrying function is made.
                let id = self.new_default_fn(&m, None, &generics);
                info.default_fn = Some(id);
                continue;
            }
            for ty in &impls {
                let id = self.new_default_fn(&m, Some(ty), &generics);
                if info.default_fn.is_none() {
                    info.default_fn = Some(id);
                }
                if let std::collections::hash_map::Entry::Vacant(e) =
                    self.methods.entry((ty.clone(), m.name.clone()))
                {
                    e.insert(id);
                    self.methods_of_type.entry(ty.clone()).or_default().push(m.name.clone());
                }
            }
        }
    }

    /// Builds one default-method function, optionally specialized to `concrete`.
    fn new_default_fn(&mut self, m: &ast::FnDecl, concrete: Option<&str>, generics: &[String]) -> FnId {
        let id = FnId(self.hir.fns.len() as u32);
        let base = concrete
            .and_then(|name| self.lookup_type_def(name))
            .map(|def| self.cx.adt(def, Vec::new()))
            .or_else(|| self.cx.primitive("u8"))
            .unwrap_or_else(|| self.cx.error());
        let has_self = m.self_param.is_some();
        let self_mutable = m.self_param.as_ref().is_some_and(|s| s.mutable);
        let self_by_ref = m.self_param.as_ref().is_some_and(|s| s.by_ref.is_some());
        let mut params = Vec::new();
        if has_self {
            let self_ty = if self_by_ref {
                if self_mutable { self.cx.ref_mut_to(base) } else { self.cx.ref_to(base) }
            } else {
                base
            };
            params.push(self_ty);
        }
        let mut sig_generics = generics.to_vec();
        sig_generics.extend(m.generics.iter().map(|g| g.name.clone()));
        params.extend(m.params.iter().map(|p| self.resolve_type(&p.ty, &sig_generics)));
        let ret = match &m.ret {
            Some(t) => self.resolve_type(t, &sig_generics),
            None => self.cx.unit(),
        };
        self.hir.fns.push(HirFn {
            id,
            name: m.name.clone(),
            span: m.name_span,
            generics: generics.to_vec(),
            params,
            ret,
            has_self,
            self_mutable,
            self_by_ref,
            locals: Vec::new(),
            body: None,
            is_pub: m.is_pub,
            owner: Some(concrete.unwrap_or("__umay_default").to_string()),
            link_name: None,
            variadic: false,
            is_async: false,
            declared_local_tys: Default::default(),
        });
        if concrete.is_some() {
            self.check_one_body(id, m, generics);
        }
        id
    }

    fn resolve_signatures_in(&mut self, items: &[ast::Item]) {
        for item in items {
            match item {
                ast::Item::Struct(s) => {
                    let generics: Vec<String> = s.generics.iter().map(|g| g.name.clone()).collect();
                    let def = self.lookup_type_def(&s.name);
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
                    let def = self.lookup_type_def(&e.name);
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
                        // The declaration is registered under the module's name,
                        // so its signature is resolved under it too.
                        let qualified = match &self.module_prefix {
                            Some(outer) => format!("{outer}::{}", m.name),
                            None => m.name.clone(),
                        };
                        let outer = self.module_prefix.replace(qualified);
                        self.resolve_signatures_in(inner);
                        self.module_prefix = outer;
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
        // The slot this declaration owns, looked up once so that the bounds it
        // records and the body that is later checked agree on it. Asking for it
        // again by name is what went wrong before: the map held a different id for
        // the same declaration, so `iterate_sum`'s bound was filed under one id
        // while its body was checked under another, and the bound was invisible.
        let slot = match receiver {
            Some(ty) => self.methods.get(&(ty.to_string(), f.name.clone())).copied(),
            // Inside a module the bare name is ambiguous the moment two modules
            // declare the same one, so the slot is looked up by the name this
            // declaration is actually registered under.
            None => {
                let key = self.scoped_name(&f.name);
                self.fn_by_name
                    .get(&key)
                    .or_else(|| self.fn_by_name.get(&f.name))
                    .copied()
            }
        };
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
                    // The bound belongs to this function, not to the name `T` in
                    // general: two functions may each have a `T` with a different
                    // bound, and a lookup by name alone would mix them.
                    let owner = slot;
                    self.current_fn = owner;
                    let entry = crate::types::ParamBound {
                        param: g.name.clone(),
                        args,
                        trait_name: name,
                        owner,
                        owner_name: slot
                            .and_then(|id| self.hir.fns.iter().find(|x| x.id == id))
                            .map(|x| x.name.clone())
                            .unwrap_or_default(),
                    };
                    if !self
                        .cx
                        .param_bounds
                        .iter()
                        .any(|b| b.param == entry.param && b.trait_name == entry.trait_name && b.owner == entry.owner)
                    {
                        self.cx.param_bounds.push(entry);
                    }
                }
            }
        }
        // A method of a generic type records which parameters its `impl` block
        // declared. Somewhere else, a receiver whose instance is not written yet
        // (`make(7i64).get()`) needs to know which parameters to fill in, and the
        // declaration is the only place that says so.
        if let (Some(owner), Some(id)) = (receiver, slot) {
            if !outer_generics.is_empty() {
                self.method_generics.insert((owner.to_string(), f.name.clone()), (id, outer_generics.to_vec()));
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
            // The receiver is the impl block's type. Inside a module the type is
            // only registered as `module::Name`, so the bare name finds nothing
            // and `self` becomes the error type -- which the backend then refuses
            // to lay out, far from the line that caused it.
            let receiver_def = receiver.and_then(|r| self.lookup_type_def(r));
            let ty = match receiver_def {
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
            Some(t) => self.resolve_return_type(t, &generics, slot),
            None => self.cx.unit(),
        };
        if let Some(id) = slot {
            self.current_fn = Some(id);
            if let Some(info) = self.hir.fns.iter_mut().find(|x| x.id == id) {
                info.generics = generics;
                info.params = params;
                info.ret = ret;
                info.has_self = has_self;
                info.self_mutable = self_mutable;
                info.self_by_ref = self_by_ref;
                info.link_name = f.link_name.clone();
                info.variadic = f.is_variadic();
            }
        }
    }

    /// Resolves a declared return type.
    ///
    /// `-> impl Trait` is the only position where `impl Trait` is accepted: it
    /// has no representation of its own, so it resolves to an `Error`
    /// placeholder and its bounds are recorded against the owning function.
    /// `check_one_body` later infers the concrete type from the body and writes
    /// it back to `hir.fns[id].ret`. Every other position still rejects
    /// `impl Trait` through `resolve_type` (argument position stays E1003).
    fn resolve_return_type(
        &mut self,
        ty: &ast::TypeExpr,
        generics: &[String],
        owner: Option<FnId>,
    ) -> TyId {
        if let ast::TypeExpr::ImplTrait { bounds, .. } = ty {
            if let Some(id) = owner {
                self.opaque_return_bounds.insert(id, bounds.clone());
            }
            // Placeholder: `Error` also suppresses the declared-vs-body return
            // check in `check_one_body` until the body supplies the real type.
            return self.cx.error();
        }
        self.resolve_type(ty, generics)
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
                // A path spelling a type, which is how a type from a module is
                // named: `kinds::Kind`. Looking only at the last segment meant no
                // module's type could be written in a type position at all.
                if let Some(seg) = path.segments.last() {
                    let args: Vec<TyId> =
                        seg.args.iter().map(|a| self.resolve_type(a, generics)).collect();
                    let spelled = path.as_string();
                    self.check_path_visibility(&spelled, path.span, "type");
                    if let Some(def) = self.def_by_name.get(&spelled).copied() {
                        return self.cx.adt(def, args);
                    }
                    if let Some(def) = self.def_by_name.get(&seg.name).copied() {
                        return self.cx.adt(def, args);
                    }
                    // A bare name inside the module that declares it, and a bare
                    // name for a type only one module declares.
                    if let Some(first) = path.segments.first() {
                        if let Some(module) = self.module_prefix.clone() {
                            let key = format!("{module}::{}", first.name);
                            if let Some(def) = self.def_by_name.get(&key).copied() {
                                return self.cx.adt(def, args);
                            }
                        }
                        let suffix = format!("::{}", first.name);
                        let only: Vec<DefId> = self
                            .def_by_name
                            .iter()
                            .filter(|(key, _)| key.ends_with(&suffix))
                            .map(|(_, def)| *def)
                            .collect();
                        if only.len() == 1 {
                            return self.cx.adt(only[0], args);
                        }
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
            ast::TypeExpr::ImplTrait { span, .. } => {
                self.error("E1003", *span, "`impl Trait` is only supported as a function return type");
                self.cx.error()
            }
            ast::TypeExpr::DynTrait { bounds, span } => self.resolve_dyn_trait(bounds, *span),
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
            (TyKind::Ref(e1), TyKind::RefMut(e2)) => self.types_compatible(e1, e2),
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
                    let key = self.scoped_name(&f.name);
                    let found = self
                        .fn_by_name
                        .get(&key)
                        .or_else(|| self.fn_by_name.get(&f.name))
                        .copied();
                    if let Some(id) = found {
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
                        // The body belongs to the name the function was
                        // registered under. Without the prefix, every module's
                        // `run` looked up bare `run` and the last one won: one
                        // function got both bodies and the other got none, which
                        // the linker reported as an undefined reference.
                        let qualified = match &self.module_prefix {
                            Some(outer) => format!("{outer}::{}", m.name),
                            None => m.name.clone(),
                        };
                        let outer = self.module_prefix.replace(qualified);
                        self.check_bodies_in(&inner);
                        self.module_prefix = outer;
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
        // A return-position `impl Trait` has no declared type: its concrete type
        // is inferred from the body. Capture the marker before `fcx` borrows
        // `self` mutably.
        let opaque_bounds = self.opaque_return_bounds.get(&id).cloned();

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

        // For `-> impl Trait`, the body supplies the hidden type. A tail
        // expression gives it directly; `{ return v }` has block type `!`, so
        // fall back to the value the return carries.
        let hidden_ty = opaque_bounds.as_ref().map(|_| {
            if matches!(fcx.cx.cx.kind(block_ty), TyKind::Never) {
                returned_value_ty(&block).unwrap_or(block_ty)
            } else {
                block_ty
            }
        });

        // Return type check: the body must produce the declared type. An opaque
        // return has no declared type to compare against (`declared_ret` is the
        // `Error` placeholder), so its consistency is left to the flow rules.
        if opaque_bounds.is_none() && !fcx.cx.cx.is_error(declared_ret) && !fcx.cx.cx.is_error(block_ty) {
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
        // An opaque return can be a bare literal (`-> impl Trait { 1 }`); settle
        // its width with the same defaults applied to the body's locals.
        let hidden_ty = hidden_ty.map(|t| defaults.get(&t).copied().unwrap_or(t));
        let declared_local_tys = fcx.declared_local_tys.clone();
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
        if f.is_async {
            // The body was checked against the declared `T` above. It now moves
            // into the generated thunk; this function becomes a constructor for
            // the generated state struct (ADR 0019 section 1).
            // ponytail: only parameters are state fields; locals that live across
            // an await are not captured yet. Upgrade: reuse ADR 0017 capture
            // analysis to add the locals that cross a suspension point.
            let body_span = body_hir.span;
            let (state_ty, state_def) = self.lower_async_fn(
                id,
                &f,
                body_hir,
                locals.clone(),
                declared_local_tys,
                declared_ret,
                body_span,
            );
            let (param_tys, field_names) = {
                let decl = self.hir.fns.iter().find(|x| x.id == id);
                let tys = decl.map(|d| d.params.clone()).unwrap_or_default();
                let names = self.hir.structs.iter().find(|s| s.def_id == state_def)
                    .map(|s| s.fields.iter().map(|fld| fld.name.clone()).collect::<Vec<_>>())
                    .unwrap_or_default();
                (tys, names)
            };
            let fields = field_names
                .iter()
                .enumerate()
                .map(|(i, name)| crate::hir::HirFieldInit {
                    name: name.clone(),
                    span: f.name_span,
                    index: i as u32,
                    value: crate::hir::HirExpr {
                        kind: crate::hir::HirExprKind::Local(crate::hir::LocalId(i as u32)),
                        ty: param_tys.get(i).copied().unwrap_or_else(|| self.cx.error()),
                        span: f.name_span,
                    },
                })
                .collect();
            let state_lit = crate::hir::HirExpr {
                kind: crate::hir::HirExprKind::StructLit { def: state_def, fields, base: None },
                ty: state_ty,
                span: f.name_span,
            };
            let param_count = field_names.len();
            let ctor_body = crate::hir::HirBlock {
                stmts: Vec::new(),
                tail: Some(Box::new(state_lit)),
                span: body_span,
                locals_introduced: 0,
            };
            if let Some(info) = self.hir.fns.iter_mut().find(|x| x.id == id) {
                info.body = Some(ctor_body);
                info.locals = locals;
                info.locals.truncate(param_count);
                info.declared_local_tys.clear();
                info.ret = state_ty;
                info.is_async = true;
            }
        } else if let Some(info) = self.hir.fns.iter_mut().find(|x| x.id == id) {
            info.body = Some(body_hir);
            info.locals = locals;
            info.declared_local_tys = declared_local_tys;
            if let Some(ty) = hidden_ty {
                // Body-inferred hidden type replaces the placeholder. From here
                // on `fn_type`, `check_call`, mono and the backends all see this
                // ordinary concrete type — there is no opaque `TyKind`.
                info.ret = ty;
            }
        }

        // Marker bounds on `-> impl Trait` are checked against the hidden type,
        // mirroring the existing rule in `check_call`: only traits with no
        // methods (markers) are enforced. A hidden type naming a generic
        // parameter is checked at the call site through monomorphisation instead.
        if let (Some(bounds), Some(ty)) = (opaque_bounds, hidden_ty) {
            if self.cx.first_param(ty).is_none() {
                let span = f.ret.as_ref().map(|r| r.span()).unwrap_or(f.name_span);
                self.check_opaque_return_bounds(&bounds, ty, span);
            }
        }
    }

    /// Checks marker bounds written on a `-> impl Trait` against its hidden type.
    fn check_opaque_return_bounds(&mut self, bounds: &[ast::Path], ty: TyId, span: Span) {
        let name = match self.cx.kind(ty).clone() {
            TyKind::Adt { def, .. } => self.hir.def_name(def),
            _ => self.ty_name(ty),
        };
        for bound in bounds {
            let Some(trait_name) = bound.segments.last().map(|s| s.name.clone()) else { continue };
            let Some(info) = self.hir.traits.iter().find(|t| t.name == trait_name) else { continue };
            if !info.methods.is_empty() {
                continue; // only markers are enforced; see check_call
            }
            let ok = info.impls.iter().any(|i| i.type_name == name);
            if !ok {
                self.error(
                    "E1017",
                    span,
                    format!("hidden type `{name}` does not implement `{trait_name}`"),
                );
            }
        }
    }
}

/// True when every path through the block falls off the end (type `()`),
/// used to avoid a spurious "returns unit" error.
fn block_reaches_end_without_value(block: &crate::hir::HirBlock) -> bool {
    block.tail.is_none()
}

/// The type of the first `return v` in `block`, if any. Used to infer the hidden
/// type of `-> impl Trait` when the body's block type is `!` (a body that ends
/// with `return v` rather than a tail expression).
fn returned_value_ty(block: &crate::hir::HirBlock) -> Option<TyId> {
    fn in_expr(e: &crate::hir::HirExpr) -> Option<TyId> {
        if let crate::hir::HirExprKind::Return(Some(v)) = &e.kind {
            return Some(v.ty);
        }
        crate::hir::child_exprs(e).into_iter().find_map(in_expr)
    }
    for stmt in &block.stmts {
        if let crate::hir::HirStmtKind::Expr(e) = &stmt.kind {
            if let Some(ty) = in_expr(e) {
                return Some(ty);
            }
        }
    }
    block.tail.as_deref().and_then(in_expr)
}

fn has_derive(attrs: &[ast::Attribute], name: &str) -> bool {
    attrs.iter().any(|a| a.name == "derive" && a.args.iter().any(|x| x == name))
}

/// Cheap, whole-word source check for `Self` in a default method body. Comments
/// and string literals can produce a false positive; the ceiling is documented
/// as "bodies must not mention `Self`" and a false positive is still refused.
fn text_mentions_self(text: &str) -> bool {
    let bytes = text.as_bytes();
    let ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    for i in 0..bytes.len().saturating_sub(3) {
        if &bytes[i..i + 4] == b"Self"
            && (i == 0 || !ident(bytes[i - 1]))
            && (i + 4 == bytes.len() || !ident(bytes[i + 4]))
        {
            return true;
        }
    }
    false
}

/// True when a type expression spells `Self` anywhere. Used to reject `Self` in
/// default method signatures (E1026): v2 HIR has no representation for it.
fn type_mentions_self(ty: &ast::TypeExpr) -> bool {
    match ty {
        ast::TypeExpr::Path(p) => p.segments.iter().any(|s| s.name == "Self"),
        ast::TypeExpr::Ref { inner, .. } | ast::TypeExpr::Ptr { inner, .. }
        | ast::TypeExpr::Slice { elem: inner, .. } => type_mentions_self(inner),
        ast::TypeExpr::Array { elem, .. } => type_mentions_self(elem),
        ast::TypeExpr::Tuple { elems, .. } => elems.iter().any(type_mentions_self),
        ast::TypeExpr::Fn { params, ret, .. } => {
            params.iter().any(type_mentions_self)
                || ret.as_ref().is_some_and(|r| type_mentions_self(r))
        }
        ast::TypeExpr::ImplTrait { bounds, .. } | ast::TypeExpr::DynTrait { bounds, .. } => {
            bounds.iter().any(|b| b.segments.iter().any(|s| s.name == "Self"))
        }
        ast::TypeExpr::Infer { .. } | ast::TypeExpr::Never { .. } => false,
    }
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
