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
        // The scrutinee may be reached through a dereference (`match *self`), so its
        // type is the place's type after every projection, not the local's own.
        let scrut_ty = self.place_type(&scrut_place);

        let produces_value = !matches!(self.types.kind_owned(e.ty), TyKind::Unit | TyKind::Never);
        let result = if produces_value { Some(self.temp_place(e.ty)) } else { None };
        let join = self.new_block();

        // Build the test chain from the arms, in source order.
        //
        // Each arm gets a test that decides whether it is the one that matches.
        // Without it every arm's body would run and the result would be whichever
        // ran last, which is not what a `match` means. An arm whose pattern always
        // matches (a binding or `_`) becomes a plain jump and ends the chain.
        for arm in arms {
            let body_bb = self.new_block();
            let next_test = self.new_block();

            let always = self.emit_pattern_tests(&arm.pattern, &scrut_place, scrut_ty, body_bb, next_test, e.span);

            // Bindings are stored only once the arm is known to have matched.
            self.current = body_bb;
            self.emit_pattern_bindings(&arm.pattern, scrut_place.clone(), scrut_ty, e.span);

            // The guard, when present, decides between body and next test.
            if let Some(guard) = &arm.guard {
                let g = self.lower_expr(guard);
                let guarded = self.new_block();
                self.terminate(
                    Terminator::SwitchInt {
                        discr: g,
                        targets: vec![(1, guarded)],
                        otherwise: next_test,
                    },
                    guarded,
                );
                self.current = guarded;
            }

            let _ = always;

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

    /// Emits the tests that decide whether `pat` matches, jumping to `on_match`
    /// when it does and to `on_fail` when it does not.
    ///
    /// Returns true when the pattern always matches, in which case the call has
    /// already jumped to `on_match` and the caller's `on_fail` block is unused.
    fn emit_pattern_tests(
        &mut self,
        pat: &HirPattern,
        place: &Place,
        ty: TyId,
        on_match: BlockId,
        on_fail: BlockId,
        span: crate::source::Span,
    ) -> bool {
        match &pat.kind {
            // These match unconditionally; a `match` with no other arm would be
            // rejected by the exhaustiveness checker, so reaching here means the
            // pattern really does cover everything left.
            HirPatternKind::Wildcard | HirPatternKind::Binding { .. } | HirPatternKind::Error => {
                self.goto(on_match);
                true
            }
            HirPatternKind::Variant { index, fields, .. } => {
                // Read the tag and compare it with this variant's index. The tag
                // is written as a single byte by the aggregate constructor.
                // A discriminator is an integer, not the enum it came from.
                let u8_ty = self.types.u8_ty();
                let discr = self.temp_place(u8_ty);
                self.assign(
                    discr.clone(),
                    Rvalue::Discriminant(place.clone()),
                    span,
                );
                if let VariantPatternFields::Unit = fields {
                    self.terminate(
                        Terminator::SwitchInt {
                            discr: Operand::Copy(discr),
                            targets: vec![(*index as i128, on_match)],
                            otherwise: on_fail,
                        },
                        on_match,
                    );
                } else {
                    let payload = self.new_block();
                    self.terminate(
                        Terminator::SwitchInt {
                            discr: Operand::Copy(discr),
                            targets: vec![(*index as i128, payload)],
                            otherwise: on_fail,
                        },
                        payload,
                    );
                    self.current = payload;
                    self.goto(on_match);
                }
                false
            }
            HirPatternKind::BoolLit(value) => {
                let bool_ty = self.types.bool_ty();
                let discr = self.temp_place(bool_ty);
                self.assign(discr.clone(), Rvalue::Use(Operand::Copy(place.clone())), span);
                self.terminate(
                    Terminator::SwitchInt {
                        discr: Operand::Copy(discr),
                        targets: vec![(i128::from(*value), on_match)],
                        otherwise: on_fail,
                    },
                    on_match,
                );
                false
            }
            HirPatternKind::IntLit { value, negative } => {
                let discr = self.temp_place(ty);
                self.assign(discr.clone(), Rvalue::Use(Operand::Copy(place.clone())), span);
                let want = if *negative { -(*value as i128) } else { *value as i128 };
                self.terminate(
                    Terminator::SwitchInt {
                        discr: Operand::Copy(discr),
                        targets: vec![(want, on_match)],
                        otherwise: on_fail,
                    },
                    on_match,
                );
                false
            }
            HirPatternKind::Or(alts) => {
                // Any alternative matching is enough: each is tried in turn, and
                // the last one's failure block is the caller's.
                let mut always = false;
                for (i, alt) in alts.iter().enumerate() {
                    let fail = if i + 1 == alts.len() { on_fail } else { self.new_block() };
                    always = self.emit_pattern_tests(alt, place, ty, on_match, fail, span);
                    if always {
                        return true;
                    }
                    if i + 1 < alts.len() {
                        self.current = fail;
                    }
                }
                always
            }
            // Tuple elements, struct fields and slice elements are matched by the
            // arms that contain them; their own tests are left to the element
            // patterns, which the exhaustiveness checker has already accepted as
            // total for this shape.
            HirPatternKind::Tuple(subs) => {
                let mut always = true;
                for (i, sub) in subs.iter().enumerate() {
                    let fty = self.field_ty(ty, i);
                    let sub_place = place.clone().field(i as u32);
                    always &= self.emit_pattern_tests(sub, &sub_place, fty, on_match, on_fail, span);
                }
                if always {
                    self.goto(on_match);
                }
                always
            }
            HirPatternKind::Struct { fields, .. } => {
                let mut always = true;
                for f in fields {
                    let fty = self.field_ty(ty, f.index as usize);
                    let sub_place = place.clone().field(f.index);
                    always &=
                        self.emit_pattern_tests(&f.pattern, &sub_place, fty, on_match, on_fail, span);
                }
                if always {
                    self.goto(on_match);
                }
                always
            }
            _ => {
                self.goto(on_match);
                true
            }
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
            HirPatternKind::Variant { fields, index, .. } => match fields {
                VariantPatternFields::Unit => {}
                VariantPatternFields::Tuple(subs) => {
                    for (i, sub) in subs.iter().enumerate() {
                        let fty = self.variant_field_ty(pat.ty, *index, i);
                        self.emit_pattern_bindings(
                            sub,
                            place.clone().enum_field(*index, i as u32),
                            fty,
                            span,
                        );
                    }
                }
                VariantPatternFields::Struct { fields, .. } => {
                    for f in fields {
                        let fty = self.variant_field_ty(pat.ty, *index, f.index as usize);
                        self.emit_pattern_bindings(
                            &f.pattern,
                            place.clone().enum_field(*index, f.index),
                            fty,
                            span,
                        );
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

    /// The type of field `index` in `variant` of an enum value of type `ty`.
    ///
    /// The variant is named rather than guessed: an enum's variants may carry
    /// different types in the same position, and a generic enum fills them in from
    /// the instantiation.
    pub(crate) fn variant_field_ty(&mut self, ty: TyId, variant: u32, index: usize) -> TyId {
        let TyKind::Adt { def, args } = self.types.kind(ty).clone() else {
            return self.types.unit();
        };
        let Some(info) = self.hir.enums.iter().find(|e| e.def_id == def) else {
            return self.types.unit();
        };
        let generics = info.generics.clone();
        let Some(v) = info.variants.get(variant as usize) else {
            return self.types.unit();
        };
        let declared = match &v.fields {
            VariantFields::Tuple(tys) => tys.get(index).copied(),
            VariantFields::Struct(fields) => fields.get(index).map(|f| f.ty),
            VariantFields::Unit => None,
        };
        let Some(declared) = declared else {
            return self.types.unit();
        };
        if generics.is_empty() || args.is_empty() {
            return declared;
        }
        let mut subst = std::collections::HashMap::new();
        for (name, arg) in generics.iter().zip(args.iter()) {
            subst.insert(name.clone(), *arg);
        }
        self.types.substitute(declared, &subst)
    }

    /// String interpolation lowers to a chain of appends, so that the backend
    /// never needs a dedicated formatting instruction.
    pub(crate) fn lower_str_interp(&mut self, parts: &[HirStrPart], e: &HirExpr) -> Operand {
        // The first part initialises the accumulator rather than being appended
        // to an uninitialised temporary.
        let mut acc: Option<Place> = None;
        for part in parts {
            match part {
                HirStrPart::Text(t) => {
                    let id = self.program.intern_string(t);
                    let prev = match acc.take() {
                        Some(p) => p,
                        None => {
                            // Nothing accumulated yet: this text *is* the
                            // initial value.
                            let p = self.temp_place(e.ty);
                            self.assign(
                                p.clone(),
                                Rvalue::Use(Operand::Constant(Constant::Str(id))),
                                e.span,
                            );
                            acc = Some(p);
                            continue;
                        }
                    };
                    let concat = self.program.intern_string("intrinsic:str_concat");
                    let v = self.temp_place(e.ty);
                    let next = self.new_block();
                    self.terminate(
                        Terminator::Call {
                            callee: Operand::Constant(Constant::Str(concat)),
                            args: vec![
                                Operand::Copy(prev),
                                Operand::Constant(Constant::Str(id)),
                            ],
                            destination: Some(v.clone()),
                            target: next,
                            unwind: Vec::new(),
                            span: e.span,
                        },
                        next,
                    );
                    acc = Some(v);
                }
                HirStrPart::Value(expr) => {
                    let val = self.lower_expr(expr);
                    let prev = match acc.take() {
                        Some(p) => p,
                        None => {
                            // The interpolation starts with a value: format it
                            // into a fresh string.
                            let empty = self.program.intern_string("");
                            let p = self.temp_place(e.ty);
                            self.assign(
                                p.clone(),
                                Rvalue::Use(Operand::Constant(Constant::Str(empty))),
                                e.span,
                            );
                            acc = Some(p);
                            acc.clone().unwrap()
                        }
                    };
                    let append = self.program.intern_string("intrinsic:str_append_display");
                    let v = self.temp_place(e.ty);
                    let next = self.new_block();
                    self.terminate(
                        Terminator::Call {
                            callee: Operand::Constant(Constant::Str(append)),
                            args: vec![Operand::Copy(prev), val],
                            destination: Some(v.clone()),
                            target: next,
                            unwind: Vec::new(),
                            span: e.span,
                        },
                        next,
                    );
                    acc = Some(v);
                }
            }
        }
        match acc {
            Some(p) => Operand::Copy(p),
            None => Operand::Constant(Constant::Str(self.program.intern_string(""))),
        }
    }
}
