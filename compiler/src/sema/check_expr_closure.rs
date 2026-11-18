//! Closure checking and lowering into ordinary HIR.
//!
//! A closure is not a new HIR or MIR form. It becomes two ordinary things:
//!
//! * a compiler-generated function (the *trampoline*) whose body is the closure
//!   body, and
//! * a compiler-generated struct (the *environment*) with one field per captured
//!   value, or, when nothing is captured, a plain function pointer.
//!
//! Everything downstream -- monomorphisation, MIR, drop elaboration, borrow
//! checking, both backends -- already knows those shapes. The only genuinely new
//! semantic rule is in `check_call`: calling an environment struct means calling
//! its trampoline with the struct as the first argument.

use std::collections::{HashMap, HashSet};

use crate::ast;
use crate::hir::*;
use crate::sema::check_expr::FnChecker;
use crate::sema::scope::LocalEntry;
use crate::source::Span;
use crate::types::{DefId, TyId, TyKind};

impl<'c, 's> FnChecker<'c, 's> {
    /// Checks `|params| body` (already unwrapped from a possible `move` prefix).
    pub fn check_closure(
        &mut self,
        params: &[ast::ClosureParam],
        body: &ast::Expr,
        hint: Option<TyId>,
        span: Span,
    ) -> HirExpr {
        // A typed context is the inference source for unannotated parameters.
        // This is how `let f: fn(i64) -> i64 = |x| x + 1` works.
        let hinted: Option<Vec<TyId>> = hint.and_then(|t| match self.cx.cx.kind(t).clone() {
            TyKind::FnPtr { params: ps, .. } if ps.len() == params.len() => Some(ps),
            _ => None,
        });

        let mut param_tys = Vec::with_capacity(params.len());
        for (i, p) in params.iter().enumerate() {
            let ty = if let Some(t) = &p.ty {
                self.cx.resolve_type(t, &self.generics)
            } else if let Some(ps) = &hinted {
                ps[i]
            } else {
                self.error(
                    "E1019",
                    p.name_span,
                    format!("cannot infer the type of closure parameter `{}`", p.name),
                );
                self.cx.cx.error()
            };
            param_tys.push(ty);
        }

        // Snapshot the outer scope before entering the body. Every outer local is
        // provisionally declared in the generated function; only the ones the
        // body actually mentions become captures, which keeps a closure from
        // consuming variables it never uses.
        let outer = self.scopes.all_entries();
        let generics = self.generics.clone();
        let fn_id = FnId(self.cx.hir.fns.len() as u32);
        let def_id = DefId((self.cx.hir.structs.len() + self.cx.hir.enums.len()) as u32);
        let name = format!("__umay_closure_{}", fn_id.0);

        self.cx.hir.fns.push(HirFn {
            id: fn_id,
            name: name.clone(),
            span: Span::DUMMY,
            generics: generics.clone(),
            params: Vec::new(),
            ret: self.cx.cx.error(),
            has_self: false,
            self_mutable: false,
            self_by_ref: false,
            locals: Vec::new(),
            body: None,
            is_pub: false,
            owner: None,
            link_name: None,
            variadic: false,
            is_async: false,
            declared_local_tys: HashMap::new(),
        });

        let mut inner = FnChecker::new(&mut *self.cx, Some(fn_id));
        inner.generics = generics;
        // The return type is inferred from the body, so an explicit `return`
        // inside it has nothing to be checked against yet.
        inner.ret_ty = inner.cx.cx.error();

        let mut provisional: Vec<(String, LocalEntry, LocalId)> = Vec::new();
        for (n, entry) in &outer {
            let id = inner.declare_param(n, entry.ty, Span::DUMMY, entry.mutable);
            provisional.push((n.clone(), *entry, id));
        }
        let param_ids: Vec<LocalId> = params
            .iter()
            .zip(&param_tys)
            .map(|(p, ty)| inner.declare_param(&p.name, *ty, p.name_span, false))
            .collect();
        let body_first_local = inner.locals.len();

        let tail = inner.check_expr(body);
        let ret_ty = tail.ty;
        let mut body_hir = HirBlock {
            stmts: Vec::new(),
            tail: Some(Box::new(tail)),
            span: body.span,
            locals_introduced: (inner.locals.len() - body_first_local) as u32,
        };
        let defaults = inner.literal_defaults();
        crate::hir::rewrite_types(&mut body_hir, &defaults);
        let locals = inner.finish_locals();

        // Keep only the outer locals the body actually referenced.
        let provisional_ids: HashSet<LocalId> = provisional.iter().map(|(_, _, id)| *id).collect();
        let mut used = HashSet::new();
        collect_used_block(&body_hir, &mut used);
        let kept: Vec<&(String, LocalEntry, LocalId)> = provisional
            .iter()
            .filter(|(_, _, id)| used.contains(id) && provisional_ids.contains(id))
            .collect();

        // Decide how each capture is stored in the environment (ADR 0017 §2).
        // The body has already been checked, so its HIR tells us whether a
        // capture is only read or is written through. Least privilege:
        //   * Copy + written  -> &mut T (mutation must reach the original);
        //   * Copy + read     -> &T    (do not move the original);
        //   * non-Copy        -> T     (keep the old by-value behaviour).
        // Mutating a non-Copy capture is a documented limitation (E1020).
        let mut assigned: HashSet<LocalId> = HashSet::new();
        collect_assigned_block(&body_hir, &mut assigned);
        let mut cap_ty: HashMap<LocalId, TyId> = HashMap::new();
        let mut cap_borrow: HashMap<LocalId, (bool, TyId)> = HashMap::new();
        for (_, entry, old) in &kept {
            let ty = entry.ty;
            let mode = if self.cx.cx.is_copy(ty) {
                let mutable = assigned.contains(old);
                let ref_ty = if mutable { self.cx.cx.ref_mut_to(ty) } else { self.cx.cx.ref_to(ty) };
                cap_borrow.insert(*old, (mutable, ref_ty));
                ref_ty
            } else {
                if assigned.contains(old) {
                    self.error(
                        "E1020",
                        span,
                        "cannot mutate a captured non-Copy value; mutable closure captures are not supported yet",
                    );
                }
                ty
            };
            cap_ty.insert(*old, mode);
        }

        // Re-number locals into the trampoline's own table: captures first (they
        // are its leading parameters), then the closure parameters, then the
        // locals the body introduced. The body's HIR is rewritten to match.
        let mut remap: HashMap<LocalId, LocalId> = HashMap::new();
        let mut new_locals = Vec::new();
        let mut next = 0u32;
        let mut push = |old: LocalId, info: LocalInfo, map: &mut HashMap<LocalId, LocalId>| {
            let new = LocalId(next);
            next += 1;
            map.insert(old, new);
            new_locals.push(info);
        };
        for (n, entry, old) in &kept {
            let borrow_mut = cap_borrow.get(old).map(|(m, _)| *m).unwrap_or(false);
            push(
                *old,
                LocalInfo {
                    name: n.clone(),
                    ty: cap_ty[old],
                    mutable: entry.mutable || borrow_mut,
                    span: Span::DUMMY,
                },
                &mut remap,
            );
        }
        for (p, old) in params.iter().zip(&param_ids) {
            let ty = locals[old.0 as usize].ty;
            push(
                *old,
                LocalInfo { name: p.name.clone(), ty, mutable: false, span: p.name_span },
                &mut remap,
            );
        }
        for (old, info) in locals.iter().enumerate().skip(body_first_local) {
            push(LocalId(old as u32), info.clone(), &mut remap);
        }
        remap_block(&mut body_hir, &remap, &cap_borrow);

        let closure_param_tys: Vec<TyId> = param_ids
            .iter()
            .map(|id| new_locals[remap[id].0 as usize].ty)
            .collect();
        let env_tys: Vec<TyId> = kept.iter().map(|(_, _, old)| cap_ty[old]).collect();
        let all_params: Vec<TyId> = env_tys.iter().chain(&closure_param_tys).copied().collect();

        let closure_ty = if kept.is_empty() {
            self.cx.cx.fn_ptr(closure_param_tys.clone(), ret_ty)
        } else {
            let fields = env_tys
                .iter()
                .enumerate()
                .map(|(i, ty)| FieldInfo {
                    name: format!("__cap{i}"),
                    span: Span::DUMMY,
                    ty: *ty,
                    ty_span: Span::DUMMY,
                })
                .collect();
            let is_copy = env_tys.iter().all(|ty| self.cx.cx.is_copy(*ty));
            self.cx.hir.structs.push(StructInfo {
                name: name.clone(),
                span: Span::DUMMY,
                def_id,
                generics: Vec::new(),
                fields,
                is_copy,
            });
            self.cx.hir.defs.push(DefInfo {
                name: name.clone(),
                span: Span::DUMMY,
                def_id,
                kind: DefKind::Struct,
                is_pub: false,
            });
            self.cx.closure_fn_by_def.insert(def_id, fn_id);
            // `stdlib/fn.um` declares the one-argument call traits. Register
            // this closure's trampoline as their synthesized impl so bounds and
            // method lookup see the same evidence a user-written impl would.
            // Call syntax itself does not depend on these entries: `check_call`
            // rewrites it directly to the trampoline.
            if params.len() == 1 {
                for (trait_name, method) in [
                    ("Fn", "call"),
                    ("FnMut", "call_mut"),
                    ("FnOnce", "call_once"),
                ] {
                    self.cx.register_synthetic_impl(trait_name, &name, method, fn_id);
                }
            }
            self.cx.cx.adt(def_id, Vec::new())
        };

        if let Some(f) = self.cx.hir.fns.iter_mut().find(|f| f.id == fn_id) {
            f.params = all_params;
            f.ret = ret_ty;
            f.locals = new_locals;
            f.body = Some(body_hir);
        }

        let kind = if kept.is_empty() {
            HirExprKind::FnRef(fn_id)
        } else {
            let fields = kept
                .iter()
                .enumerate()
                .map(|(i, (_, entry, old))| {
                    let value = match cap_borrow.get(old) {
                        Some((mutable, ref_ty)) => HirExpr {
                            kind: HirExprKind::Ref {
                                mutable: *mutable,
                                place: Box::new(HirExpr {
                                    kind: HirExprKind::Local(entry.id),
                                    ty: entry.ty,
                                    span: Span::DUMMY,
                                }),
                            },
                            ty: *ref_ty,
                            span: Span::DUMMY,
                        },
                        None => HirExpr {
                            kind: HirExprKind::Local(entry.id),
                            ty: entry.ty,
                            span: Span::DUMMY,
                        },
                    };
                    HirFieldInit {
                        name: format!("__cap{i}"),
                        span: Span::DUMMY,
                        index: i as u32,
                        value,
                    }
                })
                .collect();
            HirExprKind::StructLit { def: def_id, fields, base: None }
        };
        HirExpr { kind, ty: closure_ty, span }
    }
}

fn collect_used(e: &HirExpr, out: &mut HashSet<LocalId>) {
    if let HirExprKind::Local(id) = &e.kind {
        out.insert(*id);
    }
    for child in child_exprs(e) {
        collect_used(child, out);
    }
}

/// Scan statements as well as the tail: a capture can be mentioned anywhere in
/// the body, not just in its final expression.
fn collect_used_block(block: &HirBlock, out: &mut HashSet<LocalId>) {
    for st in &block.stmts {
        match &st.kind {
            HirStmtKind::Let { init, .. } => {
                if let Some(e) = init {
                    collect_used(e, out);
                }
            }
            HirStmtKind::Expr(e) => collect_used(e, out),
        }
    }
    if let Some(t) = &block.tail {
        collect_used(t, out);
    }
}

/// The base local a place expression ultimately refers to (`n`, `n.f`, `*n`).
fn place_root(e: &HirExpr) -> Option<LocalId> {
    match &e.kind {
        HirExprKind::Local(id) => Some(*id),
        HirExprKind::Deref(inner)
        | HirExprKind::Field { base: inner, .. }
        | HirExprKind::TupleIndex { base: inner, .. }
        | HirExprKind::Index { base: inner, .. } => place_root(inner),
        _ => None,
    }
}

fn collect_assigned(e: &HirExpr, out: &mut HashSet<LocalId>) {
    if let HirExprKind::Assign { place, .. } | HirExprKind::AssignOp { place, .. } = &e.kind {
        if let Some(id) = place_root(place) {
            out.insert(id);
        }
    }
    for child in child_exprs(e) {
        collect_assigned(child, out);
    }
}

fn collect_assigned_block(block: &HirBlock, out: &mut HashSet<LocalId>) {
    for st in &block.stmts {
        match &st.kind {
            HirStmtKind::Let { init, .. } => {
                if let Some(e) = init {
                    collect_assigned(e, out);
                }
            }
            HirStmtKind::Expr(e) => collect_assigned(e, out),
        }
    }
    if let Some(t) = &block.tail {
        collect_assigned(t, out);
    }
}

fn remap_id(id: &mut LocalId, map: &HashMap<LocalId, LocalId>) {
    if let Some(new) = map.get(id) {
        *id = *new;
    }
}

/// `borrow` maps a captured local's old id to `(is_mut, reference_ty)`. A use of
/// such a local in the trampoline body is a dereference of its reference param.
fn remap_block(
    block: &mut HirBlock,
    map: &HashMap<LocalId, LocalId>,
    borrow: &HashMap<LocalId, (bool, TyId)>,
) {
    for st in &mut block.stmts {
        match &mut st.kind {
            HirStmtKind::Let { bindings, pattern, init } => {
                for id in bindings {
                    remap_id(id, map);
                }
                remap_pattern(pattern, map);
                if let Some(e) = init {
                    remap_expr(e, map, borrow);
                }
            }
            HirStmtKind::Expr(e) => remap_expr(e, map, borrow),
        }
    }
    if let Some(t) = &mut block.tail {
        remap_expr(t, map, borrow);
    }
}

fn remap_pattern(pattern: &mut HirPattern, map: &HashMap<LocalId, LocalId>) {
    match &mut pattern.kind {
        HirPatternKind::Binding { local, .. } => remap_id(local, map),
        HirPatternKind::Tuple(items) | HirPatternKind::Or(items) | HirPatternKind::Slice { elems: items, .. } => {
            for p in items {
                remap_pattern(p, map);
            }
        }
        HirPatternKind::Struct { fields, .. } => {
            for f in fields {
                remap_pattern(&mut f.pattern, map);
            }
        }
        HirPatternKind::Variant { fields, .. } => match fields {
            VariantPatternFields::Unit => {}
            VariantPatternFields::Tuple(items) => {
                for p in items {
                    remap_pattern(p, map);
                }
            }
            VariantPatternFields::Struct { fields, .. } => {
                for f in fields {
                    remap_pattern(&mut f.pattern, map);
                }
            }
        },
        _ => {}
    }
}

fn remap_expr(
    e: &mut HirExpr,
    map: &HashMap<LocalId, LocalId>,
    borrow: &HashMap<LocalId, (bool, TyId)>,
) {
    // Replace a borrow-captured local with `*param` before the general walk, so
    // the injected inner Local carries the new id and the reference type. Its
    // own type (the original T) is left intact on the outer Deref.
    let wrapped = if let HirExprKind::Local(id) = &e.kind {
        borrow.get(id).map(|&(_, ref_ty)| {
            let new = map.get(id).copied().unwrap_or(*id);
            HirExprKind::Deref(Box::new(HirExpr {
                kind: HirExprKind::Local(new),
                ty: ref_ty,
                span: Span::DUMMY,
            }))
        })
    } else {
        None
    };
    if let Some(kind) = wrapped {
        e.kind = kind;
        return;
    }

    match &mut e.kind {
        HirExprKind::Local(id) => remap_id(id, map),
        HirExprKind::Block(b) => remap_block(b, map, borrow),
        HirExprKind::If { cond, then_block, else_branch } => {
            remap_expr(cond, map, borrow);
            remap_block(then_block, map, borrow);
            if let Some(e) = else_branch {
                remap_expr(e, map, borrow);
            }
        }
        HirExprKind::While { cond, body } => {
            remap_expr(cond, map, borrow);
            remap_block(body, map, borrow);
        }
        HirExprKind::Loop { body } | HirExprKind::Defer(body) | HirExprKind::Unsafe(body) => {
            remap_block(body, map, borrow)
        }
        HirExprKind::For { pattern_locals, iter, body } => {
            for id in pattern_locals {
                remap_id(id, map);
            }
            remap_expr(iter, map, borrow);
            remap_block(body, map, borrow);
        }
        HirExprKind::Match { scrutinee, arms } => {
            remap_expr(scrutinee, map, borrow);
            for arm in arms {
                remap_pattern(&mut arm.pattern, map);
                if let Some(g) = &mut arm.guard {
                    remap_expr(g, map, borrow);
                }
                remap_expr(&mut arm.body, map, borrow);
            }
        }
        _ => {
            for child in child_exprs_mut(e) {
                remap_expr(child, map, borrow);
            }
        }
    }
}
