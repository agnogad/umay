//! MIR optimisation passes.
//!
//! Lowering produces a *lot* of temporaries: every sub-expression gets one, so
//! that ownership reasoning stays block-local. The backend should not pay for
//! that design, so the extra copies are removed here before code generation.
//!
//! Optimisations are deliberately conservative. Each one must be a rewrite that
//! is obviously valid on the MIR as it exists, because there is no interpreter
//! to check a rewrite against — the test suite compares generated behaviour
//! instead, and a subtly wrong optimisation shows up as a wrong answer in a
//! compiled program, the most expensive kind of bug to find.
//!
//! Passes run in this order, and the order matters:
//!
//! 1. [`unreachable`] — remove code that cannot run, so the later passes are not
//!    confused by phantom predecessors.
//! 2. [`simplify`] — fold constants and resolve trivial branches.
//! 3. [`copy_prop`] — rewrite `_2 = copy _1` uses to use `_1` directly.
//! 4. [`dce`] — delete assignments nobody reads.
//!
//! Running simplify before copy propagation matters: folding a constant into a
//! branch turns a two-way branch into a `goto`, and copy propagation then has a
//! simpler graph to work on.

use crate::hir::LocalId;
use crate::mir::*;
use crate::types::TyCtxt;
use std::collections::{BTreeMap, BTreeSet};

/// How hard to work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum OptLevel {
    /// No optimisation at all. The most direct path from MIR to code, useful
    /// when debugging the backend.
    O0,
    O1,
    O2,
    O3,
}

impl OptLevel {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "0" => Some(OptLevel::O0),
            "1" => Some(OptLevel::O1),
            "2" => Some(OptLevel::O2),
            "3" => Some(OptLevel::O3),
            _ => None,
        }
    }
}

/// Counts what the passes changed, so tests and `-Ztime-passes` style output can
/// show that an optimisation actually did something.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Stats {
    pub blocks_removed: usize,
    pub branches_folded: usize,
    pub copies_removed: usize,
    pub statements_removed: usize,
}

impl Stats {
    pub fn is_empty(&self) -> bool {
        *self == Stats::default()
    }
}

/// Runs the pipeline for `level` over every body in `program`.
pub fn optimize(program: &mut MirProgram, types: &TyCtxt, level: OptLevel) -> Stats {
    let mut total = Stats::default();
    if level == OptLevel::O0 {
        return total;
    }
    for body in &mut program.bodies {
        // Repeat the whole pipeline until it stops changing anything. A pass can
        // expose work for an earlier pass — deleting a dead assignment makes a
        // branch constant, which removes more blocks — so a single sweep is not
        // enough for a fixed point.
        for _ in 0..8 {
            let mut round = Stats::default();
            round.blocks_removed += unreachable(body);
            round.branches_folded += simplify(body, types);
            if level >= OptLevel::O2 {
                round.copies_removed += copy_prop(body);
            }
            round.statements_removed += dce(body, types);
            if round.is_empty() {
                break;
            }
            total.blocks_removed += round.blocks_removed;
            total.branches_folded += round.branches_folded;
            total.copies_removed += round.copies_removed;
            total.statements_removed += round.statements_removed;
        }
    }
    total
}

// ---------------------------------------------------------------------------
// Unreachable block elimination
// ---------------------------------------------------------------------------

/// Empties every block not reachable from the entry.
///
/// Blocks are not removed from the vector, because their ids appear in jump
/// targets elsewhere and in `BlockId`s already handed out. Replacing an
/// unreachable block's terminator with [`Terminator::Unreachable`] and dropping
/// its statements is enough: the backend never emits it, and the analysis passes
/// treat it as a dead end.
pub fn unreachable(body: &mut Body) -> usize {
    let live: BTreeSet<BlockId> = body.reachable().into_iter().collect();
    let mut removed = 0;
    for (index, block) in body.blocks.iter_mut().enumerate() {
        let id = BlockId(index as u32);
        if live.contains(&id) {
            continue;
        }
        // An unreachable block that is already empty costs nothing to visit
        // again, so it does not count as removed; otherwise the fixpoint loop
        // would never terminate.
        if block.statements.is_empty() && matches!(block.terminator, Terminator::Unreachable) {
            continue;
        }
        block.statements.clear();
        block.terminator = Terminator::Unreachable;
        removed += 1;
    }
    removed
}

// ---------------------------------------------------------------------------
// Constant folding and branch simplification
// ---------------------------------------------------------------------------

/// Folds constant arithmetic and turns constant branches into jumps.
///
/// Only operations that cannot fail are folded. Division and remainder are
/// left alone: `1 / 0` must behave at run time exactly as it does in the source,
/// and folding it to anything other than a trap would change the program.
pub fn simplify(body: &mut Body, types: &TyCtxt) -> usize {
    let mut folded = 0;
    for index in 0..body.blocks.len() {
        // Constants defined earlier in this block, so `_1 = const 6  _2 = _1 + 1`
        // folds even though the two operations are separate statements. Only
        // locals assigned exactly once are recorded; a reassigned local is not a
        // constant and propagating it would be wrong.
        let mut known: BTreeMap<LocalId, (Constant, crate::types::TyId)> = BTreeMap::new();
        let mut reassigned: BTreeSet<LocalId> = BTreeSet::new();
        for stmt in &body.blocks[index].statements {
            if let Statement::Assign { place, .. } = stmt {
                if place.projection.is_empty() && known.contains_key(&place.local) {
                    reassigned.insert(place.local);
                }
            }
        }

        // Fold statements first, so a branch on a freshly-folded constant is
        // caught in the same visit.
        let mut i = 0;
        while i < body.blocks[index].statements.len() {
            let Statement::Assign { place, value, .. } = body.blocks[index].statements[i].clone() else {
                i += 1;
                continue;
            };
            // Substitute known constants for plain local reads. The constant is
            // retyped to the destination's type, because lowering interns a
            // literal with its own `{integer}` type and the width that matters
            // for folding is the destination's.
            let mut value = value;
            substitute_constants(&mut value, &known);
            let dest_ty = body.locals.get(place.local.0 as usize).map(|l| l.ty);
            if let Some(ty) = dest_ty {
                match &mut value {
                    Rvalue::Binary { op: op_ref, lhs, rhs } if !is_comparison(*op_ref) => {
                        retype_constant(lhs, ty);
                        retype_constant(rhs, ty);
                    }
                    Rvalue::Unary { operand, .. } => retype_constant(operand, ty),
                    _ => {}
                }
            }
            let replacement = fold_rvalue(&value, types).unwrap_or(value);
            let is_constant = matches!(replacement, Rvalue::Use(Operand::Constant(_)));
            if is_constant {
                folded += 1;
            }
            // Record the new binding for later statements in this block.
            if place.projection.is_empty() {
                match (&replacement, reassigned.contains(&place.local)) {
                    (Rvalue::Use(Operand::Constant(c)), false) => {
                        let ty = body.locals.get(place.local.0 as usize).map(|l| l.ty);
                        match ty {
                            Some(ty) => {
                                known.insert(place.local, (c.clone(), ty));
                            }
                            None => {
                                known.remove(&place.local);
                            }
                        }
                    }
                    _ => {
                        known.remove(&place.local);
                    }
                }
            }
            if let Statement::Assign { value: slot, .. } = &mut body.blocks[index].statements[i] {
                *slot = replacement;
            }
            i += 1;
        }

        let target = match &body.blocks[index].terminator {
            Terminator::SwitchInt { discr, targets, otherwise } => {
                let value = match discr {
                    Operand::Constant(Constant::Bool(b)) => Some(*b as i128),
                    Operand::Constant(Constant::Int { value, .. }) => Some(*value),
                    Operand::Constant(Constant::Uint { value, .. }) => Some(*value as i128),
                    _ => None,
                };
                value.map(|v| {
                    targets
                        .iter()
                        .find(|(case, _)| *case == v)
                        .map(|(_, b)| *b)
                        .unwrap_or(*otherwise)
                })
            }
            _ => None,
        };
        if let Some(target) = target {
            body.blocks[index].terminator = Terminator::Goto(target);
            folded += 1;
        }
    }
    folded
}

/// True when `op` compares two values and produces a `bool`.
fn is_comparison(op: BinOp) -> bool {
    matches!(op, BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge)
        || matches!(op, BinOp::LogicalAnd | BinOp::LogicalOr)
}

/// Rewrites a constant's type annotation, keeping its value.
fn retype_constant(op: &mut Operand, ty: crate::types::TyId) {
    if let Operand::Constant(
        Constant::Int { ty: slot, .. }
        | Constant::Uint { ty: slot, .. }
        | Constant::Float { ty: slot, .. },
    ) = op
    {
        *slot = ty;
    }
}

/// Replaces reads of locals known to hold a constant with that constant.
fn substitute_constants(
    value: &mut Rvalue,
    known: &BTreeMap<LocalId, (Constant, crate::types::TyId)>,
) {
    fn sub(op: &mut Operand, known: &BTreeMap<LocalId, (Constant, crate::types::TyId)>) {
        if let Operand::Copy(place) | Operand::Move(place) = op {
            if place.projection.is_empty() {
                if let Some((c, ty)) = known.get(&place.local) {
                    *op = Operand::Constant(c.clone());
                    retype_constant(op, *ty);
                }
            }
        }
    }
    match value {
        Rvalue::Use(op)
        | Rvalue::Unary { operand: op, .. }
        | Rvalue::Cast { operand: op, .. }
        | Rvalue::PtrToInt(op) => sub(op, known),
        // A pointer offset is not folded: its scaling depends on the pointee
        // type, which this pass does not track.
        Rvalue::PtrOffset { ptr, count } => {
            sub(ptr, known);
            sub(count, known);
        }
        Rvalue::Binary { lhs, rhs, .. } => {
            sub(lhs, known);
            sub(rhs, known);
        }
        Rvalue::Aggregate { operands, .. } => {
            for op in operands {
                sub(op, known);
            }
        }
        Rvalue::Repeat { operand, .. } => sub(operand, known),
        _ => {}
    }
}

/// Folds an rvalue whose operands are all constants.
fn fold_rvalue(value: &Rvalue, types: &TyCtxt) -> Option<Rvalue> {
    match value {
        Rvalue::Binary { op, lhs, rhs } => {
            let l = const_int(lhs)?;
            let r = const_int(rhs)?;
            let ty = const_ty(lhs).or_else(|| const_ty(rhs))?;
            let result = eval_binop(*op, l, r, types.kind(ty), types.is_signed(ty))?;
            Some(Rvalue::Use(Operand::Constant(Constant::Int { value: result, ty })))
        }
        Rvalue::Unary { op, operand } => {
            let v = const_int(operand)?;
            let ty = const_ty(operand)?;
            let kind = types.kind(ty);
            let signed = types.is_signed(ty);
            let result = match op {
                // Fold at the value's own width. `!255u8` is 0, not -256: a
                // bitwise not computed in the i128 scratch integer has to be
                // truncated back to u8 before a later cast widens it.
                UnOp::Neg => truncate_to(v.wrapping_neg(), kind, signed),
                UnOp::Not => {
                    if matches!(kind, crate::types::TyKind::Bool) {
                        if v == 0 { 1 } else { 0 }
                    } else {
                        truncate_to(!v, kind, signed)
                    }
                }
            };
            Some(Rvalue::Use(Operand::Constant(Constant::Int { value: result, ty })))
        }
        Rvalue::Cast { operand, to } => {
            // Only integer-to-integer casts are folded; float conversions and
            // pointer casts depend on run-time representation details that the
            // backend is the right place to get right.
            let v = const_int(operand)?;
            let from = const_ty(operand)?;
            if !types.is_integer(from) || !types.is_integer(*to) {
                return None;
            }
            let value = truncate_to(v, types.kind(*to), types.is_signed(*to));
            Some(Rvalue::Use(Operand::Constant(Constant::Int { value, ty: *to })))
        }
        _ => None,
    }
}

/// Evaluates a binary operation on integers, respecting the type's width and
/// signedness. Returns `None` for anything that must not be folded.
fn eval_binop(
    op: BinOp,
    l: i128,
    r: i128,
    kind: &crate::types::TyKind,
    signed: bool,
) -> Option<i128> {
    use crate::types::TyKind;
    // An unresolved literal type means checking left `{integer}` behind; it will
    // be represented as a 64-bit integer, so folding in that width is exact.
    let arithmetic = matches!(kind, TyKind::IntVar(_))
        || matches!(
        kind,
        TyKind::I8
            | TyKind::I16
            | TyKind::I32
            | TyKind::I64
            | TyKind::I128
            | TyKind::Isize
            | TyKind::U8
            | TyKind::U16
            | TyKind::U32
            | TyKind::U64
            | TyKind::U128
            | TyKind::Usize
    );
    if !arithmetic && !matches!(kind, TyKind::Bool) {
        return None;
    }

    // Comparison results are always `bool`, and are meaningful even for types
    // whose arithmetic is not foldable here.
    match op {
        BinOp::Eq => return Some((l == r) as i128),
        BinOp::Ne => return Some((l != r) as i128),
        BinOp::Lt => return Some((l < r) as i128),
        BinOp::Le => return Some((l <= r) as i128),
        BinOp::Gt => return Some((l > r) as i128),
        BinOp::Ge => return Some((l >= r) as i128),
        _ => {}
    }
    if !arithmetic {
        return None;
    }

    let raw = match op {
        BinOp::Add => l.wrapping_add(r),
        BinOp::Sub => l.wrapping_sub(r),
        BinOp::Mul => l.wrapping_mul(r),
        // Division by zero must reach run time, where it traps. Folding here
        // would silently remove the trap.
        BinOp::Div => {
            if r == 0 {
                return None;
            }
            l.checked_div(r)?
        }
        BinOp::Rem => {
            if r == 0 {
                return None;
            }
            l.checked_rem(r)?
        }
        BinOp::BitAnd => l & r,
        BinOp::BitOr => l | r,
        BinOp::BitXor => l ^ r,
        // A shift amount at or beyond the bit width is undefined in the source
        // language's own terms, so it is left for the backend to handle.
        BinOp::Shl => {
            if !(0..128).contains(&r) {
                return None;
            }
            l << r
        }
        BinOp::Shr => {
            if !(0..128).contains(&r) {
                return None;
            }
            if signed {
                l >> r
            } else {
                (l as u128 >> r) as i128
            }
        }
        BinOp::LogicalAnd | BinOp::LogicalOr => return None,
        _ => return None,
    };
    Some(truncate_to(raw, kind, signed))
}

/// Truncates a value to the width of `kind`, then sign- or zero-extends it back
/// so that the folded source is exact.
fn truncate_to(v: i128, kind: &crate::types::TyKind, signed: bool) -> i128 {
    use crate::types::TyKind;
    let bits = match kind {
        TyKind::IntVar(_) => 64,
        TyKind::I8 | TyKind::U8 => 8u32,
        TyKind::I16 | TyKind::U16 => 16,
        TyKind::I32 | TyKind::U32 => 32,
        TyKind::I64 | TyKind::U64 | TyKind::Isize | TyKind::Usize => 64,
        TyKind::I128 | TyKind::U128 => 128,
        TyKind::Bool => 1,
        _ => return v,
    };
    if bits == 128 {
        return v;
    }
    let mask = (1i128 << bits) - 1;
    let raw = v & mask;
    if signed && (raw >> (bits - 1)) & 1 == 1 {
        // Sign-extend.
        raw | !mask
    } else {
        raw
    }
}

fn const_int(op: &Operand) -> Option<i128> {
    match op {
        Operand::Constant(Constant::Int { value, .. }) => Some(*value),
        Operand::Constant(Constant::Uint { value, .. }) => Some(*value as i128),
        Operand::Constant(Constant::Bool(b)) => Some(*b as i128),
        Operand::Constant(Constant::Char(c)) => Some(*c as i128),
        _ => None,
    }
}

fn const_ty(op: &Operand) -> Option<crate::types::TyId> {
    match op {
        Operand::Constant(Constant::Int { ty, .. })
        | Operand::Constant(Constant::Uint { ty, .. })
        | Operand::Constant(Constant::Float { ty, .. }) => Some(*ty),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Copy propagation
// ---------------------------------------------------------------------------

/// Rewrites reads of a local that only ever holds `copy _x` to read `_x`.
///
/// A copy produced by lowering (`_2 = copy _1`) is replayed at every use. This
/// pass finds such single-definition copies and substitutes them, so the
/// backend sees fewer moves. It bails out on a local that is assigned more than
/// once, after a move from it, or that has its address taken — in each case the
/// substitution would change which value a use observes.
pub fn copy_prop(body: &mut Body) -> usize {
    // Which locals are plain copies of another local, and of which one.
    let mut source: BTreeMap<LocalId, LocalId> = BTreeMap::new();
    // Locals that must not be propagated away.
    let mut blocked: BTreeSet<LocalId> = BTreeSet::new();
    // How many times each local is assigned.
    let mut assigns: BTreeMap<LocalId, usize> = BTreeMap::new();

    for block in &body.blocks {
        for stmt in &block.statements {
            if let Statement::Assign { place, value, .. } = stmt {
                if !place.projection.is_empty() {
                    blocked.insert(place.local);
                    continue;
                }
                *assigns.entry(place.local).or_default() += 1;
                match value {
                    Rvalue::Use(Operand::Copy(src)) if src.projection.is_empty() => {
                        source.insert(place.local, src.local);
                    }
                    // Anything else assigned to this local makes it more than a
                    // copy, so the substitution is no longer safe.
                    _ => {
                        blocked.insert(place.local);
                    }
                }
            }
        }
        // Taking the address of a local, or moving out of it, means its storage
        // identity matters.
        for op in operands_of_block(block) {
            if let Operand::Move(place) = op {
                blocked.insert(place.local);
            }
        }
        for place in borrowed_places(block) {
            blocked.insert(place.local);
        }
    }

    // A local assigned more than once is not a stable alias.
    for (local, count) in &assigns {
        if *count > 1 {
            blocked.insert(*local);
        }
    }
    source.retain(|local, _| !blocked.contains(local));

    // Resolve chains, so `_3 = copy _2`, `_2 = copy _1` reads `_1`. The bound
    // stops a cycle (which a correct program cannot have) from hanging.
    let resolved: BTreeMap<LocalId, LocalId> = source
        .keys()
        .map(|&start| {
            let mut current = start;
            for _ in 0..16 {
                match source.get(&current) {
                    Some(&next) if next != current => current = next,
                    _ => break,
                }
            }
            (start, current)
        })
        .collect();
    if resolved.is_empty() {
        return 0;
    }

    let mut changed = 0;
    for index in 0..body.blocks.len() {
        for i in 0..body.blocks[index].statements.len() {
            let Statement::Assign { value, .. } = &mut body.blocks[index].statements[i] else {
                continue;
            };
            if rewrite_operands(value, &resolved) {
                changed += 1;
            }
        }
        if rewrite_terminator(&mut body.blocks[index].terminator, &resolved) {
            changed += 1;
        }
    }
    changed
}

/// Rewrites every operand in `value` through `resolved`. Returns whether
/// anything changed.
fn rewrite_operands(value: &mut Rvalue, resolved: &BTreeMap<LocalId, LocalId>) -> bool {
    let mut changed = false;
    match value {
        Rvalue::Use(op)
        | Rvalue::Unary { operand: op, .. }
        | Rvalue::Cast { operand: op, .. }
        | Rvalue::PtrToInt(op) => changed |= rewrite_operand(op, resolved),
        Rvalue::PtrOffset { ptr, count } => {
            changed |= rewrite_operand(ptr, resolved);
            changed |= rewrite_operand(count, resolved);
        }
        Rvalue::Binary { lhs, rhs, .. } => {
            changed |= rewrite_operand(lhs, resolved);
            changed |= rewrite_operand(rhs, resolved);
        }
        Rvalue::Aggregate { operands, .. } => {
            for op in operands {
                changed |= rewrite_operand(op, resolved);
            }
        }
        Rvalue::Repeat { operand, .. } => changed |= rewrite_operand(operand, resolved),
        // `Ref`, `Reborrow`, `Len` and `Discriminant` carry places, whose index
        // operands are handled by the shared helper below.
        Rvalue::Ref { .. } | Rvalue::Reborrow { .. } | Rvalue::Discriminant(_) | Rvalue::Len(_) => {}
    }
    // Index projections hold operands too.
    changed |= rewrite_place_indices(value, resolved);
    changed
}

/// Rewrites the operands of any `Index` projection inside a place used by
/// `value`. Places appear in `Ref`, `Reborrow`, `Len` and `Discriminant`.
fn rewrite_place_indices(value: &mut Rvalue, resolved: &BTreeMap<LocalId, LocalId>) -> bool {
    let mut changed = false;
    let mut visit = |place: &mut Place| {
        for projection in &mut place.projection {
            if let Projection::Index(op) = projection {
                changed |= rewrite_operand(op, resolved);
            }
        }
    };
    match value {
        Rvalue::Ref { place, .. } | Rvalue::Reborrow { place, .. } => visit(place),
        Rvalue::Len(place) | Rvalue::Discriminant(place) => visit(place),
        _ => {}
    }
    changed
}

fn rewrite_operand(op: &mut Operand, resolved: &BTreeMap<LocalId, LocalId>) -> bool {
    let local = match op {
        Operand::Copy(place) | Operand::Move(place) if place.projection.is_empty() => place.local,
        _ => return false,
    };
    match resolved.get(&local) {
        Some(&target) if target != local => {
            let place = match op {
                Operand::Copy(p) | Operand::Move(p) => p,
                _ => unreachable!("checked above"),
            };
            place.local = target;
            true
        }
        _ => false,
    }
}

fn rewrite_terminator(t: &mut Terminator, resolved: &BTreeMap<LocalId, LocalId>) -> bool {
    let mut changed = false;
    match t {
        Terminator::SwitchInt { discr, .. } => changed |= rewrite_operand(discr, resolved),
        Terminator::Call { callee, args, .. } => {
            changed |= rewrite_operand(callee, resolved);
            for arg in args {
                changed |= rewrite_operand(arg, resolved);
            }
        }
        Terminator::Drop { .. } | Terminator::Goto(_) | Terminator::Return | Terminator::Unreachable => {}
    }
    changed
}

// ---------------------------------------------------------------------------
// Dead code elimination
// ---------------------------------------------------------------------------

/// Removes assignments whose destination is never read.
///
/// Reachability of a definition is decided by a liveness dataflow, not by a
/// syntactic count, so `x = 1; x = 2; use(x)` keeps only the second assignment.
/// This is what makes the safety net useful: lowering emits a store for every
/// intermediate value, and most of them are read exactly never.
// The index is used to mutate the block whose live-out set is being read, so an
// iterator over `blocks` would borrow it twice.
#[allow(clippy::needless_range_loop)]
pub fn dce(body: &mut Body, types: &TyCtxt) -> usize {
    let mut removed = 0;
    // Repeat until nothing changes, because deleting one statement can make
    // another dead.
    for _ in 0..8 {
        let live_out = live_out_per_block(body, types);
        let mut removed_this_round = 0;
        for index in 0..body.blocks.len() {
            // Walk backwards carrying a live set, so each statement is judged by
            // what is live *after* it. A definition is dead when nothing later
            // reads it; that is exactly `!live.contains(&dest)` just before the
            // statement removes it.
            let mut live = live_out[index].clone();
            let mut i = body.blocks[index].statements.len();
            while i > 0 {
                i -= 1;
                let statement = body.blocks[index].statements[i].clone();
                let dead = match &statement {
                    Statement::Assign { place, value, .. } => {
                        !live.contains(&place.local)
                            && !has_observable_effect(value)
                            && !place_writes_memory(place)
                    }
                    // Drop flags and storage markers are bookkeeping for passes
                    // that have already run. Drop elaboration turned every drop
                    // into an explicit `Terminator::Drop`, so the flags carry no
                    // information the backend can use.
                    Statement::StorageLive(_)
                    | Statement::StorageDead(_)
                    | Statement::SetDropFlag { .. }
                    | Statement::Nop => true,
                };
                if dead {
                    body.blocks[index].statements.remove(i);
                    removed_this_round += 1;
                } else {
                    // Not removed: its reads become live before it.
                    for local in statement_uses(&statement) {
                        live.insert(local);
                    }
                }
                // A definition ends the liveness of its destination -- but only a
                // definition *of that local*. A write through a projection
                // (`_0.(deref) = 5`) stores to memory, not to `_0`; killing `_0`
                // here erased the use that `statement_uses` had just recorded, so
                // the statement that produced the pointer looked dead and was
                // deleted. The program then stored through an undefined register
                // and segfaulted at `-O1` and above.
                if let Statement::Assign { place, .. } = &statement {
                    if place.projection.is_empty() {
                        live.remove(&place.local);
                    }
                }
            }
        }
        removed += removed_this_round;
        if removed_this_round == 0 {
            break;
        }
    }
    removed
}

/// Locals read anywhere in a statement, including index operands and the
/// implicit read performed by a drop.
fn statement_uses(stmt: &Statement) -> Vec<LocalId> {
    match stmt {
        Statement::Assign { place, value, .. } => {
            let mut out = rvalue_uses(value);
            // Writing through a projection reads what it projects through.
            // `_0.(deref) = 5` reads the *pointer* `_0` to find where to store;
            // only `Index` was considered here, so that statement looked like it
            // read nothing at all. Dead-code elimination then judged `_0` dead,
            // deleted the two assignments that produced it, and the program
            // stored through an uninitialised register.
            if !place.projection.is_empty() {
                out.push(place.local);
            }
            for projection in &place.projection {
                if let Projection::Index(op) = projection {
                    out.extend(operand_uses(op));
                }
            }
            out
        }
        // Bookkeeping for passes that already ran; see `dce`.
        Statement::SetDropFlag { .. }
        | Statement::StorageLive(_)
        | Statement::StorageDead(_)
        | Statement::Nop => Vec::new(),
    }
}

/// True if removing an assignment of this rvalue could be observed.
///
/// Only `Ref`/`Reborrow` qualify today: forming a reference may be visible
/// through the reference even if the reference's own local is never read, and
/// this pass has no alias analysis to prove otherwise. Being conservative here
/// costs one dead store and avoids a miscompile.
fn has_observable_effect(value: &Rvalue) -> bool {
    matches!(value, Rvalue::Ref { .. } | Rvalue::Reborrow { .. })
}

/// True when writing to `place` changes memory a reader can observe.
///
/// A destination with a `Deref` in its projection is a write *through a pointer*:
/// `*p = 5` stores to whatever `p` names, not to the local `p`. Dead-code
/// elimination judged such a statement only by whether the pointer local was
/// live, so `*p = 5` followed by a read that optimisation could not follow was
/// deleted -- and the program then read uninitialised memory, which for
/// `alloc`/`dealloc` turned into `double free or corruption`. A store through a
/// pointer is always observable, so it is never removed.
///
/// A field write (`s.x = 1`) is not covered: it writes into a local aggregate
/// that this pass tracks, and removing it when the aggregate is dead is correct.
fn place_writes_memory(place: &Place) -> bool {
    place.projection.iter().any(|p| matches!(p, Projection::Deref))
}

/// For each block, the locals live at its *exit*.
fn live_out_per_block(body: &Body, _types: &TyCtxt) -> Vec<BTreeSet<LocalId>> {
    let count = body.blocks.len();
    let mut live_in: Vec<BTreeSet<LocalId>> = vec![BTreeSet::new(); count];
    let mut live_out: Vec<BTreeSet<LocalId>> = vec![BTreeSet::new(); count];
    let ret_slot = return_slot(body);

    for _ in 0..16 {
        let mut changed = false;
        for index in (0..count).rev() {
            let block = &body.blocks[index];
            // Live-out is the union of live-in of every successor, plus what the
            // terminator itself reads.
            let mut out: BTreeSet<LocalId> = BTreeSet::new();
            for succ in block.terminator.successors() {
                if let Some(s) = live_in.get(succ.0 as usize) {
                    out.extend(s.iter().copied());
                }
            }
            out.extend(terminator_uses(&block.terminator, ret_slot));
            // Walk the block backwards from its exit.
            let mut live = out.clone();
            for stmt in block.statements.iter().rev() {
                if let Statement::Assign { place, .. } = stmt {
                    live.remove(&place.local);
                }
                live.extend(statement_uses(stmt));
            }
            if live_out[index] != out {
                live_out[index] = out;
                changed = true;
            }
            if live_in[index] != live {
                live_in[index] = live;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    live_out
}

fn rvalue_uses(value: &Rvalue) -> Vec<LocalId> {
    let mut out = Vec::new();
    match value {
        Rvalue::PtrOffset { ptr, count } => {
            out.extend(operand_uses(ptr));
            out.extend(operand_uses(count));
        }
        Rvalue::Use(op) | Rvalue::Unary { operand: op, .. } | Rvalue::Cast { operand: op, .. } | Rvalue::PtrToInt(op) => {
            out.extend(operand_uses(op))
        }
        Rvalue::Binary { lhs, rhs, .. } => {
            out.extend(operand_uses(lhs));
            out.extend(operand_uses(rhs));
        }
        Rvalue::Aggregate { operands, .. } => {
            for op in operands {
                out.extend(operand_uses(op));
            }
        }
        Rvalue::Repeat { operand, .. } => out.extend(operand_uses(operand)),
        Rvalue::Ref { place, .. } | Rvalue::Reborrow { place, .. } => {
            out.push(place.local);
            for projection in &place.projection {
                if let Projection::Index(op) = projection {
                    out.extend(operand_uses(op));
                }
            }
        }
        Rvalue::Len(place) | Rvalue::Discriminant(place) => out.push(place.local),
    }
    out
}

fn operand_uses(op: &Operand) -> Vec<LocalId> {
    match op {
        Operand::Copy(place) | Operand::Move(place) => {
            let mut out = vec![place.local];
            for projection in &place.projection {
                if let Projection::Index(inner) = projection {
                    for local in operand_uses(inner) {
                        out.push(local);
                    }
                }
            }
            out
        }
        Operand::Constant(_) => Vec::new(),
    }
}

fn terminator_uses(t: &Terminator, ret_slot: Option<LocalId>) -> Vec<LocalId> {
    match t {
        Terminator::SwitchInt { discr, .. } => operand_uses(discr),
        Terminator::Call { callee, args, .. } => {
            let mut out = operand_uses(callee);
            for arg in args {
                out.extend(operand_uses(arg));
            }
            out
        }
        Terminator::Drop { place, .. } => vec![place.local],
        // `return` reads the return slot. It is the function's output, so it is
        // live even though no statement mentions it again.
        Terminator::Return => ret_slot.into_iter().collect(),
        Terminator::Goto(_) | Terminator::Unreachable => Vec::new(),
    }
}

/// The local a body returns through, if it has one.
///
/// Lowering names it `_ret`; a body without one either returns unit or is a
/// constant initialiser that never reaches a `return`.
fn return_slot(body: &Body) -> Option<LocalId> {
    body.locals.iter().position(|l| l.name == "_ret").map(|i| LocalId(i as u32))
}

fn operands_of_block(block: &BasicBlock) -> Vec<&Operand> {
    let mut out = Vec::new();
    for stmt in &block.statements {
        if let Statement::Assign { value, .. } = stmt {
            match value {
                Rvalue::Use(op)
                | Rvalue::Unary { operand: op, .. }
                | Rvalue::Cast { operand: op, .. }
                | Rvalue::PtrToInt(op) => out.push(op),
                Rvalue::PtrOffset { ptr, count } => {
                    out.push(ptr);
                    out.push(count);
                }
                Rvalue::Binary { lhs, rhs, .. } => {
                    out.push(lhs);
                    out.push(rhs);
                }
                Rvalue::Aggregate { operands, .. } => out.extend(operands.iter()),
                Rvalue::Repeat { operand, .. } => out.push(operand),
                _ => {}
            }
        }
    }
    out
}

fn borrowed_places(block: &BasicBlock) -> Vec<&Place> {
    let mut out = Vec::new();
    for stmt in &block.statements {
        if let Statement::Assign {
            value: Rvalue::Ref { place, .. } | Rvalue::Reborrow { place, .. },
            ..
        } = stmt
        {
            out.push(place);
        }
    }
    out
}

#[cfg(test)]
mod deref_use_tests {
    use super::*;

    #[test]
    fn a_deref_write_reads_its_base() {
        let place = Place { local: LocalId(0), projection: vec![Projection::Deref] };
        let stmt = Statement::Assign {
            place,
            value: Rvalue::Use(Operand::Constant(Constant::Bool(true))),
            span: crate::source::Span::new(crate::source::FileId(0), 0, 0),
        };
        assert_eq!(statement_uses(&stmt), vec![LocalId(0)]);
    }
}
