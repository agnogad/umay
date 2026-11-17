# ADR 0008 — Ownership and borrow checking strategy

- **Status**: accepted
- **Date**: Phase 4
- **Supersedes**: nothing

## Context

Phase 3 left MIR in which every move is an `Operand::Move`, every borrow an
`Rvalue::Ref`, and every drop a `Terminator::Drop`. What was missing was the
analysis that decides whether those operations are legal.

Two properties of the memory model drive the design: a value is owned by exactly
one binding, and a place may be read by many or written by one, but not both.
Both are properties of a *program point*, not of a statement in isolation: "is
this place initialised" and "is this borrow still needed" are questions about all
the paths that reach the point.

## Decision

### 1. Places are tracked as paths, not as whole locals

A `MovePath` is a local plus a sequence of field, index and deref steps.
`[Local(0), Field(1)]` and `[Local(0), Field(2)]` are different places, so
borrowing `p.x` while writing `p.y` is accepted, and moving `p.x` out of `p`
leaves `p.y` intact.

The alternative — one bit per local — is simpler and rejects `p.x`/`p.y` disjoint
access, which is the single most common thing a borrow checker gets wrong in
practice. Paths cost a `BTreeSet` per node and buy accuracy users can feel.

A runtime index (`arr[i]`) degenerates to `Index(None)`, which covers the whole
array. That is coarse but sound: it can never permit a use that might be
uninitialised, and a precise answer is impossible without knowing `i`.

### 2. Definite initialisation is a forward dataflow with intersection

At each block entry, the set of initialised paths is the *intersection* over the
predecessors. A place counts as initialised only when every path to this point
initialised it, which is exactly what "definitely" means.

The iteration repeats until no block's entry state changes. The lattice is a
subset lattice over a fixed path set, so it is finite and the transfer function
monotone; the round limit exists only so that a bug becomes a wrong answer rather
than a hang.

### 3. Borrows live until their last use, not until their scope ends

This is the decision that makes the checker usable. In

```um
let a = &x
let n = read(a)
let b = &mut x
```

`a` and `b` never overlap, because `a`'s last use is before `b` exists. A
scope-based rule would reject this, and almost every real function.

Lifetime is computed in two steps: gather the positions at which each reference
local is mentioned, then take the maximum such position after the borrow was
created as its last use. Mentions include copying the reference, passing it to a
call, and dropping it.

### 4. Positions, not blocks, for borrow lifetimes

The first implementation compared borrows per *block*. It was sound and nearly
useless: every statement in a block looked simultaneous with every other, so
`let a = &mut x  x = 5` reported a conflict even when the borrow was already
dead.

Switching to a linear `(block, statement index)` position fixed that, and the
code became simpler: a borrow is live on an interval, and two borrows conflict
when their intervals and their places overlap.

### 5. Lowering transfers ownership explicitly

The lowering marks an owned value as moved at every transfer: `let` bindings and
arguments become `Operand::Move` with a `SetDropFlag(false)`, so the analysis
sees one owner at every point. Before this, `let a = make()` left the temporary
and the binding both owning the value, and the checker reported a
use-after-move on the temporary — a bug in lowering that the checker correctly
found.

### 6. It runs only on MIR that type-checked

Ownership checking runs after semantic analysis has produced no errors. A type
error makes the shape of the program uncertain, and reporting move errors for a
program whose types do not check buries the real problem under consequences of
it.

## Consequences

- Diagnostics name a place (`p.x`) rather than a local index, because paths are
  the unit of analysis and of reporting.
- Borrow conflicts can name both origins, so "cannot borrow mutably" is always
  paired with the earlier borrow that causes it.
- The checker rejects some programs a full lifetime-inference implementation
  would accept. `E0203` is reserved for saying so explicitly: a limitation
  reported under a code of its own is never mistaken for a bug.
- Six of the thirteen documented codes are not enforced
  (`E0202` is unreachable by construction; `E0203`, `E0208`, `E0209`, `E0212`
  and `E0213` are deferred). The memory model lists exactly which, rather than
  implying full coverage.

## Alternatives considered

**Track one flag per local.** Rejected: it rejects disjoint field access, which
users write constantly.

**Check borrows per block.** Rejected after building it: too coarse to be
useful.

**Use scopes for borrow lifetimes.** Rejected: it rejects sequential,
non-overlapping borrows, which is most correct code.

**Report one diagnostic per move, at the move.** Rejected: the useful message is
where the value is used *after* the move, since that is the line the user must
change.
