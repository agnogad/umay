# ADR 0006 — Semantic analysis in one pass over a whole file

- **Status:** Accepted
- **Date:** 2026-11-07
- **Phase:** 2

## Context

Phase 2 turns the AST into HIR: it resolves names, computes types, checks
`match` exhaustiveness, and reports diagnostics in the `E10xx` range.

Three structural decisions came up that are hard to reverse later.

1. **Declaration order.** A function may call one declared later, and a struct
   may contain itself through `Option<Self>`.
2. **Where inference lives.** UMAY has no type annotations on `let` unless the
   author writes one, so literal types must be inferred.
3. **How much `match` exhaustiveness to prove.**

## Decision

### 1. Three passes over item declarations, one over bodies

`collect_items` registers every item name and allocates `DefId`/`FnId` before
any type is resolved. `resolve_signatures` then resolves field, variant,
parameter and return types. Finally `check_bodies` walks function bodies.

This means declaration order never matters at file scope. A name that is
declared anywhere in the file is visible everywhere in it. Modules get their
own scopes in a later phase; within one file, order does not matter.

### 2. Inference is local to a function body

Integer and float literals start as fresh inference variables (`IntVar`,
`FloatVar`) that adopt the first concrete type they meet — an annotation, a
parameter type, or another operand. Literals that never meet one default to
`i32`/`f64` at the end of the enclosing `let`.

There is no unification engine across functions. This keeps type checking linear
in the size of the program and makes separate compilation possible, at the cost
of requiring annotations where a value's type is not locally determined — which
is exactly what `E1002` reports.

### 3. Exhaustiveness is sound but conservative

`match` is accepted as exhaustive when it has an irrefutable arm (a binding or
`_` without a guard) or covers every variant of the scrutinee's enum, or both
`true` and `false` for `bool`.

Ranges and integer literals are **not** treated as covering an integer type.
Proving coverage of `u8` by enumeration is possible in principle, but accepting
a partial `match` on integers hides real bugs. The checker never accepts a
non-exhaustive `match`; it may reject one that a smarter analyser would accept,
and the diagnostic's explanation says so.

## Consequences

- Every emitted code must have an `umay explain` entry; a test enforces this.
- Rejecting a program is always a diagnostic, never a panic or a silent
  `Error` type that would hide the cause.
- `TyKind::Error` propagates through checking and suppresses follow-on
  diagnostics, so one mistake reports once.
- A future richer exhaustiveness check can widen what is accepted without
  changing what is rejected, so the conservative choice is forward-compatible.

## Alternatives considered

- **Unification across the whole program.** Would remove some annotations, but
  makes separate compilation and incremental checking much harder.
- **Skipping exhaustiveness until the backend exists.** Would let invalid
  programs through Phase 2, which the phase's exit criteria forbid.
- **Two-phase name resolution with explicit `use` ordering.** Adds ceremony for
  no benefit at file scope.
