//! Lowering of operators, calls and control flow.
//!
//! This is where the CFG earns its keep: short-circuit operators, `if`,
//! `match`, loops and `?` all become explicit branches. Nothing here relies on
//! evaluation order being implicit, so the resulting graph is a faithful
//! description of execution.

use crate::hir::*;
use crate::mir::*;
use crate::types::TyKind;

use crate::lower::{LoopTargets, Lowering};

impl Lowering<'_> {
    // ---- operators --------------------------------------------------------

    pub(crate) fn lower_unary(&mut self, op: HirUnaryOp, operand: &HirExpr, e: &HirExpr) -> Operand {
        let op_val = self.lower_expr(operand);
        let mir_op = match op {
            HirUnaryOp::Neg => UnOp::Neg,
            HirUnaryOp::Not => UnOp::Not,
            // `*x` in value position reads through the projection.
            HirUnaryOp::Deref => {
                let place = self.lower_place(operand).deref();
                let tmp = self.temp_place(e.ty);
                self.assign(tmp.clone(), Rvalue::Use(Operand::Copy(place)), e.span);
                return Operand::Copy(tmp);
            }
        };
        let tmp = self.temp_place(e.ty);
        self.assign(tmp.clone(), Rvalue::Unary { op: mir_op, operand: op_val }, e.span);
        Operand::Copy(tmp)
    }

    pub(crate) fn lower_binary(
        &mut self,
        op: HirBinaryOp,
        lhs: &HirExpr,
        rhs: &HirExpr,
        e: &HirExpr,
    ) -> Operand {
        // `&&` and `||` must not evaluate the right operand eagerly.
        match op {
            HirBinaryOp::And => return self.lower_short_circuit(true, lhs, rhs, e),
            HirBinaryOp::Or => return self.lower_short_circuit(false, lhs, rhs, e),
            _ => {}
        }
        // Two `str` values are pointers, so `==` on them has to compare the
        // bytes they point at. Comparing the pointers would call two equal
        // strings different, which is never what a caller means.
        if matches!(op, HirBinaryOp::Eq | HirBinaryOp::Ne) && self.is_str(lhs.ty) {
            return self.lower_str_compare(op, lhs, rhs, e);
        }
        let l = self.lower_expr(lhs);
        let r = self.lower_expr(rhs);
        let tmp = self.temp_place(e.ty);
        self.assign(
            tmp.clone(),
            Rvalue::Binary { op: convert_binop(op), lhs: l, rhs: r },
            e.span,
        );
        Operand::Copy(tmp)
    }

    /// True for `str` and `&str`, whose `==` means "the same bytes".
    fn is_str(&mut self, ty: crate::types::TyId) -> bool {
        match self.types.kind(ty) {
            TyKind::Str => true,
            TyKind::Ref(inner) | TyKind::RefMut(inner) => matches!(self.types.kind(*inner), TyKind::Str),
            _ => false,
        }
    }

    /// Lowers `a == b` on `str` to a call to the runtime's byte comparison.
    fn lower_str_compare(
        &mut self,
        op: HirBinaryOp,
        lhs: &HirExpr,
        rhs: &HirExpr,
        e: &HirExpr,
    ) -> Operand {
        let Some(fn_id) = self.hir.fns.iter().find(|f| f.name == "umay_str_eq").map(|f| f.id) else {
            // The runtime is always in the prelude, so this cannot happen; fall
            // back to the pointer comparison rather than emitting nothing.
            let l = self.lower_expr(lhs);
            let r = self.lower_expr(rhs);
            let tmp = self.temp_place(e.ty);
            self.assign(tmp.clone(), Rvalue::Binary { op: convert_binop(op), lhs: l, rhs: r }, e.span);
            return Operand::Copy(tmp);
        };
        let mut a = self.lower_expr(lhs);
        let mut b = self.lower_expr(rhs);
        // The runtime takes `*const u8`; a `&str` is the same word.
        if let TyKind::Ref(_) | TyKind::RefMut(_) = self.types.kind(lhs.ty) {
            a = self.reinterpret_str(a, lhs);
        }
        if let TyKind::Ref(_) | TyKind::RefMut(_) = self.types.kind(rhs.ty) {
            b = self.reinterpret_str(b, rhs);
        }
        let bool_ty = self.types.bool_ty();
        let callee = Operand::Constant(Constant::Fn(fn_id));
        let dest = self.temp_place(bool_ty);
        let next = self.new_block();
        self.terminate(
            Terminator::Call { callee, args: vec![a, b], destination: Some(dest.clone()), target: next, unwind: Vec::new(), span: e.span },
            next,
        );
        if op == HirBinaryOp::Ne {
            let out = self.temp_place(e.ty);
            self.assign(out.clone(), Rvalue::Unary { op: crate::mir::UnOp::Not, operand: Operand::Copy(dest) }, e.span);
            return Operand::Copy(out);
        }
        Operand::Copy(dest)
    }

    fn reinterpret_str(&mut self, op: Operand, from: &HirExpr) -> Operand {
        let u8_ty = self.types.u8_ty();
        let to = self.types.raw_ptr(u8_ty);
        let tmp = self.temp_place(to);
        self.assign(tmp.clone(), Rvalue::Cast { operand: op, to }, from.span);
        Operand::Copy(tmp)
    }

    /// Lowers `a && b` (when `is_and`) or `a || b` into two blocks.
    fn lower_short_circuit(&mut self, is_and: bool, lhs: &HirExpr, rhs: &HirExpr, e: &HirExpr) -> Operand {
        let l = self.lower_expr(lhs);
        let result = self.temp_place(e.ty);

        let rhs_block = self.new_block();
        let short_block = self.new_block();
        let join = self.new_block();

        // `a && b`: false goes straight to the join; `a || b`: true does.
        let (on_short, on_continue) = if is_and {
            (0i128, 1i128)
        } else {
            (1i128, 0i128)
        };
        // The short-circuit value is the one that skips the right operand.
        let short_constant = !is_and;
        self.terminate(
            Terminator::SwitchInt {
                discr: l,
                targets: vec![(on_short, short_block), (on_continue, rhs_block)],
                otherwise: rhs_block,
            },
            rhs_block,
        );

        // Short-circuit block: store the constant and jump to the join.
        self.current = short_block;
        self.assign(
            result.clone(),
            Rvalue::Use(Operand::Constant(Constant::Bool(short_constant))),
            e.span,
        );
        self.goto(join);

        // Right-hand side: its value is the result.
        self.current = rhs_block;
        let r = self.lower_expr(rhs);
        self.assign(result.clone(), Rvalue::Use(r), e.span);
        self.goto(join);

        self.current = join;
        Operand::Copy(result)
    }

    pub(crate) fn lower_assign(&mut self, place: &HirExpr, value: &HirExpr, e: &HirExpr) -> Operand {
        // Compound assignment reached HIR only if the checker kept it; treat it
        // as `place = place op value` so ordering is right.
        if let HirExprKind::AssignOp { op, place: p, value: v } = &value.kind {
            let left = self.lower_expr(p);
            let right = self.lower_expr(v);
            let tmp = self.temp_place(value.ty);
            self.assign(
                tmp.clone(),
                Rvalue::Binary { op: convert_binop(*op), lhs: left, rhs: right },
                e.span,
            );
            let target = self.lower_place(place);
            self.assign(target, Rvalue::Use(Operand::Copy(tmp)), e.span);
            return Operand::Constant(Constant::Unit);
        }
        let val = self.lower_expr(value);
        let target = self.lower_place(place);
        // Writing a new value to a local re-arms its drop flag.
        self.emit(Statement::SetDropFlag { local: target.local, value: true });
        self.assign(target, Rvalue::Use(val), e.span);
        Operand::Constant(Constant::Unit)
    }

    // ---- calls ------------------------------------------------------------

    pub(crate) fn lower_call(&mut self, callee: &HirExpr, args: &[HirExpr], e: &HirExpr) -> Operand {
        let callee_op = self.lower_expr(callee);
        // A bare literal argument takes its parameter's width. Without this the
        // literal keeps the default `i32`, but the selector moves an unresolved
        // literal a whole word, so the upper half of the register handed to the
        // callee is not the sign extension the ABI promises -- a `16` passed as
        // an `i32` length arrived with junk above it.
        let params: Option<Vec<crate::types::TyId>> = match self.types.kind(callee.ty) {
            TyKind::FnPtr { params, .. } => Some(params.clone()),
            _ => None,
        };
        let mut arg_ops: Vec<Operand> = Vec::with_capacity(args.len());
        for (i, a) in args.iter().enumerate() {
            let op = self.lower_expr(a);
            let want = params.as_ref().and_then(|p| p.get(i)).copied();
            match want {
                Some(w) if self.types.is_integer_var(a.ty) && self.types.is_integer(w) => {
                    let tmp = self.temp_place(w);
                    self.assign(tmp.clone(), Rvalue::Cast { operand: op, to: w }, a.span);
                    arg_ops.push(Operand::Copy(tmp));
                }
                // Passing a `&mut T` binding to a `&mut T` or `&T` parameter is a
                // fresh reborrow, not a move: moving it would leave the caller
                // unable to use the reference again, and `&mut T` coerces to `&T`.
                Some(w)
                    if matches!(self.types.kind(a.ty), TyKind::RefMut(_))
                        && matches!(self.types.kind(w), TyKind::RefMut(_) | TyKind::Ref(_))
                        && matches!(a.kind, HirExprKind::Local(_)) =>
                {
                    let place = self.lower_place(a).deref();
                    let slot = self.temp_place(w);
                    let mutable = matches!(self.types.kind(w), TyKind::RefMut(_));
                    self.assign(slot.clone(), Rvalue::Reborrow { mutable, place }, a.span);
                    arg_ops.push(Operand::Copy(slot));
                }
                _ => arg_ops.push(op),
            }
        }
        let dest = if matches!(self.types.kind(e.ty), TyKind::Unit) {
            None
        } else {
            Some(self.temp_place(e.ty))
        };
        let next = self.new_block();
        self.terminate(
            Terminator::Call {
                callee: callee_op,
                args: arg_ops,
                destination: dest.clone(),
                target: next,
                unwind: Vec::new(),
                span: e.span,
            },
            next,
        );
        match dest {
            Some(d) => Operand::Copy(d),
            None => Operand::Constant(Constant::Unit),
        }
    }

    pub(crate) fn lower_method_call(
        &mut self,
        receiver: &HirExpr,
        method: FnId,
        args: &[HirExpr],
        e: &HirExpr,
    ) -> Operand {
        // The receiver is passed as argument 0, matching the `self` parameter.
        //
        // A `&self` or `&mut self` method takes the receiver by reference, and
        // the checker accepts a plain `T` for it. That acceptance is only sound
        // if the borrow is inserted here: passing the value by move would make
        // the receiver unusable after any method call, so `s.len()` followed by
        // `s.field` would report a use-after-move.
        let recv = self.lower_receiver(receiver, method, e);
        let mut arg_ops = vec![recv];
        for a in args {
            arg_ops.push(self.lower_expr(a));
        }
        let dest = if matches!(self.types.kind(e.ty), TyKind::Unit) {
            None
        } else {
            Some(self.temp_place(e.ty))
        };
        let next = self.new_block();
        self.terminate(
            Terminator::Call {
                callee: Operand::Constant(Constant::Fn(method)),
                args: arg_ops,
                destination: dest.clone(),
                target: next,
                unwind: Vec::new(),
                span: e.span,
            },
            next,
        );
        match dest {
            Some(d) => Operand::Copy(d),
            None => Operand::Constant(Constant::Unit),
        }
    }

    /// Lowers a method receiver, adding the borrow the declaration calls for.
    fn lower_receiver(&mut self, receiver: &HirExpr, method: FnId, e: &HirExpr) -> Operand {
        let (by_ref, mutable) = match self.hir.fns.iter().find(|f| f.id == method) {
            Some(f) => (f.self_by_ref, f.self_mutable),
            None => return self.lower_expr(receiver),
        };
        if !by_ref {
            return self.lower_expr(receiver);
        }
        // An expression that is already a reference of the right mutability is
        // passed through unchanged. A `&mut T` satisfies a `&self` receiver too:
        // reborrowing it as `&&mut T` would be a different type, and moving it
        // would make the value unusable after the call.
        let already = matches!(self.types.kind(receiver.ty), TyKind::Ref(_) | TyKind::RefMut(_));
        if already {
            // A variable holding the reference is borrowed rather than moved: the
            // callee sees a fresh borrow that ends with the call.
            if let HirExprKind::Local(_) = &receiver.kind {
                let ty = receiver.ty;
                let tmp = self.temp_place(ty);
                // A reborrow reads through the reference: the callee wants a `&T`
                // pointing at the same value, not a `&&T` pointing at the local.
                let place = self.lower_place(receiver).deref();
                let is_mut = matches!(self.types.kind(ty), TyKind::RefMut(_));
                let target_ty = if is_mut { self.types.ref_mut_to(ty) } else { self.types.ref_to(ty) };
                let slot = self.temp_place(target_ty);
                self.assign(slot.clone(), Rvalue::Reborrow { mutable: is_mut, place }, e.span);
                let _ = tmp;
                return Operand::Copy(slot);
            }
            return self.lower_expr(receiver);
        }
        let place = self.lower_place(receiver);
        let ty = if mutable {
            self.types.ref_mut_to(receiver.ty)
        } else {
            self.types.ref_to(receiver.ty)
        };
        let tmp = self.temp_place(ty);
        self.assign(tmp.clone(), Rvalue::Ref { mutable, place }, e.span);
        Operand::Copy(tmp)
    }

    /// Built-in methods lower to calls of synthetic functions named
    /// `str::len` and friends. The backend resolves them; the MIR stays uniform.
    pub(crate) fn lower_builtin_method(
        &mut self,
        name: &str,
        receiver: &HirExpr,
        args: &[HirExpr],
        e: &HirExpr,
    ) -> Operand {
        let mut arg_ops = vec![self.lower_expr(receiver)];
        for a in args {
            arg_ops.push(self.lower_expr(a));
        }
        let dest = if matches!(self.types.kind(e.ty), TyKind::Unit) {
            None
        } else {
            Some(self.temp_place(e.ty))
        };
        let next = self.new_block();
        let sym = self.program.intern_string(&format!("intrinsic:{name}"));
        self.terminate(
            Terminator::Call {
                // A synthetic intrinsic: the name identifies it for codegen.
                callee: Operand::Constant(Constant::Str(sym)),
                args: arg_ops,
                destination: dest.clone(),
                target: next,
                unwind: Vec::new(),
                span: e.span,
            },
            next,
        );
        match dest {
            Some(d) => Operand::Copy(d),
            None => Operand::Constant(Constant::Unit),
        }
    }

    pub(crate) fn lower_struct_lit(
        &mut self,
        def: crate::types::DefId,
        fields: &[HirFieldInit],
        base: Option<&HirExpr>,
        e: &HirExpr,
    ) -> Operand {
        // Field order in the aggregate matches declaration order, so sort by
        // the resolved index rather than by source order.
        let mut ordered: Vec<&HirFieldInit> = fields.iter().collect();
        ordered.sort_by_key(|f| f.index);

        // A base value supplies the fields the literal omits. Copy them into
        // the new aggregate explicitly so that no partial initialisation exists.
        let base_place = base.map(|b| self.lower_place(b));

        // Build the operand list in declaration order: explicit fields from the
        // literal, remaining fields copied from `..base`.
        let field_count = self.struct_field_count(def);
        let mut ops: Vec<Operand> = Vec::new();
        for i in 0..field_count {
            if let Some(f) = ordered.iter().find(|f| f.index == i) {
                ops.push(self.lower_expr(&f.value));
            } else if let Some(bp) = &base_place {
                ops.push(Operand::Copy(bp.clone().field(i)));
            } else {
                // The type checker rejected this literal; keep arity correct.
                ops.push(Operand::Constant(Constant::Unit));
            }
        }
        let tmp = self.temp_place(e.ty);
        self.assign(
            tmp.clone(),
            Rvalue::Aggregate { kind: AggregateKind::Struct(def), operands: ops },
            e.span,
        );
        Operand::Copy(tmp)
    }

    fn struct_field_count(&self, def: crate::types::DefId) -> u32 {
        self.hir
            .structs
            .iter()
            .find(|s| s.def_id == def)
            .map(|s| s.fields.len() as u32)
            .unwrap_or(0)
    }

    // ---- control flow -----------------------------------------------------

    pub(crate) fn lower_if(
        &mut self,
        cond: &HirExpr,
        then_block: &HirBlock,
        else_branch: Option<&HirExpr>,
        e: &HirExpr,
    ) -> Operand {
        let c = self.lower_expr(cond);
        let then_bb = self.new_block();
        let else_bb = self.new_block();
        let join = self.new_block();

        self.terminate(
            Terminator::SwitchInt {
                discr: c,
                targets: vec![(1, then_bb)],
                otherwise: else_bb,
            },
            then_bb,
        );

        let produces_value = !matches!(self.types.kind(e.ty), TyKind::Unit);
        let result = if produces_value { Some(self.temp_place(e.ty)) } else { None };

        self.current = then_bb;
        let tv = self.lower_block(then_block);
        if let (Some(r), Some(v)) = (&result, tv) {
            self.assign(r.clone(), Rvalue::Use(v), e.span);
        }
        self.goto(join);

        self.current = else_bb;
        let ev = else_branch.map(|b| self.lower_expr(b));
        if let (Some(r), Some(v)) = (&result, ev) {
            self.assign(r.clone(), Rvalue::Use(v), e.span);
        }
        self.goto(join);

        self.current = join;
        match result {
            Some(r) => Operand::Copy(r),
            None => Operand::Constant(Constant::Unit),
        }
    }

    pub(crate) fn lower_while(&mut self, cond: &HirExpr, body: &HirBlock, e: &HirExpr) -> Operand {
        let head = self.new_block();
        let body_bb = self.new_block();
        let exit = self.new_block();

        self.goto(head);
        self.current = head;
        let c = self.lower_expr(cond);
        self.terminate(
            Terminator::SwitchInt { discr: c, targets: vec![(1, body_bb)], otherwise: exit },
            body_bb,
        );

        self.loop_stack.push(LoopTargets { exit, head, value: None });
        self.current = body_bb;
        self.lower_block(body);
        self.loop_stack.pop();
        if self.block_is_open() {
            self.goto(head);
        }

        self.current = exit;
        let _ = e;
        Operand::Constant(Constant::Unit)
    }

    pub(crate) fn lower_loop(&mut self, body: &HirBlock, e: &HirExpr) -> Operand {
        // A `loop` has no condition, so its head is its body: one block holds the
        // statements and a `break` jumps to the exit. A separate head block would
        // end up empty and unreachable, which is how `loop { break 5i64 }` produced
        // an unreachable value.
        let head = self.new_block();
        let body_bb = head;
        let exit = self.new_block();
        let value: Option<Place> = if !matches!(self.types.kind_owned(e.ty), TyKind::Unit | TyKind::Never) {
            Some(self.temp_place(e.ty))
        } else {
            None
        };

        self.goto(head);
        self.current = head;
        self.loop_stack.push(LoopTargets { exit, head, value: value.as_ref().map(|p| (p.local, e.ty)) });
        self.current = body_bb;
        self.lower_block(body);
        self.loop_stack.pop();
        if self.block_is_open() {
            self.goto(head);
        }

        self.current = exit;
        match value {
            Some(v) => Operand::Copy(v),
            None => Operand::Constant(Constant::Unit),
        }
    }

    /// `for x in 0..n` lowers to an induction variable and a loop; `for x in
    /// collection` lowers to an index walk over the collection.
    pub(crate) fn lower_for(
        &mut self,
        pattern_locals: &[LocalId],
        iter: &HirExpr,
        body: &HirBlock,
        e: &HirExpr,
    ) -> Operand {
        match &iter.kind {
            HirExprKind::RangeExpr { start, end, inclusive, elem } => {
                let start_op = self.lower_expr(start);
                let end_op = self.lower_expr(end);
                let index = self.temp_place(*elem);
                let end_tmp = self.temp_place(*elem);
                self.assign(index.clone(), Rvalue::Use(start_op), e.span);
                self.assign(end_tmp.clone(), Rvalue::Use(end_op), e.span);

                let head = self.new_block();
                let body_bb = self.new_block();
                let step = self.new_block();
                let exit = self.new_block();

                self.goto(head);
                self.current = head;
                let bt = self.bool_ty();
                let cmp = self.temp_place(bt);
                self.assign(
                    cmp.clone(),
                    Rvalue::Binary {
                        op: if *inclusive { BinOp::Le } else { BinOp::Lt },
                        lhs: Operand::Copy(index.clone()),
                        rhs: Operand::Copy(end_tmp.clone()),
                    },
                    e.span,
                );
                self.terminate(
                    Terminator::SwitchInt {
                        discr: Operand::Copy(cmp),
                        targets: vec![(1, body_bb)],
                        otherwise: exit,
                    },
                    body_bb,
                );

                self.loop_stack.push(LoopTargets { exit, head: step, value: None });
                self.current = body_bb;
                // The pattern binds the induction variable.
                if let Some(l) = pattern_locals.first() {
                    let target = self.place_of(*l);
                    self.assign(target, Rvalue::Use(Operand::Copy(index.clone())), e.span);
                }
                self.lower_block(body);
                self.loop_stack.pop();
                if self.block_is_open() {
                    self.goto(step);
                }

                self.current = step;
                let next_val = self.temp_place(*elem);
                let one = self.int_constant(1, *elem);
                self.assign(
                    next_val.clone(),
                    Rvalue::Binary { op: BinOp::Add, lhs: Operand::Copy(index.clone()), rhs: one },
                    e.span,
                );
                self.assign(index, Rvalue::Use(Operand::Copy(next_val)), e.span);
                self.goto(head);

                self.current = exit;
                Operand::Constant(Constant::Unit)
            }
            HirExprKind::IterSource { source, .. } => {
                // Index-based walk: arrays are contiguous, so `i` from 0 to len
                // covers them, and the pattern binds `source[i]`.
                let src = self.lower_expr(source);
                let src_place = match &src {
                    Operand::Copy(p) | Operand::Move(p) => p.clone(),
                    _ => {
                        let t = self.temp_place(self.local_ty_of_operand(&src));
                        self.assign(t.clone(), Rvalue::Use(src), e.span);
                        t
                    }
                };
                let src_ty = self.local_ty(src_place.local);
                let len_ty = self.index_ty();
                let index = self.temp_place(len_ty);
                let zero = self.int_constant(0, len_ty);
                self.assign(index.clone(), Rvalue::Use(zero), e.span);

                let head = self.new_block();
                let body_bb = self.new_block();
                let step = self.new_block();
                let exit = self.new_block();

                self.goto(head);
                self.current = head;
                let len = self.temp_place(len_ty);
                self.assign(len.clone(), Rvalue::Len(src_place.clone()), e.span);
                let bt = self.bool_ty();
                let cmp = self.temp_place(bt);
                self.assign(
                    cmp.clone(),
                    Rvalue::Binary {
                        op: BinOp::Lt,
                        lhs: Operand::Copy(index.clone()),
                        rhs: Operand::Copy(len),
                    },
                    e.span,
                );
                self.terminate(
                    Terminator::SwitchInt {
                        discr: Operand::Copy(cmp),
                        targets: vec![(1, body_bb)],
                        otherwise: exit,
                    },
                    body_bb,
                );

                self.loop_stack.push(LoopTargets { exit, head: step, value: None });
                self.current = body_bb;
                let elem_ty = match self.types.kind(src_ty).clone() {
                    TyKind::Array { elem, .. } | TyKind::Slice(elem) => elem,
                    _ => self.types.unit(),
                };
                if let Some(l) = pattern_locals.first() {
                    let target = self.place_of(*l);
                    let elem = src_place.clone().index(Operand::Copy(index.clone()));
                    let tmp = self.temp_place(elem_ty);
                    self.assign(tmp.clone(), Rvalue::Use(Operand::Copy(elem)), e.span);
                    self.assign(target, Rvalue::Use(Operand::Copy(tmp)), e.span);
                }
                self.lower_block(body);
                self.loop_stack.pop();
                if self.block_is_open() {
                    self.goto(step);
                }

                self.current = step;
                let next_val = self.temp_place(len_ty);
                let one = self.int_constant(1, len_ty);
                self.assign(
                    next_val.clone(),
                    Rvalue::Binary { op: BinOp::Add, lhs: Operand::Copy(index.clone()), rhs: one },
                    e.span,
                );
                self.assign(index, Rvalue::Use(Operand::Copy(next_val)), e.span);
                self.goto(head);

                self.current = exit;
                Operand::Constant(Constant::Unit)
            }
            _ => Operand::Constant(Constant::Unit),
        }
    }

    /// `o?` lowers to a discriminant test plus an early return.
    pub(crate) fn lower_try(&mut self, inner: &HirExpr, e: &HirExpr) -> Operand {
        let op = self.lower_expr(inner);
        let place = match &op {
            Operand::Copy(p) | Operand::Move(p) => p.clone(),
            _ => {
                let t = self.temp_place(self.local_ty_of_operand(&op));
                self.assign(t.clone(), Rvalue::Use(op), e.span);
                t
            }
        };
        let value_ty = e.ty;
        let ok_block = self.new_block();
        let fail_block = self.new_block();
        let join = self.new_block();

        let it = self.index_ty();
        let discr = self.temp_place(it);
        self.assign(discr.clone(), Rvalue::Discriminant(place.clone()), e.span);
        // Variant 0 is the success case for both `Option` (`Some`) and
        // `Result` (`Ok`), which is how the prelude declares them.
        self.terminate(
            Terminator::SwitchInt {
                discr: Operand::Copy(discr),
                targets: vec![(0, ok_block)],
                otherwise: fail_block,
            },
            ok_block,
        );

        self.current = ok_block;
        let payload = self.temp_place(value_ty);
        // The success payload is field 0 of the variant.
        self.assign(payload.clone(), Rvalue::Use(Operand::Copy(place.clone().field(0))), e.span);
        self.goto(join);

        self.current = fail_block;
        // Propagate the failure value unchanged and leave the function.
        self.assign(Place::local(self.ret_local), Rvalue::Use(Operand::Move(place)), e.span);
        let dead = self.new_block();
        self.terminate(Terminator::Return, dead);

        self.current = join;
        Operand::Copy(payload)
    }
}

pub(crate) fn convert_binop(op: HirBinaryOp) -> BinOp {
    match op {
        HirBinaryOp::Add => BinOp::Add,
        HirBinaryOp::Sub => BinOp::Sub,
        HirBinaryOp::Mul => BinOp::Mul,
        HirBinaryOp::Div => BinOp::Div,
        HirBinaryOp::Rem => BinOp::Rem,
        HirBinaryOp::BitAnd => BinOp::BitAnd,
        HirBinaryOp::BitOr => BinOp::BitOr,
        HirBinaryOp::BitXor => BinOp::BitXor,
        HirBinaryOp::Shl => BinOp::Shl,
        HirBinaryOp::Shr => BinOp::Shr,
        HirBinaryOp::Eq => BinOp::Eq,
        HirBinaryOp::Ne => BinOp::Ne,
        HirBinaryOp::Lt => BinOp::Lt,
        HirBinaryOp::Le => BinOp::Le,
        HirBinaryOp::Gt => BinOp::Gt,
        HirBinaryOp::Ge => BinOp::Ge,
        HirBinaryOp::And => BinOp::LogicalAnd,
        HirBinaryOp::Or => BinOp::LogicalOr,
    }
}
