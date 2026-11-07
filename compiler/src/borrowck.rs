//! Borrow checking: aliasing rules for references.
//!
//! A borrow is a live claim on a place. The rules are the ones the memory model
//! states:
//!
//! - Many shared borrows of a place may coexist.
//! - A mutable borrow excludes every other borrow of that place, and vice versa.
//! - A place cannot be assigned or moved while a conflicting borrow is live.
//!
//! Two decisions make this usable rather than merely sound:
//!
//! 1. **A borrow lives until its last use.** `let r = &x  let n = *r  let m = &mut
//!    x` is accepted, because `r` is dead before `m` appears. Without this every
//!    pair of borrows in a function would conflict and the checker would be
//!    useless.
//! 2. **Positions, not blocks.** A borrow's lifetime is a range of statement
//!    positions. Block granularity is sound but far too coarse: every statement
//!    in a block would look simultaneous with every other.
//!
//! Each conflict names both origins, because "cannot borrow `x` mutably" without
//! saying *what else* borrowed it leaves the reader guessing.

use std::collections::BTreeMap;

use crate::diagnostics::{Diagnostic, Suggestion};
use crate::hir::LocalId;
use crate::mir::*;
use crate::source::Span;

use crate::ownership::{local_name, path_of, path_starts_with, MovePath, PathElem};

/// A reference value produced by the lowering.
#[derive(Debug, Clone)]
struct Borrow {
    /// The place borrowed.
    place: MovePath,
    /// Whether the borrow allows mutation.
    mutable: bool,
    /// The local holding the reference.
    holder: LocalId,
    /// Where the borrow was created, for the diagnostic.
    origin: Span,
    /// Linear position of the statement that created it.
    created: Pos,
    /// Last position at which the reference is used.
    last_use: Pos,
}

/// A point in the program, ordered by execution position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Pos {
    block: u32,
    index: u32,
}

impl Pos {
    /// The position of a block's terminator: after all its statements.
    fn terminator(block: u32, statements: usize) -> Pos {
        Pos { block, index: statements as u32 }
    }
}

impl Borrow {
    /// Whether this borrow is still claimed at `pos`.
    fn live_at(&self, pos: Pos) -> bool {
        // Live from its creation up to and including its last use.
        pos >= self.created && pos <= self.last_use
    }
}

/// Checks the borrow rules for one body.
pub(crate) fn check_body(body: &Body, _types: &crate::types::TyCtxt, diagnostics: &mut Vec<Diagnostic>) {
    check_returned_references(body, diagnostics);
    let mut borrows = collect_borrows(body);
    if borrows.is_empty() {
        return;
    }
    compute_lifetimes(body, &mut borrows);
    borrows.retain(|b| b.last_use >= b.created);

    for (i, block) in body.blocks.iter().enumerate() {
        let bid = i as u32;

        // Conflict between two borrows that are both live at this point.
        let live: Vec<&Borrow> = borrows
            .iter()
            .filter(|b| {
                let start = Pos { block: b.created.block, index: 0 };
                let end = Pos { block: b.last_use.block, index: u32::MAX };
                let at = Pos { block: bid, index: 0 };
                at >= start && at <= end
            })
            .collect();
        report_borrow_conflicts(&live, body, diagnostics);

        // Assignments and moves of a borrowed place.
        for (sidx, stmt) in block.statements.iter().enumerate() {
            let pos = Pos { block: bid, index: sidx as u32 };
            if let Statement::Assign { place, span, .. } = stmt {
                report_borrowed_write(&borrows, &path_of(place), pos, *span, body, diagnostics);
            }
        }
        if let Terminator::Call { args, span, .. } = &block.terminator {
            let pos = Pos::terminator(bid, block.statements.len());
            for a in args {
                if let Operand::Move(p) = a {
                    report_borrowed_write(&borrows, &path_of(p), pos, *span, body, diagnostics);
                }
            }
        }
    }
}

/// Rejects a reference to storage the function owns escaping in its return value.
///
/// A borrow of a parameter is fine: the caller owns that value and outlives the
/// call, which is exactly what lifetime elision relies on. A borrow of a `let`
/// binding or of a temporary is not — that storage ends when the function
/// returns, so the caller would receive a dangling reference.
fn check_returned_references(body: &Body, diagnostics: &mut Vec<Diagnostic>) {
    let Some(ret_local) = return_local(body) else { return };

    for block in &body.blocks {
        for stmt in &block.statements {
            let Statement::Assign { place, value, span } = stmt else { continue };
            if place.local != ret_local || !place.projection.is_empty() {
                continue;
            }
            // The returned value is a reference when it is a copy of one, or
            // the reference itself.
            let Rvalue::Use(Operand::Copy(src) | Operand::Move(src)) = value else { continue };
            let Some(borrowed) = reference_origin(body, src.local) else { continue };
            let Some(PathElem::Local(local)) = borrowed.first().cloned() else { continue };
            if local < body.arg_count {
                continue;
            }
            let name = local_name(body, LocalId(local));
            let d = Diagnostic::error(
                "E0207",
                format!("this function returns a reference to `{name}`, which it does not own"),
            )
            .at(*span)
            .with_cause(
                "the reference points at storage that ends when the function returns, so the caller would receive a dangling reference",
            )
            .with_suggestion(
                Suggestion::new(
                    "return the value itself, or take an owning value as a parameter",
                    *span,
                    String::new(),
                )
                .with_detail("a returned borrow must point at something that outlives the call"),
            );
            diagnostics.push(d);
        }
    }
}

/// The local used as the return slot, if this body has one.
fn return_local(body: &Body) -> Option<LocalId> {
    body.locals.iter().position(|l| l.name == "_ret").map(|i| LocalId(i as u32))
}

/// The place a local's reference value was created from.
fn reference_origin(body: &Body, local: LocalId) -> Option<MovePath> {
    for block in &body.blocks {
        for stmt in &block.statements {
            if let Statement::Assign { place, value: Rvalue::Ref { place: borrowed, .. }, .. } = stmt {
                if place.local == local && place.projection.is_empty() {
                    return Some(path_of(borrowed));
                }
            }
        }
    }
    None
}

fn places_overlap(a: &MovePath, b: &MovePath) -> bool {
    path_starts_with(a, b) || path_starts_with(b, a)
}

fn report_borrow_conflicts(live: &[&Borrow], body: &Body, diagnostics: &mut Vec<Diagnostic>) {
    for i in 0..live.len() {
        for j in (i + 1)..live.len() {
            let (a, b) = (live[i], live[j]);
            if !places_overlap(&a.place, &b.place) {
                continue;
            }
            // Shared borrows never conflict with each other.
            if !a.mutable && !b.mutable {
                continue;
            }
            // Order them so the message reads chronologically.
            let (first, second) = if a.created <= b.created { (a, b) } else { (b, a) };
            let name = place_name(body, &first.place);
            let (code, message) = if first.mutable && !second.mutable {
                (
                    "E0205",
                    format!("cannot borrow `{name}` as shared because it is borrowed as mutable"),
                )
            } else if !first.mutable && second.mutable {
                (
                    "E0205",
                    format!("cannot borrow `{name}` as mutable because it is also borrowed as shared"),
                )
            } else {
                (
                    "E0204",
                    format!("cannot borrow `{name}` as mutable more than once at a time"),
                )
            };
            let first_kind = if first.mutable { "mutable" } else { "shared" };
            let d = Diagnostic::error(code, message)
                .at(second.origin)
                .with_cause(format!(
                    "the {first_kind} borrow is still in use here, and borrows of the same place must not overlap"
                ))
                .with_label(first.origin, format!("the {first_kind} borrow starts here"))
                .with_suggestion(
                    Suggestion::new(
                        "end the earlier borrow before creating this one",
                        second.origin,
                        String::new(),
                    )
                    .with_detail("a borrow lasts until its last use; move that use after this point"),
                );
            diagnostics.push(d);
        }
    }
}

fn report_borrowed_write(
    borrows: &[Borrow],
    path: &MovePath,
    pos: Pos,
    span: Span,
    body: &Body,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for borrow in borrows {
        // A borrow cannot outlive the write that invalidates it.
        if !borrow.live_at(pos) {
            continue;
        }
        // The borrow's own creation writes the reference, not the referent.
        if pos == borrow.created {
            continue;
        }
        if !places_overlap(&borrow.place, path) {
            continue;
        }
        let name = place_name(body, path);
        let kind = if borrow.mutable { "mutably" } else { "shared" };
        let d = Diagnostic::error(
            "E0206",
            format!("cannot assign to `{name}` because it is borrowed"),
        )
        .at(span)
        .with_cause(format!(
            "`{name}` is still borrowed {kind} at this point, so changing it would be seen through the reference"
        ))
        .with_label(
            borrow.origin,
            format!("borrowed {kind} here, and that borrow is used later"),
        )
        .with_suggestion(
            Suggestion::new("finish using the borrow before assigning", span, String::new())
                .with_detail("the last use of the borrow must come before this assignment"),
        );
        diagnostics.push(d);
    }
}

/// Collects every reference-producing rvalue, in program order.
fn collect_borrows(body: &Body) -> Vec<Borrow> {
    let mut out = Vec::new();
    for (i, block) in body.blocks.iter().enumerate() {
        for (sidx, stmt) in block.statements.iter().enumerate() {
            let Statement::Assign { place, value, span } = stmt else { continue };
            if let Rvalue::Ref { mutable, place: borrowed } = value {
                let pos = Pos { block: i as u32, index: sidx as u32 };
                out.push(Borrow {
                    place: path_of(borrowed),
                    mutable: *mutable,
                    holder: place.local,
                    origin: *span,
                    created: pos,
                    last_use: pos,
                });
            }
        }
    }
    out
}

/// Follows the reference local through `X = move Y` transfers.
///
/// Lowering parks a borrow in a temporary and then moves it into the binding
/// that names it (`let a = &mut x` becomes `_t = &mut _x`, `_a = move _t`). The
/// borrow's life is therefore the life of the *final* holder, not of the
/// temporary that first received it.
fn final_holder(body: &Body, start: LocalId) -> LocalId {
    let mut holder = start;
    // A reference is moved at most a few times; bound the walk defensively.
    for _ in 0..16 {
        let mut next = None;
        for block in &body.blocks {
            for stmt in &block.statements {
                // A shared reference is `Copy`, so lowering transfers it with
                // `copy`; a mutable one with `move`. Both re-home the borrow.
                if let Statement::Assign { place, value: Rvalue::Use(op), .. } = stmt {
                    let src = match op {
                        Operand::Move(p) | Operand::Copy(p) => Some(p),
                        Operand::Constant(_) => None,
                    };
                    if let Some(src) = src {
                        if src.projection.is_empty() && src.local == holder && place.projection.is_empty() {
                            next = Some(place.local);
                        }
                    }
                }
            }
        }
        match next {
            Some(n) if n != holder => holder = n,
            _ => break,
        }
    }
    holder
}

/// Fills in the last use of each borrow's reference local.
fn compute_lifetimes(body: &Body, borrows: &mut [Borrow]) {
    for borrow in borrows.iter_mut() {
        borrow.holder = final_holder(body, borrow.holder);
    }

    // Every position at which each local is mentioned.
    let mut mentions: BTreeMap<LocalId, Vec<Pos>> = BTreeMap::new();
    for (i, block) in body.blocks.iter().enumerate() {
        let bid = i as u32;
        for (sidx, stmt) in block.statements.iter().enumerate() {
            let pos = Pos { block: bid, index: sidx as u32 };
            if let Statement::Assign { value, place, .. } = stmt {
                for l in locals_in_rvalue(value) {
                    mentions.entry(l).or_default().push(pos);
                }
                // Re-assigning the reference local ends the old borrow.
                if place.projection.is_empty() {
                    mentions.entry(place.local).or_default().push(pos);
                }
            }
        }
        let tpos = Pos::terminator(bid, block.statements.len());
        match &block.terminator {
            Terminator::Call { callee, args, .. } => {
                for l in locals_in_operand(callee) {
                    mentions.entry(l).or_default().push(tpos);
                }
                for a in args {
                    for l in locals_in_operand(a) {
                        mentions.entry(l).or_default().push(tpos);
                    }
                }
            }
            Terminator::SwitchInt { discr, .. } => {
                for l in locals_in_operand(discr) {
                    mentions.entry(l).or_default().push(tpos);
                }
            }
            Terminator::Drop { place, .. } => {
                mentions.entry(place.local).or_default().push(tpos);
            }
            Terminator::Goto(_) | Terminator::Return | Terminator::Unreachable => {}
        }
    }

    for borrow in borrows.iter_mut() {
        let last = mentions
            .get(&borrow.holder)
            .and_then(|v| v.iter().filter(|p| **p > borrow.created).max().copied())
            .unwrap_or(borrow.created);
        borrow.last_use = last;
    }
}

fn locals_in_operand(op: &Operand) -> Vec<LocalId> {
    match op {
        Operand::Copy(p) | Operand::Move(p) => vec![p.local],
        Operand::Constant(_) => Vec::new(),
    }
}

fn locals_in_rvalue(value: &Rvalue) -> Vec<LocalId> {
    let mut out = Vec::new();
    let scan = |op: &Operand, out: &mut Vec<LocalId>| out.extend(locals_in_operand(op));
    match value {
        Rvalue::Use(op) => scan(op, &mut out),
        Rvalue::Binary { lhs, rhs, .. } => {
            scan(lhs, &mut out);
            scan(rhs, &mut out);
        }
        Rvalue::Unary { operand, .. } | Rvalue::Cast { operand, .. } | Rvalue::PtrToInt(operand) => {
            scan(operand, &mut out)
        }
        Rvalue::Aggregate { operands, .. } => {
            for o in operands {
                scan(o, &mut out);
            }
        }
        Rvalue::Repeat { operand, .. } => scan(operand, &mut out),
        Rvalue::Ref { place, .. } | Rvalue::Reborrow { place, .. } => out.push(place.local),
        Rvalue::Discriminant(p) | Rvalue::Len(p) => out.push(p.local),
    }
    out
}

/// A readable name for a place, e.g. `p` or `p.x`.
fn place_name(body: &Body, path: &MovePath) -> String {
    let mut name = String::new();
    for elem in path {
        match elem {
            PathElem::Local(l) => name.push_str(&local_name(body, LocalId(*l))),
            PathElem::Field(f) => {
                name.push('.');
                name.push_str(&f.to_string());
            }
            PathElem::Index(_) => name.push_str("[..]"),
            PathElem::Deref => name.push_str(".*"),
        }
    }
    name
}
