# UMAY Roadmap

Each phase has explicit exit criteria. A phase is not "complete" until its
criteria are met and the full test suite passes.

Legend: **done** · **in progress** · planned

---

## Phase 0 — Architecture and RFC · **done**

Deliverables:

- [x] `README.md`
- [x] `docs/language-specification.md` (v0.1)
- [x] `docs/compiler-architecture.md`
- [x] `docs/memory-model.md`
- [x] `docs/type-system.md`
- [x] `docs/diagnostics.md`
- [x] `docs/build-system.md`
- [x] `docs/adr/` — Architecture Decision Records
- [x] `ROADMAP.md`
- [x] `CONTRIBUTING.md`
- [x] Minimal Rust workspace with the `umay` binary

Exit criteria: syntax frozen for v0.1; module boundaries documented; diagnostic
code ranges reserved.

---

## Phase 1 — Lexer and Parser · **done**

Deliverables:

- [x] Source manager: `FileId`, `Span`, line/column mapping (character-based)
- [x] Tokenizer with full literal support (radix integers, suffixes, chars,
      strings, escapes, interpolation, raw braces)
- [x] Comment handling including nested block comments
- [x] AST with spans on every node
- [x] Recursive-descent parser: items, types, statements, expressions, patterns
- [x] Precedence climbing; assignment and compound assignment
- [x] Newline-aware optional semicolons
- [x] Error recovery: item-level and statement-level, no duplicate reports
- [x] Nesting-depth guard (`E0052`)
- [x] Diagnostics: human rendering with carets, versioned JSON schema
- [x] `umay explain` database covering every emitted code
- [x] CLI: `check`, `ast`, `explain`, `doctor`, `new`, `clean`, `version`
- [x] Tests: 30 unit + 49 integration, including panic-freedom invariants

Exit criteria (met):

- parse and validate realistic UMAY programs,
- reject malformed programs with a stable code and a cause line,
- never panic on truncated or mutated input,
- JSON output valid and versioned,
- every emitted code has an `umay explain` entry.

Known limitations at the end of Phase 1 are listed in the final report of this
milestone and in §13 of the language specification.

---

## Phase 2 — Semantic Analysis · planned

- Scopes, symbol tables, module resolution from the entry point.
- Type representations and local inference (defaults: `i32`, `f64`).
- Struct/enum/tuple/array/slice/reference typing.
- `Option`/`Result` as ordinary enums in the standard library.
- Exhaustiveness checking for `match`.
- Operator typing; `as` cast validation.
- Trait declaration and impl collection (bounds checking minimal).
- `pub` visibility enforcement.
- Diagnostics: `E1001`–`E1015`.

Exit criteria: a suite of accept/reject program pairs exists for every rule; no
invalid program is accepted; every valid fixture produces an HIR.

---

## Phase 3 — HIR and MIR · planned

- HIR: names resolved, sugar lowered (`for`, `?`, method calls, interpolation).
- MIR: basic blocks, places, assignments, moves, borrows, drops, calls.
- Lowering from HIR to MIR, with drop elaboration.

Exit criteria: every valid program lowers to MIR; MIR is printable and
round-trippable for tests.

---

## Phase 4 — Ownership and Borrow Checking · planned

- Definite initialisation (CFG dataflow).
- Move analysis; partial moves.
- Borrow analysis with both conflicting origins reported.
- Lifetime inference and elision rules from `docs/memory-model.md`.
- Drop elaboration with drop flags on conditional paths.
- `unsafe` tracking and raw-pointer restrictions.
- Diagnostics: `E0201`–`E0213`.
- Adversarial test suite: use-after-move, double mutable borrow,
  mutable/immutable conflict, dangling reference, returning local reference,
  invalid pointer deref, invalid drop order, uninitialised read, data race.

Exit criteria: every adversarial fixture is rejected with the documented code;
the accept corpus still compiles. UMAY only claims Rust-comparable safety after
this phase's suite is reviewed and extended by fuzzing.

---

## Phase 5 — Custom x86_64 Backend · planned

- Target description abstraction.
- Machine instruction representation and instruction selection from MIR.
- Linear-scan register allocation with spilling.
- Stack frames, System V AMD64 ABI calling convention.
- ELF64 object emission with relocations.
- Linking via the system linker.
- Optimisation levels 0–3: constant folding/propagation, DCE, unreachable block
  elimination, copy propagation, simple inlining, arithmetic simplification.

Exit criteria: `umay build` produces a working Linux executable for a program
using arithmetic, control flow, structs, and calls; output is verified by tests.

---

## Phase 6 — Runtime and Standard Library · planned

- Allocation (`alloc`/`dealloc`), `String`, `Vec<T>`, `HashMap`.
- `Option`, `Result`, `Iterator` basics.
- I/O: stdout/stderr/`print`/`println`, buffered readers.
- `fs`: read, write, open, metadata.
- `process`: exit codes, arguments.
- `net`: TCP basics (only after the memory model is stable).

Exit criteria: a non-trivial CLI program can be written and built with only the
standard library.

---

## Phase 7 — AI Developer Experience · planned

- LSP server: diagnostics, hover, go-to-definition, completion.
- `umay fmt` — deterministic formatter.
- `umay lint` — style and correctness lints (`W*` codes).
- JSON diagnostics for build and run, not just check.
- Stable machine-readable compiler metadata (symbol and type dumps).

Exit criteria: an editor session on a multi-file project shows live diagnostics
and navigation; `umay fmt` is idempotent on the full test corpus.

---

## Phase 8 — Build System and Packages · planned

- Content-addressed cache with the key derivation in
  `docs/build-system.md`.
- Module-level invalidation; parallel compilation.
- Reproducible builds.
- `upm` package manager with `umay.lock`.

Exit criteria: incremental rebuild after a one-module edit does not recompile
independent modules; no `target/` ever appears in a project; two clean builds of
the same input produce identical artifacts.

---

## Phase 9 — Stabilization · planned

- Extended test corpora, property-based and fuzz testing at each phase boundary.
- Benchmarks: clean time, incremental time, compiler RSS, runtime speed, binary
  size — each compared against Go, Rust, Zig and C equivalents where meaningful.
- Documentation review; specification promoted from draft to 1.0.
- Release engineering: versioning, release notes, binary distribution.

Exit criteria: UMAY MVP release.

---

## Self-hosting (after Phase 9)

Only once the specification, compiler architecture and standard library are
stable. The Rust compiler remains the reference implementation until the UMAY
compiler passes the same test corpus.

---

## Rules that do not relax

- No LLVM or Cranelift in the production backend.
- No mandatory garbage collector.
- No project-local `target/` directory.
- No claimed feature without tests.
- No weakening of memory safety to simplify implementation.
