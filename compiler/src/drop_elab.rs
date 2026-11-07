//! Drop elaboration.
//!
//! Ownership decides *that* something is dropped; this pass decides *where*.
//! The rules are the ones a user expects:
//!
//! - A local is dropped when its scope ends, which in MIR is when control
//!   leaves the region the local was declared in.
//! - A local is not dropped if it was moved out, because the value now lives
//!   somewhere else and will be dropped there.
//! - A local is not dropped twice: after a drop on one path, other paths out of
//!   the same region must not drop it again.
//!
//! The pass runs over the CFG rather than the statement list, because the
//! second rule is a property of paths, not of a single block: a `return` in the
//! middle of a loop has to drop the bindings of the enclosing function scope
//! even though the statements that declared them are in another block.

use std::collections::{BTreeMap, BTreeSet};

use crate::hir::LocalId;
use crate::mir::*;
use crate::types::TyCtxt;

/// Adds drop terminators and `StorageDead` statements to every body.
///
/// Bodies whose locals all need no drop are left untouched, so simple
/// functions do not gain a single block.
pub fn elaborate(program: &mut MirProgram, types: &TyCtxt) {
    for body in &mut program.bodies {
        elaborate_body(body, types);
    }
}

fn elaborate_body(body: &mut Body, types: &TyCtxt) {
    let droppable: BTreeSet<LocalId> = body
        .locals
        .iter()
        .enumerate()
        .filter(|(_, d)| needs_drop(d.ty, types))
        .map(|(i, _)| LocalId(i as u32))
        .collect();
    if droppable.is_empty() {
        return;
    }

    // Reachability bounds the live analysis: a block nothing jumps to has no
    // effect on the program and must not contribute drops.
    let reachable = compute_reachable(body);

    // A local is live at a block's entry when some path reaches the block with
    // the drop flag still set. A `SetDropFlag(false)` (a move) kills it, and a
    // `Drop` kills it for every path that continues from there.
    let mut live_in: Vec<BTreeSet<LocalId>> = vec![BTreeSet::new(); body.blocks.len()];
    let mut changed = true;
    let mut iterations = 0;
    // The lattice is finite and the transfer function monotone, so this
    // terminates; the bound only guards against a bug turning it into a hang.
    let limit = body.blocks.len().saturating_mul(droppable.len()) + 16;
    while changed && iterations < limit {
        changed = false;
        iterations += 1;
        for i in 0..body.blocks.len() {
            // Union of the predecessors' drop state after their terminator.
            // Only reachable predecessors contribute: a block that can never
            // run must not make a value look live.
            let mut incoming = BTreeSet::new();
            for (j, other) in body.blocks.iter().enumerate() {
                if !reachable[j] {
                    continue;
                }
                if other.terminator.successors().contains(&BlockId(i as u32)) {
                    let mut out = live_in[j].clone();
                    apply_statements(&other.statements, &mut out);
                    apply_terminator(&other.terminator, &mut out);
                    incoming.extend(out);
                }
            }
            if i == body.entry.0 as usize {
                incoming.extend(droppable.iter().copied());
            }
            if incoming != live_in[i] {
                live_in[i] = incoming;
                changed = true;
            }
        }
    }

    // Insert drops at the end of each block, before the terminator.
    for i in 0..body.blocks.len() {
        if !reachable[i] {
            continue;
        }
        let mut live = live_in[i].clone();
        apply_statements(&body.blocks[i].statements, &mut live);

        let exits_scope = matches!(
            body.blocks[i].terminator,
            Terminator::Return | Terminator::Unreachable
        );
        if !exits_scope {
            continue;
        }
        // Locals that are still live must be dropped here. They are dropped in
        // reverse declaration order so that a value is always dropped before
        // the values it may refer to.
        let to_drop: Vec<LocalId> = live.iter().copied().rev().collect();
        if to_drop.is_empty() {
            continue;
        }

        // Chain one block per drop so each has a single terminator.
        let mut current = BlockId(i as u32);
        for local in to_drop {
            let next = BlockId(body.blocks.len() as u32);
            body.blocks.push(BasicBlock::new(Terminator::Return));
            let term = body.blocks[current.0 as usize].terminator.clone();
            body.blocks[current.0 as usize].terminator = Terminator::Drop {
                place: Place::local(local),
                target: next,
            };
            body.blocks[next.0 as usize].terminator = term;
            current = next;
        }
    }
}

/// Which blocks can be entered from the entry block.
fn compute_reachable(body: &Body) -> Vec<bool> {
    let mut seen = vec![false; body.blocks.len()];
    let mut stack = vec![body.entry];
    while let Some(b) = stack.pop() {
        let i = b.0 as usize;
        if i >= seen.len() || seen[i] {
            continue;
        }
        seen[i] = true;
        for s in body.blocks[i].terminator.successors() {
            stack.push(s);
        }
    }
    seen
}

/// Applies the effect of a statement list to a set of live drop flags.
fn apply_statements(stmts: &[Statement], live: &mut BTreeSet<LocalId>) {
    for s in stmts {
        match s {
            // The drop flag is the only authority on whether a value is still
            // owned. Assignments to Copy locals do not set it, and assignments
            // to owned locals set it before the assignment in lowering.
            Statement::SetDropFlag { local, value } => {
                if *value {
                    live.insert(*local);
                } else {
                    live.remove(local);
                }
            }
            Statement::StorageDead(local) => {
                live.remove(local);
            }
            _ => {}
        }
    }
}

fn apply_terminator(t: &Terminator, live: &mut BTreeSet<LocalId>) {
    match t {
        Terminator::Drop { place, .. } => {
            if place.projection.is_empty() {
                live.remove(&place.local);
            }
        }
        Terminator::Call { args, .. } => {
            // Arguments passed by move are gone from the caller.
            for a in args {
                if let Operand::Move(p) = a {
                    if p.projection.is_empty() {
                        live.remove(&p.local);
                    }
                }
            }
        }
        _ => {}
    }
}

/// Whether dropping a value of this type can do anything.
///
/// Types that own heap memory need a drop; plain data does not. The answer is
/// used to skip elaboration entirely for functions without owned values, which
/// keeps their MIR readable.
pub fn needs_drop(ty: crate::types::TyId, types: &TyCtxt) -> bool {
    // Copy types are duplicated on move and freed with their owner; dropping
    // them individually would be work with no effect.
    if types.is_copy(ty) {
        return false;
    }
    match types.kind(ty) {
        // `str` is the only string type in Phase 2/3 and is treated as owning
        // its buffer; `String` will join it once the stdlib lands.
        crate::types::TyKind::String
        | crate::types::TyKind::Str
        | crate::types::TyKind::Slice(_) => true,
        // Every non-Copy ADT is treated as owning. Phase 4 refines this with
        // real `Drop` impls and field-wise analysis.
        crate::types::TyKind::Adt { .. } => true,
        crate::types::TyKind::Array { elem, .. } => needs_drop(*elem, types),
        crate::types::TyKind::Tuple(elems) => elems.iter().any(|e| needs_drop(*e, types)),
        _ => false,
    }
}

/// Reports, for tests, which locals of a body get a drop.
pub fn dropped_locals(body: &Body) -> BTreeMap<LocalId, usize> {
    let mut counts = BTreeMap::new();
    for b in &body.blocks {
        if let Terminator::Drop { place, .. } = &b.terminator {
            *counts.entry(place.local).or_insert(0) += 1;
        }
    }
    counts
}
