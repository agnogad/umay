# Changelog

All notable changes to UMAY are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[Semantic Versioning](https://semver.org/spec/v2.0.0.html). The language
specification carries its own version (`v1.0`), which moves only when the
language surface changes; the compiler's package version can move for
implementation-only changes.

## [0.1.0] — 2026-11-08

The first release: a language, a compiler, a standard library and a toolchain,
all working end to end. This version is the MVP that Phase 9 of the roadmap
defines — every phase from architecture to stabilization is done.

### Language

- Statically typed, natively compiled systems language with ownership and
  borrow checking and no garbage collector.
- Modules from files (`mod name;`), with dependencies reachable as modules.
- Generics with inline trait bounds, traits and `impl` blocks, `Option`/`Result`
  as ordinary enums, exhaustive `match`, `?` error propagation.
- Explicit, checked `as` casts, no implicit numeric conversion.

### Compiler

- Lexer and recursive-descent parser with error recovery and a nesting guard.
- Semantic analysis: interning type context, three-pass checking so declaration
  order never matters, local inference, exhaustiveness checking.
- HIR and MIR with a set of optimisations.
- Custom x86_64 backend: instruction selection, linear-scan register allocation
  with honest handling of fixed registers, and an assembler printer. No LLVM,
  no Cranelift.
- Ownership and borrow checking, drop elaboration.
- Diagnostics as data: stable codes, one rendering for humans and one for
  machines, `umay explain CODE` for every code.

### Standard library and runtime

- Written in UMAY and compiled by the same backend (ADR 0010): strings, `Vec`,
  `HashMap`, JSON over counted bytes, TCP, files and processes, and a
  concurrency model with `Send`/`Sync` checked at the spawn boundary.

### Toolchain

- `umay check | build | run | asm | ast | mir | fmt | lint | explain | doctor |
  new | clean | lock | metadata | version`.
- A language server that runs the compiler rather than a copy of it: live
  diagnostics, hover, go-to-definition (including across modules), completion,
  outline and formatting over JSON-RPC.
- A deterministic formatter that rewrites the gaps between tokens and nothing
  else.
- A content-addressed build cache keyed on the compiler's own binary, the source
  bytes and the module tree, so a stale artifact is impossible and an unchanged
  rebuild recompiles neither the program nor the runtime.
- `upm`: a manifest (`umay.toml`), a lockfile (`umay.lock`), resolution of the
  whole dependency graph, and fetching from a registry named by `UMAY_REGISTRY`,
  with every fetched package pinned by SHA-256.

### Known gaps

- `where` clauses and `let`-`else` are not parsed; a program that uses them is a
  clear syntax error. Use inline bounds and `match`.
- `pub` is accepted but there is no cross-module visibility enforcement yet.
- Attributes are parsed and carried but have no semantics; an unused one is
  `W0010`.
- Zig is not part of the build-time comparison in `docs/measurements.md` because
  it was not installed on the measurement machine; it is named, not dropped.

[0.1.0]: https://example.invalid/umay/releases/tag/v0.1.0
