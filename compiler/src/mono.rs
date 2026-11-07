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
        Mono { types, worklist: Vec::new(), produced: Default::default(), decl_generics, impl_generics, next_id }
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
            let mut rw = Rewriter { mono: self, hir, collect: false };
            rw.rewrite_fn(i);
        }
    }

    /// Records the instances `hir.fns[index]` needs, without rewriting anything.
    fn collect_fn(&mut self, hir: &mut Hir, index: usize) {
        let mut rw = Rewriter { mono: self, hir, collect: true };
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
            let TyKind::Param { name } = self.types.kind(*p).clone() else { continue };
            let Some(pos) = names.iter().position(|n| *n == name) else { continue };
            if subst[pos].is_none() {
                subst[pos] = Some(*a);
            }
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

        if std::env::var("UMAY_TRACE_MONO").is_ok() {
            eprintln!("INST {} {:?} names {:?} args {:?}", instance.callee.0, hir.fns[index].name, names, instance.args);
            for t in &hir.fns[index].params {
                eprintln!("  param {} => {:?}", self.types.display(*t), self.types.kind(*t).clone());
                if let TyKind::Ref(inner) = self.types.kind(*t).clone() {
                    eprintln!("   inner {:?}", self.types.kind(inner).clone());
                    if let TyKind::Adt { args, .. } = self.types.kind(inner).clone() {
                        for a in args {
                            eprintln!("    arg {} => {:?}", self.types.display(a), self.types.kind(a).clone());
                        }
                    }
                }
            }
        }
        let mut copy = hir.fns[index].clone();
        copy.id = id;
        copy.name = mangled(&hir.fns[index].name, &instance.args);
        // The copy is no longer generic: its parameters have been decided, so
        // leaving them in place would make the checker's own rule refuse it.
        copy.generics.clear();
        if std::env::var("UMAY_TRACE_MONO").is_ok() {
            eprintln!("  subst {:?} ret {} first_param {:?}", subst.iter().map(|(k,v)| (k.clone(), v.0)).collect::<Vec<_>>(), self.types.display(hir.fns[index].ret), self.types.first_param(hir.fns[index].ret));
        }
        substitute_fn_types(self.types, &mut copy, &subst);
        if std::env::var("UMAY_TRACE_MONO").is_ok() {
            eprintln!("  copy params {:?}", copy.params.iter().map(|t| self.types.display(*t)).collect::<Vec<_>>());
        }
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
        // An argument that is still a parameter means this call was reached from
        // inside another generic body, not from code with a concrete type: it is
        // that template's own business and produces no instance. Recording it
        // would create a copy whose `T` is another `T`, which no caller can use.
        if args.iter().any(|a| self.types.first_param(*a).is_some()) {
            return None;
        }
        let instance = Instance::new(callee, args.to_vec());
        if let Some(&id) = self.produced.get(&instance) {
            return Some(id);
        }
        self.worklist.push(instance);
        None
    }

    /// The produced copy for an instance, if it exists.
    fn lookup(&self, callee: FnId, args: &[TyId]) -> Option<FnId> {
        if !self.is_generic(callee) {
            return Some(callee);
        }
        self.produced.get(&Instance::new(callee, args.to_vec())).copied()
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
}

impl<'m, 't, 'h> Rewriter<'m, 't, 'h> {
    /// Visits every expression in `hir.fns[index]`.
    fn rewrite_fn(&mut self, index: usize) {
        let mut body = std::mem::take(&mut self.hir.fns[index].body);
        if let Some(body) = &mut body {
            self.rewrite_block(body);
        }
        self.hir.fns[index].body = body;
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
                }
            }
            HirExprKind::MethodCall { receiver, method, args } => {
                self.rewrite_expr(receiver);
                for a in args.iter_mut() {
                    self.rewrite_expr(a);
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
    if std::env::var("UMAY_TRACE_MONO").is_ok() {
        eprintln!("   SUBST in ret {} params {:?} map {:?}", types.display(f.ret), f.params.iter().map(|t| types.display(*t)).collect::<Vec<_>>(), subst.iter().map(|(k,v)| (k.clone(), v.0)).collect::<Vec<_>>());
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
    if std::env::var("UMAY_TRACE_MONO").is_ok() {
        eprintln!("   SUBST out ret {} params {:?}", types.display(f.ret), f.params.iter().map(|t| types.display(*t)).collect::<Vec<_>>());
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
