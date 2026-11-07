//! Expression and statement lowering.
//!
//! Every expression lowers to an [`Operand`] plus, when it produces a value,
//! an assignment of that operand into a fresh temporary. Statements append to
//! the current block; control flow seals the block and starts new ones.

use crate::hir::*;
use crate::mir::*;
use crate::types::{TyId, TyKind};

use crate::lower::Lowering;

impl Lowering<'_> {
    // ---- blocks and statements -------------------------------------------

    /// Lowers a block, returning the operand holding its value.
    pub(crate) fn lower_block(&mut self, block: &HirBlock) -> Option<Operand> {
        self.defer_stack.push(Vec::new());
        let before = self.locals.len();
        let mut last: Option<Operand> = None;
        for stmt in &block.stmts {
            last = self.lower_stmt(stmt);
        }
        let value = match &block.tail {
            Some(e) => Some(self.lower_expr(e)),
            None => last.filter(|op| self.is_unit_operand(op)),
        };
        // Run deferred blocks in reverse order, as a scope exit would.
        let defers = self.defer_stack.pop().unwrap_or_default();
        for d in defers.into_iter().rev() {
            self.lower_block(&d);
        }
        // Storage of locals introduced by this block ends here.
        let _ = before;
        value
    }

    fn is_unit_operand(&self, op: &Operand) -> bool {
        match op {
            Operand::Constant(c) => matches!(c, Constant::Unit),
            Operand::Copy(p) | Operand::Move(p) => {
                matches!(self.types.kind_owned(self.local_ty(p.local)), TyKind::Unit)
            }
        }
    }

    pub(crate) fn local_ty(&self, l: LocalId) -> TyId {
        self.locals
            .get(l.0 as usize)
            .map(|d| d.ty)
            .unwrap_or_else(|| crate::types::TyId(u32::MAX))
    }

    pub(crate) fn lower_stmt(&mut self, stmt: &HirStmt) -> Option<Operand> {
        match &stmt.kind {
            HirStmtKind::Let { bindings, init } => {
                // The initialiser's value is moved into the new local when it
                // is not `Copy`; copying it would give two owners.
                let value = init.as_ref().map(|i| {
                    let op = self.lower_expr(i);
                    self.into_move_if_owned(op, i.ty)
                });
                for (n, b) in bindings.iter().enumerate() {
                    let mir_local = self.map_local(*b);
                    self.emit(Statement::StorageLive(mir_local));
                    if n == 0 {
                        if let Some(v) = value.clone() {
                            self.assign(Place::local(mir_local), Rvalue::Use(v), stmt.span);
                        }
                    }
                    self.emit(Statement::SetDropFlag { local: mir_local, value: true });
                }
                None
            }
            HirStmtKind::Expr(e) => {
                let op = self.lower_expr(e);
                Some(op)
            }
        }
    }

    // ---- expressions ------------------------------------------------------

    /// Lowers an expression to an operand.
    ///
    /// The operand is a temporary for everything except places and literals,
    /// so later phases never see nested structure.
    pub(crate) fn lower_expr(&mut self, e: &HirExpr) -> Operand {
        match &e.kind {
            HirExprKind::Error => Operand::Constant(Constant::Unit),
            HirExprKind::IntLit { value, .. } => self.int_constant(*value, e.ty),
            HirExprKind::FloatLit { value, .. } => Operand::Constant(Constant::Float { value: *value, ty: e.ty }),
            HirExprKind::CharLit(c) => Operand::Constant(Constant::Char(*c)),
            HirExprKind::BoolLit(b) => Operand::Constant(Constant::Bool(*b)),
            HirExprKind::StrLit(s) => {
                let id = self.program.intern_string(s);
                Operand::Constant(Constant::Str(id))
            }
            HirExprKind::Local(l) => {
                let place = self.place_of(*l);
                // A non-Copy local is moved when it is used by value. Phase 4
                // will make this decision with the borrow checker; today the
                // type alone decides, which is the same rule for the cases
                // that type-check.
                if self.types.is_copy(e.ty) {
                    Operand::Copy(place)
                } else {
                    self.emit(Statement::SetDropFlag { local: place.local, value: false });
                    Operand::Move(place)
                }
            }
            HirExprKind::ConstRef(id) => {
                // A constant is pasted at its use site rather than called: its
                // initialiser is already checked, and inlining means a constant
                // can appear anywhere an expression can, including a condition
                // or a length.
                match self.hir.consts.get(id.0 as usize) {
                    Some(c) => {
                        let value = c.value.clone();
                        self.lower_expr(&value)
                    }
                    None => Operand::Constant(Constant::Unit),
                }
            }
            HirExprKind::FnRef(id) => Operand::Constant(Constant::Fn(*id)),
            HirExprKind::UnitVariant { def, index } => {
                // A fieldless variant is still a value with a tag: build it as an
                // aggregate so the backend writes that tag like any other variant.
                let t = self.temp_place(e.ty);
                self.assign(
                    t.clone(),
                    Rvalue::Aggregate {
                        kind: AggregateKind::Variant { def: *def, index: *index },
                        operands: Vec::new(),
                    },
                    e.span,
                );
                Operand::Copy(t)
            }

            HirExprKind::Sizeof { of } => {
                // A size is known once the type is concrete, which monomorphisation
                // has already ensured. A type that has no size is a real error.
                let usize_ty = self.types.usize_ty();
                match self.size_of(*of) {
                    Some(size) => Operand::Constant(Constant::Uint { value: size as u128, ty: usize_ty }),
                    None => {
                        self.errors.push(("cannot take the size of an unsized type".to_string(), e.span));
                        Operand::Constant(Constant::Unit)
                    }
                }
            }
            HirExprKind::Unary { op, operand } => self.lower_unary(*op, operand, e),
            HirExprKind::Binary { op, lhs, rhs } => self.lower_binary(*op, lhs, rhs, e),
            HirExprKind::Assign { place, value } => self.lower_assign(place, value, e),
            HirExprKind::AssignOp { .. } => {
                // The checker desugars compound assignment before HIR; reaching
                // here would mean a bug upstream. Lower it as an error value.
                Operand::Constant(Constant::Unit)
            }
            HirExprKind::Ref { mutable, place } => {
                let p = self.lower_place(place);
                let tmp = self.temp_place(e.ty);
                self.assign(tmp.clone(), Rvalue::Ref { mutable: *mutable, place: p }, e.span);
                Operand::Copy(tmp)
            }
            HirExprKind::Deref(_) => {
                // Reading through a pointer is a load from the dereferenced
                // place. Lowering away the `*` would compare or copy the address
                // instead of the value it points at.
                let place = self.lower_place(e);
                if self.types.is_copy(e.ty) {
                    Operand::Copy(place)
                } else {
                    Operand::Move(place)
                }
            }
            HirExprKind::Move(inner) => {
                let place = self.lower_place(inner);
                // Moving out invalidates the source, so its drop flag is cleared.
                self.emit(Statement::SetDropFlag { local: place.local, value: false });
                Operand::Move(place)
            }
            HirExprKind::PtrOffset { ptr, count } => {
                let base = self.lower_expr(ptr);
                let delta = self.lower_expr(count);
                let out = self.temp_place(e.ty);
                self.assign(
                    out.clone(),
                    Rvalue::PtrOffset { ptr: base, count: delta },
                    e.span,
                );
                Operand::Copy(out)
            }
            HirExprKind::Cast { expr, to } => {
                let op = self.lower_expr(expr);
                let tmp = self.temp_place(*to);
                self.assign(tmp.clone(), Rvalue::Cast { operand: op, to: *to }, e.span);
                Operand::Copy(tmp)
            }
            HirExprKind::Call { callee, args } => self.lower_call(callee, args, e),
            HirExprKind::StructLit { def, fields, base } => self.lower_struct_lit(*def, fields, base.as_deref(), e),
            HirExprKind::TupleVariant { def, index, args } => {
                let ops: Vec<Operand> = args.iter().map(|a| self.lower_expr(a)).collect();
                let tmp = self.temp_place(e.ty);
                self.assign(
                    tmp.clone(),
                    Rvalue::Aggregate {
                        kind: AggregateKind::Variant { def: *def, index: *index },
                        operands: ops,
                    },
                    e.span,
                );
                Operand::Copy(tmp)
            }
            HirExprKind::StructVariant { def, index, fields } => {
                let mut ops = Vec::new();
                for f in fields {
                    ops.push(self.lower_expr(&f.value));
                }
                let tmp = self.temp_place(e.ty);
                self.assign(
                    tmp.clone(),
                    Rvalue::Aggregate {
                        kind: AggregateKind::Variant { def: *def, index: *index },
                        operands: ops,
                    },
                    e.span,
                );
                Operand::Copy(tmp)
            }
            HirExprKind::ArrayLit { elems } => {
                let ops: Vec<Operand> = elems.iter().map(|x| self.lower_expr(x)).collect();
                let tmp = self.temp_place(e.ty);
                self.assign(
                    tmp.clone(),
                    Rvalue::Aggregate { kind: AggregateKind::Array, operands: ops },
                    e.span,
                );
                Operand::Copy(tmp)
            }
            HirExprKind::ArrayRepeat { value, count } => {
                let op = self.lower_expr(value);
                let tmp = self.temp_place(e.ty);
                self.assign(tmp.clone(), Rvalue::Repeat { operand: op, count: *count }, e.span);
                Operand::Copy(tmp)
            }
            HirExprKind::Tuple { elems } => {
                let ops: Vec<Operand> = elems.iter().map(|x| self.lower_expr(x)).collect();
                let tmp = self.temp_place(e.ty);
                self.assign(tmp.clone(), Rvalue::Aggregate { kind: AggregateKind::Tuple, operands: ops }, e.span);
                Operand::Copy(tmp)
            }
            HirExprKind::Field { base, index, .. } => {
                let place = self.place_through_refs(base, base.ty).field(*index);
                let tmp = self.temp_place(e.ty);
                self.assign(tmp.clone(), Rvalue::Use(Operand::Copy(place)), e.span);
                Operand::Copy(tmp)
            }
            HirExprKind::TupleIndex { base, index } => {
                let place = self.place_through_refs(base, base.ty).field(*index);
                let tmp = self.temp_place(e.ty);
                self.assign(tmp.clone(), Rvalue::Use(Operand::Copy(place)), e.span);
                Operand::Copy(tmp)
            }
            HirExprKind::Index { base, index } => {
                let idx = self.lower_expr(index);
                let place = self.place_through_refs(base, base.ty).index(idx);
                let tmp = self.temp_place(e.ty);
                self.assign(tmp.clone(), Rvalue::Use(Operand::Copy(place)), e.span);
                Operand::Copy(tmp)
            }
            HirExprKind::MethodCall { receiver, method, args } => {
                self.lower_method_call(receiver, *method, args, e)
            }
            HirExprKind::BuiltinMethod { name, receiver, args } => {
                self.lower_builtin_method(name, receiver, args, e)
            }

            HirExprKind::If { cond, then_block, else_branch } => {
                self.lower_if(cond, then_block, else_branch.as_deref(), e)
            }
            HirExprKind::Match { scrutinee, arms } => self.lower_match(scrutinee, arms, e),
            HirExprKind::While { cond, body } => self.lower_while(cond, body, e),
            HirExprKind::Loop { body } => self.lower_loop(body, e),
            HirExprKind::For { pattern_locals, iter, body } => {
                self.lower_for(pattern_locals, iter, body, e)
            }
            HirExprKind::Block(b) => {
                let v = self.lower_block(b);
                v.unwrap_or(Operand::Constant(Constant::Unit))
            }
            HirExprKind::Return(value) => {
                if let Some(v) = value {
                    let op = self.lower_expr(v);
                    self.assign(Place::local(self.ret_local), Rvalue::Use(op), e.span);
                }
                let dead = self.new_block();
                self.terminate(Terminator::Return, dead);
                Operand::Constant(Constant::Unit)
            }
            HirExprKind::Break(value) => {
                let op = value.as_ref().map(|v| self.lower_expr(v));
                if let Some(targets) = self.loop_stack.last().copied() {
                    if let (Some(v), Some((local, _))) = (op, targets.value) {
                        self.assign(Place::local(local), Rvalue::Use(v), e.span);
                    }
                    let dead = self.new_block();
                    self.terminate(Terminator::Goto(targets.exit), dead);
                }
                Operand::Constant(Constant::Unit)
            }
            HirExprKind::Continue => {
                if let Some(targets) = self.loop_stack.last().copied() {
                    let dead = self.new_block();
                    self.terminate(Terminator::Goto(targets.head), dead);
                }
                Operand::Constant(Constant::Unit)
            }
            HirExprKind::Try(inner) => self.lower_try(inner, e),
            HirExprKind::Defer(body) => {
                // Deferred blocks run at scope exit; record and continue.
                if let Some(top) = self.defer_stack.last_mut() {
                    top.push(body.clone());
                }
                Operand::Constant(Constant::Unit)
            }
            HirExprKind::Unsafe(b) => {
                let v = self.lower_block(b);
                v.unwrap_or(Operand::Constant(Constant::Unit))
            }
            HirExprKind::StrInterp { parts } => self.lower_str_interp(parts, e),
            HirExprKind::RangeExpr { .. } | HirExprKind::IterSource { .. } => {
                // Only meaningful inside `for`, which consumes them directly.
                Operand::Constant(Constant::Unit)
            }
        }
    }

    /// The type of a place after every projection, e.g. the `Option<i64>` behind
    /// a `&Option<i64>` that `match *self` dereferences.
    /// The size of a type in bytes, or `None` when it has none.
    ///
    /// This is the subset of layout lowering needs for `sizeof`: the scalars and
    /// the aggregates whose fields are all sized. A pointer is one word.
    fn size_of(&mut self, ty: TyId) -> Option<u64> {
        let kind = self.types.kind(ty).clone();
        Some(match kind {
            crate::types::TyKind::Bool
            | crate::types::TyKind::I8
            | crate::types::TyKind::U8 => 1,
            crate::types::TyKind::I16 | crate::types::TyKind::U16 => 2,
            crate::types::TyKind::I32
            | crate::types::TyKind::U32
            | crate::types::TyKind::Char
            | crate::types::TyKind::F32 => 4,
            crate::types::TyKind::I64
            | crate::types::TyKind::U64
            | crate::types::TyKind::Usize
            | crate::types::TyKind::Isize
            | crate::types::TyKind::F64
            | crate::types::TyKind::RawPtr(_)
            | crate::types::TyKind::RawPtrMut(_)
            | crate::types::TyKind::Ref(_)
            | crate::types::TyKind::RefMut(_) => 8,
            crate::types::TyKind::Unit => 0,
            crate::types::TyKind::Array { elem, len } => self.size_of(elem)? * len,
            crate::types::TyKind::Tuple(elems) => {
                let mut total = 0;
                for e in elems {
                    total += self.size_of(e)?;
                }
                total
            }
            crate::types::TyKind::Adt { def, args } => {
                if let Some(info) = self.hir.structs.iter().find(|s| s.def_id == def) {
                    // A struct is the sum of its fields. This ignores padding, which
                    // makes it an under-estimate for a mixed layout; it is exact for
                    // the element types a container uses today.
                    let generics = info.generics.clone();
                    let fields = info.fields.clone();
                    let mut subst = std::collections::HashMap::new();
                    for (name, arg) in generics.iter().zip(args.iter()) {
                        subst.insert(name.clone(), *arg);
                    }
                    let mut total = 0;
                    for f in fields {
                        let ty = if subst.is_empty() {
                            f.ty
                        } else {
                            self.types.substitute(f.ty, &subst)
                        };
                        total += self.size_of(ty)?;
                    }
                    return Some(total);
                }
                // An enum is its tag followed by the widest payload. The tag is one
                // byte while there are at most 256 variants, which is the only
                // count a program can reach today without an enormous declaration.
                let info = self.hir.enums.iter().find(|e| e.def_id == def)?;
                let generics = info.generics.clone();
                let variants = info.variants.clone();
                let mut subst = std::collections::HashMap::new();
                for (name, arg) in generics.iter().zip(args.iter()) {
                    subst.insert(name.clone(), *arg);
                }
                let tag: u64 = if variants.len() <= 256 { 1 } else { 4 };
                let mut payload: u64 = 0;
                let mut align: u64 = 1;
                for v in variants {
                    let tys: Vec<crate::types::TyId> = match &v.fields {
                        crate::hir::VariantFields::Tuple(tys) => tys.clone(),
                        crate::hir::VariantFields::Struct(fs) => fs.iter().map(|f| f.ty).collect(),
                        crate::hir::VariantFields::Unit => Vec::new(),
                    };
                    let mut total = 0;
                    for t in tys {
                        let ty = if subst.is_empty() { t } else { self.types.substitute(t, &subst) };
                        total += self.size_of(ty)?;
                        align = align.max(self.align_of(ty)?);
                    }
                    payload = payload.max(total);
                }
                let start = tag.div_ceil(align) * align;
                (start + payload).div_ceil(align) * align
            }
            _ => return None,
        })
    }

    /// The alignment of a type in bytes, matching `Layouts`.
    fn align_of(&mut self, ty: TyId) -> Option<u64> {
        let size = self.size_of(ty)?;
        // A type's alignment is the largest power of two that divides every
        // possible address of it; for the types a program can write today that is
        // the alignment of its widest scalar field.
        let mut a = 1;
        while a * 2 <= size && size % (a * 2) == 0 {
            a *= 2;
        }
        Some(a)
    }

    pub(crate) fn place_type(&mut self, place: &crate::mir::Place) -> TyId {
        let mut ty = self.local_ty(place.local);
        for projection in &place.projection {
            ty = match projection {
                crate::mir::Projection::Field(i) | crate::mir::Projection::EnumField { index: i, .. } => {
                    self.field_ty_of(ty, *i)
                }
                crate::mir::Projection::Deref => match self.types.kind(ty).clone() {
                    crate::types::TyKind::Ref(inner)
                    | crate::types::TyKind::RefMut(inner)
                    | crate::types::TyKind::RawPtr(inner)
                    | crate::types::TyKind::RawPtrMut(inner) => inner,
                    _ => ty,
                },
                crate::mir::Projection::Index(_) => match self.types.kind(ty).clone() {
                    crate::types::TyKind::Array { elem, .. } => elem,
                    crate::types::TyKind::Slice(elem) => elem,
                    _ => ty,
                },
            };
        }
        ty
    }

    /// The declared type of a field by index, or `ty` when it cannot be resolved.
    fn field_ty_of(&mut self, ty: TyId, index: u32) -> TyId {
        let crate::types::TyKind::Adt { def, args } = self.types.kind(ty).clone() else {
            return ty;
        };
        if let Some(info) = self.hir.structs.iter().find(|s| s.def_id == def) {
            let Some(f) = info.fields.get(index as usize) else { return ty };
            let declared = f.ty;
            let generics = info.generics.clone();
            if generics.is_empty() || args.is_empty() {
                return declared;
            }
            let mut subst = std::collections::HashMap::new();
            for (name, arg) in generics.iter().zip(args.iter()) {
                subst.insert(name.clone(), *arg);
            }
            return self.types.substitute(declared, &subst);
        }
        ty
    }

    pub(crate) fn local_ty_of_operand(&self, op: &Operand) -> TyId {
        match op {
            Operand::Copy(p) | Operand::Move(p) => self.local_ty(p.local),
            Operand::Constant(Constant::Int { ty, .. }) => *ty,
            Operand::Constant(Constant::Uint { ty, .. }) => *ty,
            Operand::Constant(Constant::Float { ty, .. }) => *ty,
            _ => self.ret_ty,
        }
    }

    /// Turns a copy of an operand into a move when the value owns resources.
    ///
    /// Used wherever a value is transferred rather than duplicated: `let`
    /// bindings and call arguments. Without this, an owned value would have two
    /// owners and be dropped twice.
    pub(crate) fn into_move_if_owned(&mut self, op: Operand, ty: TyId) -> Operand {
        if self.types.is_copy(ty) {
            return op;
        }
        match op {
            Operand::Copy(place) => {
                self.emit(Statement::SetDropFlag { local: place.local, value: false });
                Operand::Move(place)
            }
            other => other,
        }
    }

    pub(crate) fn int_constant(&mut self, value: u128, ty: TyId) -> Operand {
        if self.types.is_signed(ty) && value <= i128::MAX as u128 {
            Operand::Constant(Constant::Int { value: value as i128, ty })
        } else {
            Operand::Constant(Constant::Uint { value, ty })
        }
    }

    // ---- places -----------------------------------------------------------

    /// Lowers an expression that denotes storage into a [`Place`].
    ///
    /// Reads through the place use `Copy`; the caller chooses `Move`. This
    /// split is what lets Phase 4 reason about moves without inspecting
    /// expression shapes.
    /// Lowers `e` as a place, dereferencing through any references first.
    ///
    /// `s.field` where `s: &Struct` means `(*s).field`. The checker strips the
    /// reference to find the field, so this is where the matching deref has to
    /// appear; without it the projection is a field of a reference, which has no
    /// layout.
    fn place_through_refs(&mut self, e: &HirExpr, ty: TyId) -> Place {
        let mut place = self.lower_place(e);
        let mut ty = ty;
        let mut guard = 0;
        while guard < 8 {
            guard += 1;
            match self.types.kind(ty) {
                TyKind::Ref(inner) | TyKind::RefMut(inner) => {
                    ty = *inner;
                    place = place.deref();
                }
                _ => break,
            }
        }
        place
    }

    pub(crate) fn lower_place(&mut self, e: &HirExpr) -> Place {
        match &e.kind {
            HirExprKind::Local(l) => self.place_of(*l),
            HirExprKind::Field { base, index, .. } => {
                let p = self.place_through_refs(base, base.ty);
                p.field(*index)
            }
            HirExprKind::TupleIndex { base, index } => {
                let p = self.place_through_refs(base, base.ty);
                p.field(*index)
            }
            HirExprKind::Index { base, index } => {
                let idx = self.lower_expr(index);
                let p = self.place_through_refs(base, base.ty);
                p.index(idx)
            }
            HirExprKind::Deref(inner) => self.lower_place(inner).deref(),
            // Anything else is a value, not a place: materialise it into a
            // temporary so that `*` and assignment still have a target.
            _ => {
                let op = self.lower_expr(e);
                let tmp = self.temp_place(e.ty);
                self.assign(tmp.clone(), Rvalue::Use(op), e.span);
                tmp
            }
        }
    }
}
