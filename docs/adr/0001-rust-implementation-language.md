# ADR 0001 — Implement the compiler in Rust

**Status:** accepted
**Date:** Phase 0

## Context

UMAY needs a compiler written in a language that supports safe data structure
manipulation, fast development, good concurrency and a mature testing ecosystem.

## Decision

The reference compiler is written in Rust. The production *backend* is written
by the UMAY project in Rust too, but it emits native machine code directly; it is
not LLVM, Cranelift, or a binding to either.

## Consequences

- Memory-safe compiler development, with `unsafe_code = "deny"` at crate level.
- Fast iteration and strong test tooling.
- The compiler's own performance is adequate for a systems language of this size.
- Self-hosting is possible later but requires the specification, architecture and
  standard library to stabilize first. It is explicitly not a Phase 1–9 goal.

## Alternatives considered

- **C++**: faster to bootstrap a backend but weaker tooling and memory safety.
- **Self-hosting immediately**: rejected — a language cannot bootstrap itself
  before its memory model is proven.
