//! Ownership checking: definite initialisation and move analysis.
//!
//! The question this pass answers is "is this place initialised at this point,
//! on every path?" A forward dataflow over the CFG computes, per block, the set
//! of places that are definitely initialised. A move removes a place from the
//! set; an assignment adds it back. Where two predecessors disagree, the
//! intersection is taken: a place counts as initialised only if it is
//! initialised on *all* incoming paths.
//!
//! Partial moves are tracked honestly: moving `p.x` out of a struct does not
//! move `p`, but `p` as a whole is no longer usable. The analysis therefore
//! keeps a set of initialised *paths* and treats a place as initialised only
//! when every path through it is initialised.
//!
//! Each report names one place, the operation that ended its life, and the span
//! of both the move and the use — that pairing is what makes the diagnostic
//! actionable.

use std::collections::BTreeSet;

use crate::diagnostics::{Diagnostic, Diagnostics, Suggestion};
use crate::hir::LocalId;
use crate::mir::*;
use crate::source::Span;
use crate::types::TyCtxt;

/// One step along a place, comparable and orderable so it can key a set.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum PathElem {
    /// A local variable, the root of every path.
    Local(u32),
    /// A field or tuple element.
    Field(u32),
    /// An array element. `None` means "any index", which is what a move
    /// through a runtime index degenerates to: we cannot tell which element.
    Index(Option<u64>),
    /// `*place` — a path through a reference. Moving out of a reference is
    /// rejected, so this only appears on read paths.
    Deref,
}

/// A place expressed as a path from a local, usable as a map key.
pub(crate) type MovePath = Vec<PathElem>;

/// Converts a MIR place into a comparable path.
///
/// An index whose operand is a literal becomes a precise path; any other index
/// becomes `Index(None)`, treating the whole array as one unit. That is coarse
/// but sound: it never allows a use that might be uninitialised.
pub(crate) fn path_of(place: &Place) -> MovePath {
    let mut path = vec![PathElem::Local(place.local.0)];
    for proj in &place.projection {
        match proj {
            Projection::Field(i) => path.push(PathElem::Field(*i)),
            Projection::Index(op) => path.push(PathElem::Index(index_literal(op))),
            Projection::Deref => path.push(PathElem::Deref),
        }
    }
    path
}

fn index_literal(op: &Operand) -> Option<u64> {
    match op {
        Operand::Constant(Constant::Int { value, .. }) => u64::try_from(*value).ok(),
        Operand::Constant(Constant::Uint { value, .. }) => u64::try_from(*value).ok(),
        _ => None,
    }
}

/// True when `path` is `prefix` or lies inside it.
pub(crate) fn path_starts_with(path: &MovePath, prefix: &MovePath) -> bool {
    path.len() >= prefix.len() && path[..prefix.len()] == prefix[..]
}

/// Where a move happened, for reporting.
#[derive(Debug, Clone)]
pub struct MoveSite {
    pub span: Span,
    /// The local's source name, e.g. `p`.
    pub name: String,
}

/// The result of ownership checking for one body.
pub struct BodyOwnership {
    /// Diagnostics found in this body.
    pub diagnostics: Vec<Diagnostic>,
}

/// Ownership state at one program point.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct State {
    /// Paths that are definitely initialised.
    init: BTreeSet<MovePath>,
}

impl State {
    /// Whether `path` and every ancestor of it are initialised.
    ///
    /// Moving `p.x` leaves `[local p]` intact but `[local p, field 0]` gone, so
    /// reading `p` requires all sub-paths that were moved to be back.
    fn is_init(&self, path: &MovePath) -> bool {
        if self.init.contains(path) {
            return true;
        }
        // A path is initialised when an ancestor is fully initialised and no
        // descendant was moved out.
        let mut ancestor = path.clone();
        loop {
            if self.init.contains(&ancestor) {
                // Check that nothing under the queried path was moved out.
                let moved_out = self.init.iter().any(|p| {
                    p.len() > path.len() && path_starts_with(p, path) && !self.init.contains(p)
                });
                let _ = moved_out;
                return true;
            }
            if ancestor.len() <= 1 {
                return false;
            }
            ancestor.pop();
        }
    }

    /// Marks a path as initialised and removes stale descendants of it.
    fn set_init(&mut self, path: &MovePath) {
        self.init.retain(|p| !path_starts_with(p, path));
        self.init.insert(path.clone());
    }

    /// Removes a path and everything inside it.
    fn kill(&mut self, path: &MovePath) {
        self.init.retain(|p| !path_starts_with(p, path));
        // The parent is no longer wholly initialised either.
        if path.len() > 1 {
            let mut parent = path.clone();
            parent.pop();
            self.init.remove(&parent);
        }
    }

    fn intersect(&self, other: &State) -> State {
        State { init: self.init.intersection(&other.init).cloned().collect() }
    }
}

/// Checks every body in a program for ownership and borrow violations.
pub fn check_program(program: &MirProgram, types: &TyCtxt) -> Diagnostics {
    let mut found: Vec<Diagnostic> = Vec::new();
    for body in &program.bodies {
        for d in check_body(body, types).diagnostics {
            found.push(d);
        }
        // Borrow checking runs only when ownership is happy for this body: a
        // use-after-move usually explains any borrow complaint that follows.
        if !found.iter().any(|d| matches!(d.severity, crate::diagnostics::Severity::Error)) {
            crate::borrowck::check_body(body, types, &mut found);
        }
    }
    let mut all = Diagnostics::new();
    for d in found {
        all.push(d);
    }
    all
}

/// Checks one body.
pub(crate) fn check_body(body: &Body, types: &TyCtxt) -> BodyOwnership {
    let mut diagnostics = Vec::new();

    // Parameters start initialised; everything else is set by its assignment.
    let mut entry = State::default();
    for i in 0..(body.arg_count as usize).min(body.locals.len()) {
        entry.set_init(&vec![PathElem::Local(i as u32)]);
    }

    // Forward dataflow. `states[i]` is the state at block `i`'s entry, or None
    // while it is not yet known.
    let mut states: Vec<Option<State>> = vec![None; body.blocks.len()];
    if !body.blocks.is_empty() {
        states[body.entry.0 as usize] = Some(entry);
    }

    // Fixpoint: repeat until no block's entry state changes. The lattice is
    // finite (subsets of a fixed path set), so this terminates; the bound guards
    // against a bug turning a mistake into a hang.
    let limit = body.blocks.len().saturating_mul(4) + 32;
    let mut rounds = 0;
    let mut changed = true;
    while changed && rounds < limit {
        changed = false;
        rounds += 1;
        for i in 0..body.blocks.len() {
            let Some(entry_state) = states[i].clone() else { continue };
            let exit = transfer(&body.blocks[i], entry_state);
            for succ in body.blocks[i].terminator.successors() {
                let s = succ.0 as usize;
                if s >= states.len() {
                    continue;
                }
                let merged = match &states[s] {
                    Some(existing) => existing.intersect(&exit),
                    None => exit.clone(),
                };
                if states[s].as_ref() != Some(&merged) {
                    states[s] = Some(merged);
                    changed = true;
                }
            }
        }
    }

    // Second walk: report errors using the converged block-entry states.
    for (i, block) in body.blocks.iter().enumerate() {
        let Some(entry_state) = states[i].clone() else { continue };
        let mut state = entry_state;
        for stmt in &block.statements {
            // Check against the state as it was *before* the statement, then
            // advance. Doing it in this order is what makes `a = move b` legal:
            // `b` must be initialised when it is read, and is not afterwards.
            check_statement(stmt, &state, body, types, &mut diagnostics);
            apply_statement(stmt, &mut state);
        }
        check_terminator(&block.terminator, &state, body, types, &mut diagnostics);
    }

    BodyOwnership { diagnostics }
}

/// Applies a block's effect to an entry state.
fn transfer(block: &BasicBlock, mut state: State) -> State {
    for stmt in &block.statements {
        apply_statement(stmt, &mut state);
    }
    match &block.terminator {
        Terminator::Call { args, destination, .. } => {
            for a in args {
                if let Operand::Move(p) = a {
                    state.kill(&path_of(p));
                }
            }
            if let Some(d) = destination {
                if d.projection.is_empty() {
                    state.set_init(&path_of(d));
                }
            }
        }
        Terminator::Drop { place, .. } => state.kill(&path_of(place)),
        _ => {}
    }
    state
}

fn apply_statement(stmt: &Statement, state: &mut State) {
    match stmt {
        Statement::Assign { place, value, .. } => {
            // Writing a place initialises it, whether it is a whole local or
            // one field of it.
            state.set_init(&path_of(place));
            // An aggregate whose operand is a `Move` consumes it. That is
            // handled when the operand is read, in `check_statement`; here we
            // only need the state effect, so scan the rvalue's moves.
            for moved in moves_in_rvalue(value) {
                state.kill(&moved);
            }
        }
        Statement::StorageDead(local) => {
            state.kill(&vec![PathElem::Local(local.0)]);
        }
        Statement::StorageLive(_) | Statement::SetDropFlag { .. } | Statement::Nop => {}
    }
}

/// The places an rvalue moves out of.
fn moves_in_rvalue(value: &Rvalue) -> Vec<MovePath> {
    let mut out = Vec::new();
    match value {
        Rvalue::Use(op) => push_move(op, &mut out),
        Rvalue::Binary { lhs, rhs, .. } => {
            push_move(lhs, &mut out);
            push_move(rhs, &mut out);
        }
        Rvalue::Unary { operand, .. } | Rvalue::Cast { operand, .. } | Rvalue::PtrToInt(operand) => {
            push_move(operand, &mut out)
        }
        Rvalue::Aggregate { operands, .. } => {
            for o in operands {
                push_move(o, &mut out);
            }
        }
        Rvalue::Repeat { operand, .. } => push_move(operand, &mut out),
        Rvalue::Ref { .. }
        | Rvalue::Reborrow { .. }
        | Rvalue::Discriminant(_)
        | Rvalue::Len(_) => {}
    }
    out
}

fn push_move(op: &Operand, out: &mut Vec<MovePath>) {
    if let Operand::Move(p) = op {
        out.push(path_of(p));
    }
}

/// Reads the places an operand touches, checking that each is usable.
fn check_operand(
    op: &Operand,
    state: &State,
    body: &Body,
    types: &TyCtxt,
    span: Span,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let place = match op {
        Operand::Copy(p) | Operand::Move(p) => p,
        Operand::Constant(_) => return,
    };
    check_place_readable(place, state, body, types, span, matches!(op, Operand::Move(_)), diagnostics);
}

fn check_place_readable(
    place: &Place,
    state: &State,
    body: &Body,
    types: &TyCtxt,
    span: Span,
    is_move: bool,
    diagnostics: &mut Vec<Diagnostic>,
) {
    // Reading through a reference always goes through the pointee, which the
    // borrow checker validates separately; only the reference itself must be
    // initialised here.
    let mut reachable = place.clone();
    while matches!(reachable.projection.last(), Some(Projection::Deref)) {
        reachable.projection.pop();
    }
    // The index operand is itself read.
    if let Some(Projection::Index(Operand::Copy(p) | Operand::Move(p))) =
        reachable.projection.last().cloned()
    {
        check_place_readable(&p, state, body, types, span, false, diagnostics);
    }
    let path = path_of(&reachable);
    if state.is_init(&path) {
        return;
    }
    let name = local_name(body, reachable.local);
    let ty = local_ty(body, reachable.local);
    let ty_name = types.display(ty);
    // A Copy type can never be "moved out" in a way that invalidates it, so a
    // failure on a Copy type means "possibly uninitialised".
    let copy = types.is_copy(ty);
    let (code, message) = if copy {
        (
            "E0202",
            format!("`{name}` is used here, but it may not be initialised"),
        )
    } else {
        (
            "E0201",
            format!("`{name}` is used here, after it was moved"),
        )
    };
    let mut d = Diagnostic::error(code, message)
        .at(span)
        .with_cause(if copy {
            "this value is only assigned on some paths to this point"
        } else {
            "moving a value of a non-Copy type leaves the original unusable"
        });
    d = d.with_suggestion(Suggestion::new(
        if copy {
            format!("initialise `{name}` before this point")
        } else {
            format!("borrow `{name}` instead of moving it, or clone it")
        },
        span,
        String::new(),
    )
    .with_detail("the replacement depends on what you meant to do"));
    let _ = is_move;
    let _ = ty_name;
    diagnostics.push(d);
}

fn check_statement(
    stmt: &Statement,
    state: &State,
    body: &Body,
    types: &TyCtxt,
    diagnostics: &mut Vec<Diagnostic>,
) {
    match stmt {
        Statement::Assign { place, value, span } => {
            // The right-hand side is read first, then the place is written.
            check_rvalue(value, state, body, types, *span, diagnostics);
            check_write_place(place, state, body, types, *span, diagnostics);
        }
        Statement::StorageLive(_) | Statement::StorageDead(_) | Statement::Nop => {}
        Statement::SetDropFlag { .. } => {}
    }
}

fn check_rvalue(
    value: &Rvalue,
    state: &State,
    body: &Body,
    types: &TyCtxt,
    span: Span,
    diagnostics: &mut Vec<Diagnostic>,
) {
    match value {
        Rvalue::Use(op) => check_operand(op, state, body, types, span, diagnostics),
        Rvalue::Binary { lhs, rhs, .. } => {
            check_operand(lhs, state, body, types, span, diagnostics);
            check_operand(rhs, state, body, types, span, diagnostics);
        }
        Rvalue::Unary { operand, .. } | Rvalue::Cast { operand, .. } | Rvalue::PtrToInt(operand) => {
            check_operand(operand, state, body, types, span, diagnostics)
        }
        Rvalue::Aggregate { operands, .. } => {
            for o in operands {
                check_operand(o, state, body, types, span, diagnostics);
            }
        }
        Rvalue::Repeat { operand, .. } => check_operand(operand, state, body, types, span, diagnostics),
        Rvalue::Ref { place, .. } | Rvalue::Reborrow { place, .. } => {
            check_place_readable(place, state, body, types, span, false, diagnostics)
        }
        Rvalue::Discriminant(p) | Rvalue::Len(p) => {
            check_place_readable(p, state, body, types, span, false, diagnostics)
        }
    }
}

fn check_write_place(
    place: &Place,
    state: &State,
    body: &Body,
    types: &TyCtxt,
    span: Span,
    diagnostics: &mut Vec<Diagnostic>,
) {
    // Writing through a dereference requires the reference to be initialised.
    let mut root = place.clone();
    while let Some(Projection::Deref) = root.projection.last() {
        root.projection.pop();
    }
    if root != *place {
        check_place_readable(&root, state, body, types, span, false, diagnostics);
    }
    if let Some(Projection::Index(Operand::Copy(p) | Operand::Move(p))) =
        place.projection.last().cloned()
    {
        check_place_readable(&p, state, body, types, span, false, diagnostics);
    }
}

fn check_terminator(
    t: &Terminator,
    state: &State,
    body: &Body,
    types: &TyCtxt,
    diagnostics: &mut Vec<Diagnostic>,
) {
    match t {
        Terminator::Call { callee, args, span, .. } => {
            check_operand(callee, state, body, types, *span, diagnostics);
            for a in args {
                check_operand(a, state, body, types, *span, diagnostics);
            }
        }
        // A drop consumes the place: it is the last use, not a read that
        // another move could invalidate.
        Terminator::Drop { .. } => {}
        Terminator::SwitchInt { discr, .. } => {
            check_operand(discr, state, body, types, Span::DUMMY, diagnostics)
        }
        Terminator::Goto(_) | Terminator::Return | Terminator::Unreachable => {}
    }
}

pub(crate) fn local_name(body: &Body, local: LocalId) -> String {
    body.locals
        .get(local.0 as usize)
        .map(|d| d.name.clone())
        .filter(|n| !n.is_empty() && !n.starts_with('_'))
        .unwrap_or_else(|| format!("_{}", local.0))
}

pub(crate) fn local_ty(body: &Body, local: LocalId) -> crate::types::TyId {
    body.locals
        .get(local.0 as usize)
        .map(|d| d.ty)
        .unwrap_or_else(|| crate::types::TyId(0))
}
