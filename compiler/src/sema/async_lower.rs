//! First async lowering slice (ADR 0019).
//!
//! An `async fn name(...) -> T` becomes four ordinary HIR functions plus a
//! non-`Copy` state struct. No new MIR terminator and no backend change: the
//! generated functions are plain UMAY functions and the struct is a plain
//! struct, so monomorphisation, MIR, drop elaboration and both backends already
//! know how to compile them.
//!
//! * `__umay_async_<name>_<n>` — the state, one field per parameter.
//! * `__umay_thunk_<name>(*mut State) -> T` — the *original* body, with every
//!   parameter use rewritten to `(*state).field`. Side effects therefore happen
//!   on first poll, not when the future is created.
//! * `__umay_poll_<name>(*mut State, *mut Context) -> Poll<T>` — calls the thunk
//!   and wraps the result in `Poll::Ready`.
//! * `__umay_block_on_<name>(*mut State) -> T` — makes a context, polls once,
//!   matches `Ready(v) => v` and aborts on `Pending`.
//!
//! The async function itself returns a state value (a struct literal of its
//! parameters) and its declared return type is rewritten to that struct, so
//! `Future` bounds see an ordinary implementor registered with
//! `Checker::register_synthetic_impl`.
//!
//! A second, independent path handles `block_on(value)` where `value` is a
//! *concrete user* type with a registered `Future<T>` impl (e.g.
//! `PendingOnce` in `stdlib/future.um`). It synthesises
//! `__umay_block_on_user_<Type>(*mut <Type>) -> T`, which calls the impl's
//! concrete `poll` FnId directly (no dyn/vtable) and loops while it returns
//! `Poll::Pending`, unwrapping `Poll::Ready(v)` into `v`. The generated-async
//! path is tried first and is untouched.
//!
//! ponytail: this is deliberately the smallest honest slice. Ceilings, each
//! with its upgrade path:
//! * Only parameters are state fields — locals living across an await are not
//!   captured. Upgrade: reuse ADR 0017 capture analysis.
//! * `Pending` from a generated async fn still aborts through `panic`: its
//!   thunk never returns `Pending`, so the one-poll helper cannot resume it.
//!   The new looping path is ONLY for concrete user `Future` impls. Upgrade: a
//!   real state machine with one MIR state per suspension point plus a reactor.
//! * The user-Future loop is bounded busy-poll: no waker wake, no reactor, no
//!   spawn/select/timeout, no cancellation, no `Send`, no `Pin`. `make_context`
//!   hands back a no-op waker that is never notified. Upgrade: ADR 0019
//!   sections 3-6 in order.
//! * An awaited async fn must be declared before the awaiting fn: the helper
//!   table is filled while bodies are checked in declaration order. Upgrade:
//!   create the state struct at signature-resolution time.

use std::collections::HashMap;

use crate::ast;
use crate::diagnostics::Diagnostic;
use crate::hir::*;
use crate::sema::check_expr::FnChecker;
use crate::sema::Checker;
use crate::source::Span;
use crate::types::{DefId, TyId, TyKind};

/// Busy-poll budget for the concrete-user-Future `block_on` helper. Reaching it
/// panics rather than spinning forever. ponytail: a flat global cap — a real
/// reactor would block on the waker instead of counting iterations (ADR 0019).
const USER_POLL_LIMIT: u128 = 1_000_000;

/// Everything lowered from one `async fn`, keyed by its original `FnId`.
///
/// Only the def -> block_on half is read in this slice; the rest is the seam
/// the state-machine slices will use to find the thunk and output type.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
pub(crate) struct AsyncFnInfo {
    pub state_def: DefId,
    pub thunk: FnId,
    pub poll: FnId,
    pub block_on: FnId,
    pub output: TyId,
    pub state_ty: TyId,
}

impl<'a> Checker<'a> {
    /// Synthesizes the state struct, thunk, poll and block_on helper for one
    /// checked `async fn`. Returns the state type and definition so the caller
    /// can rewrite the original function into a state constructor.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn lower_async_fn(
        &mut self,
        fn_id: FnId,
        f: &ast::FnDecl,
        body: HirBlock,
        locals: Vec<LocalInfo>,
        declared_local_tys: HashMap<usize, TyId>,
        output: TyId,
        span: Span,
    ) -> (TyId, DefId) {
        let param_tys: Vec<TyId> = self
            .hir
            .fns
            .iter()
            .find(|x| x.id == fn_id)
            .map(|x| x.params.clone())
            .unwrap_or_default();
        let mut field_names: Vec<String> = Vec::new();
        if f.self_param.is_some() {
            field_names.push("self".to_string());
        }
        for (i, p) in f.params.iter().enumerate() {
            field_names.push(p.name.clone().unwrap_or_else(|| format!("__arg{i}")));
        }
        while field_names.len() < param_tys.len() {
            let i = field_names.len();
            field_names.push(format!("__arg{i}"));
        }
        field_names.truncate(param_tys.len());
        let param_count = param_tys.len();

        let counter = self.hir.structs.iter().filter(|s| s.name.starts_with("__umay_async_")).count();
        let base_name = f.name.replace("::", "_");
        let state_name = format!("__umay_async_{base_name}_{counter}");
        let state_def = DefId((self.hir.structs.len() + self.hir.enums.len()) as u32);
        let state_ty = self.cx.adt(state_def, Vec::new());
        let fields: Vec<FieldInfo> = field_names
            .iter()
            .zip(&param_tys)
            .map(|(n, t)| FieldInfo { name: n.clone(), span, ty: *t, ty_span: span })
            .collect();
        // ponytail: non-Copy on purpose — a future is a resource whose drop
        // will later have to run through drop elaboration (ADR 0007).
        self.hir.structs.push(StructInfo {
            name: state_name.clone(),
            span,
            def_id: state_def,
            generics: Vec::new(),
            fields,
            is_copy: false,
        });
        self.hir.defs.push(DefInfo {
            name: state_name.clone(),
            span,
            def_id: state_def,
            kind: DefKind::Struct,
            is_pub: false,
        });

        let ptr_ty = self.cx.raw_ptr_mut(state_ty);

        // ---- thunk: the original body, parameters read out of *state ----
        let thunk_id = FnId(self.hir.fns.len() as u32);
        let thunk_name = format!("__umay_thunk_{base_name}");
        let mut thunk_locals =
            vec![LocalInfo { name: "__state".into(), ty: ptr_ty, mutable: false, span }];
        thunk_locals.extend(locals.iter().cloned());
        let mut thunk_declared: HashMap<usize, TyId> = HashMap::new();
        for (i, t) in declared_local_tys {
            thunk_declared.insert(i + 1, t);
        }
        let mut thunk_body = body;
        let ctx = ThunkCtx {
            param_count,
            param_tys: param_tys.clone(),
            field_names: field_names.clone(),
            state_ty,
            ptr_ty,
            span,
        };
        lower_thunk_block(&mut thunk_body, &ctx);
        self.hir.fns.push(HirFn {
            id: thunk_id,
            name: thunk_name,
            span,
            generics: Vec::new(),
            params: vec![ptr_ty],
            ret: output,
            has_self: false,
            self_mutable: false,
            self_by_ref: false,
            locals: thunk_locals,
            body: Some(thunk_body),
            is_pub: false,
            owner: None,
            link_name: None,
            variadic: false,
            is_async: false,
            declared_local_tys: thunk_declared,
        });

        // ---- poll: thunk(state) wrapped in Poll::Ready ----
        let poll_def = self.def_by_name.get("Poll").copied();
        let context_def = self.def_by_name.get("Context").copied();
        let context_ty = context_def
            .map(|d| self.cx.adt(d, Vec::new()))
            .unwrap_or_else(|| self.cx.error());
        let poll_ty = poll_def
            .map(|d| self.cx.adt(d, vec![output]))
            .unwrap_or_else(|| self.cx.error());
        let cx_ty = self.cx.raw_ptr_mut(context_ty);
        let variants = poll_def
            .and_then(|d| self.hir.enums.iter().find(|e| e.def_id == d))
            .map(|e| e.variants.clone())
            .unwrap_or_default();
        let ready_index = variants.iter().position(|v| v.name == "Ready").unwrap_or(0) as u32;
        let pending_index = variants.iter().position(|v| v.name == "Pending").unwrap_or(1) as u32;
        let poll_def_id = poll_def.unwrap_or(DefId(u32::MAX));

        let poll_id = FnId(self.hir.fns.len() as u32);
        let poll_name = format!("__umay_poll_{base_name}");
        let thunk_call = HirExpr {
            kind: HirExprKind::Call {
                callee: Box::new(HirExpr {
                    kind: HirExprKind::FnRef(thunk_id),
                    ty: self.cx.fn_ptr(vec![ptr_ty], output),
                    span,
                }),
                args: vec![HirExpr {
                    kind: HirExprKind::Local(LocalId(0)),
                    ty: ptr_ty,
                    span,
                }],
            },
            ty: output,
            span,
        };
        let ready = HirExpr {
            kind: HirExprKind::TupleVariant {
                def: poll_def_id,
                index: ready_index,
                args: vec![thunk_call],
            },
            ty: poll_ty,
            span,
        };
        self.hir.fns.push(HirFn {
            id: poll_id,
            name: poll_name.clone(),
            span,
            generics: Vec::new(),
            params: vec![ptr_ty, cx_ty],
            ret: poll_ty,
            has_self: false,
            self_mutable: false,
            self_by_ref: false,
            locals: vec![
                LocalInfo { name: "__state".into(), ty: ptr_ty, mutable: false, span },
                LocalInfo { name: "__cx".into(), ty: cx_ty, mutable: false, span },
            ],
            body: Some(HirBlock {
                stmts: Vec::new(),
                tail: Some(Box::new(ready)),
                span,
                locals_introduced: 0,
            }),
            is_pub: false,
            owner: None,
            link_name: None,
            variadic: false,
            is_async: false,
            declared_local_tys: HashMap::new(),
        });
        // Evidence for `T: Future` bounds. The recorded method is the free
        // helper, not the trait's `&mut self` declaration: method calls are not
        // how await lowers, so the signature difference is not observed.
        self.register_synthetic_impl("Future", &state_name, "poll", poll_id);

        // ---- block_on: make_context; poll; Ready(v) => v, Pending => abort ----
        let block_id = FnId(self.hir.fns.len() as u32);
        let block_name = format!("__umay_block_on_{base_name}");
        let cx_local = LocalId(1);
        let poll_result_local = LocalId(2);
        let value_local = LocalId(3);

        let make_ty = self.cx.fn_ptr(Vec::new(), context_ty);
        let make_callee = match self.fn_by_name.get("make_context").copied() {
            Some(id) => HirExpr { kind: HirExprKind::FnRef(id), ty: make_ty, span },
            None => HirExpr { kind: HirExprKind::NamedFn("make_context".into()), ty: make_ty, span },
        };
        let make_call = HirExpr {
            kind: HirExprKind::Call { callee: Box::new(make_callee), args: Vec::new() },
            ty: context_ty,
            span,
        };
        let cx_ref = HirExpr {
            kind: HirExprKind::Ref {
                mutable: true,
                place: Box::new(HirExpr { kind: HirExprKind::Local(cx_local), ty: context_ty, span }),
            },
            ty: self.cx.ref_mut_to(context_ty),
            span,
        };
        let cx_ptr = HirExpr {
            kind: HirExprKind::Cast { expr: Box::new(cx_ref), to: cx_ty },
            ty: cx_ty,
            span,
        };
        let poll_call = HirExpr {
            kind: HirExprKind::Call {
                callee: Box::new(HirExpr {
                    kind: HirExprKind::FnRef(poll_id),
                    ty: self.cx.fn_ptr(vec![ptr_ty, cx_ty], poll_ty),
                    span,
                }),
                args: vec![
                    HirExpr { kind: HirExprKind::Local(LocalId(0)), ty: ptr_ty, span },
                    cx_ptr,
                ],
            },
            ty: poll_ty,
            span,
        };

        let ready_pattern = HirPattern {
            kind: HirPatternKind::Variant {
                def: poll_def_id,
                index: ready_index,
                fields: VariantPatternFields::Tuple(vec![HirPattern {
                    kind: HirPatternKind::Binding { local: value_local, mutable: false },
                    ty: output,
                    span,
                }]),
            },
            ty: poll_ty,
            span,
        };
        let pending_pattern = HirPattern {
            kind: HirPatternKind::Variant {
                def: poll_def_id,
                index: pending_index,
                fields: VariantPatternFields::Unit,
            },
            ty: poll_ty,
            span,
        };
        // ponytail: `Pending` has no executor in this slice, so it aborts with a
        // clear message. Upgrade: a real reactor that stores the waker and
        // re-polls; this arm becomes a suspension instead of an abort.
        let msg_ty = self.cx.str_ref();
        let unit_ty = self.cx.unit();
        let panic_ty = self.cx.fn_ptr(vec![msg_ty], unit_ty);
        let panic_callee = match self.fn_by_name.get("panic").copied() {
            Some(id) => HirExpr { kind: HirExprKind::FnRef(id), ty: panic_ty, span },
            None => HirExpr { kind: HirExprKind::NamedFn("umay_fail".into()), ty: panic_ty, span },
        };
        let abort = HirExpr {
            kind: HirExprKind::Call {
                callee: Box::new(panic_callee),
                args: vec![HirExpr {
                    kind: HirExprKind::StrLit(
                        "block_on: future returned Pending (no executor in this slice)".into(),
                    ),
                    ty: self.cx.str_ref(),
                    span,
                }],
            },
            // The call never returns; giving it the match's type keeps the MIR
            // arm's result assignment well-typed.
            ty: output,
            span,
        };
        let match_expr = HirExpr {
            kind: HirExprKind::Match {
                scrutinee: Box::new(HirExpr {
                    kind: HirExprKind::Local(poll_result_local),
                    ty: poll_ty,
                    span,
                }),
                arms: vec![
                    HirMatchArm {
                        pattern: ready_pattern,
                        guard: None,
                        body: HirExpr { kind: HirExprKind::Local(value_local), ty: output, span },
                        span,
                    },
                    HirMatchArm { pattern: pending_pattern, guard: None, body: abort, span },
                ],
            },
            ty: output,
            span,
        };
        let binding = |local: LocalId, ty: TyId, mutable: bool| HirPattern {
            kind: HirPatternKind::Binding { local, mutable },
            ty,
            span,
        };
        self.hir.fns.push(HirFn {
            id: block_id,
            name: block_name,
            span,
            generics: Vec::new(),
            params: vec![ptr_ty],
            ret: output,
            has_self: false,
            self_mutable: false,
            self_by_ref: false,
            locals: vec![
                LocalInfo { name: "__state".into(), ty: ptr_ty, mutable: false, span },
                LocalInfo { name: "__cx".into(), ty: context_ty, mutable: true, span },
                LocalInfo { name: "__poll".into(), ty: poll_ty, mutable: false, span },
                LocalInfo { name: "__value".into(), ty: output, mutable: false, span },
            ],
            body: Some(HirBlock {
                stmts: vec![
                    HirStmt {
                        kind: HirStmtKind::Let {
                            bindings: vec![cx_local],
                            init: Some(Box::new(make_call)),
                            pattern: binding(cx_local, context_ty, true),
                        },
                        span,
                    },
                    HirStmt {
                        kind: HirStmtKind::Let {
                            bindings: vec![poll_result_local],
                            init: Some(Box::new(poll_call)),
                            pattern: binding(poll_result_local, poll_ty, false),
                        },
                        span,
                    },
                ],
                tail: Some(Box::new(match_expr)),
                span,
                locals_introduced: 2,
            }),
            is_pub: false,
            owner: None,
            link_name: None,
            variadic: false,
            is_async: false,
            declared_local_tys: HashMap::new(),
        });

        self.async_helper_by_def.insert(state_def, block_id);
        self.async_state_by_fn.insert(
            fn_id,
            AsyncFnInfo {
                state_def,
                thunk: thunk_id,
                poll: poll_id,
                block_on: block_id,
                output,
                state_ty,
            },
        );
        (state_ty, state_def)
    }

    /// Synthesizes (or reuses) a bounded busy-poll `block_on` helper for a
    /// concrete user type implementing `Future<T>`. `poll_fn` is the impl's
    /// concrete `poll` method, called directly — no dyn, no vtable. The helper
    /// makes one context, then loops: `Ready(v)` breaks with `v`, `Pending`
    /// re-polls until `USER_POLL_LIMIT` is exhausted and panics.
    ///
    /// The helper takes `*mut State`, the same pointer `call_block_on_helper`
    /// passes for the generated-async path, and dereferences it for the `poll`
    /// receiver. Declaring the parameter as `State` while the caller passes a
    /// pointer made `&mut self` point at the pointer's bytes: a one-byte struct
    /// like `PendingOnce` happened to work, but an `i64` deadline never did.
    ///
    /// ponytail ceilings: no waker wake (the `make_context` waker is a no-op),
    /// no reactor, no spawn/select/timeout, no cancellation, no Send, no Pin.
    /// A `Pending` from a generated async fn still aborts — its thunk never
    /// returns `Pending`; this path is only for concrete user Future impls.
    /// Upgrade: ADR 0019 sections 3-6 (state machine + reactor + waker).
    pub(crate) fn lower_user_future_block_on(
        &mut self,
        state_ty: TyId,
        poll_fn: FnId,
        output: TyId,
        span: Span,
    ) -> FnId {
        let state_def = match self.cx.kind(state_ty) {
            TyKind::Adt { def, .. } => *def,
            _ => DefId(u32::MAX),
        };
        // `call_block_on_helper` passes `&mut state as *mut State`, so the helper
        // must declare the same pointer type and dereference it before polling.
        let ptr_ty = self.cx.raw_ptr_mut(state_ty);
        let type_name = self
            .hir
            .structs
            .iter()
            .find(|s| s.def_id == state_def)
            .map(|s| s.name.clone())
            .unwrap_or_else(|| format!("__def{}", state_def.0));
        let helper_name = format!("__umay_block_on_user_{}", type_name.replace("::", "_"));
        // One helper per concrete type; several call sites share it.
        if let Some(existing) = self.hir.fns.iter().find(|f| f.name == helper_name) {
            return existing.id;
        }

        let poll_def = self.def_by_name.get("Poll").copied().unwrap_or(DefId(u32::MAX));
        let poll_ty = self
            .hir
            .fns
            .iter()
            .find(|f| f.id == poll_fn)
            .map(|f| f.ret)
            .unwrap_or_else(|| self.cx.adt(poll_def, vec![output]));
        let context_ty = self
            .def_by_name
            .get("Context")
            .copied()
            .map(|d| self.cx.adt(d, Vec::new()))
            .unwrap_or_else(|| self.cx.error());
        let i64_ty = self.cx.primitive("i64").unwrap_or_else(|| self.cx.error());
        let bool_ty = self.cx.bool();
        let unit_ty = self.cx.unit();
        let never_ty = self.cx.never();
        let str_ty = self.cx.str_ref();
        let variants = self
            .hir
            .enums
            .iter()
            .find(|e| e.def_id == poll_def)
            .map(|e| e.variants.clone())
            .unwrap_or_default();
        let ready_index = variants.iter().position(|v| v.name == "Ready").unwrap_or(0) as u32;
        let pending_index = variants.iter().position(|v| v.name == "Pending").unwrap_or(1) as u32;

        // Locals: 0 = state (param), 1 = context, 2 = poll count,
        // 3 = current Poll, 4 = the Ready payload.
        let cx_local = LocalId(1);
        let count_local = LocalId(2);
        let poll_local = LocalId(3);
        let value_local = LocalId(4);
        let local = |id, ty| HirExpr { kind: HirExprKind::Local(id), ty, span };
        let binding = |id, ty, mutable| HirPattern {
            kind: HirPatternKind::Binding { local: id, mutable },
            ty,
            span,
        };

        // let __cx = make_context()
        let make_ty = self.cx.fn_ptr(Vec::new(), context_ty);
        let make_callee = match self.fn_by_name.get("make_context").copied() {
            Some(id) => HirExpr { kind: HirExprKind::FnRef(id), ty: make_ty, span },
            None => HirExpr { kind: HirExprKind::NamedFn("make_context".into()), ty: make_ty, span },
        };
        let make_call = HirExpr {
            kind: HirExprKind::Call { callee: Box::new(make_callee), args: Vec::new() },
            ty: context_ty,
            span,
        };
        // let mut __poll_count = 0i64
        let zero = HirExpr { kind: HirExprKind::IntLit { value: 0, suffix: None }, ty: i64_ty, span };

        // The direct poll call. `MethodCall` with a concrete FnId is a static
        // call; `lower_receiver` inserts the `&mut state` the method declares.
        // Local 0 holds the `*mut State` the caller passed, so dereference it to
        // get the state place the borrow must point at.
        let state_ref = HirExpr {
            kind: HirExprKind::Deref(Box::new(local(LocalId(0), ptr_ty))),
            ty: state_ty,
            span,
        };
        let cx_ref = HirExpr {
            kind: HirExprKind::Ref {
                mutable: true,
                place: Box::new(local(cx_local, context_ty)),
            },
            ty: self.cx.ref_mut_to(context_ty),
            span,
        };
        let poll_call = HirExpr {
            kind: HirExprKind::MethodCall {
                receiver: Box::new(state_ref),
                method: poll_fn,
                args: vec![cx_ref],
            },
            ty: poll_ty,
            span,
        };

        let ready_pattern = HirPattern {
            kind: HirPatternKind::Variant {
                def: poll_def,
                index: ready_index,
                fields: VariantPatternFields::Tuple(vec![binding(value_local, output, false)]),
            },
            ty: poll_ty,
            span,
        };
        let pending_pattern = HirPattern {
            kind: HirPatternKind::Variant {
                def: poll_def,
                index: pending_index,
                fields: VariantPatternFields::Unit,
            },
            ty: poll_ty,
            span,
        };
        let break_ready = HirExpr {
            kind: HirExprKind::Break(Some(Box::new(local(value_local, output)))),
            ty: never_ty,
            span,
        };
        let unit_expr = HirExpr {
            kind: HirExprKind::Block(HirBlock {
                stmts: Vec::new(),
                tail: None,
                span,
                locals_introduced: 0,
            }),
            ty: unit_ty,
            span,
        };
        let poll_match = HirExpr {
            kind: HirExprKind::Match {
                scrutinee: Box::new(local(poll_local, poll_ty)),
                arms: vec![
                    HirMatchArm { pattern: ready_pattern, guard: None, body: break_ready, span },
                    HirMatchArm { pattern: pending_pattern, guard: None, body: unit_expr, span },
                ],
            },
            ty: unit_ty,
            span,
        };

        // __poll_count = __poll_count + 1
        let count_inc = HirExpr {
            kind: HirExprKind::Assign {
                place: Box::new(local(count_local, i64_ty)),
                value: Box::new(HirExpr {
                    kind: HirExprKind::Binary {
                        op: HirBinaryOp::Add,
                        lhs: Box::new(local(count_local, i64_ty)),
                        rhs: Box::new(HirExpr {
                            kind: HirExprKind::IntLit { value: 1, suffix: None },
                            ty: i64_ty,
                            span,
                        }),
                    },
                    ty: i64_ty,
                    span,
                }),
            },
            ty: unit_ty,
            span,
        };

        // if __poll_count >= LIMIT { panic("...") }
        let panic_ty = self.cx.fn_ptr(vec![str_ty], unit_ty);
        let panic_callee = match self.fn_by_name.get("panic").copied() {
            Some(id) => HirExpr { kind: HirExprKind::FnRef(id), ty: panic_ty, span },
            None => HirExpr { kind: HirExprKind::NamedFn("umay_fail".into()), ty: panic_ty, span },
        };
        let panic_msg = format!(
            "block_on: future polled {USER_POLL_LIMIT} times without completing (busy-poll only; no waker/reactor)"
        );
        let panic_call = HirExpr {
            kind: HirExprKind::Call {
                callee: Box::new(panic_callee),
                args: vec![HirExpr {
                    kind: HirExprKind::StrLit(panic_msg),
                    ty: str_ty,
                    span,
                }],
            },
            ty: unit_ty,
            span,
        };
        let guard = HirExpr {
            kind: HirExprKind::If {
                cond: Box::new(HirExpr {
                    kind: HirExprKind::Binary {
                        op: HirBinaryOp::Ge,
                        lhs: Box::new(local(count_local, i64_ty)),
                        rhs: Box::new(HirExpr {
                            kind: HirExprKind::IntLit { value: USER_POLL_LIMIT, suffix: None },
                            ty: i64_ty,
                            span,
                        }),
                    },
                    ty: bool_ty,
                    span,
                }),
                then_block: HirBlock {
                    stmts: vec![HirStmt { kind: HirStmtKind::Expr(panic_call), span }],
                    tail: None,
                    span,
                    locals_introduced: 0,
                },
                else_branch: None,
            },
            ty: unit_ty,
            span,
        };

        // loop { let __poll = poll(...); match ... { Ready => break v, ... };
        //        __poll_count += 1; if __poll_count >= LIMIT { panic } }
        let loop_expr = HirExpr {
            kind: HirExprKind::Loop {
                body: HirBlock {
                    stmts: vec![
                        HirStmt {
                            kind: HirStmtKind::Let {
                                bindings: vec![poll_local],
                                init: Some(Box::new(poll_call)),
                                pattern: binding(poll_local, poll_ty, false),
                            },
                            span,
                        },
                        HirStmt { kind: HirStmtKind::Expr(poll_match), span },
                        HirStmt { kind: HirStmtKind::Expr(count_inc), span },
                        HirStmt { kind: HirStmtKind::Expr(guard), span },
                    ],
                    tail: None,
                    span,
                    locals_introduced: 1,
                },
            },
            ty: output,
            span,
        };

        let fn_id = FnId(self.hir.fns.len() as u32);
        self.hir.fns.push(HirFn {
            id: fn_id,
            name: helper_name,
            span,
            generics: Vec::new(),
            params: vec![ptr_ty],
            ret: output,
            has_self: false,
            self_mutable: false,
            self_by_ref: false,
            locals: vec![
                LocalInfo { name: "__state".into(), ty: ptr_ty, mutable: true, span },
                LocalInfo { name: "__cx".into(), ty: context_ty, mutable: true, span },
                LocalInfo { name: "__poll_count".into(), ty: i64_ty, mutable: true, span },
                LocalInfo { name: "__poll".into(), ty: poll_ty, mutable: false, span },
                LocalInfo { name: "__value".into(), ty: output, mutable: false, span },
            ],
            body: Some(HirBlock {
                stmts: vec![
                    HirStmt {
                        kind: HirStmtKind::Let {
                            bindings: vec![cx_local],
                            init: Some(Box::new(make_call)),
                            pattern: binding(cx_local, context_ty, true),
                        },
                        span,
                    },
                    HirStmt {
                        kind: HirStmtKind::Let {
                            bindings: vec![count_local],
                            init: Some(Box::new(zero)),
                            pattern: binding(count_local, i64_ty, true),
                        },
                        span,
                    },
                ],
                tail: Some(Box::new(loop_expr)),
                span,
                locals_introduced: 2,
            }),
            is_pub: false,
            owner: None,
            link_name: None,
            variadic: false,
            is_async: false,
            declared_local_tys: HashMap::new(),
        });
        fn_id
    }
}

// ---- rewriting an async body into the thunk -------------------------------

struct ThunkCtx {
    param_count: usize,
    param_tys: Vec<TyId>,
    field_names: Vec<String>,
    state_ty: TyId,
    ptr_ty: TyId,
    span: Span,
}

fn shift_id(id: &mut LocalId) {
    id.0 += 1;
}

fn lower_thunk_block(block: &mut HirBlock, ctx: &ThunkCtx) {
    for st in &mut block.stmts {
        match &mut st.kind {
            HirStmtKind::Let { bindings, pattern, init } => {
                for id in bindings {
                    shift_id(id);
                }
                lower_thunk_pattern(pattern, ctx);
                if let Some(e) = init {
                    lower_thunk_expr(e, ctx);
                }
            }
            HirStmtKind::Expr(e) => lower_thunk_expr(e, ctx),
        }
    }
    if let Some(t) = &mut block.tail {
        lower_thunk_expr(t, ctx);
    }
}

fn lower_thunk_pattern(p: &mut HirPattern, _ctx: &ThunkCtx) {
    match &mut p.kind {
        HirPatternKind::Binding { local, .. } => shift_id(local),
        HirPatternKind::Tuple(ps)
        | HirPatternKind::Or(ps)
        | HirPatternKind::Slice { elems: ps, .. } => {
            for p in ps {
                lower_thunk_pattern(p, _ctx);
            }
        }
        HirPatternKind::Struct { fields, .. } => {
            for f in fields {
                lower_thunk_pattern(&mut f.pattern, _ctx);
            }
        }
        HirPatternKind::Variant { fields, .. } => match fields {
            VariantPatternFields::Unit => {}
            VariantPatternFields::Tuple(ps) => {
                for p in ps {
                    lower_thunk_pattern(p, _ctx);
                }
            }
            VariantPatternFields::Struct { fields, .. } => {
                for f in fields {
                    lower_thunk_pattern(&mut f.pattern, _ctx);
                }
            }
        },
        _ => {}
    }
}

fn lower_thunk_expr(e: &mut HirExpr, ctx: &ThunkCtx) {
    match &mut e.kind {
        HirExprKind::Block(b) => lower_thunk_block(b, ctx),
        HirExprKind::If { cond, then_block, else_branch } => {
            lower_thunk_expr(cond, ctx);
            lower_thunk_block(then_block, ctx);
            if let Some(branch) = else_branch {
                lower_thunk_expr(branch, ctx);
            }
        }
        HirExprKind::While { cond, body } => {
            lower_thunk_expr(cond, ctx);
            lower_thunk_block(body, ctx);
        }
        HirExprKind::Loop { body } | HirExprKind::Defer(body) | HirExprKind::Unsafe(body) => {
            lower_thunk_block(body, ctx)
        }
        HirExprKind::For { pattern_locals, iter, body } => {
            for id in pattern_locals {
                shift_id(id);
            }
            lower_thunk_expr(iter, ctx);
            lower_thunk_block(body, ctx);
        }
        HirExprKind::Match { scrutinee, arms } => {
            lower_thunk_expr(scrutinee, ctx);
            for arm in arms {
                lower_thunk_pattern(&mut arm.pattern, ctx);
                if let Some(g) = &mut arm.guard {
                    lower_thunk_expr(g, ctx);
                }
                lower_thunk_expr(&mut arm.body, ctx);
            }
        }
        _ => {
            for child in child_exprs_mut(e) {
                lower_thunk_expr(child, ctx);
            }
        }
    }
    // Parameter uses become `(*state).field`; every other local is shifted by
    // one because the thunk's local 0 is the state pointer.
    if let HirExprKind::Local(id) = &mut e.kind {
        let idx = id.0 as usize;
        if idx < ctx.param_count {
            let state = HirExpr {
                kind: HirExprKind::Local(LocalId(0)),
                ty: ctx.ptr_ty,
                span: ctx.span,
            };
            let deref = HirExpr {
                kind: HirExprKind::Deref(Box::new(state)),
                ty: ctx.state_ty,
                span: ctx.span,
            };
            e.kind = HirExprKind::Field {
                base: Box::new(deref),
                index: idx as u32,
                name: ctx.field_names[idx].clone(),
            };
            e.ty = ctx.param_tys[idx];
        } else {
            shift_id(id);
        }
    }
}

// ---- call-site lowering: `.await` and `block_on(expr)` ---------------------

impl<'c, 's> FnChecker<'c, 's> {
    /// `.await` on a generated state is a direct call to its block_on helper.
    pub(crate) fn check_await(&mut self, base: &ast::Expr, span: Span) -> HirExpr {
        let value = self.check_expr(base);
        if let Some(e) = self.lower_async_block_on(value, span) {
            return e;
        }
        self.push(
            Diagnostic::error("E1021", "`.await` needs a future returned by an `async fn`")
                .at(span)
                .with_cause("only compiler-generated async state structs can be awaited in this slice"),
        );
        self.error_hir(span)
    }

    /// Recognises `block_on(future)` by name. It is a compiler bridge, not a
    /// prelude function, so it is intercepted before ordinary name resolution.
    pub(crate) fn try_block_on_call(
        &mut self,
        callee: &ast::Expr,
        args: &[ast::Expr],
        span: Span,
    ) -> Option<HirExpr> {
        let named = match &callee.kind {
            ast::ExprKind::Ident(n) => n == "block_on",
            ast::ExprKind::Path(p) => {
                p.segments.len() == 1 && p.segments[0].name == "block_on"
            }
            _ => false,
        };
        if !named || args.len() != 1 {
            return None;
        }
        let value = self.check_expr(&args[0]);
        // Generated async states are one-poll; try them first so that path is
        // byte-for-byte unchanged. Concrete user Futures get the looping path.
        if let Some(e) = self.lower_async_block_on(value.clone(), span) {
            return Some(e);
        }
        self.lower_user_future_block_on_call(value, span)
    }

    /// Binds the state value to a local (so a pointer to it is well-formed) and
    /// calls the concrete block_on helper. Returns `None` when the value is not
    /// a generated async state, letting the ordinary call rules take over.
    pub(crate) fn lower_async_block_on(
        &mut self,
        value: HirExpr,
        span: Span,
    ) -> Option<HirExpr> {
        let def = match self.cx.cx.kind(value.ty) {
            TyKind::Adt { def, .. } => *def,
            _ => return None,
        };
        let block_on = *self.cx.async_helper_by_def.get(&def)?;
        let state_ty = value.ty;
        let out_ty = self
            .cx
            .hir
            .fns
            .iter()
            .find(|f| f.id == block_on)
            .map(|f| f.ret)
            .unwrap_or_else(|| self.cx.cx.error());
        Some(self.call_block_on_helper(value, block_on, out_ty, state_ty, span))
    }

    /// Recognises a concrete user type with a registered `Future<T>` impl and
    /// lowers `block_on(value)` to a call to its generated bounded-loop helper.
    /// Returns `None` for values that are not such a type, leaving ordinary call
    /// rules (and their diagnostics) in charge.
    pub(crate) fn lower_user_future_block_on_call(
        &mut self,
        value: HirExpr,
        span: Span,
    ) -> Option<HirExpr> {
        let def = match self.cx.cx.kind(value.ty) {
            TyKind::Adt { def, .. } => *def,
            _ => return None,
        };
        let type_name = self
            .cx
            .hir
            .structs
            .iter()
            .find(|s| s.def_id == def)
            .map(|s| s.name.clone())?;
        // Evidence that the type really is a Future: the same `impls` list the
        // trait-bound checker reads, plus the method map for the concrete poll.
        let is_future = self.cx.hir.traits.iter().find(|t| t.name == "Future").is_some_and(|t| {
            t.impls.iter().any(|i: &crate::hir::TraitImplInfo| i.type_name == type_name)
        });
        if !is_future {
            return None;
        }
        let poll_fn = *self.cx.methods.get(&(type_name.clone(), "poll".to_string()))?;
        let poll_ret = self.cx.hir.fns.iter().find(|f| f.id == poll_fn).map(|f| f.ret)?;
        let output = match self.cx.cx.kind(poll_ret) {
            TyKind::Adt { args, .. } => *args.first()?,
            _ => return None,
        };
        let state_ty = value.ty;
        let block_on = self.cx.lower_user_future_block_on(state_ty, poll_fn, output, span);
        Some(self.call_block_on_helper(value, block_on, output, state_ty, span))
    }

    /// Binds a future value to a mutable local (so `&mut`/pointer to it is
    /// well-formed) and calls a concrete `block_on` helper. Shared by the
    /// generated-async-state path and the concrete-user-Future path.
    fn call_block_on_helper(
        &mut self,
        value: HirExpr,
        block_on: FnId,
        out_ty: TyId,
        state_ty: TyId,
        span: Span,
    ) -> HirExpr {
        let ptr_ty = self.cx.cx.raw_ptr_mut(state_ty);

        let local = LocalId(self.locals.len() as u32);
        self.locals
            .push(LocalInfo { name: "__async_state".into(), ty: state_ty, mutable: true, span });
        self.local_tys.insert(local, state_ty);
        let state_ref = HirExpr {
            kind: HirExprKind::Ref {
                mutable: true,
                place: Box::new(HirExpr {
                    kind: HirExprKind::Local(local),
                    ty: state_ty,
                    span,
                }),
            },
            ty: self.cx.cx.ref_mut_to(state_ty),
            span,
        };
        let state_ptr = HirExpr {
            kind: HirExprKind::Cast { expr: Box::new(state_ref), to: ptr_ty },
            ty: ptr_ty,
            span,
        };
        let call = HirExpr {
            kind: HirExprKind::Call {
                callee: Box::new(HirExpr {
                    kind: HirExprKind::FnRef(block_on),
                    ty: self.cx.cx.fn_ptr(vec![ptr_ty], out_ty),
                    span,
                }),
                args: vec![state_ptr],
            },
            ty: out_ty,
            span,
        };
        let block = HirBlock {
            stmts: vec![HirStmt {
                kind: HirStmtKind::Let {
                    bindings: vec![local],
                    init: Some(Box::new(value)),
                    pattern: HirPattern {
                        kind: HirPatternKind::Binding { local, mutable: true },
                        ty: state_ty,
                        span,
                    },
                },
                span,
            }],
            tail: Some(Box::new(call)),
            span,
            locals_introduced: 1,
        };
        HirExpr { kind: HirExprKind::Block(block), ty: out_ty, span }
    }
}
