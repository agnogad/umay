//! MIR pass: dropping an owned `dyn Trait` goes through its vtable.
//!
//! Sema gives an owned trait object a non-`Copy` wrapper struct so drop
//! elaboration emits a `Terminator::Drop` for it. The backends intentionally
//! treat `Drop` as a no-op; this pass turns that one terminator into the indirect
//! call the ABI actually requires: load the vtable pointer from word 1 of the
//! fat pointer, load word 2 of the table (the deallocation shim), and call it
//! with word 0 (the data pointer). No new MIR or backend shape is needed.

use crate::hir::{Hir, LocalId};
use crate::mir::*;
use crate::types::{DefId, TyCtxt, TyId, TyKind};

/// Vtable word 2: `{ size, align, drop, methods... }` (ADR 0018).
const DROP_SLOT_OFFSET: u128 = 2 * 8;

/// Rewrites drops of compiler-generated owning trait objects into indirect
/// calls. Runs after drop elaboration and ownership checking, before optimisation.
pub fn rewrite_owned_dyn_drops(program: &mut MirProgram, types: &mut TyCtxt, hir: &Hir) {
    let own_defs: Vec<DefId> = hir
        .structs
        .iter()
        .filter(|s| s.name.starts_with("__umay_dyn_own_"))
        .map(|s| s.def_id)
        .collect();
    if own_defs.is_empty() {
        return;
    }

    for body in &mut program.bodies {
        let targets: Vec<(BlockId, Place, BlockId)> = body
            .blocks
            .iter()
            .enumerate()
            .filter_map(|(i, block)| match &block.terminator {
                Terminator::Drop { place, target }
                    if place.projection.is_empty()
                        && is_own(types, body.locals[place.local.0 as usize].ty, &own_defs) =>
                {
                    Some((BlockId(i as u32), place.clone(), *target))
                }
                _ => None,
            })
            .collect();
        if targets.is_empty() {
            continue;
        }

        let span = body.span;
        let u8 = types.primitive("u8").unwrap_or_else(|| types.error());
        let data_ty = types.raw_ptr_mut(u8);
        let unit = types.unit();
        let drop_fn_ty = types.fn_ptr(vec![data_ty], unit);
        let slot_ty = types.raw_ptr(drop_fn_ty);
        let slot_ptr_ty = types.raw_ptr(slot_ty);
        let byte_ty = types.raw_ptr(u8);
        let usize_ty = types.usize_ty();

        for (block_id, place, target) in targets {
            // The wrapper has one field, the fat pointer: word 0 is data,
            // word 1 is the vtable address.
            let data_word = Place::local(place.local).field(0).field(0);
            let vtable_word = Place::local(place.local).field(0).field(1);

            // Mirror the shape a dyn method call already lowers to: the loaded
            // slot value lives in a local, and the callee is that local with one
            // dereference, which the backends' `fn_ptr_place` folds away.
            // `PtrOffset` scales by the destination's pointee size, so the
            // byte offset is computed on a byte pointer and then cast.
            let byte_slot = LocalId(body.locals.len() as u32);
            body.locals.push(LocalDecl {
                name: "__dyn_drop_byte".into(),
                ty: byte_ty,
                mutable: false,
                span,
                is_temp: true,
            });
            let slot_ptr = LocalId(body.locals.len() as u32);
            body.locals.push(LocalDecl {
                name: "__dyn_drop_slot".into(),
                ty: slot_ptr_ty,
                mutable: false,
                span,
                is_temp: true,
            });
            let drop_fn = LocalId(body.locals.len() as u32);
            body.locals.push(LocalDecl {
                name: "__dyn_drop_fn".into(),
                ty: slot_ty,
                mutable: false,
                span,
                is_temp: true,
            });

            let block = body.block_mut(block_id);
            block.statements.push(Statement::Assign {
                place: Place::local(byte_slot),
                value: Rvalue::PtrOffset {
                    ptr: Operand::Copy(vtable_word),
                    count: Operand::Constant(Constant::Uint { value: DROP_SLOT_OFFSET, ty: usize_ty }),
                },
                span,
            });
            block.statements.push(Statement::Assign {
                place: Place::local(slot_ptr),
                value: Rvalue::Cast { operand: Operand::Copy(Place::local(byte_slot)), to: slot_ptr_ty },
                span,
            });
            block.statements.push(Statement::Assign {
                place: Place::local(drop_fn),
                value: Rvalue::Use(Operand::Copy(Place::local(slot_ptr).deref())),
                span,
            });
            block.terminator = Terminator::Call {
                callee: Operand::Copy(Place::local(drop_fn).deref()),
                args: vec![Operand::Copy(data_word)],
                destination: None,
                target,
                unwind: Vec::new(),
                span,
            };
        }
    }
}

fn is_own(types: &TyCtxt, ty: TyId, defs: &[DefId]) -> bool {
    matches!(types.kind(ty), TyKind::Adt { def, .. } if defs.contains(def))
}
