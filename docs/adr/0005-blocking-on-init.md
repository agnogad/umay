# ADR 0005 — `let` always initializes; deferred initialization is rejected

**Status:** accepted
**Date:** Phase 1

## Context

Languages commonly allow `let x: T` followed by later assignment. This makes
definite-initialization analysis necessary: a variable may be uninitialised, and
the checker must prove it is initialized before every use on every path.

## Decision

Every `let` requires an initializer. Assigning later requires first initializing
with a value (`let mut x = 0`). Violations are a syntax error (`E0303`) raised at
parse time.

## Consequences

- Definite initialization becomes trivial for locals introduced by `let`:
  a binding is initialized at its declaration or it does not exist.
- The move checker still needs flow analysis for *moved* values and for
  partially moved aggregates, which is where the real work remains.
- Diagnostics are better: "no initializer" is reported where it happens rather
  than as a later "possibly uninitialized" error far from the cause.
- Users needing deferred construction use `Option<T>` explicitly, which is
  visible in the type.

## Alternatives considered

- **Allow `let x: T` with later assignment**: requires full definite-init
  dataflow for plain locals and produces hard-to-explain errors.
- **Allow it only for `let mut`**: inconsistent and still requires the analysis.
