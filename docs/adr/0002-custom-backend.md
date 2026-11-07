# ADR 0002 — Custom native backend, no LLVM or Cranelift

**Status:** accepted
**Date:** Phase 0

## Context

LLVM and Cranelift would give optimizations and target coverage for free. The
project explicitly requires a custom backend so that UMAY controls its
compilation model, diagnostic quality and binary size.

## Decision

UMAY ships its own code generator. The first target is x86_64 Linux (ELF64,
System V AMD64 ABI). The initial implementation calls the system linker for the
final link. Backends are target-abstracted so additional targets do not touch the
frontend.

## Consequences

- Slower path to peak code quality than LLVM; mitigated by starting with
  correctness-first instruction selection and a small, well-tested optimisation
  set (constant folding, DCE, copy propagation, simple inlining).
- Full control over object layout, ABI choices and debuggability.
- Optimisation must be conservative: semantics preservation is a hard
  requirement and is covered by tests.

## Alternatives considered

- **LLVM**: forbidden by project requirements; also slow compiles and large
  dependency footprint.
- **Cranelift**: lighter than LLVM but still a shared-IR dependency that would
  constrain ownership/MIR design.
