//! Monomorphisation: turning generic declarations into concrete functions.
//!
//! The checker accepts a generic body and leaves its parameter types in place.
//! That is the right shape for checking — one body, checked once — but the
//! backend needs a size for every local, and a parameter has none. This module
//! bridges the two: for each *instantiation* the program actually uses it produces
//! a copy of the body with the parameters replaced by concrete types and a fresh
//! function id, then rewrites the call site to name that copy.
//!
//! The pass works on HIR rather than MIR on purpose. HIR is where types live, so
//! substitution is a rewrite of a type id plus a walk of the expression tree.
//! After lowering there would be locals, projections and layout keys to rewrite by
//! hand, with more places to miss.
//!
//! # Shapes handled
//!
//! * A generic function called with inferred arguments: `fn first<A>(a: A) -> A`.
//! * A method of a generic `impl`, called on an instantiated receiver:
//!   `Box<i64>::get`.
//! * A generic body calling another instantiation, itself included — which is
//!   what makes a growable container work.
//!
//! # Shapes refused
//!
//! An instantiation is only produced from a call site whose type arguments are
//! fully known. A generic function used as a *value* has no call site, so no copy
//! is produced and the backend reports `E3001`. That is the honest answer:
//! guessing a type would silently compile the wrong one.

use std::collections::HashMap;

use crate::hir::*;
use crate::source::Span;
use crate::types::{TyCtxt, TyId, TyKind};

/// A single instantiation: a declaration plus the types it was used at.
///
/// The type ids are ordered so the set of produced functions is deterministic
/// and does not depend on hash order.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Instance {
    pub callee: FnId,
    pub args: Vec<u32>,
}

impl Instance {
    fn new(callee: FnId, args: Vec<TyId>) -> Self {
        Instance { callee, args: args.iter().map(|t| t.0).collect() }
    }
}


/// Fills in concrete copies of every generic function the program uses.
pub struct Mono<'t> {
    types: &'t mut TyCtxt,
    /// Instances discovered but not yet produced.
    worklist: Vec<Instance>,
    /// Instances already produced, mapped to the function id of the copy.
    produced: std::collections::BTreeMap<Instance, FnId>,
    /// Generic parameter names of a declaration, in order.
    decl_generics: HashMap<FnId, Vec<String>>,
    /// The generics of the struct whose `impl` holds a method.
    impl_generics: HashMap<FnId, Vec<String>>,
    /// Next free function id.
    next_id: u32,
    /// `(type name, method name)` to the function that implements it.
    ///
    /// Kept here so that a `TraitMethodCall` on a generic parameter can be
    /// resolved once monomorphisation knows the parameter's real type: the trait
    /// bound promised the method exists, and this is where it is found.
    methods: HashMap<(String, String), FnId>,
    /// Each struct or enum's name, keyed by definition id.
    def_names: HashMap<crate::types::DefId, String>,
    /// Each function's declared return type, keyed by id.
    returns: HashMap<FnId, TyId>,
    /// Trait method calls whose receiver never became a concrete type, as
    /// `(trait, method, span)`. Reported rather than dropped.
    pub unresolved_trait_calls: Vec<(String, String, crate::source::Span)>,
}

impl<'t> Mono<'t> {
    pub fn new(types: &'t mut TyCtxt, hir: &Hir) -> Self {
        let next_id = hir.fns.iter().map(|f| f.id.0 + 1).max().unwrap_or(0);
        let mut decl_generics = HashMap::new();
        for f in &hir.fns {
            if !f.generics.is_empty() {
                decl_generics.insert(f.id, f.generics.clone());
            }
        }
        let mut impl_generics = HashMap::new();
        for f in &hir.fns {
            if !f.has_self {
                continue;
            }
            let Some(self_ty) = f.params.first().copied() else { continue };
            let base = peel_refs(types, self_ty);
            if let TyKind::Adt { def, .. } = types.kind(base).clone() {
                // An `impl` may be written for a struct or an enum; both carry
                // their own parameter list, and either one decides what `T` means
                // inside the method.
                let generics = hir
                    .structs
                    .iter()
                    .find(|s| s.def_id == def)
                    .map(|s| s.generics.clone())
                    .or_else(|| {
                        hir.enums
                            .iter()
                            .find(|e| e.def_id == def)
                            .map(|e| e.generics.clone())
                    });
                if let Some(g) = generics {
                    if !g.is_empty() {
                        impl_generics.insert(f.id, g);
                    }
                }
            }
        }
        let mut methods = HashMap::new();
        for f in &hir.fns {
            if let Some(owner) = &f.owner {
                methods.insert((owner.clone(), f.name.clone()), f.id);
            }
        }
        let mut def_names = HashMap::new();
        for s in &hir.structs {
            def_names.insert(s.def_id, s.name.clone());
        }
        for e in &hir.enums {
            def_names.insert(e.def_id, e.name.clone());
        }
        let returns = hir.fns.iter().map(|f| (f.id, f.ret)).collect();
        Mono {
            types,
            returns,
            worklist: Vec::new(),
            produced: Default::default(),
            decl_generics,
            impl_generics,
            next_id,
            methods,
            def_names,
            unresolved_trait_calls: Vec::new(),
        }
    }

    /// True when `f` needs an instantiation before it can be compiled.
    fn is_generic(&self, f: FnId) -> bool {
        self.decl_generics.contains_key(&f) || self.impl_generics.contains_key(&f)
    }

    /// Produces every instantiation `hir` needs and rewrites its call sites.
    ///
    /// Two phases, in this order and no other. The first discovers instances by
    /// walking every body, and building a copy discovers more, so it runs to a
    /// fixed point. Only when every copy exists is a call site pointed at one: a
    /// single pass cannot do both, because the call site that first mentions an
    /// instance is visited before that instance has a function id to name.
    pub fn run(&mut self, hir: &mut Hir) {
        if self.decl_generics.is_empty() && self.impl_generics.is_empty() {
            return;
        }
        self.discover(hir);
        self.resolve(hir);
        // A binding whose type is settled changes the receiver type of every call
        // on it, and a call site is resolved against the receiver, so settling and
        // resolving have to trade places until neither changes anything. In
        // practice one round is enough; the loop is here because a binding
        // settled by a later round would otherwise keep a stale call.
        let mut rounds = 0;
        loop {
            let settled = self.settle_let_types(hir);
            // A settled binding makes a call site name an instance that no earlier
            // pass could have seen: `let a = make(7i64)` settles to `B<i64>` only
            // after `make`'s copy exists, so `a.get()` demands its method for the
            // first time here. Demand before resolving again, or the call site is
            // looked up in a table that has no entry for it.
            let demanded;
            {
                let mut rw = Rewriter { mono: self, hir, collect: true, in_template: false };
                for i in 0..rw.hir.fns.len() {
                    rw.rewrite_fn(i);
                }
                demanded = !rw.mono.worklist.is_empty();
            }
            while let Some(instance) = self.worklist.pop() {
                self.instantiate(hir, instance);
            }
            self.resolve(hir);
            // `resolve` walks every body again and records any instance it newly
            // needs, so those copies have to be produced too.
            while let Some(instance) = self.worklist.pop() {
                self.instantiate(hir, instance);
            }
            if !settled && !demanded {
                break;
            }
            rounds += 1;
            if rounds > 8 {
                break;
            }
        }
    }

    /// Gives every `let` binding the type its initialiser finally has.
    ///
    /// A binding's type is the initialiser's, decided before monomorphisation. A
    /// call like `let c = make(7i64)` is written `B<T>` there because `make` is
    /// generic, and the call site is what fills `T` in. The local table kept the
    /// `B<T>`, and a type parameter nothing resolved has no size, so the function
    /// could not be laid out. This runs after instantiation has settled.
    /// Returns true when any binding or call type changed.
    fn settle_let_types(&mut self, hir: &mut Hir) -> bool {
        let mut changed = false;
        for f in hir.fns.iter_mut() {
            let mut fixes: Vec<(usize, TyId)> = Vec::new();
            // A binding's type is recorded twice: as a local, and inside the
            // statement's pattern. The local is what lowering lays out, so it is
            // the one that must be settled; the pattern follows it, which keeps
            // the two from disagreeing about a destructuring initialiser.
            if let Some(body) = &f.body {
                collect_let_fixes(self.types, body, &f.declared_local_tys, &mut fixes);
            }
            for (local, ty) in fixes {
                if let Some(info) = f.locals.get_mut(local) {
                    if mentions_unresolved_param(self.types, info.ty) {
                        info.ty = ty;
                        changed = true;
                    }
                }
            }
            // A call on such a local was checked against the parametric type, and
            // the receiver's type is stored on the expression before the call is
            // resolved, so it is settled too: `let b = make(9i64)` then
            // `b.get()` must name `B<i64>`, not `B<T>`.
            let settled: Vec<(usize, TyId)> = f
                .locals
                .iter()
                .enumerate()
                .map(|(i, l)| (i, l.ty))
                .collect();
            if let Some(body) = &mut f.body {
                if retype_local_refs(body, &settled) {
                    changed = true;
                }
            }
        }
        changed
    }

    /// Phase one: walks every body and produces the copies call sites need.
    fn discover(&mut self, hir: &mut Hir) {
        let count = hir.fns.len();
        for i in 0..count {
            self.collect_fn(hir, i);
        }
        while let Some(instance) = self.worklist.pop() {
            self.instantiate(hir, instance);
        }
    }

    /// Phase two: points every generic call site at the copy it resolves to.
    fn resolve(&mut self, hir: &mut Hir) {
        let count = hir.fns.len();
        for i in 0..count {
            let mut rw = Rewriter { mono: self, hir, collect: false, in_template: false };
            rw.rewrite_fn(i);
        }
    }

    /// Records the instances `hir.fns[index]` needs, without rewriting anything.
    fn collect_fn(&mut self, hir: &mut Hir, index: usize) {
        let mut rw = Rewriter { mono: self, hir, collect: true, in_template: false };
        rw.rewrite_fn(index);
    }

    /// The instance a generic free function is called as, inferred from arguments.
    ///
    /// A parameter type that is exactly one generic parameter takes the argument's
    /// type. Anything else leaves the position unknown and the call uninstantiable,
    /// which is reported later rather than guessed at.
    fn infer_free(&self, hir: &Hir, callee: FnId, arg_tys: &[TyId]) -> Option<Vec<TyId>> {
        let param_tys = hir.fns.iter().find(|f| f.id == callee)?.params.clone();
        let names = self.decl_generics.get(&callee)?;
        let mut subst: Vec<Option<TyId>> = vec![None; names.len()];
        for (p, a) in param_tys.iter().zip(arg_tys.iter()) {
            unify_param(self.types, *p, *a, &names, &mut subst);
        }
        subst.into_iter().collect()
    }

    /// The types an associated function is instantiated with, from its result.
    ///
    /// `Vec::with_capacity(n)` returns a `Vec<T>` whose parameter no argument
    /// mentions; the call site's expected type already made it concrete, so the
    /// result names the instance.
    fn infer_result(&self, hir: &Hir, callee: FnId, result: TyId) -> Option<Vec<TyId>> {
        let ret = hir.fns.iter().find(|f| f.id == callee)?.ret;
        let names = self.decl_generics.get(&callee)?;
        let mut subst: Vec<Option<TyId>> = vec![None; names.len()];
        match (self.types.kind(ret).clone(), self.types.kind(result).clone()) {
            (
                TyKind::Adt { def: d1, args: declared },
                TyKind::Adt { def: d2, args: concrete },
            ) if d1 == d2 => {
                for (i, decl) in declared.iter().enumerate() {
                    let TyKind::Param { name } = self.types.kind(*decl).clone() else { continue };
                    let Some(pos) = names.iter().position(|n| *n == name) else { continue };
                    if let Some(arg) = concrete.get(i) {
                        subst[pos] = Some(*arg);
                    }
                }
            }
            _ => return None,
        }
        subst.into_iter().collect()
    }

    /// The types a method call instantiates its `impl` with, from the receiver.
    fn infer_method(&self, method: FnId, recv_ty: TyId) -> Option<Vec<TyId>> {
        self.impl_generics.get(&method)?;
        let base = peel_refs(self.types, recv_ty);
        match self.types.kind(base).clone() {
            TyKind::Adt { args, .. } => Some(args),
            _ => None,
        }
    }

    /// Builds the concrete copy of `instance` and appends it to `hir.fns`.
    fn instantiate(&mut self, hir: &mut Hir, instance: Instance) {
        if self.produced.contains_key(&instance) {
            return;
        }
        let Some(index) = hir.fns.iter().position(|f| f.id == instance.callee) else { return };
        let names: Vec<String> = self
            .impl_generics
            .get(&instance.callee)
            .or_else(|| self.decl_generics.get(&instance.callee))
            .cloned()
            .unwrap_or_default();
        let mut subst = HashMap::new();
        for (name, id) in names.iter().zip(instance.args.iter()) {
            subst.insert(name.clone(), TyId(*id));
        }
        let id = FnId(self.next_id);
        self.next_id += 1;
        self.produced.insert(instance.clone(), id);

                let mut copy = hir.fns[index].clone();
        copy.id = id;
        copy.name = mangled(&hir.fns[index].name, &instance.args);
        // The copy is no longer generic: its parameters have been decided, so
        // leaving them in place would make the checker's own rule refuse it.
        copy.generics.clear();
                substitute_fn_types(self.types, &mut copy, &subst);
                let new_index = hir.fns.len();
        hir.fns.push(copy);
        // The copy may call other instantiations, so its body is scanned too.
        self.collect_fn(hir, new_index);
    }

    /// Records that `callee` is used at `args`.
    ///
    /// Phase one only records; phase two only looks up. Keeping the two apart is
    /// what lets a call site be rewritten in the same pass that discovers it,
    /// because by then every copy already exists.
    fn demand(&mut self, callee: FnId, args: &[TyId]) -> Option<FnId> {
        if !self.is_generic(callee) {
            return Some(callee);
        }
        // An argument that *is* a parameter means this call was reached from
        // inside another generic body, not from code with a concrete type: it is
        // that template's own business and produces no instance. Recording it
        // would create a copy whose `T` is another `T`, which no caller can use.
        //
        // The argument is asked about on its own, not through `first_param`: a
        // `Pair<A, B>` built from two parameters is still the one instance `Pair`
        // of them, and asking whether a parameter appears anywhere inside would
        // refuse it along with the bare `T` this rule is about.
        if args.iter().any(|a| matches!(self.types.kind(*a), TyKind::Param { .. })) {
            return None;
        }
        let instance = Instance::new(callee, args.to_vec());
        if let Some(&id) = self.produced.get(&instance) {
            return Some(id);
        }
        self.worklist.push(instance);
        None
    }

    /// A function's declared return type, for resolving a receiver from it.
    fn hir_ret(&self, id: FnId) -> Option<TyId> {
        self.returns.get(&id).copied()
    }

    /// The instance a still-parametric local now has, once its value is known.
    ///
    /// Exposed for the one place a receiver can be settled after instantiation:
    /// a method call on a local whose type was decided before the initialiser was
    /// instantiated.
    pub(crate) fn settle_binding(&mut self, ty: TyId, init: TyId, hint: TyId) -> TyId {
        settle_binding(self.types, ty, init, Some(hint))
    }

    /// The produced copy for an instance, if it exists.
    fn lookup(&self, callee: FnId, args: &[TyId]) -> Option<FnId> {
        if !self.is_generic(callee) {
            return Some(callee);
        }
        let inst = Instance::new(callee, args.to_vec());
        self.produced.get(&inst).copied()
    }
}

/// The symbol name of a copy: `first$i64$` reads well in `umay asm` output.
fn mangled(name: &str, args: &[u32]) -> String {
    if args.is_empty() {
        return name.to_string();
    }
    // Only characters an assembler accepts in a symbol: a `$` survives the
    // printer in one place and is escaped in another, which shows up as a symbol
    // the linker can reference but not find.
    let parts: Vec<String> = args.iter().map(|t| format!("t{t}")).collect();
    format!("{name}__{}", parts.join("_"))
}

/// Strips every reference and pointer from `ty`.
/// Fills `subst` in from one declared/actual type pair.
///
/// The walk goes through references, raw pointers and ADT arguments, because a
/// parameter is rarely the bare parameter: `fn total<T>(it: &mut T, ..)` declares
/// `&mut T`, and matching only the outermost `Param` would leave `T` unknown and
/// instantiate it as nothing at all.
fn unify_param(
    types: &TyCtxt,
    declared: TyId,
    actual: TyId,
    names: &[String],
    subst: &mut [Option<TyId>],
) {
    match (types.kind(declared).clone(), types.kind(actual).clone()) {
        (TyKind::Param { name }, _) => {
            if let Some(pos) = names.iter().position(|n| *n == name) {
                if subst[pos].is_none() {
                    subst[pos] = Some(actual);
                }
            }
        }
        (TyKind::Ref(d), TyKind::Ref(a))
        | (TyKind::RefMut(d), TyKind::RefMut(a))
        | (TyKind::RawPtr(d), TyKind::RawPtr(a))
        | (TyKind::RawPtrMut(d), TyKind::RawPtrMut(a))
        | (TyKind::Slice(d), TyKind::Slice(a)) => unify_param(types, d, a, names, subst),
        // A reference and a value of the same shape line up too: a call may pass
        // `T` where `&T` is declared once auto-borrow has done its work.
        (TyKind::Ref(d), _) | (TyKind::RefMut(d), _) => unify_param(types, d, actual, names, subst),
        (TyKind::Adt { def: d1, args: d_args }, TyKind::Adt { def: d2, args: a_args }) if d1 == d2 => {
            for (d, a) in d_args.iter().zip(a_args.iter()) {
                unify_param(types, *d, *a, names, subst);
            }
        }
        (TyKind::Array { elem: d, .. }, TyKind::Array { elem: a, .. }) => {
            unify_param(types, d, a, names, subst)
        }
        (TyKind::Tuple(ds), TyKind::Tuple(as_)) => {
            for (d, a) in ds.iter().zip(as_.iter()) {
                unify_param(types, *d, *a, names, subst);
            }
        }
        _ => {}
    }
}

fn peel_refs(types: &TyCtxt, ty: TyId) -> TyId {
    let mut cur = ty;
    loop {
        match types.kind(cur).clone() {
            TyKind::Ref(inner) | TyKind::RefMut(inner) | TyKind::RawPtr(inner)
            | TyKind::RawPtrMut(inner) => cur = inner,
            _ => return cur,
        }
    }
}

/// Rewrites one function body, replacing generic call sites with instantiations.
struct Rewriter<'m, 't, 'h> {
    mono: &'m mut Mono<'t>,
    hir: &'h mut Hir,
    /// Phase one when true: record the instances a call site needs but leave the
    /// tree alone. Phase two when false: point the call site at its copy.
    collect: bool,
    /// True while walking a generic template. Its body names parameters, so a
    /// construct that needs a concrete type cannot be resolved here; the copies
    /// are where it becomes resolvable, and they are walked separately.
    in_template: bool,
}

impl<'m, 't, 'h> Rewriter<'m, 't, 'h> {
    /// Visits every expression in `hir.fns[index]`.
    fn rewrite_fn(&mut self, index: usize) {
        let was_template = self.in_template;
        if !self.hir.fns[index].generics.is_empty() {
            self.in_template = true;
        }
        let mut body = std::mem::take(&mut self.hir.fns[index].body);
        if let Some(body) = &mut body {
            self.rewrite_block(body);
        }
        self.hir.fns[index].body = body;
        self.in_template = was_template;
    }

    fn rewrite_block(&mut self, block: &mut HirBlock) {
        for stmt in &mut block.stmts {
            match &mut stmt.kind {
                HirStmtKind::Let { init, .. } => {
                    if let Some(init) = init {
                        self.rewrite_expr(init);
                    }
                }
                HirStmtKind::Expr(e) => self.rewrite_expr(e),
            }
        }
        if let Some(tail) = &mut block.tail {
            self.rewrite_expr(tail);
        }
    }

    fn rewrite_expr(&mut self, e: &mut HirExpr) {
        // The pattern is: rewrite children first, then decide whether this node is
        // a generic call whose arguments are now all concrete.
        match &mut e.kind {
            HirExprKind::Call { callee, args } => {
                let call_ty = e.ty;
                self.rewrite_expr(callee);
                for a in args.iter_mut() {
                    self.rewrite_expr(a);
                }
                let fn_id = match &callee.kind {
                    HirExprKind::FnRef(id) => Some(*id),
                    _ => None,
                };
                if let Some(id) = fn_id {
                    let arg_tys: Vec<TyId> = args.iter().map(|a| a.ty).collect();
                    // A generic callee whose arguments do not mention its parameter
                    // is an associated function such as `Vec::with_capacity(n)`,
                    // where the result type is the only thing that says which
                    // instance is meant: it was already made concrete against the
                    // annotation at the call site.
                    let Some(instance_args) = self.mono.infer_free(self.hir, id, &arg_tys).or_else(|| {
                        self.mono.infer_result(self.hir, id, call_ty)
                    }) else {
                        return;
                    };
                    let resolved = if self.collect {
                        self.mono.demand(id, &instance_args);
                        None
                    } else {
                        self.mono.lookup(id, &instance_args)
                    };
                    if let Some(concrete) = resolved {
                        if concrete != id {
                            callee.kind = HirExprKind::FnRef(concrete);
                        }
                    }
                    // The call's own type is written in terms of the callee's
                    // parameters -- `fn make<T>(v: T) -> B<T>` has `B<T>` here --
                    // so it has to be instantiated too. Leaving it alone made the
                    // result keep a bare `T`, and a `B<T>` whose `T` was never
                    // filled in has no size, so the function could not be lowered.
                    if let Some(names) = self.mono.decl_generics.get(&id) {
                        if names.len() == instance_args.len() {
                            let subst: std::collections::HashMap<String, TyId> = names
                                .iter()
                                .cloned()
                                .zip(instance_args.iter().copied())
                                .collect();
                            e.ty = self.mono.types.substitute(e.ty, &subst);
                        }
                    }
                }
            }
            HirExprKind::TraitMethodCall { receiver, trait_name, method, method_span, args } => {
                self.rewrite_expr(receiver);
                for a in args.iter_mut() {
                    self.rewrite_expr(a);
                }
                // The receiver's type is concrete by now, so the bound can be
                // cashed in: the trait promised the method, and the `impl` for the
                // real type is the one that runs. Dispatch stays static.
                let base = peel_refs(self.mono.types, receiver.ty);
                let found = match self.mono.types.kind(base).clone() {
                    TyKind::Adt { def, .. } => self
                        .mono
                        .def_names
                        .get(&def)
                        .and_then(|name| self.mono.methods.get(&(name.clone(), method.clone())).copied()),
                    _ => None,
                };
                match found {
                    Some(fn_id) => {
                        e.kind = HirExprKind::MethodCall {
                            receiver: Box::new((**receiver).clone()),
                            method: fn_id,
                            args: std::mem::take(args),
                        };
                    }
                    None if self.in_template => {}
                    None => {
                        self.mono.unresolved_trait_calls.push((
                            trait_name.clone(),
                            method.clone(),
                            *method_span,
                        ));
                    }
                }
            }
            HirExprKind::MethodCall { receiver, method, args } => {
                self.rewrite_expr(receiver);
                for a in args.iter_mut() {
                    self.rewrite_expr(a);
                }
                // A receiver's own type is the answer, and so is the type the call
                // is used at: `let b = make(9i64)` records the receiver as `B<T>`
                // at check time, but the value it holds is a `B<i64>`, and
                // `b.get()` is used where an `i64` is expected.
                let base = peel_refs(self.mono.types, receiver.ty);
                if self.mono.types.first_param(base).is_some() {
                    // The instance may also come from the method's own result: a
                    // method that answers `T` used where an `i64` is expected says
                    // the receiver is the `T = i64` instance.
                    let names = self
                        .mono
                        .impl_generics
                        .get(method)
                        .cloned()
                        .unwrap_or_default();
                    let from_result = self
                        .mono
                        .hir_ret(*method)
                        .and_then(|ret| fill_from_result(self.mono.types, base, ret, e.ty, &names));
                    let hint = from_result.unwrap_or(e.ty);
                    let settled = self.mono.settle_binding(base, receiver.ty, hint);
                    if !crate::sema::mentions_unresolved_param(self.mono.types, settled) {
                        receiver.ty = settled;
                    }
                }
                let Some(instance_args) = self.mono.infer_method(*method, receiver.ty) else {
                    return;
                };
                let resolved = if self.collect {
                    self.mono.demand(*method, &instance_args);
                    None
                } else {
                    self.mono.lookup(*method, &instance_args)
                };
                if let Some(concrete) = resolved {
                    if concrete != *method {
                        *method = concrete;
                    }
                }
                // The call's own type is written in terms of the `impl`'s
                // parameters: `fn second(&self) -> B` records a bare `B`, which
                // the binding it initialises would then keep, and a parameter
                // nothing resolved has no size. Substituting the instance's
                // arguments is what turns it into the `bool` the copy answers.
                if let Some(names) = self.mono.impl_generics.get(method).cloned() {
                    if names.len() == instance_args.len() {
                        let subst: std::collections::HashMap<String, TyId> = names
                            .into_iter()
                            .zip(instance_args.iter().copied())
                            .collect();
                        e.ty = self.mono.types.substitute(e.ty, &subst);
                    }
                }
            }
            // Everything else only needs its children visited.
            _ => self.rewrite_children(e),
        }
    }

    /// Visits the sub-expressions of every node except the call forms, which
    /// `rewrite_expr` handles itself so it can rewrite them afterwards.
    fn rewrite_children(&mut self, e: &mut HirExpr) {
        match &mut e.kind {
            HirExprKind::Unary { operand, .. } => self.rewrite_expr(operand),
            HirExprKind::Binary { lhs, rhs, .. } => {
                self.rewrite_expr(lhs);
                self.rewrite_expr(rhs);
            }
            HirExprKind::Assign { place, value } | HirExprKind::AssignOp { place, value, .. } => {
                self.rewrite_expr(place);
                self.rewrite_expr(value);
            }
            HirExprKind::Ref { place, .. } => self.rewrite_expr(place),
            HirExprKind::Deref(inner) | HirExprKind::Move(inner) | HirExprKind::Try(inner) => {
                self.rewrite_expr(inner)
            }
            HirExprKind::Cast { expr, .. } => self.rewrite_expr(expr),
            HirExprKind::PtrOffset { ptr, count } => {
                self.rewrite_expr(ptr);
                self.rewrite_expr(count);
            }
            HirExprKind::StructLit { fields, base, .. } => {
                for f in fields.iter_mut() {
                    self.rewrite_expr(&mut f.value);
                }
                if let Some(base) = base {
                    self.rewrite_expr(base);
                }
            }
            HirExprKind::TupleVariant { args, .. } => {
                for a in args.iter_mut() {
                    self.rewrite_expr(a);
                }
            }
            HirExprKind::StructVariant { fields, .. } => {
                for f in fields.iter_mut() {
                    self.rewrite_expr(&mut f.value);
                }
            }
            HirExprKind::ArrayLit { elems } | HirExprKind::Tuple { elems } => {
                for x in elems.iter_mut() {
                    self.rewrite_expr(x);
                }
            }
            HirExprKind::ArrayRepeat { value, .. } => self.rewrite_expr(value),
            HirExprKind::RangeExpr { start, end, .. } => {
                self.rewrite_expr(start);
                self.rewrite_expr(end);
            }
            HirExprKind::IterSource { source, .. } => self.rewrite_expr(source),
            HirExprKind::Field { base, .. } | HirExprKind::TupleIndex { base, .. } => {
                self.rewrite_expr(base)
            }
            HirExprKind::Index { base, index } => {
                self.rewrite_expr(base);
                self.rewrite_expr(index);
            }
            HirExprKind::BuiltinMethod { receiver, args, .. } => {
                self.rewrite_expr(receiver);
                for a in args.iter_mut() {
                    self.rewrite_expr(a);
                }
            }
            HirExprKind::If { cond, then_block, else_branch } => {
                self.rewrite_expr(cond);
                self.rewrite_block(then_block);
                if let Some(else_branch) = else_branch {
                    self.rewrite_expr(else_branch);
                }
            }
            HirExprKind::Match { scrutinee, arms } => {
                self.rewrite_expr(scrutinee);
                for arm in arms.iter_mut() {
                    if let Some(g) = &mut arm.guard {
                        self.rewrite_expr(g);
                    }
                    self.rewrite_expr(&mut arm.body);
                }
            }
            HirExprKind::While { cond, body } => {
                self.rewrite_expr(cond);
                self.rewrite_block(body);
            }
            HirExprKind::Loop { body } | HirExprKind::Defer(body) | HirExprKind::Unsafe(body) => {
                self.rewrite_block(body)
            }
            HirExprKind::For { iter, body, .. } => {
                self.rewrite_expr(iter);
                self.rewrite_block(body);
            }
            HirExprKind::Block(block) => self.rewrite_block(block),
            HirExprKind::Return(v) | HirExprKind::Break(v) => {
                if let Some(v) = v {
                    self.rewrite_expr(v);
                }
            }
            HirExprKind::StrInterp { parts } => {
                for p in parts.iter_mut() {
                    if let HirStrPart::Value(v) = p {
                        self.rewrite_expr(v);
                    }
                }
            }
            // Leaves: literals, names, `Continue`, `Error`.
            _ => {}
        }
    }
}

/// Rewrites every type in a function's signature and body through `subst`.
fn substitute_fn_types(types: &mut TyCtxt, f: &mut HirFn, subst: &HashMap<String, TyId>) {
    if subst.is_empty() {
        return;
    }
        let params: Vec<TyId> = f.params.iter().map(|t| types.substitute(*t, subst)).collect();
    f.params = params;
    f.ret = types.substitute(f.ret, subst);
    for l in &mut f.locals {
        l.ty = types.substitute(l.ty, subst);
    }
    if let Some(body) = &mut f.body {
        substitute_block_types(types, body, subst);
    }
    }

fn substitute_block_types(types: &mut TyCtxt, block: &mut HirBlock, subst: &HashMap<String, TyId>) {
    for stmt in &mut block.stmts {
        match &mut stmt.kind {
            HirStmtKind::Let { init, .. } => {
                if let Some(init) = init {
                    substitute_expr_types(types, init, subst);
                }
            }
            HirStmtKind::Expr(e) => substitute_expr_types(types, e, subst),
        }
    }
    if let Some(tail) = &mut block.tail {
        substitute_expr_types(types, tail, subst);
    }
}

fn substitute_expr_types(types: &mut TyCtxt, e: &mut HirExpr, subst: &HashMap<String, TyId>) {
    e.ty = types.substitute(e.ty, subst);
    match &mut e.kind {
        HirExprKind::Unary { operand, .. } => substitute_expr_types(types, operand, subst),
        HirExprKind::Binary { lhs, rhs, .. } => {
            substitute_expr_types(types, lhs, subst);
            substitute_expr_types(types, rhs, subst);
        }
        HirExprKind::Assign { place, value } | HirExprKind::AssignOp { place, value, .. } => {
            substitute_expr_types(types, place, subst);
            substitute_expr_types(types, value, subst);
        }
        HirExprKind::Ref { place, .. } => substitute_expr_types(types, place, subst),
        HirExprKind::Deref(inner) | HirExprKind::Move(inner) | HirExprKind::Try(inner) => {
            substitute_expr_types(types, inner, subst)
        }
        HirExprKind::Cast { expr, to } => {
            substitute_expr_types(types, expr, subst);
            *to = types.substitute(*to, subst);
        }
        // `sizeof(T)` names a type directly, so it is the one expression whose
        // operand must be substituted even though nothing is evaluated.
        HirExprKind::Sizeof { of } => *of = types.substitute(*of, subst),
        HirExprKind::PtrOffset { ptr, count } => {
            substitute_expr_types(types, ptr, subst);
            substitute_expr_types(types, count, subst);
        }
        HirExprKind::TraitMethodCall { receiver, args, .. } => {
            substitute_expr_types(types, receiver, subst);
            for a in args.iter_mut() {
                substitute_expr_types(types, a, subst);
            }
        }
        HirExprKind::Call { callee, args } => {
            substitute_expr_types(types, callee, subst);
            for a in args.iter_mut() {
                substitute_expr_types(types, a, subst);
            }
        }
        HirExprKind::MethodCall { receiver, args, .. } => {
            substitute_expr_types(types, receiver, subst);
            for a in args.iter_mut() {
                substitute_expr_types(types, a, subst);
            }
        }
        HirExprKind::BuiltinMethod { receiver, args, .. } => {
            substitute_expr_types(types, receiver, subst);
            for a in args.iter_mut() {
                substitute_expr_types(types, a, subst);
            }
        }
        HirExprKind::StructLit { fields, base, .. } => {
            for f in fields.iter_mut() {
                substitute_expr_types(types, &mut f.value, subst);
            }
            if let Some(base) = base {
                substitute_expr_types(types, base, subst);
            }
        }
        HirExprKind::TupleVariant { args, .. } => {
            for a in args.iter_mut() {
                substitute_expr_types(types, a, subst);
            }
        }
        HirExprKind::StructVariant { fields, .. } => {
            for f in fields.iter_mut() {
                substitute_expr_types(types, &mut f.value, subst);
            }
        }
        HirExprKind::ArrayLit { elems } | HirExprKind::Tuple { elems } => {
            for x in elems.iter_mut() {
                substitute_expr_types(types, x, subst);
            }
        }
        HirExprKind::ArrayRepeat { value, .. } => substitute_expr_types(types, value, subst),
        HirExprKind::RangeExpr { start, end, elem, .. } => {
            substitute_expr_types(types, start, subst);
            substitute_expr_types(types, end, subst);
            *elem = types.substitute(*elem, subst);
        }
        HirExprKind::IterSource { source, elem } => {
            substitute_expr_types(types, source, subst);
            *elem = types.substitute(*elem, subst);
        }
        HirExprKind::Field { base, .. } | HirExprKind::TupleIndex { base, .. } => {
            substitute_expr_types(types, base, subst)
        }
        HirExprKind::Index { base, index } => {
            substitute_expr_types(types, base, subst);
            substitute_expr_types(types, index, subst);
        }
        HirExprKind::If { cond, then_block, else_branch } => {
            substitute_expr_types(types, cond, subst);
            substitute_block_types(types, then_block, subst);
            if let Some(else_branch) = else_branch {
                substitute_expr_types(types, else_branch, subst);
            }
        }
        HirExprKind::Match { scrutinee, arms } => {
            substitute_expr_types(types, scrutinee, subst);
            for arm in arms.iter_mut() {
                substitute_pattern_types(types, &mut arm.pattern, subst);
                if let Some(g) = &mut arm.guard {
                    substitute_expr_types(types, g, subst);
                }
                substitute_expr_types(types, &mut arm.body, subst);
            }
        }
        HirExprKind::While { cond, body } => {
            substitute_expr_types(types, cond, subst);
            substitute_block_types(types, body, subst);
        }
        HirExprKind::Loop { body } | HirExprKind::Defer(body) | HirExprKind::Unsafe(body) => {
            substitute_block_types(types, body, subst)
        }
        HirExprKind::For { iter, body, .. } => {
            substitute_expr_types(types, iter, subst);
            substitute_block_types(types, body, subst);
        }
        HirExprKind::Block(block) => substitute_block_types(types, block, subst),
        HirExprKind::Return(v) | HirExprKind::Break(v) => {
            if let Some(v) = v {
                substitute_expr_types(types, v, subst);
            }
        }
        HirExprKind::StrInterp { parts } => {
            for p in parts.iter_mut() {
                if let HirStrPart::Value(v) = p {
                    substitute_expr_types(types, v, subst);
                }
            }
        }
        _ => {}
    }
}

fn substitute_pattern_types(types: &mut TyCtxt, p: &mut HirPattern, subst: &HashMap<String, TyId>) {
    p.ty = types.substitute(p.ty, subst);
    match &mut p.kind {
        HirPatternKind::Tuple(items) | HirPatternKind::Or(items) => {
            for item in items.iter_mut() {
                substitute_pattern_types(types, item, subst);
            }
        }
        HirPatternKind::Struct { fields, .. } => {
            for f in fields.iter_mut() {
                substitute_pattern_types(types, &mut f.pattern, subst);
            }
        }
        HirPatternKind::Variant { fields, .. } => match fields {
            VariantPatternFields::Tuple(items) => {
                for item in items.iter_mut() {
                    substitute_pattern_types(types, item, subst);
                }
            }
            VariantPatternFields::Struct { fields, .. } => {
                for f in fields.iter_mut() {
                    substitute_pattern_types(types, &mut f.pattern, subst);
                }
            }
            VariantPatternFields::Unit => {}
        },
        _ => {}
    }
}

/// A span that no real source produces, for synthesised code.
#[allow(dead_code)]
pub fn synthetic() -> Span {
    Span::DUMMY
}

/// Pairs every `let` binding with the type its initialiser now has, when the
/// binding's type still names a parameter.
fn collect_let_fixes(
    types: &mut TyCtxt,
    block: &HirBlock,
    hints: &std::collections::HashMap<usize, TyId>,
    out: &mut Vec<(usize, TyId)>,
) {
    for stmt in &block.stmts {
        match &stmt.kind {
            HirStmtKind::Let { bindings, init, pattern } => {
                if let Some(init) = init {
                    let hint = match &pattern.kind {
                        HirPatternKind::Binding { local, .. } => hints.get(&(local.0 as usize)).copied(),
                        _ => None,
                    };
                    if let Some(b) = bindings.first() {
                        out.push((b.0 as usize, settle_binding(types, init.ty, init.ty, hint)));
                    }
                }
            }
            HirStmtKind::Expr(e) => collect_let_fixes_expr(types, e, hints, out),
        }
    }
    if let Some(tail) = &block.tail {
        collect_let_fixes_expr(types, tail, hints, out);
    }
}

fn collect_let_fixes_expr(
    types: &mut TyCtxt,
    e: &HirExpr,
    hints: &std::collections::HashMap<usize, TyId>,
    out: &mut Vec<(usize, TyId)>,
) {
    match &e.kind {
        HirExprKind::If { then_block, else_branch, .. } => {
            collect_let_fixes(types, then_block, hints, out);
            if let Some(else_branch) = else_branch {
                collect_let_fixes_expr(types, else_branch, hints, out);
            }
        }
        HirExprKind::While { body, .. }
        | HirExprKind::Loop { body }
        | HirExprKind::Defer(body)
        | HirExprKind::Unsafe(body)
        | HirExprKind::For { body, .. }
        | HirExprKind::Block(body) => collect_let_fixes(types, body, hints, out),
        HirExprKind::Match { arms, .. } => {
            for arm in arms.iter() {
                collect_let_fixes_expr(types, &arm.body, hints, out);
            }
        }
        _ => {}
    }
    for child in crate::hir::child_exprs(e) {
        collect_let_fixes_expr(types, child, hints, out);
    }
}

/// The instance a receiver must have for a method's result to be `want`.
///
/// `fn get(self) -> T` on `impl<T> B<T>` answers a bare `T`, so a use that wants
/// an `i64` says the receiver is `T = i64`. A result that names no parameter, or
/// one that wraps the parameter in a different declaration, gives nothing.
fn fill_from_result(
    types: &mut TyCtxt,
    recv_base: TyId,
    declared_ret: TyId,
    want: TyId,
    names: &[String],
) -> Option<TyId> {
    if crate::sema::mentions_unresolved_param(types, want) {
        return None;
    }
    let TyKind::Adt { def, args } = types.kind(recv_base).clone() else { return None };
    if args.len() != names.len() {
        return None;
    }
    let TyKind::Param { name } = types.kind(declared_ret).clone() else { return None };
    let position = names.iter().position(|g| *g == name)?;
    let mut fixed = args;
    fixed[position] = want;
    Some(types.adt(def, fixed))
}

/// Gives every reference to a local the local's settled type.
///
/// The type of `b` in `b.get()` is a field of the receiver expression, decided
/// when the expression was checked. Once the local's type is settled that copy is
/// stale, and the call behind it resolves against the stale one.
fn retype_local_refs(block: &mut HirBlock, locals: &[(usize, TyId)]) -> bool {
    let mut changed = false;
    for stmt in &mut block.stmts {
        match &mut stmt.kind {
            HirStmtKind::Let { init, .. } => {
                if let Some(init) = init {
                    changed |= retype_local_refs_expr(init, locals);
                }
            }
            HirStmtKind::Expr(e) => changed |= retype_local_refs_expr(e, locals),
        }
    }
    if let Some(tail) = &mut block.tail {
        changed |= retype_local_refs_expr(tail, locals);
    }
    changed
}

fn retype_local_refs_expr(e: &mut HirExpr, locals: &[(usize, TyId)]) -> bool {
    let mut changed = false;
    if let HirExprKind::Local(id) = e.kind {
        if let Some((_, ty)) = locals.get(id.0 as usize) {
            if e.ty != *ty {
                e.ty = *ty;
                changed = true;
            }
        }
    }
    for child in crate::hir::child_exprs_mut(e) {
        changed |= retype_local_refs_expr(child, locals);
    }
    changed
}

/// The type a `let` binding finally has, once its initialiser is concrete.
///
/// An initialiser that answered `B<T>` while its value carried `B<i64>` naming
/// the same declaration is the case this exists for: a generic function whose
/// result names its own parameter inside another type answers with the template,
/// because the call is checked before it is instantiated. The declaration is the
/// same on both sides, so the known instance is the answer.
fn settle_binding(types: &mut TyCtxt, ty: TyId, init: TyId, hint: Option<TyId>) -> TyId {
    if !mentions_unresolved_param(types, ty) {
        return ty;
    }
    // A parameter with no structure around it is the one `fn id<T>(v: T) -> T`
    // answers with, and the value itself is the answer. A method that answers the
    // parameter of its `impl` -- `fn second(&self) -> B` -- is the same case: the
    // call's recorded type is the template's `B`, and the copy it resolved to
    // answers a `bool`, because the `impl`'s arguments are what `B` stands for.
    // The declared result type is no help there, so the resolved copy's own is
    // asked instead.
    if matches!(types.kind(ty), TyKind::Param { .. }) && !mentions_unresolved_param(types, init) {
        return init;
    }
    // Otherwise the binding names a declaration whose instance the initialiser
    // or the annotation wrote out, and that instance is the answer.
    for candidate in [Some(init), hint].into_iter().flatten() {
        if let (
            TyKind::Adt { def: d1, .. },
            TyKind::Adt { def: d2, args: candidate_args },
        ) = (types.kind(ty).clone(), types.kind(candidate).clone())
        {
            if d1 == d2 && instance_is_known(types, &candidate_args) {
                return types.adt(d1, candidate_args);
            }
        }
    }
    ty
}

fn args_mention_param(types: &TyCtxt, args: &[TyId]) -> bool {
    args.iter().any(|a| types.first_param(*a).is_some())
}

/// True when an instance has already been decided, rather than still naming a
/// parameter.
///
/// `B<i64>` is one; `B<T>` and `B` (no arguments at all) are not. The empty case
/// matters: a type written without arguments is what `make` answers before
/// instantiation, and it is just as undecided as one holding a parameter.
fn instance_is_known(types: &TyCtxt, args: &[TyId]) -> bool {
    !args.is_empty() && !args_mention_param(types, args)
}

/// True when `ty` is, or contains, a type parameter nothing has decided.
///
/// This is deliberately wider than the checker's version of the same question: a
/// `let` that took its type from a generic call records the parameter itself, not
/// an error standing in for one, so only looking for errors would miss it.
fn mentions_unresolved_param(types: &TyCtxt, ty: TyId) -> bool {
    types.first_param(ty).is_some() || crate::sema::mentions_unresolved_param(types, ty)
}
