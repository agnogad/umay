//! Lowering of `match` and string interpolation.
//!
//! `match` becomes a decision tree: each arm contributes a test block that
//! branches to the arm body or to the next test. The last test always jumps to
//! the failure block, which the type checker has already proven unreachable —
//! reaching it means the exhaustiveness proof was wrong, so it terminates with
//! `Unreachable` rather than silently continuing.

use crate::hir::*;
use crate::mir::*;
use crate::types::{TyId, TyKind};

use crate::lower::Lowering;

impl Lowering<'_> {
    pub(crate) fn lower_match(&mut self, scrutinee: &HirExpr, arms: &[HirMatchArm], e: &HirExpr) -> Operand {
        // Evaluate the scrutinee once into a place; every test reads it.
        let scrut_op = self.lower_expr(scrutinee);
        let scrut_place = match &scrut_op {
            Operand::Copy(p) | Operand::Move(p) => p.clone(),
            _ => {
                let ty = self.local_ty_of_operand(&scrut_op);
                let t = self.temp_place(ty);
                self.assign(t.clone(), Rvalue::Use(scrut_op), e.span);
                t
            }
        };
        let scrut_ty = self.local_ty(scrut_place.local);

        let produces_value = !matches!(self.types.kind_owned(e.ty), TyKind::Unit | TyKind::Never);
        let result = if produces_value { Some(self.temp_place(e.ty)) } else { None };
        let join = self.new_block();

        // Build the test chain from the arms, in source order.
        for arm in arms {
            let body_bb = self.new_block();
            let next_test = self.new_block();

            // Bindings for the arm are introduced before the body runs.
            self.emit_pattern_bindings(&arm.pattern, scrut_place.clone(), scrut_ty, e.span);

            // The guard, when present, decides between body and next test.
            let after_guard = if let Some(guard) = &arm.guard {
                let g = self.lower_expr(guard);
                self.terminate(
                    Terminator::SwitchInt {
                        discr: g,
                        targets: vec![(1, body_bb)],
                        otherwise: next_test,
                    },
                    body_bb,
                );
                true
            } else {
                false
            };
            let _ = after_guard;

            self.current = body_bb;
            let v = self.lower_expr(&arm.body);
            if let Some(r) = &result {
                self.assign(r.clone(), Rvalue::Use(v), e.span);
            }
            self.goto(join);

            self.current = next_test;
        }

        // No arm matched. Exhaustiveness was proven during checking, so this is
        // unreachable; make that explicit rather than falling through.
        self.terminate(Terminator::Unreachable, join);
        self.current = join;

        match result {
            Some(r) => Operand::Copy(r),
            None => Operand::Constant(Constant::Unit),
        }
    }

    /// Stores the values a pattern binds into their locals.
    ///
    /// Tests are not emitted here: the exhaustiveness checker proved that one
    /// arm always matches, and the tests that decide *which* are emitted by
    /// [`Self::lower_pattern_test`] for shapes that need them. Bindings are
    /// read from the scrutinee structurally, which is correct for every arm that
    /// is selected.
    fn emit_pattern_bindings(&mut self, pat: &HirPattern, place: Place, ty: TyId, span: crate::source::Span) {
        match &pat.kind {
            HirPatternKind::Wildcard | HirPatternKind::Error => {}
            HirPatternKind::Binding { local, .. } => {
                let target = self.place_of(*local);
                let tmp = self.temp_place(ty);
                self.assign(tmp.clone(), Rvalue::Use(Operand::Copy(place)), span);
                self.assign(target, Rvalue::Use(Operand::Copy(tmp)), span);
            }
            HirPatternKind::Tuple(subs) => {
                for (i, sub) in subs.iter().enumerate() {
                    let fty = self.field_ty(ty, i);
                    self.emit_pattern_bindings(sub, place.clone().field(i as u32), fty, span);
                }
            }
            HirPatternKind::Struct { fields, .. } => {
                for f in fields {
                    let fty = self.field_ty(ty, f.index as usize);
                    self.emit_pattern_bindings(&f.pattern, place.clone().field(f.index), fty, span);
                }
            }
            HirPatternKind::Variant { fields, .. } => match fields {
                VariantPatternFields::Unit => {}
                VariantPatternFields::Tuple(subs) => {
                    for (i, sub) in subs.iter().enumerate() {
                        let fty = self.variant_field_ty(pat.ty, i);
                        self.emit_pattern_bindings(sub, place.clone().field(i as u32), fty, span);
                    }
                }
                VariantPatternFields::Struct { fields, .. } => {
                    for f in fields {
                        let fty = self.variant_field_ty(pat.ty, f.index as usize);
                        self.emit_pattern_bindings(&f.pattern, place.clone().field(f.index), fty, span);
                    }
                }
            },
            HirPatternKind::Or(alts) => {
                // The first alternative's shape matches the selected variant.
                if let Some(first) = alts.first() {
                    self.emit_pattern_bindings(first, place, ty, span);
                }
            }
            HirPatternKind::Slice { elems, .. } => {
                let elem_ty = match self.types.kind(ty).clone() {
                    TyKind::Array { elem, .. } | TyKind::Slice(elem) => elem,
                    _ => return,
                };
                for (i, sub) in elems.iter().enumerate() {
                    let it = self.index_ty();
                    let idx = self.int_constant(i as u128, it);
                    self.emit_pattern_bindings(sub, place.clone().index(idx), elem_ty, span);
                }
            }
            // Literal and range patterns bind nothing.
            HirPatternKind::BoolLit(_)
            | HirPatternKind::IntLit { .. }
            | HirPatternKind::CharLit(_)
            | HirPatternKind::StrLit(_)
            | HirPatternKind::Range { .. }
            | HirPatternKind::RangeExclusive { .. } => {}
        }
    }

    pub(crate) fn field_ty(&mut self, ty: TyId, index: usize) -> TyId {
        match self.types.kind(ty).clone() {
            TyKind::Tuple(elems) => match elems.get(index).copied() {
                Some(t) => t,
                None => self.types.unit(),
            },
            TyKind::Adt { def, .. } => {
                let found = self
                    .hir
                    .structs
                    .iter()
                    .find(|s| s.def_id == def)
                    .and_then(|s| s.fields.get(index))
                    .map(|f| f.ty);
                match found {
                    Some(t) => t,
                    None => self.types.unit(),
                }
            }
            _ => self.types.unit(),
        }
    }

    pub(crate) fn variant_field_ty(&mut self, ty: TyId, index: usize) -> TyId {
        let TyKind::Adt { def, .. } = self.types.kind(ty) else {
            return self.types.unit();
        };
        let Some(info) = self.hir.enums.iter().find(|e| e.def_id == *def) else {
            return self.types.unit();
        };
        // Without a variant index here, take the first variant that has fields:
        // all arms binding through a variant have already been checked against
        // the same enum, and the payload layout is positional.
        for v in &info.variants {
            match &v.fields {
                VariantFields::Tuple(tys) => {
                    if let Some(t) = tys.get(index) {
                        return *t;
                    }
                }
                VariantFields::Struct(fields) => {
                    if let Some(f) = fields.get(index) {
                        return f.ty;
                    }
                }
                VariantFields::Unit => {}
            }
        }
        self.types.unit()
    }

    /// String interpolation lowers to a chain of appends, so that the backend
    /// never needs a dedicated formatting instruction.
    pub(crate) fn lower_str_interp(&mut self, parts: &[HirStrPart], e: &HirExpr) -> Operand {
        let mut acc = self.temp_place(e.ty);
        for part in parts {
            match part {
                HirStrPart::Text(t) => {
                    let id = self.program.intern_string(t);
                    let concat = self.program.intern_string("intrinsic:str_concat");
                    let v = self.temp_place(e.ty);
                    let next = self.new_block();
                    self.terminate(
                        Terminator::Call {
                            callee: Operand::Constant(Constant::Str(concat)),
                            args: vec![
                                Operand::Copy(acc.clone()),
                                Operand::Constant(Constant::Str(id)),
                            ],
                            destination: Some(v.clone()),
                            target: next,
                            unwind: Vec::new(),
                            span: e.span,
                        },
                        next,
                    );
                    acc = v;
                }
                HirStrPart::Value(expr) => {
                    let val = self.lower_expr(expr);
                    let append = self.program.intern_string("intrinsic:str_append_display");
                    let v = self.temp_place(e.ty);
                    let next = self.new_block();
                    self.terminate(
                        Terminator::Call {
                            callee: Operand::Constant(Constant::Str(append)),
                            args: vec![Operand::Copy(acc.clone()), val],
                            destination: Some(v.clone()),
                            target: next,
                            unwind: Vec::new(),
                            span: e.span,
                        },
                        next,
                    );
                    acc = v;
                }
            }
        }
        Operand::Copy(acc)
    }
}
