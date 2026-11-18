//! Semantic analysis entry point.
//!
//! Phase 2 performs, in order:
//!
//! 1. **Collection** — gather every item name into the item scope, allocate
//!    `DefId`s and `FnId`s. Order-independent, so forward references work.
//! 2. **Type resolution** — resolve struct fields, enum variants, function
//!    signatures and constants to interned types.
//! 3. **Body checking** — resolve names and infer/check types expression by
//!    expression, producing HIR.
//! 4. **Exhaustiveness** — verify every `match` covers its scrutinee type.
//!
//! The checker never bails out early: a function with errors is still fully
//! traversed so that one run reports as many problems as possible. Errors
//! produce `TyKind::Error` results, which suppress cascading diagnostics.

pub(crate) mod check_expr;
mod async_lower;
mod check_expr_call;
mod check_expr_closure;
mod dyn_dispatch;
mod check_expr_core;
mod check_expr_flow;
mod check_expr_ops;
mod check_item;
mod exhaustive;
mod object_safety;
mod scope;

pub use exhaustive::{ExhaustivenessReport, Gap};
pub(crate) use check_expr::mentions_unresolved_param;

use std::collections::HashMap;

use crate::ast;
use crate::diagnostics::Diagnostics;
use crate::hir::{ConstId, DefId, FnId, Hir};
use crate::source::Span;
use async_lower::AsyncFnInfo;
use crate::types::{TyCtxt, TyId};
pub use scope::{ItemKind, ItemScope, ValueScopes};

/// Result of running semantic analysis.
pub struct SemaOutput {
    pub hir: Hir,
    pub types: TyCtxt,
    pub diagnostics: Diagnostics,
}

/// Runs semantic analysis over a parsed program.
pub fn analyze(program: &ast::Program, source: &crate::source::SourceMap) -> SemaOutput {
    let mut cx = Checker::new(source);
    cx.run(program);
    cx.finish()
}

/// Collected declaration order, used to assign ids deterministically.
pub(crate) struct DeclOrder {
    /// Struct/type names in declaration order.
    types: Vec<(String, Span)>,
    fns: Vec<(String, Span)>,
    consts: Vec<(String, Span)>,
}

/// The semantic checker. Public only within the crate.
pub struct Checker<'a> {
    /// Name of the crate being checked, used in diagnostics.
    pub(crate) cx: TyCtxt,
    pub(crate) items: ItemScope,
    pub(crate) diags: Diagnostics,
    pub(crate) hir: Hir,
    pub(crate) def_by_name: HashMap<String, DefId>,
    pub(crate) fn_by_name: HashMap<String, FnId>,
    pub(crate) const_by_name: HashMap<String, ConstId>,
    /// Whether an item was declared `pub`, keyed by the qualified name it is
    /// registered under (`module::name`, or just `name` at the top level).
    /// Missing entries mean a prelude/builtin name, which is always visible.
    pub(crate) visibility: HashMap<String, bool>,
    /// Methods declared in `impl` blocks, keyed by (type name, method name).
    pub(crate) methods: HashMap<(String, String), FnId>,
    /// A method's `pub` flag and declaring module, keyed the same way. A method
    /// without `pub` is callable only from code in that same module.
    pub(crate) method_visibility: HashMap<(String, String), (bool, Option<String>)>,
    /// Method names belonging to each type, for "unknown method" suggestions.
    pub(crate) methods_of_type: HashMap<String, Vec<String>>,
    /// The parameters the `impl` block around a method declared, keyed by
    /// `(type name, method name)` and paired with the method's id.
    ///
    /// Recorded so that a receiver whose instance is not written down can still
    /// be filled in: `make(7i64).get()` answers `B<T>` at check time, and only
    /// this table says which `T` the receiver means.
    pub(crate) method_generics: HashMap<(String, String), (FnId, Vec<String>)>,
    /// Trait declarations by name.
    pub(crate) traits: HashMap<String, ast::TraitDecl>,
    /// Constants with resolved types and bodies, checked in a later pass so
    /// that they may refer to other constants.
    pub(crate) pending_consts: Vec<(ConstId, ast::ConstDecl)>,
    /// Literal variables that met a concrete type, mapped to it.
    ///
    /// A `{integer}` starts with no width. The first concrete integer it meets
    /// decides the width for good, and recording that here lets the final pass
    /// rewrite every local and temporary that still names the variable, so
    /// nothing keeps a 4-byte slot while being read as 8.
    pub(crate) settled_vars: HashMap<crate::types::TyId, crate::types::TyId>,
    /// The module whose items are being collected, if any.
    ///
    /// An item inside `mod foo;` is reachable as `foo::name` and is not a name in
    /// the enclosing scope. Registering it bare as well made two dependencies
    /// that both declare `run` collide, and made whichever was collected first
    /// answer for the other: `b::run()` resolved to `a::run()`.
    pub(crate) module_prefix: Option<String>,
    /// The function whose signature was resolved last.
    ///
    /// Generic parameter names repeat across functions, so a bound is only
    /// meaningful together with the function that declared it.
    pub(crate) current_fn: Option<FnId>,
    /// Closure environment struct -> the trampoline that calls its body.
    ///
    /// A closure value is an ordinary struct holding its captures. Calling it
    /// needs the generated function that knows the body; the struct cannot name
    /// it, so the checker keeps the association here and `check_call` uses it
    /// to turn `f(args)` into `trampoline(f, args)`.
    pub(crate) closure_fn_by_def: HashMap<crate::types::DefId, FnId>,
    /// Generated async state struct -> its `block_on` helper.
    ///
    /// `.await` and `block_on(expr)` both look here to turn a state value into a
    /// direct call to the concrete blocking helper, the same way `check_call`
    /// turns a closure struct into a trampoline call.
    pub(crate) async_helper_by_def: HashMap<crate::types::DefId, FnId>,
    /// Async `fn` -> everything lowered from it. Kept next to the def->helper
    /// map so a caller that already knows the `FnId` can reach the same evidence.
    pub(crate) async_state_by_fn: HashMap<FnId, AsyncFnInfo>,
    /// Functions declared with `-> impl Trait`, keyed by their `FnId`.
    ///
    /// The value is the list of bounds written on the `impl Trait`. The type
    /// itself is *not* stored: the hidden type is inferred from the body in
    /// `check_one_body` and written back into `hir.fns[id].ret`, so `fn_type`,
    /// `check_call`, monomorphisation and both backends see an ordinary concrete
    /// type. This means erasure is deliberately not hidden from callers — a
    /// caller can observe the concrete hidden type. A real opaque `TyKind` is a
    /// separate, larger change and is out of scope for this slice.
    pub(crate) opaque_return_bounds: HashMap<FnId, Vec<ast::Path>>,
    /// Kept for diagnostics that need to quote source text (Phase 3 will use
    /// it to render MIR annotations).
    #[allow(dead_code)]
    pub(crate) sources: &'a crate::source::SourceMap,
}

impl<'a> Checker<'a> {
    /// Records that a literal variable now stands for a concrete type.
    pub(crate) fn settle_var(&mut self, var: crate::types::TyId, concrete: crate::types::TyId) {
        if !self.cx.is_infer_var(var) || self.cx.is_infer_var(concrete) {
            return;
        }
        self.settled_vars.insert(var, concrete);
    }

    pub fn new(sources: &'a crate::source::SourceMap) -> Self {
        let mut checker = Checker {
            module_prefix: None,
            current_fn: None,
            cx: TyCtxt::new(),
            items: ItemScope::new(),
            diags: Diagnostics::new(),
            hir: Hir {
                defs: Vec::new(),
                fns: Vec::new(),
                consts: Vec::new(),
                structs: Vec::new(),
                enums: Vec::new(),
                traits: Vec::new(),
            },
            def_by_name: HashMap::new(),
            fn_by_name: HashMap::new(),
            const_by_name: HashMap::new(),
            visibility: HashMap::new(),
            method_visibility: HashMap::new(),
            methods: HashMap::new(),
            methods_of_type: HashMap::new(),
            method_generics: HashMap::new(),
            traits: HashMap::new(),
            pending_consts: Vec::new(),
            settled_vars: HashMap::new(),
            closure_fn_by_def: HashMap::new(),
            async_helper_by_def: HashMap::new(),
            async_state_by_fn: HashMap::new(),
            opaque_return_bounds: HashMap::new(),
            sources,
        };
        checker.install_prelude();
        checker
    }

    pub(crate) fn finish(mut self) -> SemaOutput {
        self.diags.sort();
        SemaOutput { hir: self.hir, types: self.cx, diagnostics: self.diags }
    }

    fn run(&mut self, program: &ast::Program) {
        let order = collect_order(program);
        self.collect_items(program, &order);
        self.resolve_signatures(program);
        self.check_consts();
        self.check_bodies(program);
    }

    // ---- helpers shared by submodules ------------------------------------

    /// Reports a diagnostic with code, message and span.
    pub(crate) fn error(&mut self, code: &'static str, span: Span, message: impl Into<String>) {
        self.diags.push(crate::diagnostics::Diagnostic::error(code, message).at(span));
    }

    /// Appends a diagnostic built elsewhere.
    pub(crate) fn push(&mut self, d: crate::diagnostics::Diagnostic) {
        self.diags.push(d);
    }

    /// Resolves a `dyn Trait + ...` bound into an interned trait-object type.
    ///
    /// The object-safety checker runs first (E1022/E1023/E1024); the names are
    /// then interned as a `TyKind::DynTrait`. `check_item.rs` calls this from its
    /// `TypeExpr::DynTrait` arm in place of the old E1003 rejection.
    pub(crate) fn resolve_dyn_trait(&mut self, bounds: &[ast::Path], span: Span) -> TyId {
        let generics: Vec<String> = Vec::new();
        let resolved: Vec<crate::types::TraitBound> = bounds
            .iter()
            .map(|p| {
                let seg = p.segments.last();
                // The segment's bare name, not `as_string()`: the latter now
                // includes arguments and bindings and would miss the trait
                // table entry.
                crate::types::TraitBound {
                    name: seg.map(|s| s.name.clone()).unwrap_or_default(),
                    args: seg
                        .map(|s| s.args.iter().map(|a| self.resolve_type(a, &generics)).collect())
                        .unwrap_or_default(),
                    assoc: seg
                        .map(|s| {
                            s.bindings
                                .iter()
                                .map(|b| (b.name.clone(), self.resolve_type(&b.ty, &generics)))
                                .collect()
                        })
                        .unwrap_or_default(),
                }
            })
            .collect();
        object_safety::check_object_safety(&self.traits, &resolved, span, &mut self.diags);
        self.cx.dyn_trait(resolved)
    }

    /// Display name for a type, including user-defined names.
    pub(crate) fn ty_name(&mut self, ty: crate::types::TyId) -> String {
        let kind = self.cx.kind(ty).clone();
        match kind {
            crate::types::TyKind::Adt { def, args } => {
                let base = if let Some(s) = self.hir.structs.iter().find(|s| s.def_id == def) {
                    s.name.clone()
                } else if let Some(e) = self.hir.enums.iter().find(|e| e.def_id == def) {
                    e.name.clone()
                } else {
                    "<unknown>".to_string()
                };
                if args.is_empty() {
                    base
                } else {
                    let inner: Vec<String> = args.iter().map(|a| self.ty_name(*a)).collect();
                    format!("{}<{}>", base, inner.join(", "))
                }
            }
            _ => self.cx.display(ty),
        }
    }

    fn install_prelude(&mut self) {
        // Prelude types that user code can name without an import.
        //
        // `Option` and `Result` are declared as real enums so that `Some`/`None`
        // resolve, patterns match, `?` computes its success type, and `match`
        // can prove exhaustiveness. The other names are opaque for now: they
        // are recognised by name but their representation arrives with the
        // standard library in Phase 6.
        let option_def = DefId(self.hir.enums.len() as u32);
        self.hir.enums.push(crate::hir::EnumInfo {
            name: "Option".to_string(),
            span: Span::DUMMY,
            def_id: option_def,
            generics: vec!["T".to_string()],
            variants: vec![
                crate::hir::VariantInfo {
                    name: "Some".to_string(),
                    span: Span::DUMMY,
                    fields: crate::hir::VariantFields::Tuple(vec![self.cx.param("T")]),
                },
                crate::hir::VariantInfo {
                    name: "None".to_string(),
                    span: Span::DUMMY,
                    fields: crate::hir::VariantFields::Unit,
                },
            ],
        });
        self.def_by_name.insert("Option".to_string(), option_def);
        self.items.declare_prelude(
            "Option".to_string(),
            scope::ItemEntry {
                res: crate::hir::Res::Type(option_def),
                kind: ItemKind::Enum,
                span: Span::DUMMY,
                arity: 1,
            },
        );

        let result_def = DefId(self.hir.enums.len() as u32);
        self.hir.enums.push(crate::hir::EnumInfo {
            name: "Result".to_string(),
            span: Span::DUMMY,
            def_id: result_def,
            generics: vec!["T".to_string(), "E".to_string()],
            variants: vec![
                crate::hir::VariantInfo {
                    name: "Ok".to_string(),
                    span: Span::DUMMY,
                    fields: crate::hir::VariantFields::Tuple(vec![self.cx.param("T")]),
                },
                crate::hir::VariantInfo {
                    name: "Err".to_string(),
                    span: Span::DUMMY,
                    fields: crate::hir::VariantFields::Tuple(vec![self.cx.param("E")]),
                },
            ],
        });
        self.def_by_name.insert("Result".to_string(), result_def);
        self.items.declare_prelude(
            "Result".to_string(),
            scope::ItemEntry {
                res: crate::hir::Res::Type(result_def),
                kind: ItemKind::Enum,
                span: Span::DUMMY,
                arity: 2,
            },
        );

        // Names the checker recognises but whose representation is deferred.
        for name in ["Vec", "String", "Box"] {
            let entry = scope::ItemEntry {
                res: crate::hir::Res::Type(DefId(u32::MAX)),
                kind: ItemKind::Struct,
                span: Span::DUMMY,
                arity: 1,
            };
            self.items.declare_prelude(name.to_string(), entry);
        }
        // Prelude functions are real UMAY functions, parsed from
        // `stdlib/prelude.um` and prepended to every program, so they are
        // declared by the normal item collection and need nothing here.
    }
}

fn collect_order(program: &ast::Program) -> DeclOrder {
    let mut order = DeclOrder { types: Vec::new(), fns: Vec::new(), consts: Vec::new() };
    walk_items(&program.items, &mut order);
    order
}

fn walk_items(items: &[ast::Item], order: &mut DeclOrder) {
    for item in items {
        match item {
            ast::Item::Struct(s) => order.types.push((s.name.clone(), s.name_span)),
            ast::Item::Enum(e) => order.types.push((e.name.clone(), e.name_span)),
            ast::Item::Trait(t) => order.types.push((t.name.clone(), t.name_span)),
            ast::Item::Fn(f) => order.fns.push((f.name.clone(), f.name_span)),
            ast::Item::Const(c) => order.consts.push((c.name.clone(), c.name_span)),
            ast::Item::Impl(i) => {
                for m in &i.methods {
                    order.fns.push((m.name.clone(), m.name_span));
                }
            }
            ast::Item::Mod(m) => {
                if let Some(inner) = &m.items {
                    walk_items(inner, order);
                }
            }
            ast::Item::Use(_) | ast::Item::TypeAlias(_) => {}
        }
    }
}

/// Convenience wrapper used by tests and the driver.
pub fn check(program: &ast::Program, source: &crate::source::SourceMap) -> SemaOutput {
    analyze(program, source)
}
